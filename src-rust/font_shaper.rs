use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use ttf_parser::{Face as TtfFace, GlyphId};

static SYSTEM_FONT_DB: OnceLock<Arc<Database>> = OnceLock::new();

pub fn get_font_db() -> Arc<Database> {
    SYSTEM_FONT_DB
        .get_or_init(|| {
            let mut db = Database::new();
            // Load bundled offline TTF/OTF fonts from embedded assets first
            for name in crate::Assets::iter() {
                let s = name.as_ref();
                if (s.starts_with("fonts/") || s.starts_with("standard_fonts/"))
                    && (s.ends_with(".ttf") || s.ends_with(".otf") || s.ends_with(".ttc"))
                {
                    if let Some(asset) = crate::Assets::get(s) {
                        db.load_font_data(asset.data.to_vec());
                    }
                }
            }
            // Also load all offline Windows/OS system fonts
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontCatalogItem {
    pub name: String,
    pub css_family: String,
    pub category: String,
    pub is_bundled: bool,
}

pub fn list_font_catalog() -> Vec<FontCatalogItem> {
    let bundled = [
        ("Noto Sans", "'Noto Sans', 'Liberation Sans', sans-serif", "Sans-Serif"),
        ("Noto Serif", "'Noto Serif', 'Times New Roman', serif", "Serif"),
        ("Noto Sans Bengali", "'Noto Sans Bengali', 'Nirmala UI', 'Vrinda', sans-serif", "Bengali / Unicode"),
        ("Noto Sans Mono", "'Noto Sans Mono', 'Courier New', monospace", "Monospace"),
        ("Roboto", "'Roboto', 'Liberation Sans', sans-serif", "Sans-Serif"),
        ("Open Sans", "'Open Sans', 'Liberation Sans', sans-serif", "Sans-Serif"),
        ("Lato", "'Lato', 'Liberation Sans', sans-serif", "Sans-Serif"),
        ("Source Sans 3", "'Source Sans 3', 'Liberation Sans', sans-serif", "Sans-Serif"),
        ("Liberation Sans", "'Liberation Sans', Arial, sans-serif", "Sans-Serif"),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<FontCatalogItem> = Vec::new();
    for (name, css, cat) in bundled {
        seen.insert(name.to_lowercase());
        out.push(FontCatalogItem {
            name: name.to_string(),
            css_family: css.to_string(),
            category: cat.to_string(),
            is_bundled: true,
        });
    }
    let db = get_font_db();
    let mut sys_names = std::collections::BTreeSet::new();
    for face in db.faces() {
        if let Some((fam, _)) = face.families.first() {
            let trimmed = fam.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('@') && !seen.contains(&trimmed.to_lowercase()) {
                sys_names.insert(trimmed.to_string());
            }
        }
    }
    for fam in sys_names {
        let low = fam.to_lowercase();
        let cat = if low.contains("mono") || low.contains("courier") || low.contains("consolas") {
            "Monospace"
        } else if (low.contains("serif") && !low.contains("sans")) || low.contains("times") || low.contains("georgia") || low.contains("garamond") || low.contains("cambria") {
            "Serif"
        } else if low.contains("bengali") || low.contains("vrinda") || low.contains("nirmala") || low.contains("shonar") {
            "Bengali / Unicode"
        } else {
            "System Font"
        };
        out.push(FontCatalogItem {
            css_family: format!("'{}', sans-serif", fam.replace('\'', "")),
            name: fam,
            category: cat.to_string(),
            is_bundled: false,
        });
    }
    out
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapedGlyphRun {
    pub text: String,
    pub width: f32,
    /// Glyph ID -> Unicode scalar string (used for CIDFont Identity-H + ToUnicode CMap)
    pub glyph_ids: Vec<u16>,
    pub glyph_advances_1000: Vec<i32>,
    pub glyph_unicodes: Vec<(u16, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapedLine {
    pub text: String,
    pub x_offset: f32,
    pub y_offset: f32,
    pub width: f32,
    pub word_spacing: f32,
    pub is_bullet: bool,
    pub is_rtl: bool,
    pub glyph_ids: Vec<u16>,
    pub glyph_unicodes: Vec<(u16, String)>,
    pub glyph_widths_1000: Vec<(u16, i32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflowBlockResult {
    pub lines: Vec<ShapedLine>,
    pub line_height: f32,
    pub total_height: f32,
    pub original_font: String,
    pub resolved_font: String,
    pub is_subset: bool,
    pub substituted: bool,
    pub missing_chars: Vec<String>,
    pub notice: Option<String>,
    pub requires_cid_unicode: bool,
}

#[derive(Debug, Clone)]
pub enum PdfFontWriteMode {
    StandardWinAnsi {
        resource_name: String,
    },
    EmbeddedCidTrueType {
        resource_name: String,
        char_to_gid: HashMap<char, u16>,
    },
}

pub fn strip_subset_prefix(name: &str) -> String {
    let trimmed = name.trim().trim_start_matches('/');
    if trimmed.len() > 7 {
        let bytes = trimmed.as_bytes();
        if bytes[6] == b'+' && bytes[..6].iter().all(|b| b.is_ascii_uppercase()) {
            return trimmed[7..].to_string();
        }
    }
    trimmed.to_string()
}

pub fn looks_like_subset_font(name: &str) -> bool {
    let trimmed = name.trim().trim_start_matches('/');
    if trimmed.len() > 7 {
        let bytes = trimmed.as_bytes();
        return bytes[6] == b'+' && bytes[..6].iter().all(|b| b.is_ascii_uppercase());
    }
    false
}

fn contains_non_winansi(text: &str) -> bool {
    text.chars().any(|c| {
        if c == '\n' || c == '\r' || c == '\t' {
            return false;
        }
        let cp = c as u32;
        !(0x20..=0x7E).contains(&cp)
            && !matches!(
                cp,
                0xA0..=0xFF | 0x2022 | 0x2013 | 0x2014 | 0x2018 | 0x2019 | 0x201C | 0x201D | 0x20AC
            )
    })
}

fn text_has_bengali(text: &str) -> bool {
    text.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c))
}

fn text_has_arabic(text: &str) -> bool {
    text.chars()
        .any(|c| ('\u{0600}'..='\u{06FF}').contains(&c) || ('\u{0750}'..='\u{077F}').contains(&c))
}

fn text_has_rtl(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{0590}'..='\u{05FF}').contains(&c)
            || ('\u{0600}'..='\u{06FF}').contains(&c)
            || ('\u{0750}'..='\u{077F}').contains(&c)
            || ('\u{08A0}'..='\u{08FF}').contains(&c)
            || ('\u{FB50}'..='\u{FDFF}').contains(&c)
            || ('\u{FE70}'..='\u{FEFF}').contains(&c)
    })
}

/// Canonicalize a font name / UI key into candidate system family names.
fn candidate_families_for(font_hint: &str, text: &str) -> Vec<String> {
    let base = strip_subset_prefix(font_hint);
    let lower = base.to_lowercase();
    let mut out = Vec::new();

    if text_has_bengali(text) {
        out.extend(
            [
                "Noto Sans Bengali",
                "Nirmala UI",
                "Vrinda",
                "Shonar Bangla",
                "Kalpurush",
            ]
            .into_iter()
            .map(str::to_string),
        );
    }
    if text_has_arabic(text) {
        out.extend(
            ["Segoe UI", "Tahoma", "Arial", "Simplified Arabic", "Traditional Arabic"]
                .into_iter()
                .map(str::to_string),
        );
    }

    if lower.contains("helvetica") || lower == "inter" || lower.contains("arial") {
        out.extend(["Arial", "Liberation Sans", "Noto Sans", "Roboto", "Open Sans", "Segoe UI"].into_iter().map(str::to_string));
    } else if lower.contains("calibri") || lower.contains("carlito") {
        out.extend(["Calibri", "Open Sans", "Source Sans 3", "Lato", "Noto Sans", "Liberation Sans"].into_iter().map(str::to_string));
    } else if lower.contains("times") || (lower.contains("serif") && !lower.contains("sans")) {
        out.extend(["Times New Roman", "Noto Serif", "Georgia"].into_iter().map(str::to_string));
    } else if lower.contains("courier") || lower.contains("consolas") || lower.contains("mono") {
        out.extend(["Courier New", "Consolas", "Noto Sans Mono"].into_iter().map(str::to_string));
    } else if lower == "segoe" || lower.contains("segoe") {
        out.extend(["Segoe UI", "Open Sans", "Noto Sans"].into_iter().map(str::to_string));
    } else if lower == "trebuchet" {
        out.push("Trebuchet MS".to_string());
    } else if !base.is_empty() {
        let cleaned = base
            .split(['-', ','])
            .next()
            .unwrap_or(&base)
            .trim()
            .to_string();
        if !cleaned.is_empty() {
            out.push(cleaned);
        }
        out.push(base.clone());
    }

    out.extend(
        [
            "Noto Sans",
            "Liberation Sans",
            "Roboto",
            "Open Sans",
            "Lato",
            "Noto Serif",
            "Noto Sans Mono",
            "Nirmala UI",
            "Segoe UI",
            "Arial",
            "Calibri",
            "Times New Roman",
            "Courier New",
        ]
        .into_iter()
        .map(str::to_string),
    );
    out
}

/// Find the closest offline system TTF/OTF font bytes + resolved display name using `fontdb`.
/// Verifies missing glyphs with `ttf_parser` so script characters (e.g. Bangla, Arabic) never
/// fall back to an incomplete font when a complete system font exists.
pub fn resolve_system_font(
    font_hint: &str,
    bold: bool,
    italic: bool,
    text: &str,
) -> Option<(Vec<u8>, u32, String)> {
    let db = get_font_db();
    let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    let style = if italic { Style::Italic } else { Style::Normal };
    let candidates = candidate_families_for(font_hint, text);
    let needed_chars: Vec<char> = text
        .chars()
        .filter(|c| !c.is_control() && *c != ' ')
        .collect();

    let mut first_valid: Option<(Vec<u8>, u32, String)> = None;

    for fam in &candidates {
        let families = [Family::Name(fam.as_str())];
        let query = Query {
            families: &families,
            weight,
            stretch: Stretch::Normal,
            style,
        };
        if let Some(id) = db.query(&query) {
            if let Some((bytes, index, family_name)) = db.with_face_data(id, |data, face_idx| {
                let info = db.face(id);
                let name = info
                    .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
                    .unwrap_or_else(|| fam.clone());
                (data.to_vec(), face_idx, name)
            }) {
                if let Ok(face) = TtfFace::parse(&bytes, index) {
                    let covers_all = needed_chars
                        .iter()
                        .all(|&c| face.glyph_index(c).is_some());
                    if covers_all {
                        return Some((bytes, index, family_name));
                    }
                    if first_valid.is_none() {
                        first_valid = Some((bytes, index, family_name));
                    }
                }
            }
        }
    }

    // Broad scan across all loaded system faces if script glyphs were not in named hints
    if !needed_chars.is_empty() {
        for face_info in db.faces() {
            if let Some(res) = db.with_face_data(face_info.id, |data, face_idx| {
                if let Ok(face) = TtfFace::parse(data, face_idx) {
                    if needed_chars.iter().all(|&c| face.glyph_index(c).is_some()) {
                        let name = face_info
                            .families
                            .first()
                            .map(|(n, _)| n.clone())
                            .unwrap_or_else(|| "System Fallback".to_string());
                        return Some((data.to_vec(), face_idx, name));
                    }
                }
                None
            }) {
                if res.is_some() {
                    return res;
                }
            }
        }
    }

    first_valid
}

/// Extract an embedded TTF/OTF font stream (`FontFile2` / `FontFile3`) matching `font_hint`
/// from a `lopdf::Document` page's `/Resources -> /Font` dictionary.
pub fn extract_embedded_pdf_font(
    doc: &lopdf::Document,
    page_id: ObjectId,
    font_hint: &str,
) -> Option<(Vec<u8>, String, bool)> {
    let target_clean = strip_subset_prefix(font_hint).to_lowercase();
    let resources_dict = get_page_resources_dict(doc, page_id)?;
    let font_map_obj = resources_dict.get(b"Font").ok()?;
    let font_map = resolve_dict(doc, font_map_obj)?;

    for (_res_key, font_obj) in font_map.iter() {
        let Some(font_dict) = resolve_dict(doc, font_obj) else {
            continue;
        };
        let base_font = font_dict
            .get(b"BaseFont")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .unwrap_or_default();

        let base_clean = strip_subset_prefix(&base_font).to_lowercase();
        let is_match = target_clean.is_empty()
            || base_font.eq_ignore_ascii_case(font_hint.trim())
            || base_clean == target_clean
            || base_clean.contains(&target_clean)
            || target_clean.contains(&base_clean);
        if !is_match {
            continue;
        }

        let is_subset = looks_like_subset_font(&base_font);

        // Try direct FontDescriptor
        if let Some(bytes) = extract_fontfile_from_font_dict(doc, font_dict) {
            return Some((bytes, base_font, is_subset));
        }
        // Try DescendantFonts (Type0 CIDFont)
        if let Ok(desc_obj) = font_dict.get(b"DescendantFonts") {
            let resolved = resolve_object(doc, desc_obj);
            if let Some(Object::Array(arr)) = resolved {
                for item in arr {
                    if let Some(desc_dict) = resolve_dict(doc, item) {
                        if let Some(bytes) = extract_fontfile_from_font_dict(doc, desc_dict) {
                            return Some((bytes, base_font, is_subset));
                        }
                    }
                }
            }
        }
    }
    None
}

fn extract_fontfile_from_font_dict(
    doc: &lopdf::Document,
    font_dict: &Dictionary,
) -> Option<Vec<u8>> {
    let fd_obj = font_dict.get(b"FontDescriptor").ok()?;
    let fd = resolve_dict(doc, fd_obj)?;
    for key in [b"FontFile2".as_slice(), b"FontFile3".as_slice(), b"FontFile".as_slice()] {
        if let Ok(ff_obj) = fd.get(key) {
            if let Some(Object::Stream(stream)) = resolve_object(doc, ff_obj) {
                let mut cloned = stream.clone();
                if cloned.is_compressed() {
                    if cloned.decompress().is_ok() {
                        return Some(cloned.content);
                    }
                }
                return Some(cloned.content);
            }
        }
    }
    None
}

pub fn resolve_object<'a>(doc: &'a lopdf::Document, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

pub fn resolve_dict<'a>(doc: &'a lopdf::Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match resolve_object(doc, obj)? {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

pub fn get_page_resources_dict<'a>(
    doc: &'a lopdf::Document,
    page_id: ObjectId,
) -> Option<&'a Dictionary> {
    let page_dict = doc.get_dictionary(page_id).ok()?;
    if let Ok(res_obj) = page_dict.get(b"Resources") {
        if let Some(d) = resolve_dict(doc, res_obj) {
            return Some(d);
        }
    }
    // Check inherited /Parent /Resources
    if let Ok(parent_obj) = page_dict.get(b"Parent") {
        let mut curr = resolve_dict(doc, parent_obj);
        while let Some(pd) = curr {
            if let Ok(res_obj) = pd.get(b"Resources") {
                if let Some(d) = resolve_dict(doc, res_obj) {
                    return Some(d);
                }
            }
            curr = pd
                .get(b"Parent")
                .ok()
                .and_then(|po| resolve_dict(doc, po));
        }
    }
    None
}

fn measure_run_width_with_rustybuzz(
    face: &rustybuzz::Face<'_>,
    ttf: &TtfFace<'_>,
    segment: &str,
    font_size: f32,
) -> (f32, Vec<u16>, Vec<(u16, String)>, Vec<(u16, i32)>) {
    let upem = ttf.units_per_em().max(1) as f32;
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(segment);
    let glyph_buf = rustybuzz::shape(face, &[], buffer);
    let infos = glyph_buf.glyph_infos();
    let positions = glyph_buf.glyph_positions();

    let mut total_advance_units: i32 = 0;
    let mut gids = Vec::with_capacity(infos.len());
    let mut gid_unicodes = Vec::with_capacity(infos.len());
    let mut gid_widths = Vec::with_capacity(infos.len());

    let chars_vec: Vec<char> = segment.chars().collect();
    for (idx, (info, pos)) in infos.iter().zip(positions.iter()).enumerate() {
        total_advance_units += pos.x_advance;
        let gid = info.glyph_id as u16;
        gids.push(gid);
        let w1000 = ((pos.x_advance as f32 / upem) * 1000.0).round() as i32;
        gid_widths.push((gid, w1000));
        // Best-effort cluster character attribution for ToUnicode CMap
        let byte_start = info.cluster as usize;
        let next_cluster = infos
            .get(idx + 1)
            .map(|n| n.cluster as usize)
            .unwrap_or(segment.len());
        let slice = if byte_start < segment.len() {
            let end = next_cluster.clamp(byte_start, segment.len());
            segment.get(byte_start..end).unwrap_or("")
        } else {
            ""
        };
        let mapped = if !slice.is_empty() {
            slice.to_string()
        } else {
            chars_vec.get(idx).map(|c| c.to_string()).unwrap_or_default()
        };
        if !mapped.is_empty() {
            gid_unicodes.push((gid, mapped));
        }
    }

    let width_pts = (total_advance_units as f32 / upem) * font_size;
    (width_pts, gids, gid_unicodes, gid_widths)
}

/// Check embedded font coverage & shape reflowed text block via `rustybuzz` + `ttf_parser` + `fontdb`.
pub fn shape_and_reflow_block(
    doc: Option<&lopdf::Document>,
    page_id: Option<ObjectId>,
    text: &str,
    original_font: &str,
    requested_font: &str,
    font_size: f32,
    max_width: f32,
    bold: bool,
    italic: bool,
    align: &str,
    line_spacing: f32,
    bullets: bool,
) -> ReflowBlockResult {
    let font_size = font_size.clamp(4.0, 144.0);
    let max_width = max_width.max(font_size * 2.0);
    let line_height = (font_size * line_spacing.clamp(0.85, 3.0)).max(font_size * 1.05);

    let font_hint = if !requested_font.trim().is_empty() {
        requested_font
    } else {
        original_font
    };

    let mut is_subset = looks_like_subset_font(original_font);
    let mut substituted = false;
    let mut missing_chars: Vec<String> = Vec::new();
    let mut notice: Option<String> = None;

    // 1. Check embedded PDF font first if requested matches original
    let embedded = if let (Some(d), Some(pid)) = (doc, page_id) {
        if requested_font.trim().is_empty()
            || strip_subset_prefix(requested_font)
                .eq_ignore_ascii_case(&strip_subset_prefix(original_font))
        {
            extract_embedded_pdf_font(d, pid, original_font)
        } else {
            None
        }
    } else {
        None
    };

    let mut active_font_bytes: Option<(Vec<u8>, u32, String)> = None;

    if let Some((emb_bytes, emb_name, emb_subset)) = embedded {
        is_subset = emb_subset || is_subset;
        if let Ok(ttf) = TtfFace::parse(&emb_bytes, 0) {
            if ttf.number_of_glyphs() < 240 {
                is_subset = true;
            }
            let mut missing = Vec::new();
            for ch in text.chars() {
                if ch.is_control() || ch == ' ' {
                    continue;
                }
                if ttf.glyph_index(ch).map(|g| g.0 == 0).unwrap_or(true) {
                    let s = ch.to_string();
                    if !missing.contains(&s) {
                        missing.push(s);
                    }
                }
            }
            if missing.is_empty() && !emb_subset {
                active_font_bytes = Some((emb_bytes, 0, strip_subset_prefix(&emb_name)));
            } else {
                missing_chars = missing;
            }
        }
    }

    // 2. Fallback / substitution to offline system font via fontdb
    if active_font_bytes.is_none() {
        if let Some((sys_bytes, sys_idx, sys_name)) =
            resolve_system_font(font_hint, bold, italic, text)
        {
            let orig_clean = strip_subset_prefix(original_font);
            if !orig_clean.is_empty()
                && (!missing_chars.is_empty() || is_subset || doc.is_some())
                && !sys_name.eq_ignore_ascii_case(&orig_clean)
            {
                substituted = true;
                notice = Some(if !missing_chars.is_empty() {
                    format!(
                        "Embedded subset font \"{}\" is missing glyph(s) [{}]. Substituted offline with \"{}\".",
                        orig_clean,
                        missing_chars.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                        sys_name
                    )
                } else {
                    format!(
                        "Font \"{}\" has no editable full outline in PDF. Using closest system font \"{}\".",
                        orig_clean, sys_name
                    )
                });
            } else if !missing_chars.is_empty() || is_subset {
                substituted = true;
                notice = Some(format!(
                    "Subset font \"{}\" substituted with local system \"{}\" for reflow.",
                    orig_clean, sys_name
                ));
            }
            active_font_bytes = Some((sys_bytes, sys_idx, sys_name));
        } else {
            substituted = true;
            notice = Some(format!(
                "Font \"{}\" is not embedded; using fallback metric approximation.",
                strip_subset_prefix(font_hint)
            ));
        }
    }

    let resolved_font = active_font_bytes
        .as_ref()
        .map(|(_, _, n)| n.clone())
        .unwrap_or_else(|| strip_subset_prefix(font_hint));

    let requires_cid_unicode = contains_non_winansi(text);

    // 3. Shape and wrap paragraphs into lines using rustybuzz when font data is available
    let mut shaped_lines: Vec<ShapedLine> = Vec::new();
    let paragraphs: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();

    let ttf_face = active_font_bytes
        .as_ref()
        .and_then(|(b, idx, _)| TtfFace::parse(b, *idx).ok());
    let rb_face = active_font_bytes
        .as_ref()
        .and_then(|(b, idx, _)| rustybuzz::Face::from_slice(b, *idx));

    let measure_width = |s: &str| -> (f32, Vec<u16>, Vec<(u16, String)>, Vec<(u16, i32)>) {
        if let (Some(rf), Some(tf)) = (&rb_face, &ttf_face) {
            measure_run_width_with_rustybuzz(rf, tf, s, font_size)
        } else {
            let approx_w = s
                .chars()
                .map(|c| {
                    if c == ' ' {
                        font_size * 0.28
                    } else if c.is_ascii_uppercase() {
                        font_size * 0.66
                    } else if "iljtfr.,:;!".contains(c) {
                        font_size * 0.32
                    } else {
                        font_size * 0.54
                    }
                })
                .sum();
            (approx_w, Vec::new(), Vec::new(), Vec::new())
        }
    };

    for para in paragraphs {
        let trimmed = para.trim();
        let line_prefix = if bullets && !trimmed.is_empty() && !trimmed.starts_with("• ") {
            "• "
        } else {
            ""
        };
        let para_full = if line_prefix.is_empty() {
            para.to_string()
        } else {
            format!("{}{}", line_prefix, para)
        };

        if para_full.trim().is_empty() {
            let y_off = shaped_lines.len() as f32 * line_height;
            shaped_lines.push(ShapedLine {
                text: String::new(),
                x_offset: 0.0,
                y_offset: y_off,
                width: 0.0,
                word_spacing: 0.0,
                is_bullet: false,
                is_rtl: false,
                glyph_ids: Vec::new(),
                glyph_unicodes: Vec::new(),
                glyph_widths_1000: Vec::new(),
            });
            continue;
        }

        // Split on whitespace preserving space-separated tokens cleanly
        let words: Vec<&str> = para_full.split(' ').collect();
        let mut current = String::new();
        let mut wrapped: Vec<String> = Vec::new();

        for w in words {
            let candidate = if current.is_empty() {
                w.to_string()
            } else {
                format!("{} {}", current, w)
            };
            let (cand_w, _, _, _) = measure_width(&candidate);
            if !current.is_empty() && cand_w > max_width {
                wrapped.push(current);
                current = w.to_string();
            } else {
                current = candidate;
            }
        }
        if !current.is_empty() {
            wrapped.push(current);
        }

        let num_wrapped = wrapped.len();
        for (li, line_str) in wrapped.into_iter().enumerate() {
            let (lw, gids, unicodes, widths) = measure_width(&line_str);
            let remaining = (max_width - lw).max(0.0);
            let x_offset = match align {
                "center" => remaining * 0.5,
                "right" => remaining,
                _ => 0.0,
            };
            let word_spacing = if align == "justify" && li + 1 < num_wrapped {
                let spaces = line_str.matches(' ').count();
                if spaces > 0 && remaining > 0.5 && remaining < max_width * 0.35 {
                    remaining / spaces as f32
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let y_off = shaped_lines.len() as f32 * line_height;
            shaped_lines.push(ShapedLine {
                is_rtl: text_has_rtl(&line_str),
                is_bullet: bullets && li == 0,
                text: line_str,
                x_offset,
                y_offset: y_off,
                width: lw,
                word_spacing,
                glyph_ids: gids,
                glyph_unicodes: unicodes,
                glyph_widths_1000: widths,
            });
        }
    }

    let total_height = (shaped_lines.len().max(1) as f32) * line_height;

    ReflowBlockResult {
        lines: shaped_lines,
        line_height,
        total_height,
        original_font: strip_subset_prefix(original_font),
        resolved_font,
        is_subset,
        substituted,
        missing_chars,
        notice,
        requires_cid_unicode,
    }
}

fn map_to_standard14_name(font_hint: &str, bold: bool, italic: bool) -> &'static str {
    let lower = strip_subset_prefix(font_hint).to_lowercase();
    if ["times", "georgia", "garamond", "cambria", "palatino", "serif"]
        .iter()
        .any(|k| lower.contains(k))
        && !lower.contains("sans")
    {
        match (bold, italic) {
            (true, true) => "Times-BoldItalic",
            (true, false) => "Times-Bold",
            (false, true) => "Times-Italic",
            (false, false) => "Times-Roman",
        }
    } else if ["courier", "consolas", "mono", "menlo"]
        .iter()
        .any(|k| lower.contains(k))
    {
        match (bold, italic) {
            (true, true) => "Courier-BoldOblique",
            (true, false) => "Courier-Bold",
            (false, true) => "Courier-Oblique",
            (false, false) => "Courier",
        }
    } else {
        match (bold, italic) {
            (true, true) => "Helvetica-BoldOblique",
            (true, false) => "Helvetica-Bold",
            (false, true) => "Helvetica-Oblique",
            (false, false) => "Helvetica",
        }
    }
}

/// Ensure the PDF page `/Resources -> /Font` contains either a Standard14 WinAnsi font
/// OR a full TrueType Type0 CIDFontType2 + ToUnicode CMap for Bangla/Unicode text.
pub fn ensure_page_font_resource(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    font_hint: &str,
    bold: bool,
    italic: bool,
    full_text: &str,
) -> Result<PdfFontWriteMode, String> {
    let needs_unicode = contains_non_winansi(full_text);
    if !needs_unicode {
        let base_font = map_to_standard14_name(font_hint, bold, italic);
        let res_name = format!("PF_{}", base_font.replace('-', "_"));

        let mut fdict = Dictionary::new();
        fdict.set("Type", Object::Name(b"Font".to_vec()));
        fdict.set("Subtype", Object::Name(b"Type1".to_vec()));
        fdict.set("BaseFont", Object::Name(base_font.as_bytes().to_vec()));
        fdict.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        let font_id = doc.add_object(Object::Dictionary(fdict));
        attach_resource_entry(doc, page_id, b"Font", &res_name, Object::Reference(font_id))?;
        return Ok(PdfFontWriteMode::StandardWinAnsi {
            resource_name: res_name,
        });
    }

    let (ttf_bytes, face_idx, fam_name) =
        resolve_system_font(font_hint, bold, italic, full_text)
            .ok_or_else(|| format!("No offline system TrueType font available for '{}'", font_hint))?;
    let face = TtfFace::parse(&ttf_bytes, face_idx)
        .map_err(|e| format!("Failed parsing system font '{}': {:?}", fam_name, e))?;

    let upem = face.units_per_em().max(1) as f32;
    let mut char_to_gid: HashMap<char, u16> = HashMap::new();
    let mut gid_to_unicode: BTreeMap<u16, char> = BTreeMap::new();
    let mut gid_widths: BTreeMap<u16, i64> = BTreeMap::new();

    for ch in full_text.chars() {
        if ch == '\n' || ch == '\r' || ch == '\t' {
            continue;
        }
        let gid = face.glyph_index(ch).unwrap_or(GlyphId(0)).0;
        char_to_gid.insert(ch, gid);
        gid_to_unicode.entry(gid).or_insert(ch);
        let adv = face.glyph_hor_advance(GlyphId(gid)).unwrap_or(upem as u16 / 2);
        let w1000 = ((adv as f32 / upem) * 1000.0).round() as i64;
        gid_widths.insert(gid, w1000);
    }

    // Build /W [ gid [ width ] ... ]
    let mut w_arr: Vec<Object> = Vec::with_capacity(gid_widths.len() * 2);
    for (gid, w) in &gid_widths {
        w_arr.push(Object::Integer(*gid as i64));
        w_arr.push(Object::Array(vec![Object::Integer(*w)]));
    }

    // Build /ToUnicode CMap
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = gid_to_unicode.into_iter().collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (gid, ch) in chunk {
            let mut buf = [0u16; 2];
            let units = ch.encode_utf16(&mut buf);
            let hex: String = units.iter().map(|u| format!("{:04X}", u)).collect();
            cmap.push_str(&format!("<{:04X}> <{}>\n", gid, hex));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");

    let mut tounicode_stream = Stream::new(Dictionary::new(), cmap.into_bytes());
    let _ = tounicode_stream.compress();
    let tounicode_id = doc.add_object(Object::Stream(tounicode_stream));

    let asc = ((face.ascender() as f32 / upem) * 1000.0).round() as i64;
    let desc = ((face.descender() as f32 / upem) * 1000.0).round() as i64;
    let cap_h = ((face.capital_height().unwrap_or(face.ascender()) as f32 / upem) * 1000.0).round() as i64;

    let mut ff_dict = Dictionary::new();
    ff_dict.set("Length1", Object::Integer(ttf_bytes.len() as i64));
    let mut ff_stream = Stream::new(ff_dict, ttf_bytes);
    let _ = ff_stream.compress();
    let ff_id = doc.add_object(Object::Stream(ff_stream));

    let clean_ps_name: String = fam_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let ps_name = if clean_ps_name.is_empty() {
        "PDFFSys".to_string()
    } else {
        clean_ps_name
    };

    let mut fd_dict = Dictionary::new();
    fd_dict.set("Type", Object::Name(b"FontDescriptor".to_vec()));
    fd_dict.set("FontName", Object::Name(ps_name.as_bytes().to_vec()));
    fd_dict.set("Flags", Object::Integer(32));
    fd_dict.set(
        "FontBBox",
        Object::Array(vec![
            Object::Integer(-500),
            Object::Integer(desc.min(-300)),
            Object::Integer(1500),
            Object::Integer(asc.max(1000)),
        ]),
    );
    fd_dict.set("ItalicAngle", Object::Integer(if italic { -12 } else { 0 }));
    fd_dict.set("Ascent", Object::Integer(asc));
    fd_dict.set("Descent", Object::Integer(desc));
    fd_dict.set("CapHeight", Object::Integer(cap_h));
    fd_dict.set("StemV", Object::Integer(if bold { 120 } else { 80 }));
    fd_dict.set("FontFile2", Object::Reference(ff_id));
    let fd_id = doc.add_object(Object::Dictionary(fd_dict));

    let mut cid_sys = Dictionary::new();
    cid_sys.set("Registry", Object::string_literal("Adobe"));
    cid_sys.set("Ordering", Object::string_literal("Identity"));
    cid_sys.set("Supplement", Object::Integer(0));

    let mut cid_font = Dictionary::new();
    cid_font.set("Type", Object::Name(b"Font".to_vec()));
    cid_font.set("Subtype", Object::Name(b"CIDFontType2".to_vec()));
    cid_font.set("BaseFont", Object::Name(ps_name.as_bytes().to_vec()));
    cid_font.set("CIDSystemInfo", Object::Dictionary(cid_sys));
    cid_font.set("FontDescriptor", Object::Reference(fd_id));
    cid_font.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    cid_font.set("DW", Object::Integer(550));
    cid_font.set("W", Object::Array(w_arr));
    let cid_id = doc.add_object(Object::Dictionary(cid_font));

    let mut type0 = Dictionary::new();
    type0.set("Type", Object::Name(b"Font".to_vec()));
    type0.set("Subtype", Object::Name(b"Type0".to_vec()));
    type0.set("BaseFont", Object::Name(ps_name.as_bytes().to_vec()));
    type0.set("Encoding", Object::Name(b"Identity-H".to_vec()));
    type0.set("DescendantFonts", Object::Array(vec![Object::Reference(cid_id)]));
    type0.set("ToUnicode", Object::Reference(tounicode_id));
    let type0_id = doc.add_object(Object::Dictionary(type0));

    let res_name = format!("PFCID_{}", ps_name);
    attach_resource_entry(doc, page_id, b"Font", &res_name, Object::Reference(type0_id))?;

    Ok(PdfFontWriteMode::EmbeddedCidTrueType {
        resource_name: res_name,
        char_to_gid,
    })
}

pub fn attach_resource_entry(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    category: &[u8],
    entry_name: &str,
    entry_val: Object,
) -> Result<(), String> {
    // Ensure page has its own local /Resources dictionary (copying inherited one if needed)
    let inherited_res = get_page_resources_dict(doc, page_id).cloned();
    let res_id_opt = doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|d| d.get(b"Resources").ok())
        .and_then(|o| o.as_reference().ok());

    let mut res_dict = if let Some(rid) = res_id_opt {
        doc.get_dictionary(rid)
            .cloned()
            .unwrap_or_else(|_| Dictionary::new())
    } else if let Ok(pd) = doc.get_dictionary(page_id) {
        if let Ok(Object::Dictionary(d)) = pd.get(b"Resources") {
            d.clone()
        } else {
            inherited_res.unwrap_or_else(Dictionary::new)
        }
    } else {
        Dictionary::new()
    };

    let mut cat_dict = match res_dict.get(category) {
        Ok(Object::Reference(cid)) => doc
            .get_dictionary(*cid)
            .cloned()
            .unwrap_or_else(|_| Dictionary::new()),
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };

    cat_dict.set(entry_name.as_bytes(), entry_val);
    let cat_id = doc.add_object(Object::Dictionary(cat_dict));
    res_dict.set(category, Object::Reference(cat_id));

    let res_obj = if let Some(rid) = res_id_opt {
        if let Ok(obj) = doc.get_object_mut(rid) {
            *obj = Object::Dictionary(res_dict);
            Object::Reference(rid)
        } else {
            Object::Dictionary(res_dict)
        }
    } else {
        Object::Dictionary(res_dict)
    };

    let page_obj = doc
        .get_object_mut(page_id)
        .map_err(|e| format!("Failed accessing page {}: {}", page_id.0, e))?;
    if let Object::Dictionary(ref mut pd) = page_obj {
        pd.set("Resources", res_obj);
    }
    Ok(())
}

/// Encode string to WinAnsi byte slice cleanly (`)` / `(` / `\` escaped cleanly via lopdf Object::string_literal).
pub fn encode_winansi_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let b = match ch {
            '\u{20AC}' => 0x80,
            '\u{201A}' => 0x82,
            '\u{0192}' => 0x83,
            '\u{201E}' => 0x84,
            '\u{2026}' => 0x85,
            '\u{2020}' => 0x86,
            '\u{2021}' => 0x87,
            '\u{02C6}' => 0x88,
            '\u{2030}' => 0x89,
            '\u{0160}' => 0x8A,
            '\u{2039}' => 0x8B,
            '\u{0152}' => 0x8C,
            '\u{017D}' => 0x8E,
            '\u{2018}' => 0x91,
            '\u{2019}' => 0x92,
            '\u{201C}' => 0x93,
            '\u{201D}' => 0x94,
            '\u{2022}' => 0x95,
            '\u{2013}' => 0x96,
            '\u{2014}' => 0x97,
            '\u{02DC}' => 0x98,
            '\u{2122}' => 0x99,
            '\u{0161}' => 0x9A,
            '\u{203A}' => 0x9B,
            '\u{0153}' => 0x9C,
            '\u{017E}' => 0x9E,
            '\u{0178}' => 0x9F,
            c if (c as u32) <= 0xFF => c as u8,
            _ => b'?',
        };
        out.push(b);
    }
    out
}

pub fn encode_cid_glyph_bytes(text: &str, char_to_gid: &HashMap<char, u16>) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for ch in text.chars() {
        let gid = *char_to_gid.get(&ch).unwrap_or(&0);
        out.push((gid >> 8) as u8);
        out.push((gid & 0xFF) as u8);
    }
    out
}
