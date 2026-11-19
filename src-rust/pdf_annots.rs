use crate::font_shaper::{
    encode_cid_glyph_bytes, encode_winansi_bytes, ensure_page_font_resource, resolve_dict,
    resolve_object, PdfFontWriteMode,
};
use crate::pdf_objects::{get_page_transform, hex_to_rgb, PageTransform};
use base64::Engine;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

fn as_f32(obj: &Object) -> Option<f32> {
    match obj {
        Object::Real(v) => Some(*v),
        Object::Integer(v) => Some(*v as f32),
        _ => None,
    }
}

fn pdf_text_to_string(obj: &Object) -> String {
    match obj {
        Object::String(bytes, _) => {
            if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
                let mut u16s = Vec::with_capacity((bytes.len() - 2) / 2);
                for chunk in bytes[2..].chunks_exact(2) {
                    u16s.push(u16::from_be_bytes([chunk[0], chunk[1]]));
                }
                String::from_utf16_lossy(&u16s)
            } else {
                String::from_utf8_lossy(bytes).to_string()
            }
        }
        Object::Name(n) => String::from_utf8_lossy(n).to_string(),
        _ => String::new(),
    }
}

fn string_to_pdf_text_object(text: &str) -> Object {
    if text.chars().all(|c| (c as u32) <= 0x7F) {
        Object::string_literal(text)
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_be_bytes());
        }
        Object::String(bytes, lopdf::StringFormat::Hexadecimal)
    }
}

pub fn vp_rect_to_pdf_rect(pt: &PageTransform, rect_vp: [f32; 4]) -> [f32; 4] {
    let [vx, vy, vw, vh] = rect_vp;
    let p0 = pt.vp_to_pdf(vx, vy);
    let p1 = pt.vp_to_pdf(vx + vw, vy + vh);
    [
        p0.0.min(p1.0),
        p0.1.min(p1.1),
        p0.0.max(p1.0),
        p0.1.max(p1.1),
    ]
}

pub fn vp_rect_to_quadpoints(pt: &PageTransform, rect_vp: [f32; 4]) -> Vec<f32> {
    let [vx, vy, vw, vh] = rect_vp;
    let tl = pt.vp_to_pdf(vx, vy);
    let tr = pt.vp_to_pdf(vx + vw, vy);
    let bl = pt.vp_to_pdf(vx, vy + vh);
    let br = pt.vp_to_pdf(vx + vw, vy + vh);
    vec![tl.0, tl.1, tr.0, tr.1, bl.0, bl.1, br.0, br.1]
}

fn color_array_to_hex(arr: &[Object]) -> String {
    if arr.len() >= 3 {
        let r = (as_f32(&arr[0]).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        let g = (as_f32(&arr[1]).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        let b = (as_f32(&arr[2]).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    } else if arr.len() == 1 {
        let g = (as_f32(&arr[0]).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", g, g, g)
    } else {
        "#facc15".to_string()
    }
}

/// Smooth polyline through Catmull-Rom splines into cubic Bezier segments:
/// returns Vec<(cp1, cp2, end)> after initial Move(pts[0]).
fn catmull_rom_to_beziers(pts: &[(f32, f32)]) -> Vec<((f32, f32), (f32, f32), (f32, f32))> {
    if pts.len() < 2 {
        return Vec::new();
    }
    if pts.len() == 2 {
        let p0 = pts[0];
        let p1 = pts[1];
        let c1 = (p0.0 + (p1.0 - p0.0) / 3.0, p0.1 + (p1.1 - p0.1) / 3.0);
        let c2 = (p0.0 + 2.0 * (p1.0 - p0.0) / 3.0, p0.1 + 2.0 * (p1.1 - p0.1) / 3.0);
        return vec![(c1, c2, p1)];
    }
    let mut out = Vec::with_capacity(pts.len() - 1);
    for i in 0..(pts.len() - 1) {
        let p0 = if i == 0 { pts[0] } else { pts[i - 1] };
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = if i + 2 < pts.len() { pts[i + 2] } else { p2 };
        let cp1 = (
            p1.0 + (p2.0 - p0.0) / 6.0,
            p1.1 + (p2.1 - p0.1) / 6.0,
        );
        let cp2 = (
            p2.0 - (p3.0 - p1.0) / 6.0,
            p2.1 - (p3.1 - p1.1) / 6.0,
        );
        out.push((cp1, cp2, p2));
    }
    out
}

fn ensure_gs_in_ap(
    doc: &mut lopdf::Document,
    alpha: f32,
    multiply: bool,
) -> (String, Dictionary) {
    let a = alpha.clamp(0.05, 1.0);
    let mut gs = Dictionary::new();
    gs.set("Type", Object::Name(b"ExtGState".to_vec()));
    gs.set("ca", Object::Real(a));
    gs.set("CA", Object::Real(a));
    if multiply {
        gs.set("BM", Object::Name(b"Multiply".to_vec()));
    }
    let gs_id = doc.add_object(Object::Dictionary(gs));
    let mut gs_map = Dictionary::new();
    gs_map.set("GS0", Object::Reference(gs_id));
    let mut res = Dictionary::new();
    res.set("ExtGState", Object::Dictionary(gs_map));
    ("GS0".to_string(), res)
}

fn create_form_ap_stream(
    doc: &mut lopdf::Document,
    bbox_pdf: [f32; 4],
    ops: Vec<Operation>,
    resources: Dictionary,
) -> Result<ObjectId, String> {
    let encoded = Content { operations: ops }
        .encode()
        .map_err(|e| format!("Failed encoding /AP stream: {}", e))?;
    let mut form_dict = Dictionary::new();
    form_dict.set("Type", Object::Name(b"XObject".to_vec()));
    form_dict.set("Subtype", Object::Name(b"Form".to_vec()));
    form_dict.set("FormType", Object::Integer(1));
    form_dict.set(
        "BBox",
        Object::Array(vec![
            Object::Real(bbox_pdf[0]),
            Object::Real(bbox_pdf[1]),
            Object::Real(bbox_pdf[2]),
            Object::Real(bbox_pdf[3]),
        ]),
    );
    form_dict.set(
        "Matrix",
        Object::Array(vec![
            Object::Real(1.0),
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(1.0),
            Object::Real(0.0),
            Object::Real(0.0),
        ]),
    );
    form_dict.set("Resources", Object::Dictionary(resources));
    let mut stream = Stream::new(form_dict, encoded);
    let _ = stream.compress();
    Ok(doc.add_object(Object::Stream(stream)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfAnnotationDto {
    pub id: String,
    pub page: usize,
    pub subtype: String, // "Ink"|"Highlight"|"Underline"|"StrikeOut"|"Text"|"FreeText"|"Line"|"Square"|"Circle"|"Polygon"|"PolyLine"|"Stamp"|"Link"|"Widget"
    pub bbox_vp: [f32; 4],
    pub color: String,
    pub fill_color: Option<String>,
    pub opacity: f32,
    pub stroke_width: f32,
    pub contents: String,
    pub author: String,
    pub is_highlighter: bool,
    pub points_vp: Vec<Vec<[f32; 2]>>,
    pub rects_vp: Vec<[f32; 4]>,
    pub font_size: f32,
    pub font_family: String,
    pub align: String,
    pub stamp_name: String,
    pub url: String,
    pub target_page: Option<usize>,
    pub line_end_arrow: bool,
    pub cloudy: bool,
    pub field_name: String,
    pub field_type: String,
    pub field_value: String,
    pub field_options: Vec<String>,
}

pub fn extract_page_annotations(
    doc: &lopdf::Document,
    page: usize,
) -> Result<Vec<PdfAnnotationDto>, String> {
    let pages = doc.get_pages();
    let page_num = page.max(1) as u32;
    let page_id = *pages
        .get(&page_num)
        .ok_or_else(|| format!("Page {} not found", page_num))?;
    let pt = get_page_transform(doc, page_id);
    let page_id_to_num: HashMap<ObjectId, usize> =
        pages.iter().map(|(&num, &oid)| (oid, num as usize)).collect();

    let page_dict = doc
        .get_dictionary(page_id)
        .map_err(|e| format!("Failed reading page dict: {}", e))?;

    let annots_arr = match page_dict.get(b"Annots") {
        Ok(obj) => match resolve_object(doc, obj) {
            Some(Object::Array(arr)) => arr.clone(),
            _ => return Ok(Vec::new()),
        },
        Err(_) => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    for (idx, a_ref) in annots_arr.iter().enumerate() {
        let a_id = a_ref.as_reference().ok();
        let Some(dict) = resolve_dict(doc, a_ref) else {
            continue;
        };
        let subtype = dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .unwrap_or_default();
        if subtype == "Popup" {
            continue;
        }
        let nm = dict
            .get(b"NM")
            .ok()
            .map(pdf_text_to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if let Some(id) = a_id {
                    format!("ann-{}-{}-{}", page_num, id.0, id.1)
                } else {
                    format!("ann-{}-{}", page_num, idx)
                }
            });

        let rect_pdf = dict
            .get(b"Rect")
            .ok()
            .and_then(|o| resolve_object(doc, o))
            .and_then(|o| match o {
                Object::Array(a) if a.len() >= 4 => Some([
                    as_f32(&a[0]).unwrap_or(0.0),
                    as_f32(&a[1]).unwrap_or(0.0),
                    as_f32(&a[2]).unwrap_or(0.0),
                    as_f32(&a[3]).unwrap_or(0.0),
                ]),
                _ => None,
            })
            .unwrap_or([0.0, 0.0, 40.0, 20.0]);
        let bbox_vp = pt.pdf_rect_to_vp(
            rect_pdf[0].min(rect_pdf[2]),
            rect_pdf[1].min(rect_pdf[3]),
            rect_pdf[0].max(rect_pdf[2]),
            rect_pdf[1].max(rect_pdf[3]),
        );

        let color = dict
            .get(b"C")
            .ok()
            .and_then(|o| resolve_object(doc, o))
            .and_then(|o| o.as_array().ok())
            .map(|a| color_array_to_hex(a))
            .unwrap_or_else(|| {
                if subtype == "Highlight" {
                    "#facc15".into()
                } else if subtype == "Underline" {
                    "#10b981".into()
                } else if subtype == "StrikeOut" {
                    "#ef4444".into()
                } else {
                    "#2f6bff".into()
                }
            });

        let fill_color = dict
            .get(b"IC")
            .ok()
            .and_then(|o| resolve_object(doc, o))
            .and_then(|o| o.as_array().ok())
            .map(|a| color_array_to_hex(a));

        let opacity = dict
            .get(b"CA")
            .ok()
            .and_then(|o| resolve_object(doc, o))
            .and_then(as_f32)
            .unwrap_or(1.0);

        let stroke_width = dict
            .get(b"BS")
            .ok()
            .and_then(|o| resolve_dict(doc, o))
            .and_then(|bs| bs.get(b"W").ok().and_then(as_f32))
            .unwrap_or(2.0);

        let contents = dict
            .get(b"Contents")
            .ok()
            .map(pdf_text_to_string)
            .unwrap_or_default();
        let author = dict.get(b"T").ok().map(pdf_text_to_string).unwrap_or_default();

        let mut points_vp: Vec<Vec<[f32; 2]>> = Vec::new();
        if let Ok(il) = dict.get(b"InkList") {
            if let Some(Object::Array(paths)) = resolve_object(doc, il) {
                for p in paths {
                    if let Some(Object::Array(nums)) = resolve_object(doc, p) {
                        let mut stroke = Vec::new();
                        for pair in nums.chunks_exact(2) {
                            let px = as_f32(&pair[0]).unwrap_or(0.0);
                            let py = as_f32(&pair[1]).unwrap_or(0.0);
                            let (vx, vy) = pt.pdf_to_vp(px, py);
                            stroke.push([vx, vy]);
                        }
                        if !stroke.is_empty() {
                            points_vp.push(stroke);
                        }
                    }
                }
            }
        }
        if let Ok(l_obj) = dict.get(b"L") {
            if let Some(Object::Array(nums)) = resolve_object(doc, l_obj) {
                if nums.len() >= 4 {
                    let p0 = pt.pdf_to_vp(
                        as_f32(&nums[0]).unwrap_or(0.0),
                        as_f32(&nums[1]).unwrap_or(0.0),
                    );
                    let p1 = pt.pdf_to_vp(
                        as_f32(&nums[2]).unwrap_or(0.0),
                        as_f32(&nums[3]).unwrap_or(0.0),
                    );
                    points_vp.push(vec![[p0.0, p0.1], [p1.0, p1.1]]);
                }
            }
        }
        if let Ok(verts) = dict.get(b"Vertices") {
            if let Some(Object::Array(nums)) = resolve_object(doc, verts) {
                let mut poly = Vec::new();
                for pair in nums.chunks_exact(2) {
                    let (vx, vy) = pt.pdf_to_vp(
                        as_f32(&pair[0]).unwrap_or(0.0),
                        as_f32(&pair[1]).unwrap_or(0.0),
                    );
                    poly.push([vx, vy]);
                }
                if !poly.is_empty() {
                    points_vp.push(poly);
                }
            }
        }

        let mut rects_vp: Vec<[f32; 4]> = Vec::new();
        if let Ok(qp) = dict.get(b"QuadPoints") {
            if let Some(Object::Array(nums)) = resolve_object(doc, qp) {
                for quad in nums.chunks_exact(8) {
                    let mut min_x = f32::INFINITY;
                    let mut min_y = f32::INFINITY;
                    let mut max_x = f32::NEG_INFINITY;
                    let mut max_y = f32::NEG_INFINITY;
                    for c in 0..4 {
                        let px = as_f32(&quad[c * 2]).unwrap_or(0.0);
                        let py = as_f32(&quad[c * 2 + 1]).unwrap_or(0.0);
                        let (vx, vy) = pt.pdf_to_vp(px, py);
                        min_x = min_x.min(vx);
                        min_y = min_y.min(vy);
                        max_x = max_x.max(vx);
                        max_y = max_y.max(vy);
                    }
                    if min_x.is_finite() {
                        rects_vp.push([
                            min_x,
                            min_y,
                            (max_x - min_x).max(2.0),
                            (max_y - min_y).max(2.0),
                        ]);
                    }
                }
            }
        }

        let line_end_arrow = dict
            .get(b"LE")
            .ok()
            .and_then(|o| resolve_object(doc, o))
            .and_then(|o| o.as_array().ok())
            .map(|a| {
                a.iter().any(|it| {
                    it.as_name()
                        .map(|n| String::from_utf8_lossy(n).contains("Arrow"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        let cloudy = dict
            .get(b"BE")
            .ok()
            .and_then(|o| resolve_dict(doc, o))
            .and_then(|d| d.get(b"S").ok().and_then(|s| s.as_name().ok()))
            .map(|s| s == b"C")
            .unwrap_or(false);

        let stamp_name = dict
            .get(b"Name")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .unwrap_or_default();

        let is_highlighter = opacity < 0.85
            && (subtype == "Ink" || subtype == "Highlight")
            && dict
                .get(b"Subj")
                .ok()
                .map(pdf_text_to_string)
                .map(|s| s.to_lowercase().contains("highlight"))
                .unwrap_or(subtype == "Highlight" || (subtype == "Ink" && opacity <= 0.65));

        let mut url = String::new();
        let mut target_page = None;
        if let Ok(a_obj) = dict.get(b"A") {
            if let Some(ad) = resolve_dict(doc, a_obj) {
                if let Ok(u) = ad.get(b"URI") {
                    url = pdf_text_to_string(u);
                }
                if let Ok(d_obj) = ad.get(b"D") {
                    if let Some(Object::Array(arr)) = resolve_object(doc, d_obj) {
                        if let Some(pid) = arr.first().and_then(|o| o.as_reference().ok()) {
                            target_page = page_id_to_num.get(&pid).copied();
                        }
                    }
                }
            }
        }
        if target_page.is_none() {
            if let Ok(d_obj) = dict.get(b"Dest") {
                if let Some(Object::Array(arr)) = resolve_object(doc, d_obj) {
                    if let Some(pid) = arr.first().and_then(|o| o.as_reference().ok()) {
                        target_page = page_id_to_num.get(&pid).copied();
                    }
                }
            }
        }

        // AcroForm Widget info (supporting Parent field merge)
        let mut field_name = String::new();
        let mut field_type = String::new();
        let mut field_value = String::new();
        let mut field_options: Vec<String> = Vec::new();
        if subtype == "Widget" {
            let parent_dict = dict
                .get(b"Parent")
                .ok()
                .and_then(|o| resolve_dict(doc, o));
            field_name = dict
                .get(b"T")
                .ok()
                .map(pdf_text_to_string)
                .filter(|s| !s.is_empty())
                .or_else(|| parent_dict.and_then(|pd| pd.get(b"T").ok().map(pdf_text_to_string)))
                .unwrap_or_else(|| nm.clone());
            field_type = dict
                .get(b"FT")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).to_string())
                .or_else(|| {
                    parent_dict.and_then(|pd| {
                        pd.get(b"FT")
                            .ok()
                            .and_then(|o| o.as_name().ok())
                            .map(|n| String::from_utf8_lossy(n).to_string())
                    })
                })
                .unwrap_or_else(|| "Tx".to_string());
            let v_obj = dict
                .get(b"V")
                .ok()
                .or_else(|| parent_dict.and_then(|pd| pd.get(b"V").ok()));
            if let Some(vo) = v_obj {
                field_value = pdf_text_to_string(vo);
            } else if field_type == "Btn" {
                if let Ok(as_val) = dict.get(b"AS") {
                    field_value = pdf_text_to_string(as_val);
                }
            }
            if let Some(opt_obj) = dict
                .get(b"Opt")
                .ok()
                .or_else(|| parent_dict.and_then(|pd| pd.get(b"Opt").ok()))
            {
                if let Some(Object::Array(opts)) = resolve_object(doc, opt_obj) {
                    for it in opts {
                        match it {
                            Object::Array(pair) if !pair.is_empty() => {
                                field_options.push(pdf_text_to_string(&pair[0]));
                            }
                            other => field_options.push(pdf_text_to_string(other)),
                        }
                    }
                }
            }
        }

        let mut font_size = 12.0_f32;
        let mut font_family = "Helvetica".to_string();
        if let Ok(da) = dict.get(b"DA") {
            let da_s = pdf_text_to_string(da);
            for tok in da_s.split_whitespace().collect::<Vec<_>>().windows(3) {
                if tok[2] == "Tf" {
                    font_family = tok[0].trim_start_matches('/').to_string();
                    if let Ok(sz) = tok[1].parse::<f32>() {
                        font_size = sz.max(6.0);
                    }
                }
            }
        }
        let align = match dict.get(b"Q").ok().and_then(|o| o.as_i64().ok()) {
            Some(1) => "center".to_string(),
            Some(2) => "right".to_string(),
            _ => "left".to_string(),
        };

        out.push(PdfAnnotationDto {
            id: nm,
            page: page.max(1),
            subtype,
            bbox_vp,
            color,
            fill_color,
            opacity,
            stroke_width,
            contents,
            author,
            is_highlighter,
            points_vp,
            rects_vp,
            font_size,
            font_family,
            align,
            stamp_name,
            url,
            target_page,
            line_end_arrow,
            cloudy,
            field_name,
            field_type,
            field_value,
            field_options,
        });
    }

    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitAnnotationRequest {
    pub page: usize,
    pub action: String, // "add" | "update" | "delete"
    pub id: String,
    pub kind: String, // "ink" | "highlight" | "underline" | "strikethrough" | "note" | "freetext" | "line" | "arrow" | "square" | "circle" | "polygon" | "polyline" | "cloud" | "stamp" | "signature" | "link"
    pub bbox_vp: [f32; 4],
    pub color: Option<String>,
    pub fill_color: Option<String>,
    pub opacity: Option<f32>,
    pub stroke_width: Option<f32>,
    pub is_highlighter: Option<bool>,
    pub contents: Option<String>,
    pub author: Option<String>,
    pub points_vp: Option<Vec<Vec<[f32; 2]>>>,
    pub rects_vp: Option<Vec<[f32; 4]>>,
    pub font_size: Option<f32>,
    pub font_family: Option<String>,
    pub align: Option<String>,
    pub stamp_name: Option<String>,
    pub image_data_url: Option<String>,
    pub rgba_base64: Option<String>,
    pub pixel_width: Option<u32>,
    pub pixel_height: Option<u32>,
    pub url: Option<String>,
    pub target_page: Option<usize>,
}

fn ensure_page_annots_list(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
) -> Result<(Option<ObjectId>, Vec<Object>), String> {
    let pd = doc
        .get_dictionary(page_id)
        .map_err(|e| format!("Failed reading page dict: {}", e))?;
    match pd.get(b"Annots") {
        Ok(Object::Reference(rid)) => {
            let rid = *rid;
            let arr = doc
                .get_object(rid)
                .ok()
                .and_then(|o| o.as_array().ok())
                .cloned()
                .unwrap_or_default();
            Ok((Some(rid), arr))
        }
        Ok(Object::Array(arr)) => Ok((None, arr.clone())),
        _ => Ok((None, Vec::new())),
    }
}

fn write_page_annots_list(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    arr_id_opt: Option<ObjectId>,
    list: Vec<Object>,
) -> Result<(), String> {
    if let Some(aid) = arr_id_opt {
        if let Ok(obj) = doc.get_object_mut(aid) {
            *obj = Object::Array(list);
            return Ok(());
        }
    }
    let obj = doc
        .get_object_mut(page_id)
        .map_err(|e| format!("Failed writing page /Annots: {}", e))?;
    if let Object::Dictionary(ref mut pd) = obj {
        pd.set("Annots", Object::Array(list));
    }
    Ok(())
}

pub fn commit_annotation_op(
    doc: &mut lopdf::Document,
    req: &CommitAnnotationRequest,
) -> Result<(), String> {
    let pages = doc.get_pages();
    let page_num = req.page.max(1) as u32;
    let page_id = *pages
        .get(&page_num)
        .ok_or_else(|| format!("Page {} does not exist", page_num))?;
    let pt = get_page_transform(doc, page_id);

    let (arr_id_opt, mut annots) = ensure_page_annots_list(doc, page_id)?;
    let target_nm = req.id.clone();

    // Remove existing annotation with matching /NM (used by both update and delete)
    let mut removed_popup_ids: HashSet<ObjectId> = HashSet::new();
    annots.retain(|item| {
        let matched = resolve_dict(doc, item)
            .and_then(|d| d.get(b"NM").ok().map(pdf_text_to_string))
            .map(|nm| nm == target_nm)
            .unwrap_or(false);
        if matched {
            if let Some(d) = resolve_dict(doc, item) {
                if let Ok(Object::Reference(pid)) = d.get(b"Popup") {
                    removed_popup_ids.insert(*pid);
                }
            }
            false
        } else {
            true
        }
    });
    if !removed_popup_ids.is_empty() {
        annots.retain(|item| {
            item.as_reference()
                .map(|oid| !removed_popup_ids.contains(&oid))
                .unwrap_or(true)
        });
    }

    if req.action == "delete" {
        return write_page_annots_list(doc, page_id, arr_id_opt, annots);
    }

    let color_hex = req.color.as_deref().unwrap_or("#facc15");
    let rgb = hex_to_rgb(color_hex);
    let lw = req.stroke_width.unwrap_or(2.0).max(0.5);
    let is_hl = req.is_highlighter.unwrap_or(false) || req.kind == "highlight";
    let alpha = req.opacity.unwrap_or(if is_hl { 0.42 } else { 1.0 }).clamp(0.05, 1.0);

    let mut rect_pdf = vp_rect_to_pdf_rect(&pt, req.bbox_vp);
    // Ensure non-zero rect
    if (rect_pdf[2] - rect_pdf[0]).abs() < 2.0 {
        rect_pdf[2] = rect_pdf[0] + 4.0;
    }
    if (rect_pdf[3] - rect_pdf[1]).abs() < 2.0 {
        rect_pdf[3] = rect_pdf[1] + 4.0;
    }

    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"Annot".to_vec()));
    dict.set("NM", string_to_pdf_text_object(&target_nm));
    dict.set("P", Object::Reference(page_id));
    dict.set("F", Object::Integer(4)); // Print flag so Acrobat/Chrome/Edge render & print cleanly
    dict.set(
        "C",
        Object::Array(vec![
            Object::Real(rgb[0]),
            Object::Real(rgb[1]),
            Object::Real(rgb[2]),
        ]),
    );
    dict.set("CA", Object::Real(alpha));
    if let Some(auth) = &req.author {
        dict.set("T", string_to_pdf_text_object(auth));
    }
    if let Some(cnt) = &req.contents {
        dict.set("Contents", string_to_pdf_text_object(cnt));
    }

    let mut extra_popup: Option<ObjectId> = None;

    match req.kind.as_str() {
        "ink" => {
            dict.set("Subtype", Object::Name(b"Ink".to_vec()));
            if is_hl {
                dict.set("Subj", Object::string_literal("Highlighter Ink"));
            }
            let mut bs = Dictionary::new();
            bs.set("W", Object::Real(lw));
            bs.set("S", Object::Name(b"S".to_vec()));
            dict.set("BS", Object::Dictionary(bs));

            let strokes_vp = req.points_vp.as_deref().unwrap_or(&[]);
            let mut ink_list_pdf = Vec::new();
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;

            let (gs_name, res_dict) = ensure_gs_in_ap(doc, alpha, is_hl);
            let mut ops = Vec::new();
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new("gs", vec![Object::Name(gs_name.into_bytes())]));
            ops.push(Operation::new(
                "RG",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));
            ops.push(Operation::new("w", vec![Object::Real(lw)]));
            ops.push(Operation::new("J", vec![Object::Integer(1)])); // Round cap
            ops.push(Operation::new("j", vec![Object::Integer(1)])); // Round join

            for st in strokes_vp {
                if st.is_empty() {
                    continue;
                }
                let mut pdf_pts: Vec<(f32, f32)> = Vec::with_capacity(st.len());
                let mut arr = Vec::with_capacity(st.len() * 2);
                for pt_vp in st {
                    let (px, py) = pt.vp_to_pdf(pt_vp[0], pt_vp[1]);
                    min_x = min_x.min(px);
                    min_y = min_y.min(py);
                    max_x = max_x.max(px);
                    max_y = max_y.max(py);
                    pdf_pts.push((px, py));
                    arr.push(Object::Real(px));
                    arr.push(Object::Real(py));
                }
                ink_list_pdf.push(Object::Array(arr));

                ops.push(Operation::new(
                    "m",
                    vec![Object::Real(pdf_pts[0].0), Object::Real(pdf_pts[0].1)],
                ));
                if pdf_pts.len() == 1 {
                    ops.push(Operation::new(
                        "l",
                        vec![
                            Object::Real(pdf_pts[0].0 + 0.1),
                            Object::Real(pdf_pts[0].1 + 0.1),
                        ],
                    ));
                } else {
                    for (c1, c2, end) in catmull_rom_to_beziers(&pdf_pts) {
                        ops.push(Operation::new(
                            "c",
                            vec![
                                Object::Real(c1.0),
                                Object::Real(c1.1),
                                Object::Real(c2.0),
                                Object::Real(c2.1),
                                Object::Real(end.0),
                                Object::Real(end.1),
                            ],
                        ));
                    }
                }
                ops.push(Operation::new("S", vec![]));
            }
            ops.push(Operation::new("Q", vec![]));

            if min_x.is_finite() {
                let pad = lw.max(2.0);
                rect_pdf = [min_x - pad, min_y - pad, max_x + pad, max_y + pad];
            }
            dict.set("InkList", Object::Array(ink_list_pdf));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res_dict)?;
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(ap_id));
            dict.set("AP", Object::Dictionary(ap));
        }
        "highlight" | "underline" | "strikethrough" => {
            let sub = match req.kind.as_str() {
                "highlight" => "Highlight",
                "underline" => "Underline",
                _ => "StrikeOut",
            };
            dict.set("Subtype", Object::Name(sub.as_bytes().to_vec()));
            let rects_src = req
                .rects_vp
                .as_deref()
                .filter(|r| !r.is_empty())
                .unwrap_or(std::slice::from_ref(&req.bbox_vp));

            let mut quads_pdf = Vec::with_capacity(rects_src.len() * 8);
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;

            let (gs_name, res_dict) =
                ensure_gs_in_ap(doc, if req.kind == "highlight" { alpha.min(0.42) } else { 0.92 }, req.kind == "highlight");
            let mut ops = Vec::new();
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new("gs", vec![Object::Name(gs_name.into_bytes())]));
            ops.push(Operation::new(
                "rg",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));
            ops.push(Operation::new(
                "RG",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));

            for r_vp in rects_src {
                let q = vp_rect_to_quadpoints(&pt, *r_vp);
                for &v in &q {
                    quads_pdf.push(Object::Real(v));
                }
                let r_pdf = vp_rect_to_pdf_rect(&pt, *r_vp);
                min_x = min_x.min(r_pdf[0]);
                min_y = min_y.min(r_pdf[1]);
                max_x = max_x.max(r_pdf[2]);
                max_y = max_y.max(r_pdf[3]);

                match req.kind.as_str() {
                    "highlight" => {
                        ops.push(Operation::new(
                            "re",
                            vec![
                                Object::Real(r_pdf[0]),
                                Object::Real(r_pdf[1]),
                                Object::Real(r_pdf[2] - r_pdf[0]),
                                Object::Real(r_pdf[3] - r_pdf[1]),
                            ],
                        ));
                        ops.push(Operation::new("f", vec![]));
                    }
                    "underline" => {
                        let bl_0 = pt.vp_to_pdf(r_vp[0], r_vp[1] + r_vp[3] - 1.0);
                        let bl_1 = pt.vp_to_pdf(r_vp[0] + r_vp[2], r_vp[1] + r_vp[3] - 1.0);
                        ops.push(Operation::new("w", vec![Object::Real(1.4)]));
                        ops.push(Operation::new("m", vec![Object::Real(bl_0.0), Object::Real(bl_0.1)]));
                        ops.push(Operation::new("l", vec![Object::Real(bl_1.0), Object::Real(bl_1.1)]));
                        ops.push(Operation::new("S", vec![]));
                    }
                    _ => {
                        let m0 = pt.vp_to_pdf(r_vp[0], r_vp[1] + r_vp[3] * 0.5);
                        let m1 = pt.vp_to_pdf(r_vp[0] + r_vp[2], r_vp[1] + r_vp[3] * 0.5);
                        ops.push(Operation::new("w", vec![Object::Real(1.4)]));
                        ops.push(Operation::new("m", vec![Object::Real(m0.0), Object::Real(m0.1)]));
                        ops.push(Operation::new("l", vec![Object::Real(m1.0), Object::Real(m1.1)]));
                        ops.push(Operation::new("S", vec![]));
                    }
                }
            }
            ops.push(Operation::new("Q", vec![]));

            if min_x.is_finite() {
                rect_pdf = [min_x - 1.0, min_y - 1.0, max_x + 1.0, max_y + 1.0];
            }
            dict.set("QuadPoints", Object::Array(quads_pdf));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res_dict)?;
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(ap_id));
            dict.set("AP", Object::Dictionary(ap));
        }
        "note" => {
            dict.set("Subtype", Object::Name(b"Text".to_vec()));
            dict.set("Name", Object::Name(b"Comment".to_vec()));
            dict.set("Open", Object::Boolean(false));
            // Sticky note 20x20 icon box
            let [vx, vy, _, _] = req.bbox_vp;
            rect_pdf = vp_rect_to_pdf_rect(&pt, [vx - 10.0, vy - 10.0, 20.0, 20.0]);
            let (_, res_dict) = ensure_gs_in_ap(doc, 0.95, false);
            let mut ops = Vec::new();
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new(
                "rg",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));
            ops.push(Operation::new("RG", vec![Object::Real(0.2), Object::Real(0.2), Object::Real(0.2)]));
            ops.push(Operation::new("w", vec![Object::Real(1.0)]));
            ops.push(Operation::new(
                "re",
                vec![
                    Object::Real(rect_pdf[0] + 1.0),
                    Object::Real(rect_pdf[1] + 1.0),
                    Object::Real((rect_pdf[2] - rect_pdf[0] - 2.0).max(6.0)),
                    Object::Real((rect_pdf[3] - rect_pdf[1] - 2.0).max(6.0)),
                ],
            ));
            ops.push(Operation::new("B", vec![]));
            ops.push(Operation::new("Q", vec![]));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res_dict)?;
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(ap_id));
            dict.set("AP", Object::Dictionary(ap));

            // Linked Popup dictionary per PDF 1.7 Sec 12.5.6.14
            let mut popup = Dictionary::new();
            popup.set("Type", Object::Name(b"Annot".to_vec()));
            popup.set("Subtype", Object::Name(b"Popup".to_vec()));
            popup.set(
                "Rect",
                Object::Array(vec![
                    Object::Real(rect_pdf[2] + 6.0),
                    Object::Real(rect_pdf[1] - 80.0),
                    Object::Real(rect_pdf[2] + 186.0),
                    Object::Real(rect_pdf[3] + 20.0),
                ]),
            );
            popup.set("Open", Object::Boolean(false));
            extra_popup = Some(doc.add_object(Object::Dictionary(popup)));
        }
        "freetext" => {
            dict.set("Subtype", Object::Name(b"FreeText".to_vec()));
            let fs = req.font_size.unwrap_or(14.0).clamp(6.0, 96.0);
            let fam = req.font_family.as_deref().unwrap_or("Noto Sans");
            let text_val = req.contents.as_deref().unwrap_or("");
            let da = format!("/Helv {:.1} Tf {:.3} {:.3} {:.3} rg", fs, rgb[0], rgb[1], rgb[2]);
            dict.set("DA", Object::string_literal(da));
            let q_val = match req.align.as_deref() {
                Some("center") => 1,
                Some("right") => 2,
                _ => 0,
            };
            dict.set("Q", Object::Integer(q_val));
            dict.set("C", Object::Array(vec![])); // transparent FreeText background

            let font_mode = ensure_page_font_resource(doc, page_id, fam, false, false, text_val)?;
            let (res_name, fid_obj) = {
                let res_dict = crate::font_shaper::get_page_resources_dict(doc, page_id)
                    .cloned()
                    .unwrap_or_default();
                let rname = match &font_mode {
                    PdfFontWriteMode::StandardWinAnsi { resource_name } => resource_name.clone(),
                    PdfFontWriteMode::EmbeddedCidTrueType { resource_name, .. } => {
                        resource_name.clone()
                    }
                };
                let fid = res_dict
                    .get(b"Font")
                    .ok()
                    .and_then(|o| resolve_dict(doc, o))
                    .and_then(|fd| fd.get(rname.as_bytes()).ok().cloned())
                    .unwrap_or(Object::Null);
                (rname, fid)
            };

            let mut font_map = Dictionary::new();
            font_map.set(res_name.as_bytes(), fid_obj);
            let mut ap_res = Dictionary::new();
            ap_res.set("Font", Object::Dictionary(font_map));

            let mut ops = Vec::new();
            ops.push(Operation::new("BT", vec![]));
            ops.push(Operation::new(
                "rg",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));
            ops.push(Operation::new(
                "Tf",
                vec![Object::Name(res_name.into_bytes()), Object::Real(fs)],
            ));
            for (li, ln) in text_val.split('\n').enumerate() {
                let ln = ln.trim_end_matches('\r');
                if ln.is_empty() {
                    continue;
                }
                let vx = req.bbox_vp[0] + 2.0;
                let vy = req.bbox_vp[1] + fs * 0.92 + (li as f32) * (fs * 1.25);
                let (px, py) = pt.vp_to_pdf(vx, vy);
                let tm = match pt.rotation {
                    90 => vec![0.0, -1.0, 1.0, 0.0, px, py],
                    180 => vec![-1.0, 0.0, 0.0, -1.0, px, py],
                    270 => vec![0.0, 1.0, -1.0, 0.0, px, py],
                    _ => vec![1.0, 0.0, 0.0, 1.0, px, py],
                };
                ops.push(Operation::new(
                    "Tm",
                    tm.into_iter().map(Object::Real).collect(),
                ));
                let str_bytes = match &font_mode {
                    PdfFontWriteMode::StandardWinAnsi { .. } => encode_winansi_bytes(ln),
                    PdfFontWriteMode::EmbeddedCidTrueType { char_to_gid, .. } => {
                        encode_cid_glyph_bytes(ln, char_to_gid)
                    }
                };
                ops.push(Operation::new(
                    "Tj",
                    vec![Object::String(str_bytes, lopdf::StringFormat::Hexadecimal)],
                ));
            }
            ops.push(Operation::new("ET", vec![]));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, ap_res)?;
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(ap_id));
            dict.set("AP", Object::Dictionary(ap));
        }
        "line" | "arrow" => {
            dict.set("Subtype", Object::Name(b"Line".to_vec()));
            let pts_vp = req.points_vp.as_deref().unwrap_or(&[]);
            let (s_vp, e_vp) = if let Some(first) = pts_vp.first() {
                if first.len() >= 2 {
                    (first[0], first[1])
                } else {
                    ([req.bbox_vp[0], req.bbox_vp[1]], [req.bbox_vp[0] + req.bbox_vp[2], req.bbox_vp[1] + req.bbox_vp[3]])
                }
            } else {
                ([req.bbox_vp[0], req.bbox_vp[1]], [req.bbox_vp[0] + req.bbox_vp[2], req.bbox_vp[1] + req.bbox_vp[3]])
            };
            let p0 = pt.vp_to_pdf(s_vp[0], s_vp[1]);
            let p1 = pt.vp_to_pdf(e_vp[0], e_vp[1]);
            let pad = lw + 12.0;
            rect_pdf = [
                p0.0.min(p1.0) - pad,
                p0.1.min(p1.1) - pad,
                p0.0.max(p1.0) + pad,
                p0.1.max(p1.1) + pad,
            ];
            dict.set(
                "L",
                Object::Array(vec![
                    Object::Real(p0.0),
                    Object::Real(p0.1),
                    Object::Real(p1.0),
                    Object::Real(p1.1),
                ]),
            );
            let is_arrow = req.kind == "arrow";
            dict.set(
                "LE",
                Object::Array(vec![
                    Object::Name(b"None".to_vec()),
                    Object::Name(if is_arrow { b"OpenArrow".to_vec() } else { b"None".to_vec() }),
                ]),
            );
            let mut bs = Dictionary::new();
            bs.set("W", Object::Real(lw));
            dict.set("BS", Object::Dictionary(bs));

            let (gs_name, res_dict) = ensure_gs_in_ap(doc, alpha, false);
            let mut ops = Vec::new();
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new("gs", vec![Object::Name(gs_name.into_bytes())]));
            ops.push(Operation::new(
                "RG",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));
            ops.push(Operation::new("w", vec![Object::Real(lw)]));
            ops.push(Operation::new("m", vec![Object::Real(p0.0), Object::Real(p0.1)]));
            ops.push(Operation::new("l", vec![Object::Real(p1.0), Object::Real(p1.1)]));
            if is_arrow {
                let ang = (p1.1 - p0.1).atan2(p1.0 - p0.0);
                let head = (lw * 4.5).clamp(8.0, 20.0);
                let h1 = (p1.0 - head * (ang - 0.45).cos(), p1.1 - head * (ang - 0.45).sin());
                let h2 = (p1.0 - head * (ang + 0.45).cos(), p1.1 - head * (ang + 0.45).sin());
                ops.push(Operation::new("m", vec![Object::Real(p1.0), Object::Real(p1.1)]));
                ops.push(Operation::new("l", vec![Object::Real(h1.0), Object::Real(h1.1)]));
                ops.push(Operation::new("m", vec![Object::Real(p1.0), Object::Real(p1.1)]));
                ops.push(Operation::new("l", vec![Object::Real(h2.0), Object::Real(h2.1)]));
            }
            ops.push(Operation::new("S", vec![]));
            ops.push(Operation::new("Q", vec![]));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res_dict)?;
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(ap_id));
            dict.set("AP", Object::Dictionary(ap));
        }
        "square" | "circle" | "polygon" | "polyline" | "cloud" => {
            let sub = match req.kind.as_str() {
                "square" => "Square",
                "circle" => "Circle",
                "polyline" => "PolyLine",
                _ => "Polygon",
            };
            dict.set("Subtype", Object::Name(sub.as_bytes().to_vec()));
            if req.kind == "cloud" {
                let mut be = Dictionary::new();
                be.set("S", Object::Name(b"C".to_vec()));
                be.set("I", Object::Real(2.0));
                dict.set("BE", Object::Dictionary(be));
            }
            let mut bs = Dictionary::new();
            bs.set("W", Object::Real(lw));
            dict.set("BS", Object::Dictionary(bs));

            let has_fill = req.fill_color.is_some();
            if let Some(fc) = &req.fill_color {
                let frgb = hex_to_rgb(fc);
                dict.set(
                    "IC",
                    Object::Array(vec![
                        Object::Real(frgb[0]),
                        Object::Real(frgb[1]),
                        Object::Real(frgb[2]),
                    ]),
                );
            }

            let (gs_name, res_dict) = ensure_gs_in_ap(doc, alpha, is_hl);
            let mut ops = Vec::new();
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new("gs", vec![Object::Name(gs_name.into_bytes())]));
            ops.push(Operation::new(
                "RG",
                vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
            ));
            if let Some(fc) = &req.fill_color {
                let frgb = hex_to_rgb(fc);
                ops.push(Operation::new(
                    "rg",
                    vec![Object::Real(frgb[0]), Object::Real(frgb[1]), Object::Real(frgb[2])],
                ));
            }
            ops.push(Operation::new("w", vec![Object::Real(lw)]));

            if req.kind == "square" {
                ops.push(Operation::new(
                    "re",
                    vec![
                        Object::Real(rect_pdf[0]),
                        Object::Real(rect_pdf[1]),
                        Object::Real((rect_pdf[2] - rect_pdf[0]).max(2.0)),
                        Object::Real((rect_pdf[3] - rect_pdf[1]).max(2.0)),
                    ],
                ));
                ops.push(Operation::new(if has_fill { "B" } else { "S" }, vec![]));
            } else if req.kind == "circle" {
                let cx = (rect_pdf[0] + rect_pdf[2]) * 0.5;
                let cy = (rect_pdf[1] + rect_pdf[3]) * 0.5;
                let rx = ((rect_pdf[2] - rect_pdf[0]) * 0.5).max(1.0);
                let ry = ((rect_pdf[3] - rect_pdf[1]) * 0.5).max(1.0);
                let k = 0.55228475_f32;
                let (ox, oy) = (rx * k, ry * k);
                ops.push(Operation::new("m", vec![Object::Real(cx + rx), Object::Real(cy)]));
                for (a, b, c) in [
                    ((cx + rx, cy + oy), (cx + ox, cy + ry), (cx, cy + ry)),
                    ((cx - ox, cy + ry), (cx - rx, cy + oy), (cx - rx, cy)),
                    ((cx - rx, cy - oy), (cx - ox, cy - ry), (cx, cy - ry)),
                    ((cx + ox, cy - ry), (cx + rx, cy - oy), (cx + rx, cy)),
                ] {
                    ops.push(Operation::new(
                        "c",
                        vec![
                            Object::Real(a.0),
                            Object::Real(a.1),
                            Object::Real(b.0),
                            Object::Real(b.1),
                            Object::Real(c.0),
                            Object::Real(c.1),
                        ],
                    ));
                }
                ops.push(Operation::new("h", vec![]));
                ops.push(Operation::new(if has_fill { "B" } else { "S" }, vec![]));
            } else {
                // Polygon / PolyLine / Cloud
                let pts = req
                    .points_vp
                    .as_deref()
                    .and_then(|s| s.first())
                    .cloned()
                    .unwrap_or_else(|| {
                        let [vx, vy, vw, vh] = req.bbox_vp;
                        vec![
                            [vx + vw * 0.5, vy],
                            [vx + vw, vy + vh * 0.38],
                            [vx + vw * 0.82, vy + vh],
                            [vx + vw * 0.18, vy + vh],
                            [vx, vy + vh * 0.38],
                        ]
                    });
                let mut verts_pdf = Vec::with_capacity(pts.len() * 2);
                let mut p_min_x = f32::INFINITY;
                let mut p_min_y = f32::INFINITY;
                let mut p_max_x = f32::NEG_INFINITY;
                let mut p_max_y = f32::NEG_INFINITY;
                for (pi, p_vp) in pts.iter().enumerate() {
                    let (px, py) = pt.vp_to_pdf(p_vp[0], p_vp[1]);
                    p_min_x = p_min_x.min(px);
                    p_min_y = p_min_y.min(py);
                    p_max_x = p_max_x.max(px);
                    p_max_y = p_max_y.max(py);
                    verts_pdf.push(Object::Real(px));
                    verts_pdf.push(Object::Real(py));
                    ops.push(Operation::new(
                        if pi == 0 { "m" } else { "l" },
                        vec![Object::Real(px), Object::Real(py)],
                    ));
                }
                if req.kind != "polyline" {
                    ops.push(Operation::new("h", vec![]));
                }
                ops.push(Operation::new(
                    if has_fill && req.kind != "polyline" { "B" } else { "S" },
                    vec![],
                ));
                if p_min_x.is_finite() {
                    let pad = lw + 4.0;
                    rect_pdf = [p_min_x - pad, p_min_y - pad, p_max_x + pad, p_max_y + pad];
                }
                dict.set("Vertices", Object::Array(verts_pdf));
            }
            ops.push(Operation::new("Q", vec![]));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res_dict)?;
            let mut ap = Dictionary::new();
            ap.set("N", Object::Reference(ap_id));
            dict.set("AP", Object::Dictionary(ap));
        }
        "stamp" | "signature" => {
            dict.set("Subtype", Object::Name(b"Stamp".to_vec()));
            let stamp_label = req.stamp_name.as_deref().unwrap_or("APPROVED");
            dict.set("Name", Object::Name(stamp_label.as_bytes().to_vec()));

            if let Some(img_id) = build_image_xobject_from_req(
                doc,
                req.image_data_url.as_deref(),
                req.rgba_base64.as_deref(),
                req.pixel_width,
                req.pixel_height,
            )? {
                let mut xmap = Dictionary::new();
                xmap.set("Im0", Object::Reference(img_id));
                let mut ap_res = Dictionary::new();
                ap_res.set("XObject", Object::Dictionary(xmap));
                let rw = (rect_pdf[2] - rect_pdf[0]).max(4.0);
                let rh = (rect_pdf[3] - rect_pdf[1]).max(4.0);
                let ops = vec![
                    Operation::new("q", vec![]),
                    Operation::new(
                        "cm",
                        vec![
                            Object::Real(rw),
                            Object::Real(0.0),
                            Object::Real(0.0),
                            Object::Real(rh),
                            Object::Real(rect_pdf[0]),
                            Object::Real(rect_pdf[1]),
                        ],
                    ),
                    Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
                    Operation::new("Q", vec![]),
                ];
                let ap_id = create_form_ap_stream(doc, rect_pdf, ops, ap_res)?;
                let mut ap = Dictionary::new();
                ap.set("N", Object::Reference(ap_id));
                dict.set("AP", Object::Dictionary(ap));
            } else {
                // Vector stamp with crisp text appearance
                let _ = ensure_page_font_resource(doc, page_id, "Helvetica", true, false, stamp_label)?;
                let mut helv = Dictionary::new();
                helv.set("Type", Object::Name(b"Font".to_vec()));
                helv.set("Subtype", Object::Name(b"Type1".to_vec()));
                helv.set("BaseFont", Object::Name(b"Helvetica-Bold".to_vec()));
                helv.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
                let helv_id = doc.add_object(Object::Dictionary(helv));
                let mut font_map = Dictionary::new();
                font_map.set("FSt", Object::Reference(helv_id));
                let mut ap_res = Dictionary::new();
                ap_res.set("Font", Object::Dictionary(font_map));

                let rw = (rect_pdf[2] - rect_pdf[0]).max(24.0);
                let rh = (rect_pdf[3] - rect_pdf[1]).max(16.0);
                let fs = (rh * 0.42).clamp(8.0, 22.0);
                let mut ops = Vec::new();
                ops.push(Operation::new("q", vec![]));
                ops.push(Operation::new("w", vec![Object::Real(2.0)]));
                ops.push(Operation::new(
                    "RG",
                    vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
                ));
                ops.push(Operation::new(
                    "re",
                    vec![
                        Object::Real(rect_pdf[0] + 1.0),
                        Object::Real(rect_pdf[1] + 1.0),
                        Object::Real(rw - 2.0),
                        Object::Real(rh - 2.0),
                    ],
                ));
                ops.push(Operation::new("S", vec![]));
                ops.push(Operation::new("BT", vec![]));
                ops.push(Operation::new(
                    "rg",
                    vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
                ));
                ops.push(Operation::new("Tf", vec![Object::Name(b"FSt".to_vec()), Object::Real(fs)]));
                ops.push(Operation::new(
                    "Tm",
                    vec![
                        Object::Real(1.0),
                        Object::Real(0.0),
                        Object::Real(0.0),
                        Object::Real(1.0),
                        Object::Real(rect_pdf[0] + 10.0),
                        Object::Real(rect_pdf[1] + rh * 0.33),
                    ],
                ));
                ops.push(Operation::new(
                    "Tj",
                    vec![Object::string_literal(stamp_label)],
                ));
                ops.push(Operation::new("ET", vec![]));
                ops.push(Operation::new("Q", vec![]));
                let ap_id = create_form_ap_stream(doc, rect_pdf, ops, ap_res)?;
                let mut ap = Dictionary::new();
                ap.set("N", Object::Reference(ap_id));
                dict.set("AP", Object::Dictionary(ap));
            }
        }
        "link" => {
            dict.set("Subtype", Object::Name(b"Link".to_vec()));
            dict.set(
                "Border",
                Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]),
            );
            if let Some(tp) = req.target_page {
                if let Some(&target_pid) = pages.get(&(tp.max(1) as u32)) {
                    dict.set(
                        "Dest",
                        Object::Array(vec![
                            Object::Reference(target_pid),
                            Object::Name(b"Fit".to_vec()),
                        ]),
                    );
                }
            } else if let Some(uri) = &req.url {
                let mut act = Dictionary::new();
                act.set("Type", Object::Name(b"Action".to_vec()));
                act.set("S", Object::Name(b"URI".to_vec()));
                act.set("URI", Object::string_literal(uri.as_str()));
                dict.set("A", Object::Dictionary(act));
            }
        }
        other => return Err(format!("Unsupported annotation kind '{}'", other)),
    }

    dict.set(
        "Rect",
        Object::Array(vec![
            Object::Real(rect_pdf[0]),
            Object::Real(rect_pdf[1]),
            Object::Real(rect_pdf[2]),
            Object::Real(rect_pdf[3]),
        ]),
    );

    if let Some(popup_id) = extra_popup {
        dict.set("Popup", Object::Reference(popup_id));
    }

    let annot_id = doc.add_object(Object::Dictionary(dict));
    if let Some(popup_id) = extra_popup {
        if let Ok(Object::Dictionary(ref mut pdict)) = doc.get_object_mut(popup_id) {
            pdict.set("Parent", Object::Reference(annot_id));
        }
        annots.push(Object::Reference(annot_id));
        annots.push(Object::Reference(popup_id));
    } else {
        annots.push(Object::Reference(annot_id));
    }

    write_page_annots_list(doc, page_id, arr_id_opt, annots)
}

pub fn build_image_xobject_from_req(
    doc: &mut lopdf::Document,
    image_data_url: Option<&str>,
    rgba_base64: Option<&str>,
    pixel_w: Option<u32>,
    pixel_h: Option<u32>,
) -> Result<Option<ObjectId>, String> {
    if let Some(data_url) = image_data_url {
        let low = data_url.to_lowercase();
        if low.starts_with("data:image/jpeg;base64,") || low.starts_with("data:image/jpg;base64,") {
            let b64 = data_url.split(',').nth(1).unwrap_or("");
            let jpeg_bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| format!("Invalid JPEG base64: {}", e))?;
            let (pw, ph) = jpeg_dimensions(&jpeg_bytes).unwrap_or((
                pixel_w.unwrap_or(200) as i64,
                pixel_h.unwrap_or(200) as i64,
            ));
            let mut img_dict = Dictionary::new();
            img_dict.set("Type", Object::Name(b"XObject".to_vec()));
            img_dict.set("Subtype", Object::Name(b"Image".to_vec()));
            img_dict.set("Width", Object::Integer(pw));
            img_dict.set("Height", Object::Integer(ph));
            img_dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
            img_dict.set("BitsPerComponent", Object::Integer(8));
            img_dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            let stream = Stream::new(img_dict, jpeg_bytes);
            return Ok(Some(doc.add_object(Object::Stream(stream))));
        }
    }

    if let (Some(rgba_b64), Some(w), Some(h)) = (rgba_base64, pixel_w, pixel_h) {
        if w > 0 && h > 0 {
            let rgba = base64::engine::general_purpose::STANDARD
                .decode(rgba_b64.trim())
                .map_err(|e| format!("Invalid RGBA image bytes: {}", e))?;
            let count = (w as usize) * (h as usize);
            if rgba.len() >= count * 4 {
                let mut rgb = Vec::with_capacity(count * 3);
                let mut alpha = Vec::with_capacity(count);
                let mut has_transparency = false;
                for px in rgba[..count * 4].chunks_exact(4) {
                    rgb.push(px[0]);
                    rgb.push(px[1]);
                    rgb.push(px[2]);
                    alpha.push(px[3]);
                    if px[3] < 255 {
                        has_transparency = true;
                    }
                }
                let smask_id = if has_transparency {
                    let mut md = Dictionary::new();
                    md.set("Type", Object::Name(b"XObject".to_vec()));
                    md.set("Subtype", Object::Name(b"Image".to_vec()));
                    md.set("Width", Object::Integer(w as i64));
                    md.set("Height", Object::Integer(h as i64));
                    md.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
                    md.set("BitsPerComponent", Object::Integer(8));
                    let mut ms = Stream::new(md, alpha);
                    let _ = ms.compress();
                    Some(doc.add_object(Object::Stream(ms)))
                } else {
                    None
                };

                let mut idict = Dictionary::new();
                idict.set("Type", Object::Name(b"XObject".to_vec()));
                idict.set("Subtype", Object::Name(b"Image".to_vec()));
                idict.set("Width", Object::Integer(w as i64));
                idict.set("Height", Object::Integer(h as i64));
                idict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
                idict.set("BitsPerComponent", Object::Integer(8));
                if let Some(sm) = smask_id {
                    idict.set("SMask", Object::Reference(sm));
                }
                let mut istream = Stream::new(idict, rgb);
                let _ = istream.compress();
                return Ok(Some(doc.add_object(Object::Stream(istream))));
            }
        }
    }

    Ok(None)
}

fn jpeg_dimensions(data: &[u8]) -> Option<(i64, i64)> {
    let mut i = 2usize;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as i64;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as i64;
            return Some((w, h));
        }
        if i + 3 >= data.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if seg_len < 2 {
            break;
        }
        i += 2 + seg_len;
    }
    None
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFormFieldRequest {
    pub page: usize,
    pub field_name: String,
    pub value: String,
}

pub fn update_form_field(
    doc: &mut lopdf::Document,
    req: &UpdateFormFieldRequest,
) -> Result<(), String> {
    let pages = doc.get_pages();
    let page_num = req.page.max(1) as u32;
    let page_id = *pages
        .get(&page_num)
        .ok_or_else(|| format!("Page {} does not exist", page_num))?;

    let annots_refs: Vec<ObjectId> = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|pd| pd.get(b"Annots").ok())
        .and_then(|o| resolve_object(doc, o).cloned())
        .and_then(|o| o.as_array().ok().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| o.as_reference().ok())
        .collect();

    let mut updated = false;
    for aid in annots_refs {
        let (matched, parent_id, rect_pdf, ftype) = {
            let Ok(dict) = doc.get_dictionary(aid) else {
                continue;
            };
            let is_w = dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| n == b"Widget")
                .unwrap_or(false);
            if !is_w {
                continue;
            }
            let pid = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
            let pdict = pid.and_then(|id| doc.get_dictionary(id).ok());
            let nm = dict
                .get(b"T")
                .ok()
                .map(pdf_text_to_string)
                .filter(|s| !s.is_empty())
                .or_else(|| pdict.and_then(|pd| pd.get(b"T").ok().map(pdf_text_to_string)))
                .or_else(|| dict.get(b"NM").ok().map(pdf_text_to_string))
                .unwrap_or_default();
            if nm != req.field_name {
                continue;
            }
            let ft = dict
                .get(b"FT")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(|n| String::from_utf8_lossy(n).to_string())
                .or_else(|| {
                    pdict.and_then(|pd| {
                        pd.get(b"FT")
                            .ok()
                            .and_then(|o| o.as_name().ok())
                            .map(|n| String::from_utf8_lossy(n).to_string())
                    })
                })
                .unwrap_or_else(|| "Tx".to_string());
            let r = dict
                .get(b"Rect")
                .ok()
                .and_then(|o| resolve_object(doc, o))
                .and_then(|o| match o {
                    Object::Array(a) if a.len() >= 4 => Some([
                        as_f32(&a[0]).unwrap_or(0.0),
                        as_f32(&a[1]).unwrap_or(0.0),
                        as_f32(&a[2]).unwrap_or(80.0),
                        as_f32(&a[3]).unwrap_or(20.0),
                    ]),
                    _ => None,
                })
                .unwrap_or([0.0, 0.0, 80.0, 20.0]);
            (true, pid, r, ft)
        };
        if !matched {
            continue;
        }

        // Write /V cleanly to field or parent dictionary + regenerate /AP /N appearance
        let val_obj = if ftype == "Btn" {
            let on_state = if req.value == "true" || req.value == "Yes" || req.value == "On" {
                "Yes"
            } else {
                "Off"
            };
            Object::Name(on_state.as_bytes().to_vec())
        } else {
            string_to_pdf_text_object(&req.value)
        };

        if let Some(pid) = parent_id {
            if let Ok(Object::Dictionary(ref mut pdict)) = doc.get_object_mut(pid) {
                pdict.set("V", val_obj.clone());
            }
        }
        if ftype == "Btn" {
            let is_checked = req.value == "true" || req.value == "Yes" || req.value == "On";
            let mut helv = Dictionary::new();
            helv.set("Type", Object::Name(b"Font".to_vec()));
            helv.set("Subtype", Object::Name(b"Type1".to_vec()));
            helv.set("BaseFont", Object::Name(b"ZapfDingbats".to_vec()));
            let hid = doc.add_object(Object::Dictionary(helv));
            let mut fmap = Dictionary::new();
            fmap.set("ZaDb", Object::Reference(hid));
            let mut res = Dictionary::new();
            res.set("Font", Object::Dictionary(fmap));
            let rw = (rect_pdf[2] - rect_pdf[0]).max(8.0);
            let rh = (rect_pdf[3] - rect_pdf[1]).max(8.0);
            let mut ops = vec![
                Operation::new("q", vec![]),
                Operation::new("RG", vec![Object::Real(0.2), Object::Real(0.2), Object::Real(0.2)]),
                Operation::new("w", vec![Object::Real(1.0)]),
                Operation::new(
                    "re",
                    vec![
                        Object::Real(rect_pdf[0] + 0.5),
                        Object::Real(rect_pdf[1] + 0.5),
                        Object::Real(rw - 1.0),
                        Object::Real(rh - 1.0),
                    ],
                ),
                Operation::new("S", vec![]),
            ];
            if is_checked {
                ops.push(Operation::new("m", vec![Object::Real(rect_pdf[0] + rw * 0.22), Object::Real(rect_pdf[1] + rh * 0.48)]));
                ops.push(Operation::new("l", vec![Object::Real(rect_pdf[0] + rw * 0.42), Object::Real(rect_pdf[1] + rh * 0.24)]));
                ops.push(Operation::new("l", vec![Object::Real(rect_pdf[0] + rw * 0.78), Object::Real(rect_pdf[1] + rh * 0.76)]));
                ops.push(Operation::new("S", vec![]));
            }
            ops.push(Operation::new("Q", vec![]));
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res)?;
            if let Ok(Object::Dictionary(ref mut wdict)) = doc.get_object_mut(aid) {
                wdict.set("V", val_obj);
                wdict.set("AS", Object::Name(if is_checked { b"Yes".to_vec() } else { b"Off".to_vec() }));
                let mut ap = Dictionary::new();
                ap.set("N", Object::Reference(ap_id));
                wdict.set("AP", Object::Dictionary(ap));
            }
        } else {
            let mut helv = Dictionary::new();
            helv.set("Type", Object::Name(b"Font".to_vec()));
            helv.set("Subtype", Object::Name(b"Type1".to_vec()));
            helv.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
            helv.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
            let hid = doc.add_object(Object::Dictionary(helv));
            let mut fmap = Dictionary::new();
            fmap.set("Helv", Object::Reference(hid));
            let mut res = Dictionary::new();
            res.set("Font", Object::Dictionary(fmap));
            let rh = (rect_pdf[3] - rect_pdf[1]).max(12.0);
            let fs = (rh * 0.62).clamp(8.0, 14.0);
            let ops = vec![
                Operation::new("BT", vec![]),
                Operation::new("rg", vec![Object::Real(0.1), Object::Real(0.1), Object::Real(0.15)]),
                Operation::new("Tf", vec![Object::Name(b"Helv".to_vec()), Object::Real(fs)]),
                Operation::new(
                    "Td",
                    vec![
                        Object::Real(rect_pdf[0] + 3.0),
                        Object::Real(rect_pdf[1] + (rh - fs) * 0.5 + 1.0),
                    ],
                ),
                Operation::new(
                    "Tj",
                    vec![Object::String(
                        encode_winansi_bytes(&req.value),
                        lopdf::StringFormat::Literal,
                    )],
                ),
                Operation::new("ET", vec![]),
            ];
            let ap_id = create_form_ap_stream(doc, rect_pdf, ops, res)?;
            if let Ok(Object::Dictionary(ref mut wdict)) = doc.get_object_mut(aid) {
                wdict.set("V", val_obj);
                let mut ap = Dictionary::new();
                ap.set("N", Object::Reference(ap_id));
                wdict.set("AP", Object::Dictionary(ap));
            }
        }
        updated = true;
    }

    if updated {
        if let Ok(cat_id) = doc.trailer.get(b"Root").and_then(|o| o.as_reference()) {
            let af_id = doc
                .get_dictionary(cat_id)
                .ok()
                .and_then(|cd| cd.get(b"AcroForm").ok())
                .and_then(|o| o.as_reference().ok());
            if let Some(af) = af_id {
                if let Ok(Object::Dictionary(ref mut afd)) = doc.get_object_mut(af) {
                    afd.set("NeedAppearances", Object::Boolean(true));
                }
            } else if let Ok(Object::Dictionary(ref mut cd)) = doc.get_object_mut(cat_id) {
                if let Ok(Object::Dictionary(ref mut afd)) = cd.get_mut(b"AcroForm") {
                    afd.set("NeedAppearances", Object::Boolean(true));
                }
            }
        }
    }

    Ok(())
}

/// Real page rotation modifying `/Rotate` (normalized 0/90/180/270) in each targeted Page dictionary.
pub fn rotate_pages(
    doc: &mut lopdf::Document,
    pages_one: &[usize],
    delta_deg: i32,
) -> Result<(), String> {
    let all_pages = doc.get_pages();
    for &p in pages_one {
        let Some(&page_id) = all_pages.get(&(p as u32)) else {
            continue;
        };
        let pt = get_page_transform(doc, page_id);
        let next_rot = ((pt.rotation + delta_deg) % 360 + 360) % 360;
        let obj = doc
            .get_object_mut(page_id)
            .map_err(|e| format!("Failed accessing page {}: {}", p, e))?;
        if let Object::Dictionary(ref mut dict) = obj {
            dict.set("Rotate", Object::Integer(next_rot as i64));
        }
    }
    Ok(())
}

/// Real content removal redaction: strips text glyphs & paths intersecting the redaction rect(s)
/// from the PDF page content stream and paints opaque black redaction rectangles.
pub fn apply_redactions(
    doc: &mut lopdf::Document,
    page: usize,
    rects_vp: &[[f32; 4]],
) -> Result<(), String> {
    if rects_vp.is_empty() {
        return Ok(());
    }
    let pages = doc.get_pages();
    let page_id = *pages
        .get(&(page.max(1) as u32))
        .ok_or_else(|| format!("Page {} not found", page))?;
    let parsed = crate::pdf_objects::parse_page_objects(doc, page - 1)?;
    let pt = parsed.page_transform;

    let overlaps_any = |bbox_vp: [f32; 4]| -> bool {
        let [ax, ay, aw, ah] = bbox_vp;
        let ax2 = ax + aw;
        let ay2 = ay + ah;
        rects_vp.iter().any(|&[rx, ry, rw, rh]| {
            let ix0 = ax.max(rx);
            let iy0 = ay.max(ry);
            let ix1 = ax2.min(rx + rw);
            let iy1 = ay2.min(ry + rh);
            (ix1 - ix0) > 1.0 && (iy1 - iy0) > 1.0
        })
    };

    let mut drop_ranges: Vec<(usize, usize)> = Vec::new();
    for t in &parsed.text_objects {
        if overlaps_any(t.bbox_vp) {
            drop_ranges.push((t.op_start, t.op_end));
        }
    }
    for s in &parsed.path_objects {
        if !s.is_page_background && overlaps_any(s.bbox_vp) {
            drop_ranges.push((s.op_start, s.op_end));
        }
    }
    let mut drop_indices: HashSet<usize> = HashSet::new();
    for im in &parsed.image_objects {
        // Only delete image XObject entirely if majority of image is covered by redaction box
        let [ix, iy, iw, ih] = im.bbox_vp;
        let area = (iw * ih).max(1.0);
        let covered = rects_vp.iter().any(|&[rx, ry, rw, rh]| {
            let ox = (ix + iw).min(rx + rw) - ix.max(rx);
            let oy = (iy + ih).min(ry + rh) - iy.max(ry);
            ox > 0.0 && oy > 0.0 && (ox * oy) / area > 0.55
        });
        if covered {
            drop_indices.insert(im.op_index);
        }
    }

    let raw = doc.get_page_content(page_id);
    let mut content =
        Content::decode(&raw).map_err(|e| format!("Failed decoding page stream: {}", e))?;

    let mut next_ops: Vec<Operation> = Vec::with_capacity(content.operations.len() + 16);
    for (idx, op) in content.operations.drain(..).enumerate() {
        let in_range = drop_ranges.iter().any(|&(s, e)| idx >= s && idx <= e);
        if in_range || drop_indices.contains(&idx) {
            continue;
        }
        next_ops.push(op);
    }

    // Remove overlapping annotations cleanly too
    if let Ok((arr_id_opt, mut annots)) = ensure_page_annots_list(doc, page_id) {
        annots.retain(|item| {
            let Some(d) = resolve_dict(doc, item) else {
                return true;
            };
            let Some(Object::Array(r)) = d.get(b"Rect").ok().and_then(|o| resolve_object(doc, o)) else {
                return true;
            };
            if r.len() < 4 {
                return true;
            }
            let r_vp = pt.pdf_rect_to_vp(
                as_f32(&r[0]).unwrap_or(0.0),
                as_f32(&r[1]).unwrap_or(0.0),
                as_f32(&r[2]).unwrap_or(0.0),
                as_f32(&r[3]).unwrap_or(0.0),
            );
            !overlaps_any(r_vp)
        });
        let _ = write_page_annots_list(doc, page_id, arr_id_opt, annots);
    }

    // Paint permanent black redaction boxes directly into the page stream
    for &r_vp in rects_vp {
        let [x0, y0, x1, y1] = vp_rect_to_pdf_rect(&pt, r_vp);
        next_ops.push(Operation::new("q", vec![]));
        next_ops.push(Operation::new(
            "rg",
            vec![Object::Real(0.0), Object::Real(0.0), Object::Real(0.0)],
        ));
        next_ops.push(Operation::new(
            "re",
            vec![
                Object::Real(x0),
                Object::Real(y0),
                Object::Real((x1 - x0).max(1.0)),
                Object::Real((y1 - y0).max(1.0)),
            ],
        ));
        next_ops.push(Operation::new("f", vec![]));
        next_ops.push(Operation::new("Q", vec![]));
    }

    let encoded = Content { operations: next_ops }
        .encode()
        .map_err(|e| format!("Failed encoding redacted stream: {}", e))?;
    doc.change_page_content(page_id, encoded)
        .map_err(|e| format!("Failed writing redacted content: {}", e))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocPropertiesDto {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
    pub producer: String,
}

pub fn get_doc_properties(doc: &lopdf::Document) -> DocPropertiesDto {
    let info_opt = doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|o| resolve_dict(doc, o));
    let read_key = |k: &[u8]| -> String {
        info_opt
            .and_then(|d| d.get(k).ok())
            .map(pdf_text_to_string)
            .unwrap_or_default()
    };
    DocPropertiesDto {
        title: read_key(b"Title"),
        author: read_key(b"Author"),
        subject: read_key(b"Subject"),
        keywords: read_key(b"Keywords"),
        creator: read_key(b"Creator"),
        producer: read_key(b"Producer"),
    }
}

pub fn set_doc_properties(
    doc: &mut lopdf::Document,
    props: &DocPropertiesDto,
) -> Result<(), String> {
    let info_id = match doc.trailer.get(b"Info").ok().and_then(|o| o.as_reference().ok()) {
        Some(id) => id,
        None => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            doc.trailer.set("Info", Object::Reference(id));
            id
        }
    };
    if let Ok(Object::Dictionary(ref mut idict)) = doc.get_object_mut(info_id) {
        idict.set("Title", string_to_pdf_text_object(&props.title));
        idict.set("Author", string_to_pdf_text_object(&props.author));
        idict.set("Subject", string_to_pdf_text_object(&props.subject));
        idict.set("Keywords", string_to_pdf_text_object(&props.keywords));
        if !props.creator.trim().is_empty() {
            idict.set("Creator", string_to_pdf_text_object(&props.creator));
        }
        idict.set("Producer", string_to_pdf_text_object("PDFForge Studio Native Rust"));
    }

    // Write matching XMP Metadata stream to Catalog
    let xml_esc = |s: &str| -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let xmp = format!(
        "<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"\n\
                  xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n\
                  xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\n\
           <rdf:Description rdf:about=\"\">\n\
             <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{title}</rdf:li></rdf:Alt></dc:title>\n\
             <dc:creator><rdf:Seq><rdf:li>{author}</rdf:li></rdf:Seq></dc:creator>\n\
             <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{subject}</rdf:li></rdf:Alt></dc:description>\n\
             <pdf:Keywords>{keywords}</pdf:Keywords>\n\
             <pdf:Producer>PDFForge Studio Native Rust</pdf:Producer>\n\
           </rdf:Description>\n\
         </rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>",
        title = xml_esc(&props.title),
        author = xml_esc(&props.author),
        subject = xml_esc(&props.subject),
        keywords = xml_esc(&props.keywords),
    );
    let mut xmp_dict = Dictionary::new();
    xmp_dict.set("Type", Object::Name(b"Metadata".to_vec()));
    xmp_dict.set("Subtype", Object::Name(b"XML".to_vec()));
    let xmp_id = doc.add_object(Object::Stream(Stream::new(xmp_dict, xmp.into_bytes())));
    if let Ok(cat_id) = doc.trailer.get(b"Root").and_then(|o| o.as_reference()) {
        if let Ok(Object::Dictionary(ref mut cat)) = doc.get_object_mut(cat_id) {
            cat.set("Metadata", Object::Reference(xmp_id));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkDto {
    pub title: String,
    pub page: usize,
}

pub fn list_bookmarks(doc: &lopdf::Document) -> Vec<BookmarkDto> {
    let pages = doc.get_pages();
    let id_to_page: HashMap<ObjectId, usize> =
        pages.iter().map(|(&n, &oid)| (oid, n as usize)).collect();

    let Some(cat_id) = doc.trailer.get(b"Root").ok().and_then(|o| o.as_reference().ok()) else {
        return Vec::new();
    };
    let Ok(cat) = doc.get_dictionary(cat_id) else {
        return Vec::new();
    };
    let Some(outlines_id) = cat.get(b"Outlines").ok().and_then(|o| o.as_reference().ok()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur_opt = doc
        .get_dictionary(outlines_id)
        .ok()
        .and_then(|d| d.get(b"First").ok())
        .and_then(|o| o.as_reference().ok());
    let mut visited = HashSet::new();
    while let Some(oid) = cur_opt {
        if !visited.insert(oid) {
            break;
        }
        let Ok(d) = doc.get_dictionary(oid) else {
            break;
        };
        let title = d.get(b"Title").ok().map(pdf_text_to_string).unwrap_or_default();
        let page = d
            .get(b"Dest")
            .ok()
            .and_then(|o| resolve_object(doc, o))
            .and_then(|o| o.as_array().ok())
            .and_then(|a| a.first())
            .and_then(|o| o.as_reference().ok())
            .and_then(|pid| id_to_page.get(&pid).copied())
            .unwrap_or(1);
        if !title.is_empty() {
            out.push(BookmarkDto { title, page });
        }
        cur_opt = d.get(b"Next").ok().and_then(|o| o.as_reference().ok());
    }
    out
}

pub fn write_bookmarks(doc: &mut lopdf::Document, items: &[BookmarkDto]) -> Result<(), String> {
    let pages = doc.get_pages();
    let cat_id = doc
        .trailer
        .get(b"Root")
        .and_then(|o| o.as_reference())
        .map_err(|_| "Missing catalog /Root".to_string())?;

    if items.is_empty() {
        if let Ok(Object::Dictionary(ref mut cat)) = doc.get_object_mut(cat_id) {
            cat.remove(b"Outlines");
        }
        return Ok(());
    }

    let mut outlines_dict = Dictionary::new();
    outlines_dict.set("Type", Object::Name(b"Outlines".to_vec()));
    outlines_dict.set("Count", Object::Integer(items.len() as i64));
    let outlines_id = doc.add_object(Object::Dictionary(outlines_dict));

    let mut item_ids: Vec<ObjectId> = Vec::with_capacity(items.len());
    for bm in items {
        let pno = (bm.page.max(1) as u32).min(pages.len().max(1) as u32);
        let pid = pages.get(&pno).copied().or_else(|| pages.values().next().copied());
        let mut d = Dictionary::new();
        d.set("Title", string_to_pdf_text_object(&bm.title));
        d.set("Parent", Object::Reference(outlines_id));
        if let Some(target_pid) = pid {
            d.set(
                "Dest",
                Object::Array(vec![
                    Object::Reference(target_pid),
                    Object::Name(b"Fit".to_vec()),
                ]),
            );
        }
        item_ids.push(doc.add_object(Object::Dictionary(d)));
    }

    for i in 0..item_ids.len() {
        let cur = item_ids[i];
        let prev = if i > 0 { Some(item_ids[i - 1]) } else { None };
        let next = if i + 1 < item_ids.len() { Some(item_ids[i + 1]) } else { None };
        if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(cur) {
            if let Some(p) = prev {
                d.set("Prev", Object::Reference(p));
            }
            if let Some(n) = next {
                d.set("Next", Object::Reference(n));
            }
        }
    }

    if let Ok(Object::Dictionary(ref mut od)) = doc.get_object_mut(outlines_id) {
        od.set("First", Object::Reference(*item_ids.first().unwrap()));
        od.set("Last", Object::Reference(*item_ids.last().unwrap()));
    }
    if let Ok(Object::Dictionary(ref mut cat)) = doc.get_object_mut(cat_id) {
        cat.set("Outlines", Object::Reference(outlines_id));
    }
    Ok(())
}

pub fn set_page_labels(
    doc: &mut lopdf::Document,
    style: &str,
    prefix: &str,
    start_num: i64,
) -> Result<(), String> {
    let cat_id = doc
        .trailer
        .get(b"Root")
        .and_then(|o| o.as_reference())
        .map_err(|_| "Missing catalog /Root".to_string())?;
    let mut entry = Dictionary::new();
    let s_code = match style {
        "roman_lower" | "r" => b"r".to_vec(),
        "roman_upper" | "R" => b"R".to_vec(),
        "alpha_lower" | "a" => b"a".to_vec(),
        "alpha_upper" | "A" => b"A".to_vec(),
        _ => b"D".to_vec(),
    };
    entry.set("S", Object::Name(s_code));
    if !prefix.is_empty() {
        entry.set("P", string_to_pdf_text_object(prefix));
    }
    entry.set("St", Object::Integer(start_num.max(1)));

    let mut page_labels = Dictionary::new();
    page_labels.set(
        "Nums",
        Object::Array(vec![Object::Integer(0), Object::Dictionary(entry)]),
    );
    let pl_id = doc.add_object(Object::Dictionary(page_labels));
    if let Ok(Object::Dictionary(ref mut cat)) = doc.get_object_mut(cat_id) {
        cat.set("PageLabels", Object::Reference(pl_id));
    }
    Ok(())
}

pub fn crop_pages_real(
    doc: &mut lopdf::Document,
    pages_one: &[usize],
    margins: [f32; 4], // [left, top, right, bottom] in points
) -> Result<(), String> {
    let all_pages = doc.get_pages();
    for &p in pages_one {
        let Some(&pid) = all_pages.get(&(p as u32)) else {
            continue;
        };
        let pt = get_page_transform(doc, pid);
        let [ml, mb, mr, mt] = pt.media_box;
        let llx = (ml + margins[0].max(0.0)).min(mr - 36.0);
        let lly = (mb + margins[3].max(0.0)).min(mt - 36.0);
        let urx = (mr - margins[2].max(0.0)).max(llx + 36.0);
        let ury = (mt - margins[1].max(0.0)).max(lly + 36.0);
        if let Ok(Object::Dictionary(ref mut pd)) = doc.get_object_mut(pid) {
            pd.set(
                "CropBox",
                Object::Array(vec![
                    Object::Real(llx),
                    Object::Real(lly),
                    Object::Real(urx),
                    Object::Real(ury),
                ]),
            );
        }
    }
    Ok(())
}

/// Deep-clone all indirect objects referenced by a page into `dst` (for insert_pdf, duplicate, combine).
pub fn copy_object_recursive(
    src: &lopdf::Document,
    dst: &mut lopdf::Document,
    id: ObjectId,
    map: &mut HashMap<ObjectId, ObjectId>,
) -> Option<ObjectId> {
    if let Some(&mapped) = map.get(&id) {
        return Some(mapped);
    }
    let obj = src.get_object(id).ok()?.clone();
    let placeholder_id = dst.add_object(Object::Null);
    map.insert(id, placeholder_id);
    let rewritten = rewrite_object_refs(src, dst, obj, map);
    if let Ok(slot) = dst.get_object_mut(placeholder_id) {
        *slot = rewritten;
    }
    Some(placeholder_id)
}

fn rewrite_object_refs(
    src: &lopdf::Document,
    dst: &mut lopdf::Document,
    obj: Object,
    map: &mut HashMap<ObjectId, ObjectId>,
) -> Object {
    match obj {
        Object::Reference(rid) => {
            if let Some(new_id) = copy_object_recursive(src, dst, rid, map) {
                Object::Reference(new_id)
            } else {
                Object::Null
            }
        }
        Object::Array(arr) => Object::Array(
            arr.into_iter()
                .map(|x| rewrite_object_refs(src, dst, x, map))
                .collect(),
        ),
        Object::Dictionary(d) => {
            let mut nd = Dictionary::new();
            for (k, v) in d.iter() {
                if k == b"Parent" || k == b"P" {
                    continue;
                }
                nd.set(k.clone(), rewrite_object_refs(src, dst, v.clone(), map));
            }
            Object::Dictionary(nd)
        }
        Object::Stream(mut st) => {
            let mut nd = Dictionary::new();
            for (k, v) in st.dict.iter() {
                nd.set(k.clone(), rewrite_object_refs(src, dst, v.clone(), map));
            }
            st.dict = nd;
            Object::Stream(st)
        }
        other => other,
    }
}

fn set_pages_kids(doc: &mut lopdf::Document, new_kids: Vec<ObjectId>) -> Result<(), String> {
    let cat_id = doc
        .trailer
        .get(b"Root")
        .and_then(|o| o.as_reference())
        .map_err(|_| "Missing catalog /Root".to_string())?;
    let pages_id = doc
        .get_dictionary(cat_id)
        .and_then(|cd| cd.get(b"Pages"))
        .and_then(|o| o.as_reference())
        .map_err(|_| "Missing catalog /Pages".to_string())?;

    for &kid in &new_kids {
        if let Ok(Object::Dictionary(ref mut kd)) = doc.get_object_mut(kid) {
            kd.set("Parent", Object::Reference(pages_id));
        }
    }
    if let Ok(Object::Dictionary(ref mut pdict)) = doc.get_object_mut(pages_id) {
        pdict.set("Count", Object::Integer(new_kids.len() as i64));
        pdict.set(
            "Kids",
            Object::Array(new_kids.into_iter().map(Object::Reference).collect()),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageOpsRequest {
    pub action: String, // "insert_blank" | "insert_pdf" | "delete" | "duplicate" | "reorder" | "extract"
    pub page: Option<usize>,
    pub order: Option<Vec<usize>>, // 1-based page numbers for reorder or extract
    pub source_file_path: Option<String>,
}

pub fn apply_page_operation(
    doc: &mut lopdf::Document,
    req: &PageOpsRequest,
) -> Result<(), String> {
    let pages = doc.get_pages();
    let mut kids: Vec<ObjectId> = pages
        .iter()
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .copied()
        .collect();
    if kids.is_empty() {
        return Err("Document has zero pages".into());
    }

    match req.action.as_str() {
        "delete" => {
            if kids.len() <= 1 {
                return Err("Cannot delete the last remaining page".into());
            }
            let p_idx = req.page.unwrap_or(1).saturating_sub(1);
            if p_idx < kids.len() {
                kids.remove(p_idx);
            }
            set_pages_kids(doc, kids)?;
        }
        "duplicate" => {
            let p_idx = req.page.unwrap_or(1).saturating_sub(1).min(kids.len() - 1);
            let src_clone = doc.clone();
            let mut map = HashMap::new();
            if let Some(new_pid) = copy_object_recursive(&src_clone, doc, kids[p_idx], &mut map) {
                kids.insert(p_idx + 1, new_pid);
                set_pages_kids(doc, kids)?;
            }
        }
        "insert_blank" => {
            let p_idx = req.page.unwrap_or(1).saturating_sub(1).min(kids.len() - 1);
            let pt = get_page_transform(doc, kids[p_idx]);
            let mut page_dict = Dictionary::new();
            page_dict.set("Type", Object::Name(b"Page".to_vec()));
            page_dict.set(
                "MediaBox",
                Object::Array(vec![
                    Object::Real(pt.media_box[0]),
                    Object::Real(pt.media_box[1]),
                    Object::Real(pt.media_box[2]),
                    Object::Real(pt.media_box[3]),
                ]),
            );
            page_dict.set("Resources", Object::Dictionary(Dictionary::new()));
            let blank_id = doc.add_object(Object::Dictionary(page_dict));
            kids.insert(p_idx + 1, blank_id);
            set_pages_kids(doc, kids)?;
        }
        "insert_pdf" => {
            let src_path = req
                .source_file_path
                .as_deref()
                .ok_or("Missing sourceFilePath for insert_pdf")?;
            let src_doc = lopdf::Document::load(src_path)
                .map_err(|e| format!("Failed opening source PDF: {}", e))?;
            let src_pages = src_doc.get_pages();
            let mut map = HashMap::new();
            let mut insert_at = req.page.unwrap_or(kids.len()).min(kids.len());
            for (_, src_pid) in src_pages.into_iter().collect::<BTreeMap<_, _>>() {
                if let Some(copied) = copy_object_recursive(&src_doc, doc, src_pid, &mut map) {
                    kids.insert(insert_at, copied);
                    insert_at += 1;
                }
            }
            set_pages_kids(doc, kids)?;
        }
        "reorder" => {
            let ord = req.order.as_deref().ok_or("Missing page order")?;
            let src_clone = doc.clone();
            let mut map = HashMap::new();
            let mut seen = HashSet::new();
            let mut next_kids = Vec::with_capacity(ord.len());
            for &p1 in ord {
                let idx = p1.saturating_sub(1);
                if idx < kids.len() {
                    let orig_id = kids[idx];
                    if seen.insert(orig_id) {
                        next_kids.push(orig_id);
                    } else if let Some(dup) = copy_object_recursive(&src_clone, doc, orig_id, &mut map) {
                        next_kids.push(dup);
                    }
                }
            }
            if next_kids.is_empty() {
                return Err("Reordered page list is empty".into());
            }
            set_pages_kids(doc, next_kids)?;
        }
        other => return Err(format!("Unsupported page op '{}'", other)),
    }
    doc.prune_objects();
    Ok(())
}

pub fn extract_pages_to_bytes(doc: &lopdf::Document, pages_one: &[usize]) -> Result<Vec<u8>, String> {
    let pages_map = doc.get_pages();
    let mut out = lopdf::Document::with_version("1.6");
    let pages_id = out.new_object_id();
    let mut map = HashMap::new();
    let mut new_kids = Vec::new();
    for &p in pages_one {
        if let Some(&src_pid) = pages_map.get(&(p.max(1) as u32)) {
            if let Some(dst_pid) = copy_object_recursive(doc, &mut out, src_pid, &mut map) {
                if let Ok(Object::Dictionary(ref mut kd)) = out.get_object_mut(dst_pid) {
                    kd.set("Parent", Object::Reference(pages_id));
                }
                new_kids.push(Object::Reference(dst_pid));
            }
        }
    }
    if new_kids.is_empty() {
        return Err("No valid pages selected for extract".into());
    }
    let mut pdict = Dictionary::new();
    pdict.set("Type", Object::Name(b"Pages".to_vec()));
    pdict.set("Count", Object::Integer(new_kids.len() as i64));
    pdict.set("Kids", Object::Array(new_kids));
    out.objects.insert(pages_id, Object::Dictionary(pdict));
    let mut cat = Dictionary::new();
    cat.set("Type", Object::Name(b"Catalog".to_vec()));
    cat.set("Pages", Object::Reference(pages_id));
    let cat_id = out.add_object(Object::Dictionary(cat));
    out.trailer.set("Root", Object::Reference(cat_id));
    out.prune_objects();
    out.compress();
    let mut bytes = Vec::new();
    out.save_to(&mut bytes)
        .map_err(|e| format!("Failed serializing extracted PDF: {}", e))?;
    Ok(bytes)
}

pub fn combine_pdfs_to_bytes(paths: &[String]) -> Result<Vec<u8>, String> {
    if paths.len() < 2 {
        return Err("Select at least 2 PDF files to combine".into());
    }
    let mut out = lopdf::Document::with_version("1.6");
    let pages_id = out.new_object_id();
    let mut new_kids = Vec::new();
    for path in paths {
        let src = lopdf::Document::load(path)
            .map_err(|e| format!("Failed opening '{}': {}", path, e))?;
        let src_pages = src.get_pages();
        let mut map = HashMap::new();
        for (_, src_pid) in src_pages.into_iter().collect::<BTreeMap<_, _>>() {
            if let Some(dst_pid) = copy_object_recursive(&src, &mut out, src_pid, &mut map) {
                if let Ok(Object::Dictionary(ref mut kd)) = out.get_object_mut(dst_pid) {
                    kd.set("Parent", Object::Reference(pages_id));
                }
                new_kids.push(Object::Reference(dst_pid));
            }
        }
    }
    let mut pdict = Dictionary::new();
    pdict.set("Type", Object::Name(b"Pages".to_vec()));
    pdict.set("Count", Object::Integer(new_kids.len() as i64));
    pdict.set("Kids", Object::Array(new_kids));
    out.objects.insert(pages_id, Object::Dictionary(pdict));
    let mut cat = Dictionary::new();
    cat.set("Type", Object::Name(b"Catalog".to_vec()));
    cat.set("Pages", Object::Reference(pages_id));
    let cat_id = out.add_object(Object::Dictionary(cat));
    out.trailer.set("Root", Object::Reference(cat_id));
    out.prune_objects();
    out.compress();
    let mut bytes = Vec::new();
    out.save_to(&mut bytes)
        .map_err(|e| format!("Failed combining PDFs: {}", e))?;
    Ok(bytes)
}

/// Serialize, optionally compact or preserve incremental updates, verify with PDFium,
/// write atomically to a temp sibling file, and replace `target_path`.
pub fn save_pdf_verified(
    working_bytes: &[u8],
    orig_disk_bytes: Option<&[u8]>,
    target_path: &str,
    incremental_preferred: bool,
    compact_objects: bool,
) -> Result<(Vec<u8>, bool), String> {
    let has_sig = orig_disk_bytes
        .map(|b| b.windows(b"/ByteRange".len()).any(|w| w == b"/ByteRange"))
        .unwrap_or(false);

    let out_bytes: Vec<u8> = if (incremental_preferred || has_sig) && orig_disk_bytes.is_some() {
        let orig = orig_disk_bytes.unwrap();
        if let (Ok(prev_doc), Ok(cur_doc)) = (
            lopdf::Document::load_mem(orig),
            lopdf::Document::load_mem(working_bytes),
        ) {
            let mut inc = lopdf::IncrementalDocument::create_from(orig.to_vec(), prev_doc.clone());
            for (oid, obj) in &cur_doc.objects {
                let changed = match prev_doc.objects.get(oid) {
                    Some(prev_obj) => prev_obj != obj,
                    None => true,
                };
                if changed {
                    inc.new_document.set_object(*oid, obj.clone());
                }
            }
            if let Ok(root) = cur_doc.trailer.get(b"Root") {
                inc.new_document.trailer.set("Root", root.clone());
            }
            if let Ok(info) = cur_doc.trailer.get(b"Info") {
                inc.new_document.trailer.set("Info", info.clone());
            }
            let mut buf = Vec::new();
            if inc.save_to(&mut buf).is_ok() && !buf.is_empty() {
                buf
            } else {
                working_bytes.to_vec()
            }
        } else {
            working_bytes.to_vec()
        }
    } else if compact_objects {
        let mut lo_doc = lopdf::Document::load_mem(working_bytes)
            .map_err(|e| format!("Failed parsing PDF for compaction: {}", e))?;
        lo_doc.prune_objects();
        lo_doc.renumber_objects();
        lo_doc.compress();
        let mut buf = Vec::new();
        lo_doc
            .save_to(&mut buf)
            .map_err(|e| format!("Failed writing compacted PDF: {}", e))?;
        buf
    } else {
        working_bytes.to_vec()
    };

    // Write to temporary file first and verify via pdfium-render before replacing target
    let target = Path::new(target_path);
    if let Some(parent) = target.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp_name = format!(
        "{}.pdfforge_tmp.{}",
        target.file_name().and_then(|n| n.to_str()).unwrap_or("out.pdf"),
        std::process::id()
    );
    let tmp_path = target.with_file_name(tmp_name);
    fs::write(&tmp_path, &out_bytes)
        .map_err(|e| format!("Failed writing temp PDF '{}': {}", tmp_path.display(), e))?;

    // Verify with PDFium
    let pdfium = crate::pdf_layout::get_pdfium()?;
    {
        let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
        let verified_doc = guard.load_pdf_from_file(&tmp_path, None).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            format!("PDFium verification rejected saved PDF: {:?}", e)
        })?;
        if verified_doc.pages().len() == 0 {
            let _ = fs::remove_file(&tmp_path);
            return Err("PDFium verification failed: saved PDF has 0 pages".into());
        }
    }

    if target.exists() {
        let _ = fs::remove_file(target);
    }
    if let Err(e) = fs::rename(&tmp_path, target) {
        // Fallback copy + remove across volumes if rename fails
        fs::copy(&tmp_path, target).map_err(|ce| {
            let _ = fs::remove_file(&tmp_path);
            format!("Failed saving '{}': {} / {}", target.display(), e, ce)
        })?;
        let _ = fs::remove_file(&tmp_path);
    }

    Ok((out_bytes, has_sig))
}
