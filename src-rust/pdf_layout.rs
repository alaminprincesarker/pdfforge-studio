use crate::font_shaper::{
    extract_embedded_pdf_font, looks_like_subset_font, resolve_dict, strip_subset_prefix,
};
use crate::pdf_objects::{
    parse_page_objects, PageObjectsResult, PageTransform,
};
use lopdf::Document as LoDocument;
use memmap2::Mmap;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use unicode_bidi::BidiInfo;

static BG_PNG_ENCODE_THREADS: AtomicUsize = AtomicUsize::new(0);
const MAX_BG_PNG_ENCODE_THREADS: usize = 2;

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
static PDFIUM_INIT_LOCK: Mutex<()> = Mutex::new(());

pub fn locate_pdfium_library_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let lib_name = "pdfium.dll";
    #[cfg(target_os = "macos")]
    let lib_name = "libpdfium.dylib";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let lib_name = "libpdfium.so";

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(lib_name));
            #[cfg(target_os = "macos")]
            if let Some(contents_dir) = dir.parent() {
                candidates.push(contents_dir.join("Frameworks").join(lib_name));
                candidates.push(contents_dir.join("Resources").join(lib_name));
            }
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("pdfforge").join(lib_name));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(lib_name));
        candidates.push(cwd.join("src-rust").join(lib_name));
        candidates.push(cwd.join("release").join(lib_name));
    }
    candidates.into_iter().find(|p| p.exists())
}

pub fn get_pdfium() -> Result<Arc<Mutex<Pdfium>>, String> {
    if let Some(inst) = PDFIUM_INSTANCE.get() {
        return Ok(inst.clone());
    }
    let _init_guard = PDFIUM_INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

#[derive(Clone)]
pub enum PdfBacking {
    Mmap(Arc<Mmap>),
    Bytes(Arc<Vec<u8>>),
}

impl Default for PdfBacking {
    fn default() -> Self {
        Self::Bytes(Arc::new(Vec::new()))
    }
}

impl std::ops::Deref for PdfBacking {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            PdfBacking::Mmap(m) => &m[..],
            PdfBacking::Bytes(b) => &b[..],
        }
    }
}

impl PdfBacking {
    pub fn len(&self) -> usize {
        (**self).len()
    }
    pub fn is_mmap(&self) -> bool {
        matches!(self, PdfBacking::Mmap(_))
    }
}

fn open_mmap_or_bytes(file_path: &str) -> Result<PdfBacking, String> {
    let f = std::fs::File::open(file_path)
        .map_err(|e| format!("Failed opening PDF '{}': {}", file_path, e))?;
    let meta = f
        .metadata()
        .map_err(|e| format!("Failed reading PDF metadata '{}': {}", file_path, e))?;
    let file_len = meta.len();
    if file_len == 0 {
        return Err("PDF file is empty (0 bytes)".to_string());
    }
    if let Ok(mmap) = unsafe { memmap2::MmapOptions::new().map(&f) } {
        return Ok(PdfBacking::Mmap(Arc::new(mmap)));
    }
    if file_len > 256 * 1024 * 1024 {
        return Err(format!(
            "Failed memory-mapping large PDF ({:.1} MB); refusing full in-RAM fallback to avoid OOM",
            file_len as f64 / (1024.0 * 1024.0)
        ));
    }
    let disk_bytes = std::fs::read(file_path)
        .map_err(|e| format!("Failed reading PDF '{}': {}", file_path, e))?;
    Ok(PdfBacking::Bytes(Arc::new(disk_bytes)))
}

#[derive(Default)]
struct WorkingDocument {
    #[allow(dead_code)]
    canonical_key: String,
    disk_path: Option<String>,
    orig_disk_bytes: Option<Vec<u8>>,
    backing: PdfBacking,
    is_dirty: bool,
    revision: u64,
    undo_stack: Vec<PdfBacking>,
    redo_stack: Vec<PdfBacking>,
    layout_cache: HashMap<usize, Arc<PageLayout>>,
    layout_order: VecDeque<usize>,
    objects_cache: HashMap<usize, Arc<PageObjectsResult>>,
    objects_order: VecDeque<usize>,
}

const MAX_PAGE_CACHE_PER_DOC: usize = 16;
const LARGE_DOC_BYTES_THRESHOLD: usize = 20 * 1024 * 1024;

#[derive(Clone, Hash, Eq, PartialEq)]
pub struct RustRenderCacheKey {
    pub doc_key: String,
    pub revision: u64,
    pub page: usize,
    pub width: u32,
    pub height: u32,
    pub rotation: i32,
    pub tile_x: i32,
    pub tile_y: i32,
    pub tile_w: u32,
    pub tile_h: u32,
}

#[derive(Default)]
struct RustRenderCacheInner {
    entries: HashMap<RustRenderCacheKey, Arc<Vec<u8>>>,
    lru_order: VecDeque<RustRenderCacheKey>,
    total_bytes: usize,
    budget_bytes: usize,
}

#[derive(Clone)]
pub struct RustPageRenderCache {
    inner: Arc<Mutex<RustRenderCacheInner>>,
}

impl Default for RustPageRenderCache {
    fn default() -> Self {
        Self::new(300 * 1024 * 1024)
    }
}

impl RustPageRenderCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RustRenderCacheInner {
                entries: HashMap::new(),
                lru_order: VecDeque::new(),
                total_bytes: 0,
                budget_bytes: budget_bytes.max(64 * 1024 * 1024),
            })),
        }
    }

    pub fn set_budget_bytes(&self, budget_bytes: usize) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.budget_bytes = budget_bytes.clamp(64 * 1024 * 1024, 2048 * 1024 * 1024);
        Self::evict_to_budget(&mut g);
    }

    pub fn stats(&self) -> (usize, usize) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (g.entries.len(), g.total_bytes)
    }

    fn evict_to_budget(g: &mut RustRenderCacheInner) {
        while g.total_bytes > g.budget_bytes {
            if let Some(old_k) = g.lru_order.pop_front() {
                if let Some(v) = g.entries.remove(&old_k) {
                    g.total_bytes = g.total_bytes.saturating_sub(v.len());
                }
            } else {
                g.total_bytes = 0;
                break;
            }
        }
    }

    pub fn get(&self, key: &RustRenderCacheKey) -> Option<Arc<Vec<u8>>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = g.entries.get(key).cloned() {
            g.lru_order.retain(|k| k != key);
            g.lru_order.push_back(key.clone());
            return Some(v);
        }
        None
    }

    pub fn insert(&self, key: RustRenderCacheKey, data: Arc<Vec<u8>>) {
        let sz = data.len();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if sz > g.budget_bytes {
            return;
        }
        if let Some(prev) = g.entries.remove(&key) {
            g.total_bytes = g.total_bytes.saturating_sub(prev.len());
            g.lru_order.retain(|k| k != &key);
        }
        g.total_bytes += sz;
        g.entries.insert(key.clone(), data);
        g.lru_order.push_back(key);
        Self::evict_to_budget(&mut g);
    }

    pub fn evict_doc(&self, doc_key: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let keys: Vec<RustRenderCacheKey> = g
            .entries
            .keys()
            .filter(|k| k.doc_key == doc_key)
            .cloned()
            .collect();
        for k in keys {
            if let Some(v) = g.entries.remove(&k) {
                g.total_bytes = g.total_bytes.saturating_sub(v.len());
            }
            g.lru_order.retain(|x| x != &k);
        }
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.entries.clear();
        g.lru_order.clear();
        g.total_bytes = 0;
    }
}

fn png_chunk(chunk_type: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(chunk_type);
    hasher.update(data);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
}

pub fn encode_bgra_to_png(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let w = width as usize;
    let h = height as usize;
    if bgra.len() < w * h * 4 {
        return Err("BGRA buffer too small for PNG encoding".to_string());
    }
    let mut out = Vec::with_capacity((w * h) / 2 + 128);
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    png_chunk(b"IHDR", &ihdr, &mut out);

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut row_buf = vec![0u8; 1 + w * 4];
    for y in 0..h {
        row_buf[0] = 0; // filter None
        let src_row = &bgra[(y * w * 4)..((y + 1) * w * 4)];
        for x in 0..w {
            let si = x * 4;
            let di = 1 + x * 4;
            row_buf[di] = src_row[si + 2];     // R
            row_buf[di + 1] = src_row[si + 1]; // G
            row_buf[di + 2] = src_row[si];     // B
            row_buf[di + 3] = src_row[si + 3]; // A
        }
        encoder
            .write_all(&row_buf)
            .map_err(|e| format!("PNG deflate error: {}", e))?;
    }
    let idat = encoder
        .finish()
        .map_err(|e| format!("PNG deflate finish error: {}", e))?;
    png_chunk(b"IDAT", &idat, &mut out);
    png_chunk(b"IEND", &[], &mut out);
    Ok(out)
}

/// Zero-compression, zero-swizzle top-down 32-bit BGRA BMP encoder (~0.1ms for 1MP canvas).
/// PDFium outputs native BGRA; negative biHeight tells Chromium's native BMP decoder top-down order.
pub fn encode_bgra_to_bmp_fast(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let w = width as usize;
    let h = height as usize;
    let img_bytes = w.checked_mul(h).and_then(|p| p.checked_mul(4)).ok_or("BMP size overflow")?;
    if bgra.len() < img_bytes {
        return Err("BGRA buffer too small for BMP encoding".to_string());
    }
    let file_size = 54u32.saturating_add(img_bytes as u32);
    let mut out = Vec::with_capacity(54 + img_bytes);
    // BITMAPFILEHEADER (14 bytes)
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER (40 bytes)
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(-(height as i32)).to_le_bytes()); // negative = top-down
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // 32-bit BGRA
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(img_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&bgra[..img_bytes]);
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FastFileProbe {
    pub num_pages: usize,
    pub page1_width: f32,
    pub page1_height: f32,
    pub is_linearized: bool,
    pub file_hash: String,
    pub prefer_pdfium: bool,
    pub vector_path_count: usize,
    pub image_object_count: usize,
}

#[derive(Clone, Default)]
pub struct PdfWorkspaceEngine {
    docs: Arc<Mutex<HashMap<String, WorkingDocument>>>,
    render_cache: RustPageRenderCache,
}

impl PdfWorkspaceEngine {
    pub fn new() -> Self {
        Self {
            docs: Arc::new(Mutex::new(HashMap::new())),
            render_cache: RustPageRenderCache::default(),
        }
    }

    pub fn normalize_key(file_path: &str) -> String {
        file_path
            .trim()
            .strip_prefix(r"\\?\")
            .unwrap_or(file_path.trim())
            .to_lowercase()
    }

    pub fn set_render_budget_mb(&self, mb: u32) {
        self.render_cache
            .set_budget_bytes((mb.clamp(64, 2048) as usize) * 1024 * 1024);
    }

    pub fn render_cache_stats(&self) -> (usize, usize) {
        self.render_cache.stats()
    }

    /// Loads or returns a zero-copy `PdfBacking` (`Mmap` if untouched on disk, `Bytes` if edited).
    pub fn load_backing(&self, file_path: &str) -> Result<(PdfBacking, u64, bool), String> {
        let key = Self::normalize_key(file_path);
        {
            let guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(doc) = guard.get(&key) {
                return Ok((doc.backing.clone(), doc.revision, doc.is_dirty));
            }
        }
        let backing = open_mmap_or_bytes(file_path)?;
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(key).or_insert_with(|| WorkingDocument {
            canonical_key: file_path.to_string(),
            disk_path: Some(file_path.to_string()),
            orig_disk_bytes: None,
            backing,
            is_dirty: false,
            revision: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            layout_cache: HashMap::new(),
            layout_order: VecDeque::new(),
            objects_cache: HashMap::new(),
            objects_order: VecDeque::new(),
        });
        Ok((entry.backing.clone(), entry.revision, entry.is_dirty))
    }

    /// Reads only `[start..=end_incl]` from the backing (zero whole-file copy).
    #[allow(dead_code)]
    pub fn read_range(
        &self,
        file_path: &str,
        start: u64,
        end_incl: u64,
    ) -> Result<(Vec<u8>, u64, u64, bool), String> {
        let (backing, rev, dirty) = self.load_backing(file_path)?;
        let total = backing.len() as u64;
        if total == 0 || start >= total {
            return Err("Requested byte range out of bounds".to_string());
        }
        let clamped_end = end_incl.min(total - 1);
        let slice = backing
            .get((start as usize)..=(clamped_end as usize))
            .ok_or_else(|| "Requested byte range out of bounds".to_string())?;
        Ok((slice.to_vec(), total, rev, dirty))
    }

    pub fn close_document(&self, file_path: &str) {
        let key = Self::normalize_key(file_path);
        self.docs.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        self.render_cache.evict_doc(&key);
    }

    pub fn suspend_document(&self, file_path: &str) {
        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let should_remove = if let Some(doc) = guard.get_mut(&key) {
            doc.layout_cache.clear();
            doc.layout_order.clear();
            doc.objects_cache.clear();
            doc.objects_order.clear();
            !doc.is_dirty
        } else {
            false
        };
        if should_remove {
            guard.remove(&key);
        }
    }

    /// Drops active Mmap handle pointing to `target_path` so Windows save-in-place never hits file lock.
    pub fn unmap_for_save(&self, target_path: &str) {
        let target_key = Self::normalize_key(target_path);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(doc) = guard.get_mut(&target_key) {
            if doc.backing.is_mmap() {
                if let Ok(vec_bytes) = std::fs::read(target_path) {
                    doc.backing = PdfBacking::Bytes(Arc::new(vec_bytes));
                }
            }
        }
        // Also ensure no other entry holds a dirty-free mmap on the exact path
        guard.retain(|k, d| !(k == &target_key && !d.is_dirty && d.backing.is_mmap()));
    }

    #[allow(dead_code)]
    pub fn render_page_png_cached(
        &self,
        file_path: &str,
        page_one: usize,
        full_width: u32,
        full_height: u32,
        rotation_deg: i32,
        tile: Option<(i32, i32, u32, u32)>,
        quality_high: bool,
    ) -> Result<Arc<Vec<u8>>, String> {
        let (backing, revision, is_dirty) = self.load_backing(file_path)?;
        let key_str = Self::normalize_key(file_path);

        // Enforce max 16 MP cap on rendered canvas / tile
        let max_pixels: f64 = 16_000_000.0;
        let full_px = (full_width as f64) * (full_height as f64);
        let (adj_w, adj_h, adj_tile) = if tile.is_none() && full_px > max_pixels {
            let factor = (max_pixels / full_px).sqrt();
            (
                ((full_width as f64) * factor).round().max(1.0) as u32,
                ((full_height as f64) * factor).round().max(1.0) as u32,
                None,
            )
        } else if let Some((tx, ty, tw, th)) = tile {
            let tpx = (tw as f64) * (th as f64);
            if tpx > max_pixels {
                let factor = (max_pixels / tpx).sqrt();
                (
                    ((full_width as f64) * factor).round().max(1.0) as u32,
                    ((full_height as f64) * factor).round().max(1.0) as u32,
                    Some((
                        ((tx as f64) * factor).round() as i32,
                        ((ty as f64) * factor).round() as i32,
                        ((tw as f64) * factor).round().max(1.0) as u32,
                        ((th as f64) * factor).round().max(1.0) as u32,
                    )),
                )
            } else {
                (full_width.max(1), full_height.max(1), Some((tx, ty, tw.max(1), th.max(1))))
            }
        } else {
            (full_width.max(1), full_height.max(1), None)
        };

        let (tx, ty, tw, th) = adj_tile.unwrap_or((0, 0, adj_w, adj_h));
        let norm_rot = ((rotation_deg % 360) + 360) % 360;

        let cache_key = RustRenderCacheKey {
            doc_key: key_str,
            revision,
            page: page_one.max(1),
            width: adj_w,
            height: adj_h,
            rotation: norm_rot,
            tile_x: tx,
            tile_y: ty,
            tile_w: tw,
            tile_h: th,
        };

        if let Some(hit) = self.render_cache.get(&cache_key) {
            return Ok(hit);
        }

        let png_bytes = {
            let pdfium = get_pdfium()?;
            let pdfium_guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
            let doc = if !is_dirty && std::path::Path::new(file_path).is_file() {
                pdfium_guard
                    .load_pdf_from_file(file_path, None)
                    .or_else(|_| pdfium_guard.load_pdf_from_byte_slice(&backing, None))
                    .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?
            } else {
                pdfium_guard
                    .load_pdf_from_byte_slice(&backing, None)
                    .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?
            };
            let page_idx = (page_one.max(1) - 1) as i32;
            let pg = doc
                .pages()
                .get(page_idx)
                .map_err(|e| format!("PDFium failed opening page {}: {:?}", page_one, e))?;

            let rot_enum = match norm_rot {
                90 => PdfPageRenderRotation::Degrees90,
                180 => PdfPageRenderRotation::Degrees180,
                270 => PdfPageRenderRotation::Degrees270,
                _ => PdfPageRenderRotation::None,
            };

            let mut cfg = PdfRenderConfig::new()
                .set_target_size(adj_w as i32, adj_h as i32)
                .rotate(rot_enum, true)
                .set_clear_color(PdfColor::WHITE)
                .limit_render_image_cache_size(true)
                .set_text_smoothing(true)
                .set_image_smoothing(quality_high)
                .set_path_smoothing(true)
                .render_annotations(true)
                .render_form_data(adj_tile.is_none());

            if adj_tile.is_some() {
                cfg = cfg.set_origin(-tx, -ty);
            }

            let mut buf = vec![255u8; (tw as usize) * (th as usize) * 4];
            let mut bitmap = PdfBitmap::from_bytes(
                tw as i32,
                th as i32,
                PdfBitmapFormat::BGRA,
                &mut buf,
            )
            .map_err(|e| format!("PdfBitmap allocation failed: {:?}", e))?;
            pg.render_into_bitmap_with_config(&mut bitmap, &cfg)
                .map_err(|e| format!("PDFium render error: {:?}", e))?;
            drop(bitmap);
            encode_bgra_to_png(tw, th, &buf)?
        };

        let arc_png = Arc::new(png_bytes);
        self.render_cache.insert(cache_key, arc_png.clone());
        Ok(arc_png)
    }

    /// Ultra-fast BMP/disk-cached page & tile renderer for real-time 60fps viewing.
    /// Checks RAM LRU -> %LOCALAPPDATA%\PDFForge\cache -> PDFium native file stream -> zero-swizzle 32-bit BMP.
    /// Asynchronously persists compressed PNG to disk cache for instant file reopening.
    pub fn render_page_fast_cached(
        &self,
        file_path: &str,
        page_one: usize,
        full_width: u32,
        full_height: u32,
        rotation_deg: i32,
        tile: Option<(i32, i32, u32, u32)>,
        quality_high: bool,
        is_thumb: bool,
    ) -> Result<(Arc<Vec<u8>>, &'static str), String> {
        let (backing, revision, is_dirty) = self.load_backing(file_path)?;
        let key_str = Self::normalize_key(file_path);

        let max_pixels: f64 = 16_000_000.0;
        let full_px = (full_width as f64) * (full_height as f64);
        let (adj_w, adj_h, adj_tile) = if tile.is_none() && full_px > max_pixels {
            let factor = (max_pixels / full_px).sqrt();
            (
                ((full_width as f64) * factor).round().max(1.0) as u32,
                ((full_height as f64) * factor).round().max(1.0) as u32,
                None,
            )
        } else if let Some((tx, ty, tw, th)) = tile {
            let tpx = (tw as f64) * (th as f64);
            if tpx > max_pixels {
                let factor = (max_pixels / tpx).sqrt();
                (
                    ((full_width as f64) * factor).round().max(1.0) as u32,
                    ((full_height as f64) * factor).round().max(1.0) as u32,
                    Some((
                        ((tx as f64) * factor).round() as i32,
                        ((ty as f64) * factor).round() as i32,
                        ((tw as f64) * factor).round().max(1.0) as u32,
                        ((th as f64) * factor).round().max(1.0) as u32,
                    )),
                )
            } else {
                (full_width.max(1), full_height.max(1), Some((tx, ty, tw.max(1), th.max(1))))
            }
        } else {
            (full_width.max(1), full_height.max(1), None)
        };

        let (tx, ty, tw, th) = adj_tile.unwrap_or((0, 0, adj_w, adj_h));
        let norm_rot = ((rotation_deg % 360) + 360) % 360;
        // Zoom bucket rounded to nearest 16px width for clean disk cache reuse
        let zoom_bucket = ((adj_w + 8) / 16) * 16;

        let cache_key = RustRenderCacheKey {
            doc_key: key_str,
            revision,
            page: page_one.max(1),
            width: adj_w,
            height: adj_h,
            rotation: norm_rot,
            tile_x: tx,
            tile_y: ty,
            tile_w: tw,
            tile_h: th,
        };

        if let Some(hit) = self.render_cache.get(&cache_key) {
            let mime = if hit.starts_with(b"BM") { "image/bmp" } else { "image/png" };
            return Ok((hit, mime));
        }

        // Check persistent disk cache for full pages & thumbnails when file is clean on disk
        let file_hash = if !is_dirty && adj_tile.is_none() {
            let fh = crate::disk_cache::compute_file_hash(file_path);
            if let Some(disk_png) =
                crate::disk_cache::read_cached_render(&fh, revision, page_one.max(1), zoom_bucket, norm_rot, is_thumb)
            {
                let arc = Arc::new(disk_png);
                self.render_cache.insert(cache_key, arc.clone());
                return Ok((arc, "image/png"));
            }
            Some(fh)
        } else {
            None
        };

        let bgra = {
            let pdfium = get_pdfium()?;
            let pdfium_guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
            let doc = if !is_dirty && std::path::Path::new(file_path).is_file() {
                pdfium_guard
                    .load_pdf_from_file(file_path, None)
                    .or_else(|_| pdfium_guard.load_pdf_from_byte_slice(&backing, None))
                    .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?
            } else {
                pdfium_guard
                    .load_pdf_from_byte_slice(&backing, None)
                    .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?
            };
            let page_idx = (page_one.max(1) - 1) as i32;
            let pg = doc
                .pages()
                .get(page_idx)
                .map_err(|e| format!("PDFium failed opening page {}: {:?}", page_one, e))?;

            let rot_enum = match norm_rot {
                90 => PdfPageRenderRotation::Degrees90,
                180 => PdfPageRenderRotation::Degrees180,
                270 => PdfPageRenderRotation::Degrees270,
                _ => PdfPageRenderRotation::None,
            };

            let mut cfg = PdfRenderConfig::new()
                .set_target_size(adj_w as i32, adj_h as i32)
                .rotate(rot_enum, true)
                .set_clear_color(PdfColor::WHITE)
                .limit_render_image_cache_size(true)
                .set_text_smoothing(true)
                .set_image_smoothing(quality_high)
                .set_path_smoothing(true)
                .render_annotations(true)
                .render_form_data(adj_tile.is_none());

            if adj_tile.is_some() {
                cfg = cfg.set_origin(-tx, -ty);
            }

            let buf_len = (tw as usize)
                .checked_mul(th as usize)
                .and_then(|p| p.checked_mul(4))
                .ok_or_else(|| "Render bitmap dimensions overflow".to_string())?;
            let mut buf = vec![255u8; buf_len];
            let mut bitmap = PdfBitmap::from_bytes(
                tw as i32,
                th as i32,
                PdfBitmapFormat::BGRA,
                &mut buf,
            )
            .map_err(|e| format!("PdfBitmap allocation failed: {:?}", e))?;
            pg.render_into_bitmap_with_config(&mut bitmap, &cfg)
                .map_err(|e| format!("PDFium render error: {:?}", e))?;
            drop(bitmap);
            buf
        };

        let bmp_bytes = encode_bgra_to_bmp_fast(tw, th, &bgra)?;
        let arc_bmp = Arc::new(bmp_bytes);
        self.render_cache.insert(cache_key, arc_bmp.clone());

        // Persist compressed PNG to %LOCALAPPDATA%\PDFForge\cache in background (bounded concurrency so
        // fast scrolling across huge PDFs never spawns unbounded PNG encoding threads / RAM spikes).
        if let Some(fh) = file_hash {
            if BG_PNG_ENCODE_THREADS.fetch_add(1, Ordering::AcqRel) < MAX_BG_PNG_ENCODE_THREADS {
                let p_one = page_one.max(1);
                std::thread::spawn(move || {
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if let Ok(png) = encode_bgra_to_png(tw, th, &bgra) {
                            crate::disk_cache::write_cached_render_async(
                                fh,
                                revision,
                                p_one,
                                zoom_bucket,
                                norm_rot,
                                is_thumb,
                                png,
                            );
                        }
                    }));
                    let _ = res;
                    BG_PNG_ENCODE_THREADS.fetch_sub(1, Ordering::AcqRel);
                });
            } else {
                BG_PNG_ENCODE_THREADS.fetch_sub(1, Ordering::AcqRel);
            }
        }

        Ok((arc_bmp, "image/bmp"))
    }

    /// Probes PDF metadata, page 1 dimensions, linearization, and complexity in <5ms via native PDFium file IO.
    pub fn probe_file_fast(&self, file_path: &str, head_bytes: &[u8], file_size: u64) -> Option<FastFileProbe> {
        let is_linearized = head_bytes.windows(11).any(|w| w == b"/Linearized");
        let file_hash = crate::disk_cache::compute_file_hash(file_path);
        let pdfium = get_pdfium().ok()?;
        let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
        let doc = guard.load_pdf_from_file(file_path, None).ok()?;
        let num_pages = doc.pages().len() as usize;
        if num_pages == 0 {
            return None;
        }
        let pg1 = doc.pages().get(0).ok()?;
        let page1_width = pg1.width().value.abs().max(1.0);
        let page1_height = pg1.height().value.abs().max(1.0);
        let mut vector_paths = 0usize;
        let mut image_objs = 0usize;
        // Sample first 400 objects on page 1 to spot vector-heavy CAD/maps or multi-image sheets instantly
        for (idx, obj) in pg1.objects().iter().enumerate() {
            match obj.object_type() {
                PdfPageObjectType::Path => vector_paths += 1,
                PdfPageObjectType::Image => image_objs += 1,
                _ => {}
            }
            if idx >= 400 {
                break;
            }
        }
        let prefer_pdfium = file_size >= 8 * 1024 * 1024
            || num_pages >= 150
            || vector_paths >= 120
            || image_objs >= 8;
        Some(FastFileProbe {
            num_pages,
            page1_width,
            page1_height,
            is_linearized,
            file_hash,
            prefer_pdfium,
            vector_path_count: vector_paths,
            image_object_count: image_objs,
        })
    }

    /// Extracts plain text across all pages in one native PDFium session for fast disk search indexing.
    pub fn extract_all_pages_text_fast(&self, file_path: &str) -> Result<Vec<String>, String> {
        let (backing, _, is_dirty) = self.load_backing(file_path)?;
        let pdfium = get_pdfium()?;
        let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
        let doc = if !is_dirty && std::path::Path::new(file_path).is_file() {
            guard
                .load_pdf_from_file(file_path, None)
                .or_else(|_| guard.load_pdf_from_byte_slice(&backing, None))
                .map_err(|e| format!("PDFium open failed: {:?}", e))?
        } else {
            guard
                .load_pdf_from_byte_slice(&backing, None)
                .map_err(|e| format!("PDFium open failed: {:?}", e))?
        };
        let n = doc.pages().len() as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let txt = doc
                .pages()
                .get(i as i32)
                .ok()
                .and_then(|pg| {
                    pg.text().ok().map(|tp| {
                        tp.chars()
                            .iter()
                            .filter_map(|c| c.unicode_char())
                            .collect::<String>()
                    })
                })
                .unwrap_or_default();
            out.push(txt);
        }
        Ok(out)
    }

    pub fn load_or_sync_from_disk(&self, file_path: &str) -> Result<Vec<u8>, String> {
        let (backing, _, _) = self.load_backing(file_path)?;
        Ok(backing.to_vec())
    }

    #[allow(dead_code)]
    pub fn reload_from_disk(&self, file_path: &str) -> Result<(), String> {
        let key = Self::normalize_key(file_path);
        let backing = open_mmap_or_bytes(file_path)?;
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(
            key,
            WorkingDocument {
                canonical_key: file_path.to_string(),
                disk_path: Some(file_path.to_string()),
                orig_disk_bytes: None,
                backing,
                is_dirty: false,
                revision: 1,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
                layout_cache: HashMap::new(),
                layout_order: VecDeque::new(),
                objects_cache: HashMap::new(),
                objects_order: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub fn register_bytes_for_path(&self, file_path: &str, bytes: Vec<u8>) {
        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let arc_bytes = Arc::new(bytes.clone());
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(
            key,
            WorkingDocument {
                canonical_key: file_path.to_string(),
                disk_path: Some(file_path.to_string()),
                orig_disk_bytes: Some(bytes),
                backing: PdfBacking::Bytes(arc_bytes),
                is_dirty: false,
                revision: 1,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
                layout_cache: HashMap::new(),
                layout_order: VecDeque::new(),
                objects_cache: HashMap::new(),
                objects_order: VecDeque::new(),
            },
        );
    }

    fn max_undo_steps_for_len(len: usize) -> usize {
        if len > 40 * 1024 * 1024 {
            3
        } else if len > 10 * 1024 * 1024 {
            10
        } else {
            30
        }
    }

    /// Document-wide mutation (rotate pages, insert/delete/reorder pages, metadata, outlines, labels):
    /// clears all page layout & object caches and pushes undo history.
    pub fn push_doc_mutation<F>(
        &self,
        file_path: &str,
        mutator: F,
    ) -> Result<(u64, bool, bool, Option<String>), String>
    where
        F: FnOnce(&mut LoDocument) -> Result<Option<String>, String>,
    {
        let (current_backing, _, _) = self.load_backing(file_path)?;
        let mut lo_doc = LoDocument::load_mem(&current_backing)
            .map_err(|e| format!("Failed parsing PDF in lopdf: {}", e))?;
        let notice = mutator(&mut lo_doc)?;
        let mut new_bytes = Vec::new();
        lo_doc
            .save_to(&mut new_bytes)
            .map_err(|e| format!("Failed serializing mutated PDF: {}", e))?;

        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key).ok_or("Missing working document")?;
        let max_undo = Self::max_undo_steps_for_len(new_bytes.len());
        entry.undo_stack.push(entry.backing.clone());
        while entry.undo_stack.len() > max_undo {
            entry.undo_stack.remove(0);
        }
        entry.redo_stack.clear();
        entry.backing = PdfBacking::Bytes(Arc::new(new_bytes));
        entry.is_dirty = true;
        entry.revision += 1;
        entry.layout_cache.clear();
        entry.layout_order.clear();
        entry.objects_cache.clear();
        entry.objects_order.clear();
        Ok((
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
            notice,
        ))
    }

    pub fn replace_working_bytes_with_undo(
        &self,
        file_path: &str,
        new_bytes: Vec<u8>,
    ) -> Result<(u64, bool, bool), String> {
        let _ = self.load_backing(file_path)?;
        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key).ok_or("Missing working document")?;
        let max_undo = Self::max_undo_steps_for_len(new_bytes.len());
        entry.undo_stack.push(entry.backing.clone());
        while entry.undo_stack.len() > max_undo {
            entry.undo_stack.remove(0);
        }
        entry.redo_stack.clear();
        entry.backing = PdfBacking::Bytes(Arc::new(new_bytes));
        entry.is_dirty = true;
        entry.revision += 1;
        entry.layout_cache.clear();
        entry.layout_order.clear();
        entry.objects_cache.clear();
        entry.objects_order.clear();
        Ok((
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
        ))
    }

    /// Writes the authoritative working document atomically to `target_path`,
    /// validates it with `pdfium-render` before replacing the target, and resets clean baseline.
    pub fn save_verified_to_disk(
        &self,
        file_path: &str,
        target_path: &str,
        incremental: bool,
        compact: bool,
    ) -> Result<(u64, bool), String> {
        let (backing, orig_disk_bytes, disk_path_opt) = {
            let _ = self.load_backing(file_path)?;
            let key = Self::normalize_key(file_path);
            let guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
            let entry = guard.get(&key).ok_or("Missing working document")?;
            (
                entry.backing.clone(),
                entry.orig_disk_bytes.clone(),
                entry.disk_path.clone(),
            )
        };
        let orig_bytes_owned = match orig_disk_bytes {
            Some(b) => Some(b),
            None => disk_path_opt.and_then(|p| std::fs::read(p).ok()),
        };

        // Release any mmap on target_path before atomic replace on Windows
        let dst_key = Self::normalize_key(target_path);
        let src_key = Self::normalize_key(file_path);
        if dst_key == src_key {
            let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = guard.get_mut(&src_key) {
                if entry.backing.is_mmap() {
                    entry.backing = PdfBacking::Bytes(Arc::new(backing.to_vec()));
                }
            }
        } else {
            self.unmap_for_save(target_path);
        }

        let (saved_bytes, used_incremental) = crate::pdf_annots::save_pdf_verified(
            &backing,
            orig_bytes_owned.as_deref(),
            target_path,
            incremental,
            compact,
        )?;
        drop(backing);
        drop(orig_bytes_owned);

        self.render_cache.evict_doc(&dst_key);
        let next_backing = open_mmap_or_bytes(target_path)
            .unwrap_or_else(|_| PdfBacking::Bytes(Arc::new(saved_bytes)));

        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let next_rev = if dst_key == src_key {
            if let Some(entry) = guard.get_mut(&src_key) {
                entry.disk_path = Some(target_path.to_string());
                entry.orig_disk_bytes = None;
                entry.backing = next_backing;
                entry.is_dirty = false;
                entry.revision += 1;
                entry.layout_cache.clear();
                entry.layout_order.clear();
                entry.objects_cache.clear();
                entry.objects_order.clear();
                entry.revision
            } else {
                1
            }
        } else {
            guard.insert(
                dst_key,
                WorkingDocument {
                    canonical_key: target_path.to_string(),
                    disk_path: Some(target_path.to_string()),
                    orig_disk_bytes: None,
                    backing: next_backing,
                    is_dirty: false,
                    revision: 1,
                    undo_stack: Vec::new(),
                    redo_stack: Vec::new(),
                    layout_cache: HashMap::new(),
                    layout_order: VecDeque::new(),
                    objects_cache: HashMap::new(),
                    objects_order: VecDeque::new(),
                },
            );
            1
        };
        Ok((next_rev, used_incremental))
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
        let (current_backing, _, _) = self.load_backing(file_path)?;
        let mut lo_doc = LoDocument::load_mem(&current_backing)
            .map_err(|e| format!("Failed parsing PDF in lopdf: {}", e))?;
        let notice = mutator(&mut lo_doc)?;
        let mut new_bytes = Vec::new();
        lo_doc
            .save_to(&mut new_bytes)
            .map_err(|e| format!("Failed serializing mutated PDF: {}", e))?;

        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key).ok_or("Missing working document")?;
        let max_undo = Self::max_undo_steps_for_len(new_bytes.len());
        entry.undo_stack.push(entry.backing.clone());
        while entry.undo_stack.len() > max_undo {
            entry.undo_stack.remove(0);
        }
        entry.redo_stack.clear();
        entry.backing = PdfBacking::Bytes(Arc::new(new_bytes));
        entry.is_dirty = true;
        entry.revision += 1;
        entry.layout_cache.remove(&page);
        entry.layout_order.retain(|p| *p != page);
        entry.objects_cache.remove(&page);
        entry.objects_order.retain(|p| *p != page);
        Ok((
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
            notice,
        ))
    }

    pub fn undo(&self, file_path: &str) -> Result<(Vec<u8>, u64, bool, bool), String> {
        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key).ok_or("Document not loaded")?;
        let prev = entry.undo_stack.pop().ok_or("Nothing to undo")?;
        entry.redo_stack.push(entry.backing.clone());
        entry.backing = prev;
        entry.is_dirty = !entry.undo_stack.is_empty();
        entry.revision += 1;
        entry.layout_cache.clear();
        entry.layout_order.clear();
        entry.objects_cache.clear();
        entry.objects_order.clear();
        Ok((
            entry.backing.to_vec(),
            entry.revision,
            !entry.undo_stack.is_empty(),
            !entry.redo_stack.is_empty(),
        ))
    }

    pub fn redo(&self, file_path: &str) -> Result<(Vec<u8>, u64, bool, bool), String> {
        let key = Self::normalize_key(file_path);
        self.render_cache.evict_doc(&key);
        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get_mut(&key).ok_or("Document not loaded")?;
        let next = entry.redo_stack.pop().ok_or("Nothing to redo")?;
        entry.undo_stack.push(entry.backing.clone());
        entry.backing = next;
        entry.is_dirty = true;
        entry.revision += 1;
        entry.layout_cache.clear();
        entry.layout_order.clear();
        entry.objects_cache.clear();
        entry.objects_order.clear();
        Ok((
            entry.backing.to_vec(),
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
            let guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(doc) = guard.get(&key) {
                if let Some(cached) = doc.objects_cache.get(&page) {
                    return Ok(cached.clone());
                }
            }
        }

        let (backing, _, _) = self.load_backing(file_path)?;
        let lo_doc = LoDocument::load_mem(&backing)
            .map_err(|e| format!("Failed parsing PDF objects: {}", e))?;
        let parsed = Arc::new(parse_page_objects(&lo_doc, page - 1)?);

        let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(doc) = guard.get_mut(&key) {
            doc.objects_order.retain(|p| *p != page);
            doc.objects_order.push_back(page);
            doc.objects_cache.insert(page, parsed.clone());
            while doc.objects_order.len() > MAX_PAGE_CACHE_PER_DOC {
                if let Some(ev) = doc.objects_order.pop_front() {
                    doc.objects_cache.remove(&ev);
                }
            }
        }
        Ok(parsed)
    }

    pub fn get_page_layout(&self, file_path: &str, page: usize) -> Result<Arc<PageLayout>, String> {
        let page = page.max(1);
        let key = Self::normalize_key(file_path);
        {
            let guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(doc) = guard.get(&key) {
                if let Some(cached) = doc.layout_cache.get(&page) {
                    return Ok(cached.clone());
                }
            }
        }

        let (backing, _, is_dirty) = self.load_backing(file_path)?;
        let backing_len = backing.len();
        let objs = if backing_len <= 2_500_000 {
            self.get_objects(file_path, page).ok()
        } else {
            None
        };
        let disk_path = if !is_dirty && std::path::Path::new(file_path).is_file() {
            Some(file_path)
        } else {
            None
        };
        let layout = Arc::new(extract_page_layout_impl(
            &backing,
            disk_path,
            page,
            objs.as_deref(),
        )?);

        let num_pages = layout.num_pages;
        {
            let mut guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(doc) = guard.get_mut(&key) {
                doc.layout_order.retain(|p| *p != page);
                doc.layout_order.push_back(page);
                doc.layout_cache.insert(page, layout.clone());
                while doc.layout_order.len() > MAX_PAGE_CACHE_PER_DOC {
                    if let Some(ev) = doc.layout_order.pop_front() {
                        doc.layout_cache.remove(&ev);
                    }
                }
            }
        }

        // Only prefetch next page layout automatically for normal-sized files
        if page < num_pages && backing_len <= LARGE_DOC_BYTES_THRESHOLD {
            let next_page = page + 1;
            let engine_clone = self.clone();
            let path_clone = file_path.to_string();
            let key_clone = key.clone();
            let already_cached = {
                let guard = self.docs.lock().unwrap_or_else(|e| e.into_inner());
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

            let line_lo = lo.max(ln.start_char).min(layout.chars.len() - 1);
            let line_hi = hi.min(ln.end_char).min(layout.chars.len() - 1);
            if line_lo > line_hi {
                continue;
            }
            let mut cur_rect: Option<[f32; 4]> = None;

            for c in layout.chars.get(line_lo..=line_hi).unwrap_or(&[]) {
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
        let lo = ln.start_char.min(layout.chars.len() - 1);
        let hi = ln.end_char.min(layout.chars.len() - 1);
        if lo <= hi {
            if let Some(slice) = layout.chars.get(lo..=hi) {
                return slice
                    .iter()
                    .min_by(|a, b| {
                        let da = ((a.bbox[0] + a.bbox[2] * 0.5) - x)
                            .hypot((a.bbox[1] + a.bbox[3] * 0.5) - y);
                        let db = ((b.bbox[0] + b.bbox[2] * 0.5) - x)
                            .hypot((b.bbox[1] + b.bbox[3] * 0.5) - y);
                        da.total_cmp(&db)
                    })
                    .map(|c| c.index);
            }
        }
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

#[allow(dead_code)]
pub fn extract_page_layout_from_bytes(
    bytes: &[u8],
    page_num_one: usize,
    objects_opt: Option<&PageObjectsResult>,
) -> Result<PageLayout, String> {
    extract_page_layout_impl(bytes, None, page_num_one, objects_opt)
}

fn extract_page_layout_impl(
    bytes: &[u8],
    disk_path: Option<&str>,
    page_num_one: usize,
    objects_opt: Option<&PageObjectsResult>,
) -> Result<PageLayout, String> {
    let page_num_one = page_num_one.max(1);
    // Only parse full lopdf AST for embedded-font metadata on compact PDFs (<= 2.5 MB).
    // On large PDFs, derive exact PageTransform directly from PDFium to prevent 500MB+ RSS spikes!
    let mut embedded_font_status: HashMap<String, (bool, bool)> = HashMap::new();
    let mut pt_opt: Option<PageTransform> = None;

    if bytes.len() <= 2_500_000 {
        if let Ok(lo_doc) = LoDocument::load_mem(bytes) {
            let pages_map = lo_doc.get_pages();
            if let Some(&page_id) = pages_map.get(&(page_num_one as u32)) {
                pt_opt = Some(crate::pdf_objects::get_page_transform(&lo_doc, page_id));
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
            }
        }
    }

    let pdfium = get_pdfium()?;
    let pdfium_guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let doc = if let Some(fp) = disk_path {
        pdfium_guard
            .load_pdf_from_file(fp, None)
            .or_else(|_| pdfium_guard.load_pdf_from_byte_slice(bytes, None))
            .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?
    } else {
        pdfium_guard
            .load_pdf_from_byte_slice(bytes, None)
            .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?
    };
    let num_pages = doc.pages().len() as usize;
    if num_pages == 0 {
        return Err("PDF contains 0 pages".to_string());
    }
    let page_idx = page_num_one.saturating_sub(1).min(num_pages - 1) as i32;
    let pg = doc
        .pages()
        .get(page_idx)
        .map_err(|e| format!("PDFium failed opening page {}: {:?}", page_num_one, e))?;

    let pt = pt_opt.unwrap_or_else(|| {
        let rot_deg = match pg.rotation().unwrap_or(PdfPageRenderRotation::None) {
            PdfPageRenderRotation::Degrees90 => 90,
            PdfPageRenderRotation::Degrees180 => 180,
            PdfPageRenderRotation::Degrees270 => 270,
            _ => 0,
        };
        let raw_w = pg.width().value;
        let raw_h = pg.height().value;
        let w = if raw_w.is_finite() { raw_w.abs().max(1.0) } else { 612.0 };
        let h = if raw_h.is_finite() { raw_h.abs().max(1.0) } else { 792.0 };
        // pg.width()/height() in PDFium already reflect /Rotate; unrotate back to CropBox if 90/270
        let (cw, ch) = if rot_deg == 90 || rot_deg == 270 { (h, w) } else { (w, h) };
        PageTransform::from_boxes([0.0, 0.0, cw, ch], rot_deg)
    });

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
        if !c.baseline.is_finite() || !c.bbox[0].is_finite() || !c.bbox[2].is_finite() {
            continue;
        }
        // Bucket baseline Y into ~10pt lines so narrow char overlaps on one title line count as 1 row
        let row_bucket = (c.baseline / 10.0).round() as i32;
        let b0 = ((c.bbox[0] / bin_w).floor() as isize).clamp(0, bins as isize - 1) as usize;
        let b1 = (((c.bbox[0] + c.bbox[2]) / bin_w).floor() as isize).clamp(0, bins as isize - 1) as usize;
        let (lo, hi) = if b0 <= b1 { (b0, b1) } else { (b1, b0) };
        if let Some(slice) = row_sets.get_mut(lo..=hi) {
            for set in slice {
                set.insert(row_bucket);
            }
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
        min_x: f32,
        max_x: f32,
    }

    fn quantize_bucket(v: f32, step: f32) -> i64 {
        if v.is_finite() {
            (v / step).round().clamp(-1e12, 1e12) as i64
        } else {
            0
        }
    }

    // Sort initially using a strict, transitive total order (quantized baseline/rotation buckets + total_cmp).
    // Avoids non-transitive epsilon comparators that panic under Rust's driftsort total-order checks.
    let mut sorted = raw_chars;
    sorted.sort_by(|a, b| {
        quantize_bucket(a.baseline, 2.5)
            .cmp(&quantize_bucket(b.baseline, 2.5))
            .then_with(|| quantize_bucket(a.rotation, 15.0).cmp(&quantize_bucket(b.rotation, 15.0)))
            .then_with(|| a.center_x.total_cmp(&b.center_x))
            .then_with(|| a.baseline.total_cmp(&b.baseline))
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
        let ch_left = ch.bbox[0];
        let ch_right = ch.bbox[0] + ch.bbox[2].max(0.0);

        // Try matching existing line fragment
        let mut target_idx: Option<usize> = None;
        for (li, tl) in temp_lines.iter().enumerate().rev() {
            if ch.rotation.abs() < 15.0 && tl.rotation.abs() < 15.0 && (ch.baseline - tl.baseline) > 48.0 {
                // Sorted by ascending quantized baseline buckets; older lines are far above
                break;
            }
            let tl_rot_group = (tl.rotation / 15.0).round();
            if (rot_group - tl_rot_group).abs() > 0.5 {
                continue;
            }
            let horiz_gap = if ch_left > tl.max_x {
                ch_left - tl.max_x
            } else if tl.min_x > ch_right {
                tl.min_x - ch_right
            } else {
                0.0
            };
            if ch.rotation.abs() < 15.0 && tl.column_idx != c_col {
                // Allow single full-width spanning lines only if spanning across gutter continuously
                if horiz_gap > ch.font_size.max(8.0) * 1.6 {
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
                if horiz_gap <= ch.font_size.max(8.0) * 2.4 {
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
            tl.min_x = tl.min_x.min(ch_left);
            tl.max_x = tl.max_x.max(ch_right);
            tl.items.push(ch);
        } else {
            temp_lines.push(TempLine {
                column_idx: c_col,
                baseline: ch.baseline,
                rotation: ch.rotation,
                font_size: ch.font_size,
                min_x: ch_left,
                max_x: ch_right,
                items: vec![ch],
            });
        }
    }

    // Sort lines in reading order: spanning titles above columns first, then column-by-column top to bottom
    for tl in &mut temp_lines {
        if tl.rotation.abs() < 15.0 {
            tl.items.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
        }
    }

    // Compute top Y where column split begins
    let first_col1_y = temp_lines
        .iter()
        .filter(|l| l.column_idx > 0 && l.baseline.is_finite())
        .map(|l| l.baseline)
        .fold(f32::INFINITY, f32::min);

    temp_lines.sort_by(|a, b| {
        let a_above = gutters.is_empty() || a.baseline < first_col1_y - 24.0;
        let b_above = gutters.is_empty() || b.baseline < first_col1_y - 24.0;
        (!a_above)
            .cmp(&(!b_above))
            .then_with(|| {
                if a_above {
                    a.baseline.total_cmp(&b.baseline)
                } else {
                    a.column_idx
                        .cmp(&b.column_idx)
                        .then_with(|| a.baseline.total_cmp(&b.baseline))
                }
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
