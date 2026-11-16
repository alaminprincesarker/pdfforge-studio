use crate::font_shaper::{
    extract_embedded_pdf_font, looks_like_subset_font, resolve_dict, strip_subset_prefix,
};
use crate::pdf_objects::{
    parse_page_objects, PageObjectsResult, PageTransform,
};
use lopdf::Document as LoDocument;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use unicode_bidi::BidiInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharNode {
    pub index: usize,
    pub unicode: String,
    pub bbox: [f32; 4],
    pub baseline: f32,
    pub font_name: String,
    pub font_size: f32,
    pub fill_color: String,
    pub bold: bool,
    pub italic: bool,
    pub rotation: f32,
    pub rtl: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordNode {
    pub id: String,
    pub text: String,
    pub bbox: [f32; 4],
    pub start_char: usize,
    pub end_char: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineNode {
    pub id: String,
    pub text: String,
    pub bbox: [f32; 4],
    pub baseline: f32,
    pub rotation: f32,
    pub font_name: String,
    pub font_size: f32,
    pub fill_color: String,
    pub bold: bool,
    pub italic: bool,
    pub is_rtl: bool,
    pub start_char: usize,
    pub end_char: usize,
    pub words: Vec<WordNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockNode {
    pub id: String,
    pub column_index: usize,
    pub text: String,
    pub bbox: [f32; 4],
    pub font_name: String,
    pub font_size: f32,
    pub color: String,
    pub bold: bool,
    pub italic: bool,
    pub alignment: String,
    pub line_spacing: f32,
    pub has_bullets: bool,
    pub is_rtl: bool,
    pub rotation: f32,
    pub has_embedded_font: bool,
    pub is_subset_font: bool,
    pub start_char: usize,
    pub end_char: usize,
    pub lines: Vec<LineNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageLayout {
    pub page: usize,
    pub num_pages: usize,
    pub page_width: f32,
    pub page_height: f32,
    pub rotation: i32,
    pub page_transform: PageTransform,
    pub is_scanned: bool,
    pub column_count: usize,
    pub warnings: Vec<String>,
    pub chars: Vec<CharNode>,
    pub blocks: Vec<BlockNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HitTestResult {
    pub hit: bool,
    pub char_index: Option<usize>,
    pub word_id: Option<String>,
    pub line_id: Option<String>,
    pub block_id: Option<String>,
    pub word_range: Option<[usize; 2]>,
    pub line_range: Option<[usize; 2]>,
    pub block_range: Option<[usize; 2]>,
    pub block_bbox: Option<[f32; 4]>,
    pub shape_id: Option<String>,
    pub object_kind: String, // "char" | "shape" | "image" | "none"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRectsResult {
    pub text: String,
    pub rects: Vec<[f32; 4]>,
    pub start_char: usize,
    pub end_char: usize,
}

static PDFIUM_INSTANCE: OnceLock<Arc<Mutex<Pdfium>>> = OnceLock::new();

pub fn locate_pdfium_library_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("pdfium.dll"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("pdfforge").join("pdfium.dll"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("pdfium.dll"));
        candidates.push(cwd.join("src-rust").join("pdfium.dll"));
        candidates.push(cwd.join("release").join("pdfium.dll"));
    }
    candidates.into_iter().find(|p| p.exists())
}

pub fn get_pdfium() -> Result<Arc<Mutex<Pdfium>>, String> {
    if let Some(inst) = PDFIUM_INSTANCE.get() {
        return Ok(inst.clone());
    }
    let bindings = if let Some(path) = locate_pdfium_library_path() {
        Pdfium::bind_to_library(&path)
            .map_err(|e| format!("Failed binding PDFium at {}: {:?}", path.display(), e))?
    } else {
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
            .or_else(|_| Pdfium::bind_to_system_library())
            .map_err(|e| format!("Failed locating offline pdfium.dll: {:?}", e))?
    };
    let inst = Arc::new(Mutex::new(Pdfium::new(bindings)));
    let _ = PDFIUM_INSTANCE.set(inst.clone());
    Ok(inst)
}

#[derive(Default)]
struct WorkingDocument {
    #[allow(dead_code)]
    canonical_key: String,
    bytes: Vec<u8>,
    revision: u64,
    undo_stack: Vec<Vec<u8>>,
    redo_stack: Vec<Vec<u8>>,
    layout_cache: HashMap<usize, Arc<PageLayout>>,
    objects_cache: HashMap<usize, Arc<PageObjectsResult>>,
}

#[derive(Clone, Default)]
pub struct PdfWorkspaceEngine {
    docs: Arc<Mutex<HashMap<String, WorkingDocument>>>,
}

impl PdfWorkspaceEngine {
    pub fn new() -> Self {
        Self {
            docs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn normalize_key(file_path: &str) -> String {
        file_path
            .trim()
            .strip_prefix(r"\\?\")
            .unwrap_or(file_path.trim())
            .to_lowercase()
    }

    pub fn load_or_sync_from_disk(&self, file_path: &str) -> Result<Vec<u8>, String> {
        let key = Self::normalize_key(file_path);
        {
            let guard = self.docs.lock().unwrap();
            if let Some(doc) = guard.get(&key) {
                return Ok(doc.bytes.clone());
            }
        }
        let disk_bytes = std::fs::read(file_path)
            .map_err(|e| format!("Failed reading PDF '{}': {}", file_path, e))?;
        let mut guard = self.docs.lock().unwrap();
        let entry = guard.entry(key.clone()).or_insert_with(|| WorkingDocument {
            canonical_key: file_path.to_string(),
            bytes: disk_bytes.clone(),
            revision: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            layout_cache: HashMap::new(),
            objects_cache: HashMap::new(),
        });
        Ok(entry.bytes.clone())
    }

    #[allow(dead_code)]
    pub fn reload_from_disk(&self, file_path: &str) -> Result<(), String> {
        let key = Self::normalize_key(file_path);
        let disk_bytes = std::fs::read(file_path)
            .map_err(|e| format!("Failed reading PDF '{}': {}", file_path, e))?;
        let mut guard = self.docs.lock().unwrap();
        guard.insert(
            key,
            WorkingDocument {
                canonical_key: file_path.to_string(),
                bytes: disk_bytes,
                revision: 1,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
                layout_cache: HashMap::new(),
                objects_cache: HashMap::new(),
            },
        );
        Ok(())
    }

    pub fn register_bytes_for_path(&self, file_path: &str, bytes: Vec<u8>) {
        let key = Self::normalize_key(file_path);
        let mut guard = self.docs.lock().unwrap();
        guard.insert(
            key,
            WorkingDocument {
                canonical_key: file_path.to_string(),
                bytes,
                revision: 1,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
                layout_cache: HashMap::new(),
                objects_cache: HashMap::new(),
            },
        );
    }

    pub fn push_mutation<F>(
        &self,
        file_path: &str,
        page: usize,
        mutator: F,
    ) -> Result<(u64, bool, bool, Option<String>), String>
    where
        F: FnOnce(&mut LoDocument) -> Result<Option<String>, String>,
    {
        let current_bytes = self.load_or_sync_from_disk(file_path)?;
        let mut lo_doc = LoDocument::load_mem(&current_bytes)
            .map_err(|e| format!("Failed parsing PDF in lopdf: {}", e))?;
        let notice = mutator(&mut lo_doc)?;
        let mut new_bytes = Vec::new();
        lo_doc
            .save_to(&mut new_bytes)
            .map_err(|e| format!("Failed serializing mutated PDF: {}", e))?;

        let key = Self::normalize_key(file_path);
        let mut guard = self.docs.lock().unwrap();
        let entry = guard.get_mut(&key).ok_or("Missing working document")?;
        entry.undo_stack.push(entry.bytes.clone());
        if entry.undo_stack.len() > 40 {
            entry.undo_stack.remove(0);
        }
        entry.redo_stack.clear();
        entry.bytes = new_bytes;
        entry.revision += 1;
        entry.layout_cache.remove(&page);
        entry.objects_cache.remove(&page);
        Ok((
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
            notice,
        ))
    }

    pub fn undo(&self, file_path: &str) -> Result<(Vec<u8>, u64, bool, bool), String> {
        let key = Self::normalize_key(file_path);
        let mut guard = self.docs.lock().unwrap();
        let entry = guard.get_mut(&key).ok_or("Document not loaded")?;
        let prev = entry.undo_stack.pop().ok_or("Nothing to undo")?;
        entry.redo_stack.push(entry.bytes.clone());
        entry.bytes = prev;
        entry.revision += 1;
        entry.layout_cache.clear();
        entry.objects_cache.clear();
        Ok((
            entry.bytes.clone(),
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
        ))
    }

    pub fn redo(&self, file_path: &str) -> Result<(Vec<u8>, u64, bool, bool), String> {
        let key = Self::normalize_key(file_path);
        let mut guard = self.docs.lock().unwrap();
        let entry = guard.get_mut(&key).ok_or("Document not loaded")?;
        let next = entry.redo_stack.pop().ok_or("Nothing to redo")?;
        entry.undo_stack.push(entry.bytes.clone());
        entry.bytes = next;
        entry.revision += 1;
        entry.layout_cache.clear();
        entry.objects_cache.clear();
        Ok((
            entry.bytes.clone(),
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
        ))
    }

    pub fn get_working_bytes(&self, file_path: &str) -> Result<Vec<u8>, String> {
        self.load_or_sync_from_disk(file_path)
    }

    pub fn get_objects(&self, file_path: &str, page: usize) -> Result<Arc<PageObjectsResult>, String> {
        let page = page.max(1);
        let key = Self::normalize_key(file_path);
        {
            let guard = self.docs.lock().unwrap();
            if let Some(doc) = guard.get(&key) {
                if let Some(cached) = doc.objects_cache.get(&page) {
                    return Ok(cached.clone());
                }
            }
        }

        let bytes = self.load_or_sync_from_disk(file_path)?;
        let lo_doc = LoDocument::load_mem(&bytes)
            .map_err(|e| format!("Failed parsing PDF objects: {}", e))?;
        let parsed = Arc::new(parse_page_objects(&lo_doc, page - 1)?);

        let mut guard = self.docs.lock().unwrap();
        if let Some(doc) = guard.get_mut(&key) {
            doc.objects_cache.insert(page, parsed.clone());
        }
        Ok(parsed)
    }

    pub fn get_page_layout(&self, file_path: &str, page: usize) -> Result<Arc<PageLayout>, String> {
        let page = page.max(1);
        let key = Self::normalize_key(file_path);
        {
            let guard = self.docs.lock().unwrap();
            if let Some(doc) = guard.get(&key) {
                if let Some(cached) = doc.layout_cache.get(&page) {
                    return Ok(cached.clone());
                }
            }
        }

        let bytes = self.load_or_sync_from_disk(file_path)?;
        let objs = self.get_objects(file_path, page).ok();
        let layout = Arc::new(extract_page_layout_from_bytes(
            &bytes,
            page,
            objs.as_deref(),
        )?);

        let num_pages = layout.num_pages;
        {
            let mut guard = self.docs.lock().unwrap();
            if let Some(doc) = guard.get_mut(&key) {
                doc.layout_cache.insert(page, layout.clone());
            }
        }

        // Lazy background prefetch for next page to keep scrolling snappy
        if page < num_pages {
            let next_page = page + 1;
            let engine_clone = self.clone();
            let path_clone = file_path.to_string();
            let key_clone = key.clone();
            let already_cached = {
                let guard = self.docs.lock().unwrap();
                guard
                    .get(&key_clone)
                    .map(|d| d.layout_cache.contains_key(&next_page))
                    .unwrap_or(false)
            };
            if !already_cached {
                std::thread::spawn(move || {
                    let _ = engine_clone.get_page_layout(&path_clone, next_page);
                });
            }
        }

        Ok(layout)
    }

    pub fn hit_test(
        &self,
        file_path: &str,
        page: usize,
        x: f32,
        y: f32,
    ) -> Result<HitTestResult, String> {
        let layout = self.get_page_layout(file_path, page)?;
        let objs = self.get_objects(file_path, page).ok();

        let hit_shape = objs.as_ref().and_then(|o| {
            o.path_objects
                .iter()
                .rev()
                .find(|s| {
                    !s.is_page_background
                        && x >= s.bbox_vp[0] - 4.0
                        && x <= s.bbox_vp[0] + s.bbox_vp[2] + 4.0
                        && y >= s.bbox_vp[1] - 4.0
                        && y <= s.bbox_vp[1] + s.bbox_vp[3] + 4.0
                })
                .map(|s| s.id.clone())
        });

        let hit_char = find_char_at_point(&layout, x, y);

        if let Some(cidx) = hit_char {
            for blk in &layout.blocks {
                if cidx >= blk.start_char && cidx <= blk.end_char {
                    for ln in &blk.lines {
                        if cidx >= ln.start_char && cidx <= ln.end_char {
                            let matched_word = ln
                                .words
                                .iter()
                                .find(|w| cidx >= w.start_char && cidx <= w.end_char)
                                .or_else(|| {
                                    ln.words.iter().min_by(|a, b| {
                                        let da = (x - (a.bbox[0] + a.bbox[2] * 0.5)).abs();
                                        let db = (x - (b.bbox[0] + b.bbox[2] * 0.5)).abs();
                                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                                    })
                                });
                            return Ok(HitTestResult {
                                hit: true,
                                char_index: Some(cidx),
                                word_id: matched_word.map(|w| w.id.clone()),
                                line_id: Some(ln.id.clone()),
                                block_id: Some(blk.id.clone()),
                                word_range: matched_word.map(|w| [w.start_char, w.end_char]),
                                line_range: Some([ln.start_char, ln.end_char]),
                                block_range: Some([blk.start_char, blk.end_char]),
                                block_bbox: Some(blk.bbox),
                                shape_id: hit_shape,
                                object_kind: "char".to_string(),
                            });
                        }
                    }
                }
            }
            return Ok(HitTestResult {
                hit: true,
                char_index: Some(cidx),
                word_id: None,
                line_id: None,
                block_id: None,
                word_range: None,
                line_range: None,
                block_range: None,
                block_bbox: None,
                shape_id: hit_shape,
                object_kind: "char".to_string(),
            });
        }

        if let Some(sid) = hit_shape {
            return Ok(HitTestResult {
                hit: true,
                char_index: None,
                word_id: None,
                line_id: None,
                block_id: None,
                word_range: None,
                line_range: None,
                block_range: None,
                block_bbox: None,
                shape_id: Some(sid),
                object_kind: "shape".to_string(),
            });
        }

        let hit_img = objs.as_ref().and_then(|o| {
            o.image_objects
                .iter()
                .rev()
                .find(|im| {
                    x >= im.bbox_vp[0]
                        && x <= im.bbox_vp[0] + im.bbox_vp[2]
                        && y >= im.bbox_vp[1]
                        && y <= im.bbox_vp[1] + im.bbox_vp[3]
                })
                .map(|im| im.id.clone())
        });

        Ok(HitTestResult {
            hit: hit_img.is_some(),
            char_index: None,
            word_id: None,
            line_id: None,
            block_id: None,
            word_range: None,
            line_range: None,
            block_range: None,
            block_bbox: None,
            shape_id: hit_img,
            object_kind: if objs.as_ref().is_some_and(|o| !o.image_objects.is_empty()) {
                "none".to_string()
            } else {
                "none".to_string()
            },
        })
    }

    pub fn get_selection_rects(
        &self,
        file_path: &str,
        page: usize,
        start_char: usize,
        end_char: usize,
    ) -> Result<SelectionRectsResult, String> {
        let layout = self.get_page_layout(file_path, page)?;
        Ok(compute_selection_rects(&layout, start_char, end_char))
    }
}

pub fn compute_selection_rects(
    layout: &PageLayout,
    start_char: usize,
    end_char: usize,
) -> SelectionRectsResult {
    if layout.chars.is_empty() {
        return SelectionRectsResult {
            text: String::new(),
            rects: Vec::new(),
            start_char: 0,
            end_char: 0,
        };
    }

    let lo = start_char.min(end_char).min(layout.chars.len() - 1);
    let hi = start_char.max(end_char).min(layout.chars.len() - 1);

    let mut text = String::new();
    let mut rects: Vec<[f32; 4]> = Vec::new();

    for blk in &layout.blocks {
        if blk.end_char < lo || blk.start_char > hi {
            continue;
        }
        let mut had_line_in_blk = false;
        for ln in &blk.lines {
            if ln.end_char < lo || ln.start_char > hi {
                continue;
            }
            if had_line_in_blk {
                text.push('\n');
            } else if !text.is_empty() {
                text.push('\n');
            }
            had_line_in_blk = true;
            let mut first_in_line = true;

            let line_lo = lo.max(ln.start_char);
            let line_hi = hi.min(ln.end_char);
            let mut cur_rect: Option<[f32; 4]> = None;

            for c in &layout.chars[line_lo..=line_hi] {
                if !first_in_line && c.unicode == "\n" {
                    continue;
                }
                text.push_str(&c.unicode);
                first_in_line = false;

                let [cx, cy, cw, ch] = c.bbox;
                if cw <= 0.1 || ch <= 0.1 {
                    continue;
                }
                if let Some(ref mut r) = cur_rect {
                    // Merge chars on the same visual line when close horizontally
                    let r_right = r[0] + r[2];
                    let c_right = cx + cw;
                    let same_y = (r[1] - cy).abs() < (r[3].max(ch) * 0.65)
                        && ((cy + ch) - (r[1] + r[3])).abs() < (r[3].max(ch) * 0.65);
                    let close_x = (cx - r_right).abs() < c.font_size.max(8.0) * 1.4
                        || (r[0] - c_right).abs() < c.font_size.max(8.0) * 1.4
                        || (cx <= r_right && c_right >= r[0]);
                    if same_y && close_x {
                        let nx = r[0].min(cx);
                        let ny = r[1].min(cy);
                        let nx2 = r_right.max(c_right);
                        let ny2 = (r[1] + r[3]).max(cy + ch);
                        *r = [nx, ny, nx2 - nx, ny2 - ny];
                    } else {
                        rects.push(*r);
                        *r = [cx, cy, cw, ch];
                    }
                } else {
                    cur_rect = Some([cx, cy, cw, ch]);
                }
            }
            if let Some(r) = cur_rect {
                rects.push(r);
            }
        }
    }

    SelectionRectsResult {
        text: text.trim_end().to_string(),
        rects,
        start_char: lo,
        end_char: hi,
    }
}

fn find_char_at_point(layout: &PageLayout, x: f32, y: f32) -> Option<usize> {
    if layout.chars.is_empty() {
        return None;
    }

    // 1. Direct / tight hit on any char bbox
    for c in &layout.chars {
        let [bx, by, bw, bh] = c.bbox;
        if x >= bx - 1.5 && x <= bx + bw + 1.5 && y >= by - 2.0 && y <= by + bh + 2.0 {
            return Some(c.index);
        }
    }

    // 2. Match nearest line vertically within tolerance, then nearest char along reading axis
    let mut best_line: Option<(&LineNode, f32)> = None;
    for blk in &layout.blocks {
        for ln in &blk.lines {
            let [_, ly, _, lh] = ln.bbox;
            let dy = if y < ly {
                ly - y
            } else if y > ly + lh {
                y - (ly + lh)
            } else {
                0.0
            };
            if dy <= ln.font_size.max(10.0) * 0.9 {
                if best_line.as_ref().is_none_or(|b| dy < b.1) {
                    best_line = Some((ln, dy));
                }
            }
        }
    }

    if let Some((ln, _)) = best_line {
        return layout.chars[ln.start_char..=ln.end_char]
            .iter()
            .min_by(|a, b| {
                let da = ((a.bbox[0] + a.bbox[2] * 0.5) - x)
                    .hypot((a.bbox[1] + a.bbox[3] * 0.5) - y);
                let db = ((b.bbox[0] + b.bbox[2] * 0.5) - x)
                    .hypot((b.bbox[1] + b.bbox[3] * 0.5) - y);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.index);
    }

    // 3. Nearest overall within 36pt radius
    let mut best: Option<(usize, f32)> = None;
    for c in &layout.chars {
        let cx = c.bbox[0] + c.bbox[2] * 0.5;
        let cy = c.bbox[1] + c.bbox[3] * 0.5;
        let d = (cx - x).hypot(cy - y);
        if d <= 36.0 && best.as_ref().is_none_or(|b| d < b.1) {
            best = Some((c.index, d));
        }
    }
    best.map(|b| b.0)
}

#[derive(Clone)]
struct RawExtractedChar {
    unicode: String,
    bbox: [f32; 4],
    baseline: f32,
    center_x: f32,
    center_y: f32,
    font_name: String,
    font_size: f32,
    fill_color: String,
    bold: bool,
    italic: bool,
    rotation: f32,
    is_space: bool,
}

pub fn extract_page_layout_from_bytes(
    bytes: &[u8],
    page_num_one: usize,
    objects_opt: Option<&PageObjectsResult>,
) -> Result<PageLayout, String> {
    let lo_doc = LoDocument::load_mem(bytes)
        .map_err(|e| format!("Failed loading PDF in lopdf: {}", e))?;
    let pages_map = lo_doc.get_pages();
    let page_id = *pages_map
        .get(&(page_num_one as u32))
        .ok_or_else(|| format!("Page {} out of bounds", page_num_one))?;
    let pt = crate::pdf_objects::get_page_transform(&lo_doc, page_id);

    // Check embedded font presence per font name in Page Resources
    let mut embedded_font_status: HashMap<String, (bool, bool)> = HashMap::new();
    if let Some(res_dict) = crate::font_shaper::get_page_resources_dict(&lo_doc, page_id) {
        if let Ok(fobj) = res_dict.get(b"Font") {
            if let Some(fdict) = resolve_dict(&lo_doc, fobj) {
                for (_, val) in fdict.iter() {
                    if let Some(fd) = resolve_dict(&lo_doc, val) {
                        let base = fd
                            .get(b"BaseFont")
                            .ok()
                            .and_then(|o| o.as_name().ok())
                            .map(|n| String::from_utf8_lossy(n).to_string())
                            .unwrap_or_default();
                        let clean = strip_subset_prefix(&base).to_lowercase();
                        let is_sub = looks_like_subset_font(&base);
                        let has_emb =
                            extract_embedded_pdf_font(&lo_doc, page_id, &base).is_some();
                        if !clean.is_empty() {
                            embedded_font_status.insert(clean, (has_emb, is_sub));
                        }
                    }
                }
            }
        }
    }

    let pdfium = get_pdfium()?;
    let pdfium_guard = pdfium.lock().unwrap();
    let doc = pdfium_guard
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?;
    let num_pages = doc.pages().len() as usize;
    let pg = doc
        .pages()
        .get((page_num_one - 1) as i32)
        .map_err(|e| format!("PDFium failed opening page {}: {:?}", page_num_one, e))?;

    let mut raw_chars: Vec<RawExtractedChar> = Vec::new();
    let text_page = pg
        .text()
        .map_err(|e| format!("PDFium text extraction failed: {:?}", e))?;

    for ch in text_page.chars().iter() {
        let Some(uni_raw) = ch.unicode_string() else {
            continue;
        };
        if uni_raw == "\r" || uni_raw == "\0" {
            continue;
        }
        let uni = if uni_raw == "\u{00A0}" {
            " ".to_string()
        } else {
            uni_raw
        };
        let is_space = uni.trim().is_empty();

        let rect = ch
            .loose_bounds()
            .or_else(|_| ch.tight_bounds())
            .unwrap_or(PdfRect::ZERO);
        let bbox = pt.pdf_rect_to_vp(
            rect.left().value,
            rect.bottom().value,
            rect.right().value,
            rect.top().value,
        );

        let origin = ch
            .origin()
            .map(|(ox, oy)| pt.pdf_to_vp(ox.value, oy.value))
            .unwrap_or((bbox[0], bbox[1] + bbox[3] * 0.85));

        let char_rot_raw = ch.angle_degrees().unwrap_or(0.0);
        // Net visual angle in upright viewport
        let net_rot = ((char_rot_raw - pt.rotation as f32) % 360.0 + 360.0) % 360.0;
        let eff_rot = if net_rot > 180.0 {
            net_rot - 360.0
        } else {
            net_rot
        };

        let fs = ch.scaled_font_size().value.max(1.0);
        let fname = ch.font_name();
        let fname_low = fname.to_lowercase();
        let bold = ch.font_is_bold_reenforced()
            || matches!(
                ch.font_weight(),
                Some(PdfFontWeight::Weight700Bold)
                    | Some(PdfFontWeight::Weight800)
                    | Some(PdfFontWeight::Weight900)
            )
            || fname_low.contains("bold")
            || fname_low.contains("black")
            || fname_low.contains("heavy");
        let italic = ch.font_is_italic()
            || fname_low.contains("italic")
            || fname_low.contains("oblique");

        let fill_color = ch
            .fill_color()
            .map(|c| format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue()))
            .unwrap_or_else(|_| "#111827".to_string());

        raw_chars.push(RawExtractedChar {
            unicode: uni,
            bbox,
            baseline: origin.1,
            center_x: bbox[0] + bbox[2] * 0.5,
            center_y: bbox[1] + bbox[3] * 0.5,
            font_name: fname,
            font_size: fs,
            fill_color,
            bold,
            italic,
            rotation: eff_rot,
            is_space,
        });
    }

    drop(text_page);
    drop(pg);
    drop(doc);
    drop(pdfium_guard);

    let mut warnings: Vec<String> = Vec::new();
    let has_images = objects_opt
        .map(|o| !o.image_objects.is_empty())
        .unwrap_or(false);
    let non_space_count = raw_chars.iter().filter(|c| !c.is_space).count();
    let is_scanned = non_space_count == 0 && has_images;
    if is_scanned {
        warnings.push(
            "Scanned image-only page detected (0 vector text characters). Run OCR before inline Edit Text."
                .to_string(),
        );
    }

    // Warn on unembedded or subset fonts encountered on the page
    let mut seen_font_warn: HashSet<String> = HashSet::new();
    for rc in &raw_chars {
        let clean = strip_subset_prefix(&rc.font_name);
        if clean.is_empty() || !seen_font_warn.insert(clean.clone()) {
            continue;
        }
        if let Some(&(has_emb, is_sub)) = embedded_font_status.get(&clean.to_lowercase()) {
            if !has_emb {
                warnings.push(format!(
                    "Font \"{}\" is not embedded as an editable outline; edits substitute closest offline system font.",
                    clean
                ));
            } else if is_sub {
                warnings.push(format!(
                    "Font \"{}\" is an embedded subset; missing characters automatically use offline system fallback.",
                    clean
                ));
            }
        }
    }

    let (column_count, chars, blocks) =
        build_structured_layout(page_num_one, &pt, raw_chars, &embedded_font_status);

    Ok(PageLayout {
        page: page_num_one,
        num_pages,
        page_width: pt.vp_width,
        page_height: pt.vp_height,
        rotation: pt.rotation,
        page_transform: pt,
        is_scanned,
        column_count,
        warnings,
        chars,
        blocks,
    })
}

/// Detect clean column gutters using per-row vertical band coverage so a spanning header/footer
/// line never splits or hides genuine multi-column gutters.
fn detect_column_gutters(chars: &[RawExtractedChar], vp_width: f32) -> Vec<f32> {
    let visible: Vec<&RawExtractedChar> = chars
        .iter()
        .filter(|c| !c.is_space && c.rotation.abs() < 15.0)
        .collect();
    if visible.len() < 24 || vp_width < 200.0 {
        return Vec::new();
    }

    let bins = 120usize;
    let bin_w = vp_width / bins as f32;
    let mut row_sets: Vec<HashSet<i32>> = vec![HashSet::new(); bins];

    for c in &visible {
        // Bucket baseline Y into ~10pt lines so narrow char overlaps on one title line count as 1 row
        let row_bucket = (c.baseline / 10.0).round() as i32;
        let b0 = ((c.bbox[0] / bin_w).floor() as isize).clamp(0, bins as isize - 1) as usize;
        let b1 = (((c.bbox[0] + c.bbox[2]) / bin_w).floor() as isize).clamp(0, bins as isize - 1) as usize;
        for set in &mut row_sets[b0..=b1] {
            set.insert(row_bucket);
        }
    }

    let distinct_rows: HashSet<i32> = visible.iter().map(|c| (c.baseline / 10.0).round() as i32).collect();
    let max_title_rows = if distinct_rows.len() >= 5 { 1 } else { 0 };

    let start_bin = (bins as f32 * 0.20) as usize;
    let end_bin = (bins as f32 * 0.80) as usize;
    let mut gutters: Vec<f32> = Vec::new();
    let mut run_start: Option<usize> = None;

    for b in start_bin..=end_bin {
        if row_sets[b].len() <= max_title_rows {
            if run_start.is_none() {
                run_start = Some(b);
            }
        } else if let Some(rs) = run_start.take() {
            let gutter_w = (b - rs) as f32 * bin_w;
            if gutter_w >= 18.0 {
                let gx = ((rs + b) as f32 * 0.5) * bin_w;
                let left_cnt = visible.iter().filter(|c| c.center_x < gx - 8.0).count();
                let right_cnt = visible.iter().filter(|c| c.center_x > gx + 8.0).count();
                if left_cnt >= 16 && right_cnt >= 16 {
                    gutters.push(gx);
                }
            }
        }
    }
    gutters
}

fn column_for_x(x: f32, gutters: &[f32]) -> usize {
    for (idx, &g) in gutters.iter().enumerate() {
        if x < g {
            return idx;
        }
    }
    gutters.len()
}

fn build_structured_layout(
    page_num: usize,
    pt: &PageTransform,
    raw_chars: Vec<RawExtractedChar>,
    embedded_font_status: &HashMap<String, (bool, bool)>,
) -> (usize, Vec<CharNode>, Vec<BlockNode>) {
    if raw_chars.is_empty() {
        return (1, Vec::new(), Vec::new());
    }

    let gutters = detect_column_gutters(&raw_chars, pt.vp_width);
    let col_count = gutters.len() + 1;

    // 1. Group characters into visual line fragments sharing baseline + column + rotation
    struct TempLine {
        column_idx: usize,
        items: Vec<RawExtractedChar>,
        baseline: f32,
        rotation: f32,
        font_size: f32,
    }

    // Sort initially by baseline Y then X to cluster lines robustly even when PDF stream order is scrambled
    let mut sorted = raw_chars;
    sorted.sort_by(|a, b| {
        if (a.baseline - b.baseline).abs() <= 2.5 && (a.rotation - b.rotation).abs() <= 10.0 {
            a.center_x
                .partial_cmp(&b.center_x)
                .unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.baseline
                .partial_cmp(&b.baseline)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    let mut temp_lines: Vec<TempLine> = Vec::new();
    for ch in sorted {
        if ch.is_space && ch.bbox[2] <= 0.1 {
            continue;
        }
        let c_col = if ch.rotation.abs() < 15.0 {
            column_for_x(ch.center_x, &gutters)
        } else {
            0
        };
        let rot_group = (ch.rotation / 15.0).round();

        // Try matching existing line fragment
        let mut target_idx: Option<usize> = None;
        for (li, tl) in temp_lines.iter().enumerate().rev() {
            let tl_rot_group = (tl.rotation / 15.0).round();
            if (rot_group - tl_rot_group).abs() > 0.5 {
                continue;
            }
            if ch.rotation.abs() < 15.0 && tl.column_idx != c_col {
                // Allow single full-width spanning lines only if spanning across gutter continuously
                let last_x = tl.items.last().map(|p| p.bbox[0] + p.bbox[2]).unwrap_or(0.0);
                let gap = (ch.bbox[0] - last_x).abs();
                if gap > ch.font_size.max(8.0) * 1.6 {
                    continue;
                }
            }
            let base_tol = (tl.font_size.max(ch.font_size) * 0.42).max(2.8);
            if ch.rotation.abs() >= 15.0 {
                // For rotated text lines (vertical/angled), compare perpendicular distance along line direction
                if let Some(last) = tl.items.last() {
                    let dist = (ch.center_x - last.center_x).hypot(ch.center_y - last.center_y);
                    if dist <= tl.font_size.max(ch.font_size) * 2.5 {
                        target_idx = Some(li);
                        break;
                    }
                }
            } else if (ch.baseline - tl.baseline).abs() <= base_tol {
                let last_x = tl.items.last().map(|p| p.bbox[0] + p.bbox[2]).unwrap_or(0.0);
                if (ch.bbox[0] - last_x) <= ch.font_size.max(8.0) * 2.4 {
                    target_idx = Some(li);
                    break;
                }
            }
        }

        if let Some(li) = target_idx {
            let tl = &mut temp_lines[li];
            let n = tl.items.len() as f32;
            tl.baseline = (tl.baseline * n + ch.baseline) / (n + 1.0);
            tl.font_size = tl.font_size.max(ch.font_size);
            tl.items.push(ch);
        } else {
            temp_lines.push(TempLine {
                column_idx: c_col,
                baseline: ch.baseline,
                rotation: ch.rotation,
                font_size: ch.font_size,
                items: vec![ch],
            });
        }
    }

    // Sort lines in reading order: spanning titles above columns first, then column-by-column top to bottom
    for tl in &mut temp_lines {
        if tl.rotation.abs() < 15.0 {
            tl.items.sort_by(|a, b| {
                a.bbox[0]
                    .partial_cmp(&b.bbox[0])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    // Compute top Y where column split begins
    let first_col1_y = temp_lines
        .iter()
        .filter(|l| l.column_idx > 0)
        .map(|l| l.baseline)
        .fold(f32::INFINITY, f32::min);

    temp_lines.sort_by(|a, b| {
        let a_above = gutters.is_empty() || a.baseline < first_col1_y - 24.0;
        let b_above = gutters.is_empty() || b.baseline < first_col1_y - 24.0;
        if a_above && b_above {
            return a
                .baseline
                .partial_cmp(&b.baseline)
                .unwrap_or(std::cmp::Ordering::Equal);
        }
        if a_above != b_above {
            return if a_above {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        a.column_idx.cmp(&b.column_idx).then_with(|| {
            a.baseline
                .partial_cmp(&b.baseline)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // 2. Finalize Chars (handling Unicode Bidi RTL + Bangla graphemes), Words & Lines
    let mut final_chars: Vec<CharNode> = Vec::new();
    let mut finalized_lines: Vec<(usize, LineNode)> = Vec::with_capacity(temp_lines.len());

    for (li, mut tl) in temp_lines.into_iter().enumerate() {
        // Trim leading/trailing spaces
        while tl.items.first().is_some_and(|c| c.is_space) {
            tl.items.remove(0);
        }
        while tl.items.last().is_some_and(|c| c.is_space) {
            tl.items.pop();
        }
        if tl.items.is_empty() {
            continue;
        }

        // Insert synthetic spaces between wide kerning gaps so copy/selection has clean spaces
        let mut with_spaces: Vec<RawExtractedChar> = Vec::with_capacity(tl.items.len() + 4);
        for item in tl.items {
            if let Some(prev) = with_spaces.last() {
                let gap = item.bbox[0] - (prev.bbox[0] + prev.bbox[2]);
                let thresh = prev.font_size.max(8.0) * 0.24;
                if gap > thresh && !prev.is_space && !item.is_space && item.rotation.abs() < 15.0 {
                    with_spaces.push(RawExtractedChar {
                        unicode: " ".to_string(),
                        bbox: [
                            prev.bbox[0] + prev.bbox[2],
                            prev.bbox[1],
                            gap.min(prev.font_size * 0.6),
                            prev.bbox[3],
                        ],
                        baseline: prev.baseline,
                        center_x: prev.bbox[0] + prev.bbox[2] + gap * 0.5,
                        center_y: prev.center_y,
                        font_name: prev.font_name.clone(),
                        font_size: prev.font_size,
                        fill_color: prev.fill_color.clone(),
                        bold: prev.bold,
                        italic: prev.italic,
                        rotation: prev.rotation,
                        is_space: true,
                    });
                }
            }
            with_spaces.push(item);
        }

        // Run UAX #9 Unicode Bidi check for RTL scripts
        let line_raw_text: String = with_spaces.iter().map(|c| c.unicode.as_str()).collect();
        let bidi = BidiInfo::new(&line_raw_text, None);
        let is_rtl = bidi
            .paragraphs
            .first()
            .is_some_and(|p| p.level.is_rtl());

        // If visual left-to-right array contains pure RTL text, reverse into logical reading order
        let char_levels: Vec<bool> = with_spaces
            .iter()
            .map(|c| {
                is_rtl
                    || c.unicode.chars().any(|ch| {
                        ('\u{0590}'..='\u{05FF}').contains(&ch)
                            || ('\u{0600}'..='\u{06FF}').contains(&ch)
                            || ('\u{0750}'..='\u{077F}').contains(&ch)
                            || ('\u{FB50}'..='\u{FEFF}').contains(&ch)
                    })
            })
            .collect();

        if is_rtl && with_spaces.len() > 1 {
            // Confirm visual order is increasing X; flip into logical reading order (right -> left)
            if with_spaces.first().unwrap().center_x < with_spaces.last().unwrap().center_x {
                with_spaces.reverse();
            }
        }

        let start_char = final_chars.len();
        let mut l_min_x = f32::INFINITY;
        let mut l_min_y = f32::INFINITY;
        let mut l_max_x = f32::NEG_INFINITY;
        let mut l_max_y = f32::NEG_INFINITY;

        let mut words: Vec<WordNode> = Vec::new();
        let mut cur_word_text = String::new();
        let mut cur_word_start = start_char;
        let mut cur_word_bbox: Option<[f32; 4]> = None;

        for (ci, rc) in with_spaces.iter().enumerate() {
            let global_idx = final_chars.len();
            let is_c_rtl = char_levels.get(ci).copied().unwrap_or(is_rtl);

            l_min_x = l_min_x.min(rc.bbox[0]);
            l_min_y = l_min_y.min(rc.bbox[1]);
            l_max_x = l_max_x.max(rc.bbox[0] + rc.bbox[2]);
            l_max_y = l_max_y.max(rc.bbox[1] + rc.bbox[3]);

            final_chars.push(CharNode {
                index: global_idx,
                unicode: rc.unicode.clone(),
                bbox: rc.bbox,
                baseline: rc.baseline,
                font_name: strip_subset_prefix(&rc.font_name),
                font_size: (rc.font_size * 10.0).round() / 10.0,
                fill_color: rc.fill_color.clone(),
                bold: rc.bold,
                italic: rc.italic,
                rotation: rc.rotation,
                rtl: is_c_rtl,
            });

            if rc.is_space {
                if !cur_word_text.is_empty() {
                    if let Some(wb) = cur_word_bbox.take() {
                        words.push(WordNode {
                            id: format!("w-{}-{}-{}", page_num, li, words.len()),
                            text: std::mem::take(&mut cur_word_text),
                            bbox: wb,
                            start_char: cur_word_start,
                            end_char: global_idx - 1,
                        });
                    }
                }
                cur_word_start = global_idx + 1;
            } else {
                if cur_word_text.is_empty() {
                    cur_word_start = global_idx;
                }
                cur_word_text.push_str(&rc.unicode);
                cur_word_bbox = Some(match cur_word_bbox {
                    Some(b) => {
                        let nx = b[0].min(rc.bbox[0]);
                        let ny = b[1].min(rc.bbox[1]);
                        let nx2 = (b[0] + b[2]).max(rc.bbox[0] + rc.bbox[2]);
                        let ny2 = (b[1] + b[3]).max(rc.bbox[1] + rc.bbox[3]);
                        [nx, ny, nx2 - nx, ny2 - ny]
                    }
                    None => rc.bbox,
                });
            }
        }

        let end_char = final_chars.len() - 1;
        if !cur_word_text.is_empty() {
            if let Some(wb) = cur_word_bbox {
                words.push(WordNode {
                    id: format!("w-{}-{}-{}", page_num, li, words.len()),
                    text: cur_word_text,
                    bbox: wb,
                    start_char: cur_word_start,
                    end_char,
                });
            }
        }

        let dom = with_spaces
            .iter()
            .find(|c| !c.is_space)
            .unwrap_or(&with_spaces[0]);
        let line_text: String = with_spaces.iter().map(|c| c.unicode.as_str()).collect();
        let line_bbox = [
            l_min_x,
            l_min_y,
            (l_max_x - l_min_x).max(1.0),
            (l_max_y - l_min_y).max(1.0),
        ];

        finalized_lines.push((
            tl.column_idx,
            LineNode {
                id: format!("ln-{}-{}", page_num, finalized_lines.len()),
                text: line_text,
                bbox: line_bbox,
                baseline: tl.baseline,
                rotation: tl.rotation,
                font_name: strip_subset_prefix(&dom.font_name),
                font_size: (tl.font_size * 10.0).round() / 10.0,
                fill_color: dom.fill_color.clone(),
                bold: dom.bold,
                italic: dom.italic,
                is_rtl,
                start_char,
                end_char,
                words,
            },
        ));
    }

    // 3. Group lines into Blocks (paragraphs) based on vertical spacing, column, font size consistency
    let mut blocks: Vec<BlockNode> = Vec::new();
    let mut cur_group: Vec<(usize, LineNode)> = Vec::new();

    let flush_group = |grp: Vec<(usize, LineNode)>, out: &mut Vec<BlockNode>| {
        if grp.is_empty() {
            return;
        }
        let col_idx = grp[0].0;
        let lines: Vec<LineNode> = grp.into_iter().map(|(_, l)| l).collect();
        let start_char = lines.first().unwrap().start_char;
        let end_char = lines.last().unwrap().end_char;

        let mut bx0 = f32::INFINITY;
        let mut by0 = f32::INFINITY;
        let mut bx1 = f32::NEG_INFINITY;
        let mut by1 = f32::NEG_INFINITY;
        for l in &lines {
            bx0 = bx0.min(l.bbox[0]);
            by0 = by0.min(l.bbox[1]);
            bx1 = bx1.max(l.bbox[0] + l.bbox[2]);
            by1 = by1.max(l.bbox[1] + l.bbox[3]);
        }
        let bbox = [bx0, by0, (bx1 - bx0).max(4.0), (by1 - by0).max(4.0)];

        let avg_fs =
            lines.iter().map(|l| l.font_size).sum::<f32>() / (lines.len().max(1) as f32);
        let line_spacing = if lines.len() >= 2 {
            let mut sum_dy = 0.0;
            for w in lines.windows(2) {
                sum_dy += (w[1].baseline - w[0].baseline).abs();
            }
            ((sum_dy / (lines.len() - 1) as f32) / avg_fs.max(1.0)).clamp(0.95, 2.5)
        } else {
            1.2
        };

        let is_rtl = lines.iter().any(|l| l.is_rtl);
        let has_bullets = lines.iter().any(|l| {
            let t = l.text.trim_start();
            t.starts_with("• ") || t.starts_with("- ") || t.starts_with("* ")
        });

        let alignment = if is_rtl {
            "right".to_string()
        } else if lines.len() >= 2 {
            let bw = bbox[2];
            let left_aligned = lines.iter().all(|l| (l.bbox[0] - bx0).abs() <= 6.0);
            let right_aligned = lines
                .iter()
                .all(|l| ((l.bbox[0] + l.bbox[2]) - bx1).abs() <= 6.0);
            let centered = lines
                .iter()
                .all(|l| ((l.bbox[0] + l.bbox[2] * 0.5) - (bx0 + bw * 0.5)).abs() <= 8.0);
            if left_aligned && right_aligned && bw > 140.0 {
                "justify".to_string()
            } else if centered && !left_aligned {
                "center".to_string()
            } else if right_aligned && !left_aligned {
                "right".to_string()
            } else {
                "left".to_string()
            }
        } else {
            "left".to_string()
        };

        let first = &lines[0];
        let text = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let clean_fn = first.font_name.to_lowercase();
        let (has_embedded_font, is_subset_font) = embedded_font_status
            .get(&clean_fn)
            .copied()
            .unwrap_or((false, false));

        out.push(BlockNode {
            id: format!("blk-{}-{}", page_num, out.len()),
            column_index: col_idx,
            text,
            bbox,
            font_name: first.font_name.clone(),
            font_size: (avg_fs * 10.0).round() / 10.0,
            color: first.fill_color.clone(),
            bold: first.bold,
            italic: first.italic,
            alignment,
            line_spacing: (line_spacing * 100.0).round() / 100.0,
            has_bullets,
            is_rtl,
            rotation: first.rotation,
            has_embedded_font,
            is_subset_font,
            start_char,
            end_char,
            lines,
        });
    };

    for (col_idx, ln) in finalized_lines {
        let should_merge = if let Some((prev_col, prev)) = cur_group.last() {
            let v_gap = (ln.baseline - prev.baseline).abs();
            let max_v_gap = prev.font_size.max(ln.font_size) * 1.95;
            let fs_ratio =
                ln.font_size.max(1.0) / prev.font_size.max(1.0);
            let same_col = *prev_col == col_idx;
            let same_rot = (ln.rotation - prev.rotation).abs() < 10.0;
            let bullet_break = ln.text.trim_start().starts_with("• ");
            let horiz_overlap = ln.bbox[0] < prev.bbox[0] + prev.bbox[2] + 36.0
                && ln.bbox[0] + ln.bbox[2] + 36.0 > prev.bbox[0];
            same_col
                && same_rot
                && horiz_overlap
                && !bullet_break
                && v_gap <= max_v_gap
                && (0.78..=1.28).contains(&fs_ratio)
        } else {
            false
        };

        if !should_merge && !cur_group.is_empty() {
            let taken = std::mem::take(&mut cur_group);
            flush_group(taken, &mut blocks);
        }
        cur_group.push((col_idx, ln));
    }
    if !cur_group.is_empty() {
        flush_group(cur_group, &mut blocks);
    }

    (col_count, final_chars, blocks)
}
