use crate::font_shaper::{
    attach_resource_entry, encode_cid_glyph_bytes, encode_winansi_bytes, ensure_page_font_resource,
    resolve_dict, resolve_object, shape_and_reflow_block, PdfFontWriteMode,
};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageTransform {
    pub media_box: [f32; 4],
    pub crop_box: [f32; 4],
    pub rotation: i32,
    pub vp_width: f32,
    pub vp_height: f32,
    /// Affine matrix [a, b, c, d, e, f]: pdf -> upright viewport
    pub matrix: [f32; 6],
}

impl PageTransform {
    pub fn from_boxes(crop_box: [f32; 4], rotation: i32) -> Self {
        let rot = ((rotation % 360) + 360) % 360;
        let [llx, lly, urx, ury] = crop_box;
        let cw = (urx - llx).abs().max(1.0);
        let ch = (ury - lly).abs().max(1.0);
        let (vp_width, vp_height, matrix) = match rot {
            90 => (ch, cw, [0.0, 1.0, 1.0, 0.0, -lly, -llx]),
            180 => (cw, ch, [-1.0, 0.0, 0.0, 1.0, urx, -lly]),
            270 => (ch, cw, [0.0, -1.0, -1.0, 0.0, ury, urx]),
            _ => (cw, ch, [1.0, 0.0, 0.0, -1.0, -llx, ury]),
        };
        Self {
            media_box: crop_box,
            crop_box,
            rotation: rot,
            vp_width,
            vp_height,
            matrix,
        }
    }

    pub fn pdf_to_vp(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.matrix;
        (a * x + c * y + e, b * x + d * y + f)
    }

    pub fn vp_to_pdf(&self, vx: f32, vy: f32) -> (f32, f32) {
        let [llx, lly, urx, ury] = self.crop_box;
        match self.rotation {
            90 => (vy + llx, vx + lly),
            180 => (urx - vx, vy + lly),
            270 => (urx - vy, ury - vx),
            _ => (vx + llx, ury - vy),
        }
    }

    pub fn pdf_rect_to_vp(&self, llx: f32, lly: f32, urx: f32, ury: f32) -> [f32; 4] {
        let pts = [
            self.pdf_to_vp(llx, lly),
            self.pdf_to_vp(urx, lly),
            self.pdf_to_vp(llx, ury),
            self.pdf_to_vp(urx, ury),
        ];
        let min_x = pts.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let min_y = pts.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let max_x = pts.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let max_y = pts.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        [
            min_x,
            min_y,
            (max_x - min_x).max(0.5),
            (max_y - min_y).max(0.5),
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStreamObject {
    pub id: String,
    pub op_start: usize,
    pub op_end: usize,
    pub font_res: String,
    pub font_size: f32,
    pub color: String,
    pub text_preview: String,
    pub bbox_vp: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathSeg {
    pub cmd: String,
    pub pts: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathObject {
    pub id: String,
    pub op_start: usize,
    pub op_end: usize,
    /// "rectangle" | "ellipse" | "line" | "polygon" | "bezier"
    pub shape_kind: String,
    pub svg_path: String,
    pub bbox_vp: [f32; 4],
    pub fill_color: Option<String>,
    pub stroke_color: Option<String>,
    pub fill_opacity: f32,
    pub stroke_opacity: f32,
    pub stroke_width: f32,
    pub dash_array: Vec<f32>,
    pub closed: bool,
    pub is_page_background: bool,
    pub segments_vp: Vec<PathSeg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageStreamObject {
    pub id: String,
    pub op_index: usize,
    pub name: String,
    pub bbox_vp: [f32; 4],
    pub pixel_width: i64,
    pub pixel_height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageObjectsResult {
    pub page: usize,
    pub page_transform: PageTransform,
    pub text_objects: Vec<TextStreamObject>,
    pub path_objects: Vec<PathObject>,
    pub image_objects: Vec<ImageStreamObject>,
}

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: [f32; 6],
    clip_bbox: Option<[f32; 4]>,
    fill_rgb: [f32; 3],
    stroke_rgb: [f32; 3],
    fill_alpha: f32,
    stroke_alpha: f32,
    line_width: f32,
    line_cap: i64,
    line_join: i64,
    dash_array: Vec<f32>,
    dash_phase: f32,
    font_res: String,
    font_size: f32,
    tm: [f32; 6],
    tlm: [f32; 6],
    leading: f32,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            clip_bbox: None,
            fill_rgb: [0.0, 0.0, 0.0],
            stroke_rgb: [0.0, 0.0, 0.0],
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            dash_array: Vec::new(),
            dash_phase: 0.0,
            font_res: String::new(),
            font_size: 12.0,
            tm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            tlm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            leading: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
enum RawPathOp {
    Move(f32, f32),
    Line(f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Rect(f32, f32, f32, f32),
    Close,
}

fn mul_mat(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

fn tx_pt(m: [f32; 6], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn as_f32(obj: &Object) -> Option<f32> {
    match obj {
        Object::Real(v) => Some(*v),
        Object::Integer(v) => Some(*v as f32),
        _ => None,
    }
}

fn rgb_to_hex(rgb: [f32; 3]) -> String {
    let r = (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

pub fn hex_to_rgb(hex: &str) -> [f32; 3] {
    let s = hex.trim().trim_start_matches('#');
    if s.len() >= 6 {
        let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0) as f32 / 255.0;
        let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0) as f32 / 255.0;
        let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0) as f32 / 255.0;
        [r, g, b]
    } else {
        [0.0, 0.0, 0.0]
    }
}

fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [f32; 3] {
    [
        (1.0 - c).clamp(0.0, 1.0) * (1.0 - k).clamp(0.0, 1.0),
        (1.0 - m).clamp(0.0, 1.0) * (1.0 - k).clamp(0.0, 1.0),
        (1.0 - y).clamp(0.0, 1.0) * (1.0 - k).clamp(0.0, 1.0),
    ]
}

pub fn get_page_transform(doc: &lopdf::Document, page_id: ObjectId) -> PageTransform {
    fn read_box(doc: &lopdf::Document, page_id: ObjectId, key: &[u8]) -> Option<[f32; 4]> {
        let mut cur_id = Some(page_id);
        while let Some(pid) = cur_id {
            let dict = doc.get_dictionary(pid).ok()?;
            if let Ok(obj) = dict.get(key) {
                if let Some(Object::Array(arr)) = resolve_object(doc, obj) {
                    if arr.len() >= 4 {
                        return Some([
                            as_f32(&arr[0]).unwrap_or(0.0),
                            as_f32(&arr[1]).unwrap_or(0.0),
                            as_f32(&arr[2]).unwrap_or(612.0),
                            as_f32(&arr[3]).unwrap_or(792.0),
                        ]);
                    }
                }
            }
            cur_id = dict
                .get(b"Parent")
                .ok()
                .and_then(|o| o.as_reference().ok());
        }
        None
    }

    fn read_rot(doc: &lopdf::Document, page_id: ObjectId) -> i32 {
        let mut cur_id = Some(page_id);
        while let Some(pid) = cur_id {
            let Ok(dict) = doc.get_dictionary(pid) else {
                break;
            };
            if let Ok(obj) = dict.get(b"Rotate") {
                if let Some(Object::Integer(v)) = resolve_object(doc, obj) {
                    return *v as i32;
                }
            }
            cur_id = dict
                .get(b"Parent")
                .ok()
                .and_then(|o| o.as_reference().ok());
        }
        0
    }

    let media = read_box(doc, page_id, b"MediaBox").unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = read_box(doc, page_id, b"CropBox").unwrap_or(media);
    let rot = read_rot(doc, page_id);
    let mut pt = PageTransform::from_boxes(crop, rot);
    pt.media_box = media;
    pt
}

fn classify_and_build_svg(
    raw_ops: &[RawPathOp],
    ctm: [f32; 6],
    pt: &PageTransform,
    close_forced: bool,
) -> (String, String, [f32; 4], bool, Vec<PathSeg>) {
    let to_vp = |x: f32, y: f32| -> (f32, f32) {
        let (ux, uy) = tx_pt(ctm, x, y);
        pt.pdf_to_vp(ux, uy)
    };

    let mut segs: Vec<PathSeg> = Vec::new();
    let mut d = String::new();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut line_count = 0usize;
    let mut curve_count = 0usize;
    let mut rect_count = 0usize;
    let mut closed = close_forced;

    let mut track = |vx: f32, vy: f32| {
        if vx < min_x {
            min_x = vx;
        }
        if vy < min_y {
            min_y = vy;
        }
        if vx > max_x {
            max_x = vx;
        }
        if vy > max_y {
            max_y = vy;
        }
    };

    for op in raw_ops {
        match *op {
            RawPathOp::Move(x, y) => {
                let (vx, vy) = to_vp(x, y);
                track(vx, vy);
                d.push_str(&format!("M{:.2},{:.2} ", vx, vy));
                segs.push(PathSeg {
                    cmd: "M".into(),
                    pts: vec![vx, vy],
                });
            }
            RawPathOp::Line(x, y) => {
                let (vx, vy) = to_vp(x, y);
                track(vx, vy);
                d.push_str(&format!("L{:.2},{:.2} ", vx, vy));
                segs.push(PathSeg {
                    cmd: "L".into(),
                    pts: vec![vx, vy],
                });
                line_count += 1;
            }
            RawPathOp::Curve(x1, y1, x2, y2, x3, y3) => {
                let (vx1, vy1) = to_vp(x1, y1);
                let (vx2, vy2) = to_vp(x2, y2);
                let (vx3, vy3) = to_vp(x3, y3);
                track(vx1, vy1);
                track(vx2, vy2);
                track(vx3, vy3);
                d.push_str(&format!(
                    "C{:.2},{:.2} {:.2},{:.2} {:.2},{:.2} ",
                    vx1, vy1, vx2, vy2, vx3, vy3
                ));
                segs.push(PathSeg {
                    cmd: "C".into(),
                    pts: vec![vx1, vy1, vx2, vy2, vx3, vy3],
                });
                curve_count += 1;
            }
            RawPathOp::Rect(x, y, w, h) => {
                let p0 = to_vp(x, y);
                let p1 = to_vp(x + w, y);
                let p2 = to_vp(x + w, y + h);
                let p3 = to_vp(x, y + h);
                for (px, py) in [p0, p1, p2, p3] {
                    track(px, py);
                }
                d.push_str(&format!(
                    "M{:.2},{:.2} L{:.2},{:.2} L{:.2},{:.2} L{:.2},{:.2} Z ",
                    p0.0, p0.1, p1.0, p1.1, p2.0, p2.1, p3.0, p3.1
                ));
                segs.push(PathSeg {
                    cmd: "M".into(),
                    pts: vec![p0.0, p0.1],
                });
                segs.push(PathSeg {
                    cmd: "L".into(),
                    pts: vec![p1.0, p1.1],
                });
                segs.push(PathSeg {
                    cmd: "L".into(),
                    pts: vec![p2.0, p2.1],
                });
                segs.push(PathSeg {
                    cmd: "L".into(),
                    pts: vec![p3.0, p3.1],
                });
                segs.push(PathSeg {
                    cmd: "Z".into(),
                    pts: vec![],
                });
                rect_count += 1;
                closed = true;
            }
            RawPathOp::Close => {
                d.push_str("Z ");
                segs.push(PathSeg {
                    cmd: "Z".into(),
                    pts: vec![],
                });
                closed = true;
            }
        }
    }

    if close_forced && !d.ends_with("Z ") {
        d.push_str("Z");
    }

    let bbox = if min_x.is_finite() {
        [
            min_x,
            min_y,
            (max_x - min_x).max(1.0),
            (max_y - min_y).max(1.0),
        ]
    } else {
        [0.0, 0.0, 0.0, 0.0]
    };

    let shape_kind = if rect_count == 1 && line_count == 0 && curve_count == 0 {
        "rectangle".to_string()
    } else if curve_count == 4 && line_count == 0 && rect_count == 0 {
        "ellipse".to_string()
    } else if curve_count == 0 && line_count == 1 && !closed {
        "line".to_string()
    } else if curve_count == 0 && (line_count == 3 || line_count == 4) && closed {
        // Check axis-aligned rectangle polygon
        let pts: Vec<(f32, f32)> = segs
            .iter()
            .filter(|s| s.cmd == "M" || s.cmd == "L")
            .filter_map(|s| {
                if s.pts.len() >= 2 {
                    Some((s.pts[0], s.pts[1]))
                } else {
                    None
                }
            })
            .collect();
        let is_rect = pts.len() >= 4
            && pts.windows(2).all(|w| {
                (w[0].0 - w[1].0).abs() < 0.8 || (w[0].1 - w[1].1).abs() < 0.8
            });
        if is_rect {
            "rectangle".to_string()
        } else {
            "polygon".to_string()
        }
    } else if curve_count == 0 && closed && line_count >= 2 {
        "polygon".to_string()
    } else if curve_count == 0 && !closed && line_count >= 2 {
        "polygon".to_string()
    } else {
        "bezier".to_string()
    };

    (shape_kind, d.trim().to_string(), bbox, closed, segs)
}

pub fn parse_page_objects(
    doc: &lopdf::Document,
    page_index_zero: usize,
) -> Result<PageObjectsResult, String> {
    let pages = doc.get_pages();
    let page_num = (page_index_zero + 1) as u32;
    let page_id = *pages
        .get(&page_num)
        .ok_or_else(|| format!("Page {} not found", page_num))?;

    let pt = get_page_transform(doc, page_id);
    let raw_content = doc.get_page_content(page_id);
    let decoded = Content::decode(&raw_content)
        .map_err(|e| format!("Failed decoding page {} content stream: {}", page_num, e))?;

    // Inspect ExtGState & XObject Image dimensions in Page Resources
    let mut ext_gstates: HashMap<String, (Option<f32>, Option<f32>, Option<f32>)> = HashMap::new();
    let mut xobj_images: HashMap<String, (i64, i64)> = HashMap::new();
    if let Some(res_dict) = crate::font_shaper::get_page_resources_dict(doc, page_id) {
        if let Ok(gs_obj) = res_dict.get(b"ExtGState") {
            if let Some(gs_dict) = resolve_dict(doc, gs_obj) {
                for (name, val) in gs_dict.iter() {
                    if let Some(d) = resolve_dict(doc, val) {
                        let ca_fill = d.get(b"ca").ok().and_then(as_f32);
                        let ca_stroke = d.get(b"CA").ok().and_then(as_f32);
                        let lw = d.get(b"LW").ok().and_then(as_f32);
                        ext_gstates.insert(
                            String::from_utf8_lossy(name).to_string(),
                            (ca_fill, ca_stroke, lw),
                        );
                    }
                }
            }
        }
        if let Ok(xo_obj) = res_dict.get(b"XObject") {
            if let Some(xo_dict) = resolve_dict(doc, xo_obj) {
                for (name, val) in xo_dict.iter() {
                    if let Some(d) = resolve_dict(doc, val) {
                        let is_img = d
                            .get(b"Subtype")
                            .ok()
                            .and_then(|o| o.as_name().ok())
                            .map(|s| s == b"Image")
                            .unwrap_or(false);
                        if is_img {
                            let w = d.get(b"Width").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0);
                            let h = d.get(b"Height").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0);
                            xobj_images.insert(String::from_utf8_lossy(name).to_string(), (w, h));
                        }
                    }
                }
            }
        }
    }

    let mut stack: Vec<GraphicsState> = Vec::new();
    let mut gs = GraphicsState::default();

    let mut text_objects: Vec<TextStreamObject> = Vec::new();
    let mut path_objects: Vec<PathObject> = Vec::new();
    let mut image_objects: Vec<ImageStreamObject> = Vec::new();

    let mut in_text = false;
    let mut text_op_start = 0usize;
    let mut text_preview = String::new();
    let mut text_min_x = f32::INFINITY;
    let mut text_min_y = f32::INFINITY;
    let mut text_max_x = f32::NEG_INFINITY;
    let mut text_max_y = f32::NEG_INFINITY;

    let mut cur_path: Vec<RawPathOp> = Vec::new();
    let mut path_op_start: Option<usize> = None;
    let mut cur_pt = (0.0f32, 0.0f32);

    for (idx, op) in decoded.operations.iter().enumerate() {
        let name = op.operator.as_str();
        let ops = &op.operands;

        match name {
            "q" => stack.push(gs.clone()),
            "Q" => {
                if let Some(prev) = stack.pop() {
                    gs = prev;
                }
            }
            "cm" if ops.len() >= 6 => {
                if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                    as_f32(&ops[4]),
                    as_f32(&ops[5]),
                ) {
                    gs.ctm = mul_mat(gs.ctm, [a, b, c, d, e, f]);
                }
            }
            "w" if !ops.is_empty() => {
                if let Some(lw) = as_f32(&ops[0]) {
                    gs.line_width = lw;
                }
            }
            "J" if !ops.is_empty() => {
                if let Ok(v) = ops[0].as_i64() {
                    gs.line_cap = v;
                }
            }
            "j" if !ops.is_empty() => {
                if let Ok(v) = ops[0].as_i64() {
                    gs.line_join = v;
                }
            }
            "d" if ops.len() >= 2 => {
                if let Ok(arr) = ops[0].as_array() {
                    gs.dash_array = arr.iter().filter_map(as_f32).collect();
                }
                if let Some(ph) = as_f32(&ops[1]) {
                    gs.dash_phase = ph;
                }
            }
            "gs" if !ops.is_empty() => {
                if let Ok(nm) = ops[0].as_name() {
                    let key = String::from_utf8_lossy(nm).to_string();
                    if let Some(&(ca, ca_s, lw)) = ext_gstates.get(&key) {
                        if let Some(v) = ca {
                            gs.fill_alpha = v;
                        }
                        if let Some(v) = ca_s {
                            gs.stroke_alpha = v;
                        }
                        if let Some(v) = lw {
                            gs.line_width = v;
                        }
                    }
                }
            }
            "g" if !ops.is_empty() => {
                if let Some(v) = as_f32(&ops[0]) {
                    gs.fill_rgb = [v, v, v];
                }
            }
            "G" if !ops.is_empty() => {
                if let Some(v) = as_f32(&ops[0]) {
                    gs.stroke_rgb = [v, v, v];
                }
            }
            "rg" if ops.len() >= 3 => {
                if let (Some(r), Some(g), Some(b)) =
                    (as_f32(&ops[0]), as_f32(&ops[1]), as_f32(&ops[2]))
                {
                    gs.fill_rgb = [r, g, b];
                }
            }
            "RG" if ops.len() >= 3 => {
                if let (Some(r), Some(g), Some(b)) =
                    (as_f32(&ops[0]), as_f32(&ops[1]), as_f32(&ops[2]))
                {
                    gs.stroke_rgb = [r, g, b];
                }
            }
            "k" if ops.len() >= 4 => {
                if let (Some(c), Some(m), Some(y), Some(k)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                ) {
                    gs.fill_rgb = cmyk_to_rgb(c, m, y, k);
                }
            }
            "K" if ops.len() >= 4 => {
                if let (Some(c), Some(m), Some(y), Some(k)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                ) {
                    gs.stroke_rgb = cmyk_to_rgb(c, m, y, k);
                }
            }
            "sc" | "scn" => {
                if ops.len() >= 3 {
                    if let (Some(r), Some(g), Some(b)) =
                        (as_f32(&ops[0]), as_f32(&ops[1]), as_f32(&ops[2]))
                    {
                        gs.fill_rgb = [r, g, b];
                    }
                } else if ops.len() == 1 {
                    if let Some(v) = as_f32(&ops[0]) {
                        gs.fill_rgb = [v, v, v];
                    }
                }
            }
            "SC" | "SCN" => {
                if ops.len() >= 3 {
                    if let (Some(r), Some(g), Some(b)) =
                        (as_f32(&ops[0]), as_f32(&ops[1]), as_f32(&ops[2]))
                    {
                        gs.stroke_rgb = [r, g, b];
                    }
                } else if ops.len() == 1 {
                    if let Some(v) = as_f32(&ops[0]) {
                        gs.stroke_rgb = [v, v, v];
                    }
                }
            }
            // Path construction
            "m" if ops.len() >= 2 => {
                if path_op_start.is_none() {
                    path_op_start = Some(idx);
                }
                if let (Some(x), Some(y)) = (as_f32(&ops[0]), as_f32(&ops[1])) {
                    cur_pt = (x, y);
                    cur_path.push(RawPathOp::Move(x, y));
                }
            }
            "l" if ops.len() >= 2 => {
                if let (Some(x), Some(y)) = (as_f32(&ops[0]), as_f32(&ops[1])) {
                    cur_pt = (x, y);
                    cur_path.push(RawPathOp::Line(x, y));
                }
            }
            "c" if ops.len() >= 6 => {
                if let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x3), Some(y3)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                    as_f32(&ops[4]),
                    as_f32(&ops[5]),
                ) {
                    cur_pt = (x3, y3);
                    cur_path.push(RawPathOp::Curve(x1, y1, x2, y2, x3, y3));
                }
            }
            "v" if ops.len() >= 4 => {
                if let (Some(x2), Some(y2), Some(x3), Some(y3)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                ) {
                    cur_path.push(RawPathOp::Curve(cur_pt.0, cur_pt.1, x2, y2, x3, y3));
                    cur_pt = (x3, y3);
                }
            }
            "y" if ops.len() >= 4 => {
                if let (Some(x1), Some(y1), Some(x3), Some(y3)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                ) {
                    cur_path.push(RawPathOp::Curve(x1, y1, x3, y3, x3, y3));
                    cur_pt = (x3, y3);
                }
            }
            "h" => {
                cur_path.push(RawPathOp::Close);
            }
            "re" if ops.len() >= 4 => {
                if path_op_start.is_none() {
                    path_op_start = Some(idx);
                }
                if let (Some(x), Some(y), Some(w), Some(h)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                ) {
                    cur_pt = (x, y);
                    cur_path.push(RawPathOp::Rect(x, y, w, h));
                }
            }
            // Clip operators (set clip_bbox; actual painting resets or follows)
            "W" | "W*" => {
                if !cur_path.is_empty() {
                    let (_, _, cb, _, _) = classify_and_build_svg(&cur_path, gs.ctm, &pt, false);
                    gs.clip_bbox = Some(cb);
                }
            }
            "n" => {
                cur_path.clear();
                path_op_start = None;
            }
            // Path painting operators
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                let start_i = path_op_start.unwrap_or(idx);
                let does_stroke = matches!(name, "S" | "s" | "B" | "B*" | "b" | "b*");
                let does_fill = matches!(name, "f" | "F" | "f*" | "B" | "B*" | "b" | "b*");
                let force_close = matches!(name, "s" | "b" | "b*");

                if !cur_path.is_empty() {
                    let (shape_kind, svg_path, bbox_vp, closed, segments_vp) =
                        classify_and_build_svg(&cur_path, gs.ctm, &pt, force_close);
                    let fill_hex = if does_fill {
                        Some(rgb_to_hex(gs.fill_rgb))
                    } else {
                        None
                    };
                    let stroke_hex = if does_stroke {
                        Some(rgb_to_hex(gs.stroke_rgb))
                    } else {
                        None
                    };

                    let scale_est =
                        ((gs.ctm[0].hypot(gs.ctm[1]) + gs.ctm[2].hypot(gs.ctm[3])) * 0.5).max(0.1);
                    let eff_lw = (gs.line_width * scale_est).max(0.5);

                    let is_bg = shape_kind == "rectangle"
                        && fill_hex.as_deref() == Some("#ffffff")
                        && stroke_hex.is_none()
                        && bbox_vp[2] >= pt.vp_width * 0.92
                        && bbox_vp[3] >= pt.vp_height * 0.92;

                    if bbox_vp[2] >= 0.8 || bbox_vp[3] >= 0.8 {
                        path_objects.push(PathObject {
                            id: format!("shp-{}-{}-{}", page_num, start_i, idx),
                            op_start: start_i,
                            op_end: idx,
                            shape_kind,
                            svg_path,
                            bbox_vp,
                            fill_color: fill_hex,
                            stroke_color: stroke_hex,
                            fill_opacity: gs.fill_alpha,
                            stroke_opacity: gs.stroke_alpha,
                            stroke_width: eff_lw,
                            dash_array: gs.dash_array.clone(),
                            closed,
                            is_page_background: is_bg,
                            segments_vp,
                        });
                    }
                }
                cur_path.clear();
                path_op_start = None;
            }
            // Images
            "Do" if !ops.is_empty() => {
                if let Ok(nm) = ops[0].as_name() {
                    let xname = String::from_utf8_lossy(nm).to_string();
                    if let Some(&(pw, ph)) = xobj_images.get(&xname) {
                        let p0 = tx_pt(gs.ctm, 0.0, 0.0);
                        let p1 = tx_pt(gs.ctm, 1.0, 1.0);
                        let (vx0, vy0) = pt.pdf_to_vp(p0.0, p0.1);
                        let (vx1, vy1) = pt.pdf_to_vp(p1.0, p1.1);
                        let bx = vx0.min(vx1);
                        let by = vy0.min(vy1);
                        let bw = (vx0 - vx1).abs().max(1.0);
                        let bh = (vy0 - vy1).abs().max(1.0);
                        image_objects.push(ImageStreamObject {
                            id: format!("img-{}-{}", page_num, idx),
                            op_index: idx,
                            name: xname,
                            bbox_vp: [bx, by, bw, bh],
                            pixel_width: pw,
                            pixel_height: ph,
                        });
                    }
                }
            }
            // Text object tracking
            "BT" => {
                in_text = true;
                text_op_start = idx;
                gs.tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                gs.tlm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                text_preview.clear();
                text_min_x = f32::INFINITY;
                text_min_y = f32::INFINITY;
                text_max_x = f32::NEG_INFINITY;
                text_max_y = f32::NEG_INFINITY;
            }
            "Tf" if ops.len() >= 2 => {
                if let Ok(f) = ops[0].as_name() {
                    gs.font_res = String::from_utf8_lossy(f).to_string();
                }
                if let Some(sz) = as_f32(&ops[1]) {
                    gs.font_size = sz.abs();
                }
            }
            "TL" if !ops.is_empty() => {
                if let Some(l) = as_f32(&ops[0]) {
                    gs.leading = l;
                }
            }
            "Tm" if ops.len() >= 6 && in_text => {
                if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                    as_f32(&ops[0]),
                    as_f32(&ops[1]),
                    as_f32(&ops[2]),
                    as_f32(&ops[3]),
                    as_f32(&ops[4]),
                    as_f32(&ops[5]),
                ) {
                    gs.tm = [a, b, c, d, e, f];
                    gs.tlm = gs.tm;
                }
            }
            "Td" if ops.len() >= 2 && in_text => {
                if let (Some(tx), Some(ty)) = (as_f32(&ops[0]), as_f32(&ops[1])) {
                    gs.tlm = mul_mat(gs.tlm, [1.0, 0.0, 0.0, 1.0, tx, ty]);
                    gs.tm = gs.tlm;
                }
            }
            "TD" if ops.len() >= 2 && in_text => {
                if let (Some(tx), Some(ty)) = (as_f32(&ops[0]), as_f32(&ops[1])) {
                    gs.leading = -ty;
                    gs.tlm = mul_mat(gs.tlm, [1.0, 0.0, 0.0, 1.0, tx, ty]);
                    gs.tm = gs.tlm;
                }
            }
            "T*" if in_text => {
                gs.tlm = mul_mat(gs.tlm, [1.0, 0.0, 0.0, 1.0, 0.0, -gs.leading]);
                gs.tm = gs.tlm;
            }
            "Tj" | "TJ" | "'" | "\"" if in_text => {
                let txt = extract_text_operand(op);
                if !txt.is_empty() {
                    if !text_preview.is_empty() {
                        text_preview.push(' ');
                    }
                    text_preview.push_str(&txt);
                }
                let eff = mul_mat(gs.ctm, gs.tm);
                let scale_y = eff[2].hypot(eff[3]).max(0.01);
                let eff_fs = (gs.font_size * scale_y).max(6.0);
                let est_w = (txt.chars().count().max(1) as f32) * eff_fs * 0.52;
                let (ux0, uy0) = tx_pt(eff, 0.0, -gs.font_size * 0.2);
                let (ux1, uy1) = tx_pt(eff, est_w / scale_y, gs.font_size * 0.9);
                let (vx0, vy0) = pt.pdf_to_vp(ux0, uy0);
                let (vx1, vy1) = pt.pdf_to_vp(ux1, uy1);
                text_min_x = text_min_x.min(vx0).min(vx1);
                text_min_y = text_min_y.min(vy0).min(vy1);
                text_max_x = text_max_x.max(vx0).max(vx1);
                text_max_y = text_max_y.max(vy0).max(vy1);
            }
            "ET" if in_text => {
                in_text = false;
                let bbox = if text_min_x.is_finite() {
                    [
                        text_min_x,
                        text_min_y,
                        (text_max_x - text_min_x).max(2.0),
                        (text_max_y - text_min_y).max(2.0),
                    ]
                } else {
                    [0.0, 0.0, 0.0, 0.0]
                };
                text_objects.push(TextStreamObject {
                    id: format!("txt-{}-{}-{}", page_num, text_op_start, idx),
                    op_start: text_op_start,
                    op_end: idx,
                    font_res: gs.font_res.clone(),
                    font_size: gs.font_size,
                    color: rgb_to_hex(gs.fill_rgb),
                    text_preview: text_preview.trim().to_string(),
                    bbox_vp: bbox,
                });
            }
            _ => {}
        }
    }

    Ok(PageObjectsResult {
        page: page_num as usize,
        page_transform: pt,
        text_objects,
        path_objects,
        image_objects,
    })
}

fn extract_text_operand(op: &Operation) -> String {
    let mut s = String::new();
    for operand in &op.operands {
        match operand {
            Object::String(bytes, _) => {
                s.push_str(&String::from_utf8_lossy(bytes));
            }
            Object::Array(arr) => {
                for item in arr {
                    if let Object::String(bytes, _) = item {
                        s.push_str(&String::from_utf8_lossy(bytes));
                    }
                }
            }
            _ => {}
        }
    }
    s
}

fn ensure_alpha_gstate(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    fill_alpha: f32,
    stroke_alpha: f32,
) -> Result<Option<String>, String> {
    if fill_alpha >= 0.99 && stroke_alpha >= 0.99 {
        return Ok(None);
    }
    let name = format!(
        "PFGS_{}_{}",
        (fill_alpha.clamp(0.0, 1.0) * 100.0).round() as i32,
        (stroke_alpha.clamp(0.0, 1.0) * 100.0).round() as i32
    );
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"ExtGState".to_vec()));
    dict.set("ca", Object::Real(fill_alpha.clamp(0.0, 1.0)));
    dict.set("CA", Object::Real(stroke_alpha.clamp(0.0, 1.0)));
    let id = doc.add_object(Object::Dictionary(dict));
    attach_resource_entry(doc, page_id, b"ExtGState", &name, Object::Reference(id))?;
    Ok(Some(name))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTextCommitRequest {
    pub page: usize,
    pub orig_bbox_vp: [f32; 4],
    pub new_bbox_vp: [f32; 4],
    pub text: String,
    pub original_font: String,
    pub font_family: String,
    pub font_size: f32,
    pub color: String,
    pub bold: bool,
    pub italic: bool,
    pub align: String,
    pub line_spacing: f32,
    pub bullets: bool,
    pub bg_color: Option<String>,
    pub delete_block: Option<bool>,
}

/// Mutates ONLY the affected text block in the page content stream (`lopdf`),
/// preserving unrelated text, paths, images, and metadata.
pub fn commit_edit_text_block(
    doc: &mut lopdf::Document,
    req: &EditTextCommitRequest,
) -> Result<Option<String>, String> {
    let page_num = req.page.max(1) as u32;
    let pages = doc.get_pages();
    let page_id = *pages
        .get(&page_num)
        .ok_or_else(|| format!("Page {} does not exist", page_num))?;

    let parsed = parse_page_objects(doc, req.page - 1)?;
    let pt = parsed.page_transform;
    let [ox, oy, ow, oh] = req.orig_bbox_vp;

    // Identify BT..ET objects whose bbox overlaps original block bbox cleanly
    let mut matched_ranges: Vec<(usize, usize)> = parsed
        .text_objects
        .iter()
        .filter(|t| {
            let [tx, ty, tw, th] = t.bbox_vp;
            let ix0 = ox.max(tx);
            let iy0 = oy.max(ty);
            let ix1 = (ox + ow).min(tx + tw);
            let iy1 = (oy + oh).min(ty + th);
            let iw = (ix1 - ix0).max(0.0);
            let ih = (iy1 - iy0).max(0.0);
            let area = iw * ih;
            let obj_area = (tw * th).max(1.0);
            area > 0.0 && (area / obj_area > 0.35 || (tx >= ox - 4.0 && tx + tw <= ox + ow + 4.0 && ty >= oy - 4.0 && ty + th <= oy + oh + 4.0))
        })
        .map(|t| (t.op_start, t.op_end))
        .collect();

    matched_ranges.sort_by_key(|r| r.0);

    let raw = doc.get_page_content(page_id);
    let mut content = Content::decode(&raw)
        .map_err(|e| format!("Failed decoding content stream: {}", e))?;

    let mut new_ops: Vec<Operation> = Vec::with_capacity(content.operations.len() + 32);
    let mut skip_until: Option<usize> = None;

    for (idx, op) in content.operations.drain(..).enumerate() {
        if let Some(end_idx) = skip_until {
            if idx <= end_idx {
                if idx == end_idx {
                    skip_until = None;
                }
                continue;
            }
            skip_until = None;
        }
        if let Some(&(_, end_idx)) = matched_ranges.iter().find(|&&(s, _)| s == idx) {
            skip_until = Some(end_idx);
            continue;
        }
        new_ops.push(op);
    }

    // Also add clean background cover rect only if no whole BT..ET isolated cleanly
    let mut reflow_notice: Option<String> = None;
    if matched_ranges.is_empty() {
        let bg_rgb = hex_to_rgb(req.bg_color.as_deref().unwrap_or("#ffffff"));
        let (p0x, p0y) = pt.vp_to_pdf(ox - 1.0, oy - 1.0);
        let (p1x, p1y) = pt.vp_to_pdf(ox + ow + 2.0, oy + oh + 2.0);
        let rx = p0x.min(p1x);
        let ry = p0y.min(p1y);
        let rw = (p1x - p0x).abs();
        let rh = (p1y - p0y).abs();
        new_ops.push(Operation::new("q", vec![]));
        new_ops.push(Operation::new(
            "rg",
            vec![
                Object::Real(bg_rgb[0]),
                Object::Real(bg_rgb[1]),
                Object::Real(bg_rgb[2]),
            ],
        ));
        new_ops.push(Operation::new(
            "re",
            vec![
                Object::Real(rx),
                Object::Real(ry),
                Object::Real(rw),
                Object::Real(rh),
            ],
        ));
        new_ops.push(Operation::new("f", vec![]));
        new_ops.push(Operation::new("Q", vec![]));
    }

    if !req.delete_block.unwrap_or(false) && !req.text.is_empty() {
        let reflow = shape_and_reflow_block(
            Some(doc),
            Some(page_id),
            &req.text,
            &req.original_font,
            &req.font_family,
            req.font_size,
            req.new_bbox_vp[2].max(24.0),
            req.bold,
            req.italic,
            &req.align,
            req.line_spacing,
            req.bullets,
        );
        reflow_notice = reflow.notice.clone();

        let rendered_text = reflow
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let font_mode = ensure_page_font_resource(
            doc,
            page_id,
            &req.font_family,
            req.bold,
            req.italic,
            &rendered_text,
        )?;
        let rgb = hex_to_rgb(&req.color);

        new_ops.push(Operation::new("q", vec![]));
        new_ops.push(Operation::new("BT", vec![]));
        new_ops.push(Operation::new(
            "rg",
            vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
        ));

        let res_name = match &font_mode {
            PdfFontWriteMode::StandardWinAnsi { resource_name } => resource_name,
            PdfFontWriteMode::EmbeddedCidTrueType { resource_name, .. } => resource_name,
        };
        new_ops.push(Operation::new(
            "Tf",
            vec![
                Object::Name(res_name.as_bytes().to_vec()),
                Object::Real(req.font_size.max(4.0)),
            ],
        ));

        for line in &reflow.lines {
            if line.text.is_empty() {
                continue;
            }
            let vx = req.new_bbox_vp[0] + line.x_offset;
            // Upright visual baseline is top + 0.84 * font_size + y_offset
            let vy = req.new_bbox_vp[1] + req.font_size * 0.84 + line.y_offset;
            let (px, py) = pt.vp_to_pdf(vx, vy);

            // Keep text upright relative to page /Rotate
            let tm_ops = match pt.rotation {
                90 => vec![
                    Object::Real(0.0),
                    Object::Real(-1.0),
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(px),
                    Object::Real(py),
                ],
                180 => vec![
                    Object::Real(-1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(-1.0),
                    Object::Real(px),
                    Object::Real(py),
                ],
                270 => vec![
                    Object::Real(0.0),
                    Object::Real(1.0),
                    Object::Real(-1.0),
                    Object::Real(0.0),
                    Object::Real(px),
                    Object::Real(py),
                ],
                _ => vec![
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(1.0),
                    Object::Real(px),
                    Object::Real(py),
                ],
            };
            new_ops.push(Operation::new("Tm", tm_ops));
            if line.word_spacing > 0.01 {
                new_ops.push(Operation::new("Tw", vec![Object::Real(line.word_spacing)]));
            } else {
                new_ops.push(Operation::new("Tw", vec![Object::Real(0.0)]));
            }

            let str_bytes = match &font_mode {
                PdfFontWriteMode::StandardWinAnsi { .. } => encode_winansi_bytes(&line.text),
                PdfFontWriteMode::EmbeddedCidTrueType { char_to_gid, .. } => {
                    encode_cid_glyph_bytes(&line.text, char_to_gid)
                }
            };
            new_ops.push(Operation::new(
                "Tj",
                vec![Object::String(str_bytes, lopdf::StringFormat::Hexadecimal)],
            ));
        }

        new_ops.push(Operation::new("ET", vec![]));
        new_ops.push(Operation::new("Q", vec![]));
    }

    let encoded = Content { operations: new_ops }
        .encode()
        .map_err(|e| format!("Failed encoding updated content stream: {}", e))?;
    doc.change_page_content(page_id, encoded)
        .map_err(|e| format!("Failed writing updated page content: {}", e))?;

    Ok(reflow_notice)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapeCommitRequest {
    pub page: usize,
    pub action: String, // "update" | "delete" | "add"
    pub shape_kind: String, // "rectangle" | "ellipse" | "line" | "arrow" | "polygon" | "bezier" | "freehand"
    pub op_start: Option<usize>,
    pub op_end: Option<usize>,
    pub bbox_vp: [f32; 4],
    pub orig_bbox_vp: Option<[f32; 4]>,
    pub orig_segments_vp: Option<Vec<PathSeg>>,
    pub points_vp: Option<Vec<[f32; 2]>>,
    pub rotation_deg: Option<f32>,
    pub fill_color: Option<String>,
    pub stroke_color: Option<String>,
    pub fill_opacity: Option<f32>,
    pub stroke_opacity: Option<f32>,
    pub stroke_width: Option<f32>,
}

fn emit_pdf_shape_ops(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    pt: &PageTransform,
    req: &ShapeCommitRequest,
) -> Result<Vec<Operation>, String> {
    let mut ops = Vec::new();
    let [bx, by, bw, bh] = req.bbox_vp;
    let fill_a = req.fill_opacity.unwrap_or(1.0);
    let stroke_a = req.stroke_opacity.unwrap_or(1.0);
    let lw = req.stroke_width.unwrap_or(2.0).max(0.25);

    ops.push(Operation::new("q", vec![]));

    if let Some(gs_name) = ensure_alpha_gstate(doc, page_id, fill_a, stroke_a)? {
        ops.push(Operation::new("gs", vec![Object::Name(gs_name.into_bytes())]));
    }
    if let Some(fc) = &req.fill_color {
        let rgb = hex_to_rgb(fc);
        ops.push(Operation::new(
            "rg",
            vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
        ));
    }
    if let Some(sc) = &req.stroke_color {
        let rgb = hex_to_rgb(sc);
        ops.push(Operation::new(
            "RG",
            vec![Object::Real(rgb[0]), Object::Real(rgb[1]), Object::Real(rgb[2])],
        ));
    }
    ops.push(Operation::new("w", vec![Object::Real(lw)]));

    let rot = req.rotation_deg.unwrap_or(0.0);
    let cx = bx + bw * 0.5;
    let cy = by + bh * 0.5;
    let xform_vp = |vx: f32, vy: f32| -> (f32, f32) {
        if rot.abs() < 0.01 {
            pt.vp_to_pdf(vx, vy)
        } else {
            let rad = rot.to_radians();
            let (cos_r, sin_r) = (rad.cos(), rad.sin());
            let dx = vx - cx;
            let dy = vy - cy;
            pt.vp_to_pdf(cx + dx * cos_r - dy * sin_r, cy + dx * sin_r + dy * cos_r)
        }
    };

    match req.shape_kind.as_str() {
        "rectangle" => {
            let p0 = xform_vp(bx, by);
            let p1 = xform_vp(bx + bw, by);
            let p2 = xform_vp(bx + bw, by + bh);
            let p3 = xform_vp(bx, by + bh);
            ops.push(Operation::new("m", vec![Object::Real(p0.0), Object::Real(p0.1)]));
            ops.push(Operation::new("l", vec![Object::Real(p1.0), Object::Real(p1.1)]));
            ops.push(Operation::new("l", vec![Object::Real(p2.0), Object::Real(p2.1)]));
            ops.push(Operation::new("l", vec![Object::Real(p3.0), Object::Real(p3.1)]));
            ops.push(Operation::new("h", vec![]));
        }
        "ellipse" => {
            let k = 0.55228475_f32;
            let rx = bw * 0.5;
            let ry = bh * 0.5;
            let ox = rx * k;
            let oy = ry * k;
            let pt_s = xform_vp(cx + rx, cy);
            let c1a = xform_vp(cx + rx, cy + oy);
            let c1b = xform_vp(cx + ox, cy + ry);
            let c1c = xform_vp(cx, cy + ry);
            let c2a = xform_vp(cx - ox, cy + ry);
            let c2b = xform_vp(cx - rx, cy + oy);
            let c2c = xform_vp(cx - rx, cy);
            let c3a = xform_vp(cx - rx, cy - oy);
            let c3b = xform_vp(cx - ox, cy - ry);
            let c3c = xform_vp(cx, cy - ry);
            let c4a = xform_vp(cx + ox, cy - ry);
            let c4b = xform_vp(cx + rx, cy - oy);
            let c4c = xform_vp(cx + rx, cy);

            ops.push(Operation::new("m", vec![Object::Real(pt_s.0), Object::Real(pt_s.1)]));
            for (a, b, c) in [
                (c1a, c1b, c1c),
                (c2a, c2b, c2c),
                (c3a, c3b, c3c),
                (c4a, c4b, c4c),
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
        }
        "line" | "arrow" => {
            let pts = req.points_vp.as_deref().unwrap_or(&[]);
            let (sx, sy, ex, ey) = if pts.len() >= 2 {
                (pts[0][0], pts[0][1], pts[1][0], pts[1][1])
            } else {
                (bx, by, bx + bw, by + bh)
            };
            let p0 = xform_vp(sx, sy);
            let p1 = xform_vp(ex, ey);
            ops.push(Operation::new("m", vec![Object::Real(p0.0), Object::Real(p0.1)]));
            ops.push(Operation::new("l", vec![Object::Real(p1.0), Object::Real(p1.1)]));
            if req.shape_kind == "arrow" {
                let ang = (ey - sy).atan2(ex - sx);
                let head = (lw * 4.2).clamp(8.0, 22.0);
                let a1 = ang - 0.45;
                let a2 = ang + 0.45;
                let h1 = xform_vp(ex - head * a1.cos(), ey - head * a1.sin());
                let h2 = xform_vp(ex - head * a2.cos(), ey - head * a2.sin());
                ops.push(Operation::new("m", vec![Object::Real(p1.0), Object::Real(p1.1)]));
                ops.push(Operation::new("l", vec![Object::Real(h1.0), Object::Real(h1.1)]));
                ops.push(Operation::new("m", vec![Object::Real(p1.0), Object::Real(p1.1)]));
                ops.push(Operation::new("l", vec![Object::Real(h2.0), Object::Real(h2.1)]));
            }
        }
        "freehand" => {
            let pts = req.points_vp.as_deref().unwrap_or(&[]);
            if let Some(first) = pts.first() {
                let p0 = xform_vp(first[0], first[1]);
                ops.push(Operation::new("m", vec![Object::Real(p0.0), Object::Real(p0.1)]));
                for pt_xy in &pts[1..] {
                    let pn = xform_vp(pt_xy[0], pt_xy[1]);
                    ops.push(Operation::new("l", vec![Object::Real(pn.0), Object::Real(pn.1)]));
                }
            }
        }
        _ => {
            // General polygon / bezier mapped cleanly from orig_bbox_vp -> new bbox_vp
            let ob = req.orig_bbox_vp.unwrap_or(req.bbox_vp);
            let ow = ob[2].max(1.0);
            let oh = ob[3].max(1.0);
            let map_xy = |vx: f32, vy: f32| -> (f32, f32) {
                let nx = bx + ((vx - ob[0]) / ow) * bw;
                let ny = by + ((vy - ob[1]) / oh) * bh;
                xform_vp(nx, ny)
            };
            if let Some(segs) = &req.orig_segments_vp {
                for seg in segs {
                    match seg.cmd.as_str() {
                        "M" if seg.pts.len() >= 2 => {
                            let p = map_xy(seg.pts[0], seg.pts[1]);
                            ops.push(Operation::new("m", vec![Object::Real(p.0), Object::Real(p.1)]));
                        }
                        "L" if seg.pts.len() >= 2 => {
                            let p = map_xy(seg.pts[0], seg.pts[1]);
                            ops.push(Operation::new("l", vec![Object::Real(p.0), Object::Real(p.1)]));
                        }
                        "C" if seg.pts.len() >= 6 => {
                            let p1 = map_xy(seg.pts[0], seg.pts[1]);
                            let p2 = map_xy(seg.pts[2], seg.pts[3]);
                            let p3 = map_xy(seg.pts[4], seg.pts[5]);
                            ops.push(Operation::new(
                                "c",
                                vec![
                                    Object::Real(p1.0),
                                    Object::Real(p1.1),
                                    Object::Real(p2.0),
                                    Object::Real(p2.1),
                                    Object::Real(p3.0),
                                    Object::Real(p3.1),
                                ],
                            ));
                        }
                        "Z" => ops.push(Operation::new("h", vec![])),
                        _ => {}
                    }
                }
            }
        }
    }

    let paint_op = match (req.fill_color.is_some(), req.stroke_color.is_some()) {
        (true, true) => "B",
        (true, false) => "f",
        (false, true) => "S",
        (false, false) => "S",
    };
    ops.push(Operation::new(paint_op, vec![]));
    ops.push(Operation::new("Q", vec![]));
    Ok(ops)
}

pub fn commit_shape_operation(
    doc: &mut lopdf::Document,
    req: &ShapeCommitRequest,
) -> Result<(), String> {
    let page_num = req.page.max(1) as u32;
    let pages = doc.get_pages();
    let page_id = *pages
        .get(&page_num)
        .ok_or_else(|| format!("Page {} does not exist", page_num))?;

    let pt = get_page_transform(doc, page_id);
    let raw = doc.get_page_content(page_id);
    let mut content = Content::decode(&raw)
        .map_err(|e| format!("Failed decoding page {} stream: {}", page_num, e))?;

    match req.action.as_str() {
        "delete" => {
            let (Some(os), Some(oe)) = (req.op_start, req.op_end) else {
                return Err("Missing op_start/op_end for shape delete".into());
            };
            let mut filtered = Vec::with_capacity(content.operations.len());
            for (idx, op) in content.operations.drain(..).enumerate() {
                if idx >= os && idx <= oe {
                    continue;
                }
                filtered.push(op);
            }
            content.operations = filtered;
        }
        "update" => {
            let (Some(os), Some(oe)) = (req.op_start, req.op_end) else {
                return Err("Missing op_start/op_end for shape update".into());
            };
            let replacement = emit_pdf_shape_ops(doc, page_id, &pt, req)?;
            let mut next_ops = Vec::with_capacity(content.operations.len() + replacement.len());
            let mut inserted = false;
            for (idx, op) in content.operations.drain(..).enumerate() {
                if idx >= os && idx <= oe {
                    if !inserted {
                        next_ops.extend(replacement.clone());
                        inserted = true;
                    }
                    continue;
                }
                next_ops.push(op);
            }
            if !inserted {
                next_ops.extend(replacement);
            }
            content.operations = next_ops;
        }
        "add" => {
            let addition = emit_pdf_shape_ops(doc, page_id, &pt, req)?;
            content.operations.extend(addition);
        }
        other => return Err(format!("Unsupported shape action '{}'", other)),
    }

    let encoded = content
        .encode()
        .map_err(|e| format!("Failed encoding updated shape stream: {}", e))?;
    doc.change_page_content(page_id, encoded)
        .map_err(|e| format!("Failed saving updated shape stream: {}", e))?;
    Ok(())
}
