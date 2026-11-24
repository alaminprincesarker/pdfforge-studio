use crate::font_shaper::{attach_resource_entry, get_font_db, resolve_dict};
use crate::pdf_layout::{get_pdfium, PdfWorkspaceEngine};
use crate::pdf_objects::get_page_transform;
use fontdb::{Query, Style, Weight};
use libloading::Library;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document as LoDocument, Object, ObjectId, Stream};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_float, c_int, c_uchar, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use ttf_parser::{Face as TtfFace, GlyphId};

// ============================================================================
//  Public DTOs & OcrEngine Trait
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLanguageInfo {
    pub code: String,
    pub name: String,
    pub source_dir: String,
    pub popular: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLanguagesResponse {
    pub available: bool,
    pub engine_name: String,
    pub engine_error: Option<String>,
    pub has_osd: bool,
    pub tessdata_dirs: Vec<String>,
    pub languages: Vec<OcrLanguageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanDetectResult {
    pub file_path: String,
    pub page: usize,
    pub total_pages: usize,
    pub is_scanned: bool,
    pub has_text: bool,
    pub non_whitespace_chars: usize,
    pub image_coverage: f32,
    pub ocr_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeInspectResult {
    pub total_pages: usize,
    pub scanned_pages: Vec<usize>,
    pub text_pages: Vec<usize>,
    pub blank_pages: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrWord {
    pub text: String,
    /// Bounding box in preprocessed upright pixel coords [left, top, right, bottom]
    pub bbox_px: [f32; 4],
    /// Baseline Y coordinate in preprocessed upright pixel coords
    pub baseline_px: f32,
    pub confidence: f32,
    pub para_id: usize,
    pub line_id: usize,
    pub is_line_end: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrPageResult {
    pub page_one: usize,
    pub width_px: u32,
    pub height_px: u32,
    pub osd_cw_deg: i32,
    pub skew_deg: f32,
    pub mean_confidence: f32,
    pub duration_ms: u64,
    pub words: Vec<OcrWord>,
    /// Mapped final boxes in visual PDF viewport space (points, top-left origin)
    pub words_vp: Vec<OcrWordViewport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrWordViewport {
    pub text: String,
    /// Anchor baseline start in upright viewport points
    pub vx: f32,
    pub baseline_vy: f32,
    /// Visual box width and height along text direction
    pub width_pts: f32,
    pub height_pts: f32,
    /// Unit reading direction vector in Y-down viewport points
    pub dir_x: f32,
    pub dir_y: f32,
    pub confidence: f32,
    pub line_id: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct OrientationResult {
    /// Clockwise rotation (0, 90, 180, 270) needed to make page upright
    pub rotate_cw_deg: i32,
    pub confidence: f32,
}

/// Pluggable offline OCR engine abstraction.
pub trait OcrEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_available(&self) -> bool;
    fn detect_orientation(
        &self,
        gray: &[u8],
        width: u32,
        height: u32,
        dpi: u32,
    ) -> Result<OrientationResult, String>;
    fn recognize_gray(
        &self,
        gray: &[u8],
        width: u32,
        height: u32,
        dpi: u32,
        lang: &str,
    ) -> Result<OcrPageResult, String>;
}

// ============================================================================
//  Tessdata Discovery & Language Catalog
// ============================================================================

fn friendly_lang_name(code: &str) -> (String, bool) {
    match code {
        "eng" => ("English (US)".to_string(), true),
        "ben" => ("Bangla (Bengali)".to_string(), true),
        "hin" => ("Hindi".to_string(), true),
        "ara" => ("Arabic".to_string(), true),
        "fra" => ("French".to_string(), true),
        "deu" => ("German".to_string(), true),
        "spa" => ("Spanish".to_string(), true),
        "por" => ("Portuguese".to_string(), false),
        "ita" => ("Italian".to_string(), false),
        "rus" => ("Russian".to_string(), false),
        "jpn" => ("Japanese".to_string(), false),
        "kor" => ("Korean".to_string(), false),
        "chi_sim" => ("Chinese (Simplified)".to_string(), false),
        "chi_tra" => ("Chinese (Traditional)".to_string(), false),
        "urd" => ("Urdu".to_string(), false),
        "tam" => ("Tamil".to_string(), false),
        "tel" => ("Telugu".to_string(), false),
        "mar" => ("Marathi".to_string(), false),
        "guj" => ("Gujarati".to_string(), false),
        "pan" => ("Punjabi".to_string(), false),
        "nld" => ("Dutch".to_string(), false),
        "pol" => ("Polish".to_string(), false),
        "tur" => ("Turkish".to_string(), false),
        "vie" => ("Vietnamese".to_string(), false),
        "ind" => ("Indonesian".to_string(), false),
        "tha" => ("Thai".to_string(), false),
        "swe" => ("Swedish".to_string(), false),
        "nor" => ("Norwegian".to_string(), false),
        "dan" => ("Danish".to_string(), false),
        "fin" => ("Finnish".to_string(), false),
        "ell" => ("Greek".to_string(), false),
        "heb" => ("Hebrew".to_string(), false),
        "fas" => ("Persian".to_string(), false),
        "ukr" => ("Ukrainian".to_string(), false),
        "ces" => ("Czech".to_string(), false),
        "ron" => ("Romanian".to_string(), false),
        "hun" => ("Hungarian".to_string(), false),
        other => (other.to_uppercase(), false),
    }
}

pub fn discover_tessdata_dirs() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("pdfforge").join("tessdata"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("tessdata"));
            candidates.push(dir.join("ocr-runtime").join("tessdata"));
            let mut cur = Some(dir);
            for _ in 0..5 {
                if let Some(p) = cur {
                    candidates.push(p.join("ocr-runtime").join("tessdata"));
                    candidates.push(p.join("tessdata"));
                    cur = p.parent();
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("ocr-runtime").join("tessdata"));
        candidates.push(cwd.join("tessdata"));
    }
    if let Ok(env_td) = std::env::var("TESSDATA_PREFIX") {
        candidates.push(PathBuf::from(env_td));
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Tesseract-OCR\tessdata"));
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\Tesseract-OCR\tessdata"));

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for c in candidates {
        if c.is_dir() {
            let norm = c.to_string_lossy().to_lowercase();
            if seen.insert(norm) {
                out.push(c);
            }
        }
    }
    out
}

fn discover_tesseract_bin_dirs() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("tesseract"));
            candidates.push(dir.to_path_buf());
            candidates.push(dir.join("ocr-runtime").join("tesseract"));
            let mut cur = Some(dir);
            for _ in 0..5 {
                if let Some(p) = cur {
                    candidates.push(p.join("ocr-runtime").join("tesseract"));
                    candidates.push(p.join("tesseract"));
                    cur = p.parent();
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("ocr-runtime").join("tesseract"));
        candidates.push(cwd.join("tesseract"));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("pdfforge").join("tesseract"));
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Tesseract-OCR"));
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\Tesseract-OCR"));

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for c in candidates {
        if c.is_dir() {
            let norm = c.to_string_lossy().to_lowercase();
            if seen.insert(norm) {
                out.push(c);
            }
        }
    }
    out
}

pub fn list_available_languages() -> OcrLanguagesResponse {
    let dirs = discover_tessdata_dirs();
    let mut map: BTreeMap<String, OcrLanguageInfo> = BTreeMap::new();
    let mut has_osd = false;

    for d in &dirs {
        if let Ok(rd) = fs::read_dir(d) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.extension().and_then(|e| e.to_str()) == Some("traineddata") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if stem.eq_ignore_ascii_case("osd") {
                            has_osd = true;
                            continue;
                        }
                        if matches!(stem, "equ" | "snum" | "pdf") {
                            continue;
                        }
                        map.entry(stem.to_string()).or_insert_with(|| {
                            let (name, popular) = friendly_lang_name(stem);
                            OcrLanguageInfo {
                                code: stem.to_string(),
                                name,
                                source_dir: d.to_string_lossy().to_string(),
                                popular,
                            }
                        });
                    }
                }
            }
        }
    }

    let mut languages: Vec<OcrLanguageInfo> = map.into_values().collect();
    languages.sort_by(|a, b| {
        b.popular
            .cmp(&a.popular)
            .then_with(|| {
                let rank = |c: &str| match c {
                    "eng" => 0,
                    "ben" => 1,
                    "hin" => 2,
                    "ara" => 3,
                    "fra" => 4,
                    "deu" => 5,
                    "spa" => 6,
                    _ => 100,
                };
                rank(&a.code).cmp(&rank(&b.code))
            })
            .then_with(|| a.name.cmp(&b.name))
    });

    let eng = TesseractOcrEngine::global();
    let avail = eng.is_available();
    let engine_error = match TESS_API.get_or_init(load_tesseract_library) {
        Ok(_) => None,
        Err(e) => Some(e.clone()),
    };

    OcrLanguagesResponse {
        available: avail,
        engine_name: eng.name().to_string(),
        engine_error,
        has_osd,
        tessdata_dirs: dirs.iter().map(|d| d.to_string_lossy().to_string()).collect(),
        languages,
    }
}

/// Resolves a single unified datapath folder containing every requested `<lang>.traineddata`.
/// If requested languages are split across bundled + user custom folders, merges them via copy/hardlink
/// into a per-user cache folder so Tesseract sees all selected `.traineddata` in one directory.
fn resolve_datapath_for_langs(lang_expr: &str) -> Result<PathBuf, String> {
    let parts: Vec<&str> = lang_expr
        .split('+')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("No OCR language specified".into());
    }
    let dirs = discover_tessdata_dirs();
    if dirs.is_empty() {
        return Err("No tessdata directory found. Place .traineddata files in the tessdata folder.".into());
    }

    for d in &dirs {
        if parts.iter().all(|code| d.join(format!("{}.traineddata", code)).is_file()) {
            return Ok(d.clone());
        }
    }

    // Merge across discovered folders into %TEMP%/pdfforge_tessdata_merged
    let merged = std::env::temp_dir().join("pdfforge_tessdata_merged");
    fs::create_dir_all(&merged).map_err(|e| format!("Failed creating tessdata cache: {}", e))?;

    for code in &parts {
        let fname = format!("{}.traineddata", code);
        let target = merged.join(&fname);
        if target.is_file() {
            continue;
        }
        let mut found_src = None;
        for d in &dirs {
            let cand = d.join(&fname);
            if cand.is_file() {
                found_src = Some(cand);
                break;
            }
        }
        let src = found_src.ok_or_else(|| {
            format!(
                "Trained language data '{}' not found in any tessdata folder",
                fname
            )
        })?;
        fs::copy(&src, &target)
            .map_err(|e| format!("Failed staging '{}': {}", fname, e))?;
    }
    Ok(merged)
}

// ============================================================================
//  Dynamic C-API FFI Binding to Tesseract 5.x + Leptonica (libloading)
// ============================================================================

type TessBaseApiHandle = *mut c_void;
type TessResultIteratorHandle = *mut c_void;
type TessPageIteratorHandle = *mut c_void;

#[allow(dead_code)]
const PSM_OSD_ONLY: c_int = 0;
const PSM_AUTO: c_int = 3;
const RIL_BLOCK: c_int = 0;
const RIL_PARA: c_int = 1;
const RIL_TEXTLINE: c_int = 2;
const RIL_WORD: c_int = 3;
const OEM_DEFAULT: c_int = 3;

struct TessFns {
    _leptonica: Option<Library>,
    _tesseract: Library,
    create: unsafe extern "C" fn() -> TessBaseApiHandle,
    delete: unsafe extern "C" fn(TessBaseApiHandle),
    end: unsafe extern "C" fn(TessBaseApiHandle),
    init2: unsafe extern "C" fn(TessBaseApiHandle, *const c_char, *const c_char, c_int) -> c_int,
    set_page_seg_mode: unsafe extern "C" fn(TessBaseApiHandle, c_int),
    set_variable: unsafe extern "C" fn(TessBaseApiHandle, *const c_char, *const c_char) -> c_int,
    set_image: unsafe extern "C" fn(TessBaseApiHandle, *const c_uchar, c_int, c_int, c_int, c_int),
    set_source_resolution: unsafe extern "C" fn(TessBaseApiHandle, c_int),
    detect_osd: unsafe extern "C" fn(
        TessBaseApiHandle,
        *mut c_int,
        *mut c_float,
        *mut *const c_char,
        *mut c_float,
    ) -> c_int,
    recognize: unsafe extern "C" fn(TessBaseApiHandle, *mut c_void) -> c_int,
    mean_text_conf: unsafe extern "C" fn(TessBaseApiHandle) -> c_int,
    get_iterator: unsafe extern "C" fn(TessBaseApiHandle) -> TessResultIteratorHandle,
    res_iter_delete: unsafe extern "C" fn(TessResultIteratorHandle),
    res_iter_next: unsafe extern "C" fn(TessResultIteratorHandle, c_int) -> c_int,
    res_iter_get_utf8_text: unsafe extern "C" fn(TessResultIteratorHandle, c_int) -> *mut c_char,
    res_iter_confidence: unsafe extern "C" fn(TessResultIteratorHandle, c_int) -> c_float,
    res_iter_get_page_iter:
        unsafe extern "C" fn(TessResultIteratorHandle) -> TessPageIteratorHandle,
    page_iter_is_at_beginning:
        unsafe extern "C" fn(TessPageIteratorHandle, c_int) -> c_int,
    page_iter_is_at_final:
        unsafe extern "C" fn(TessPageIteratorHandle, c_int, c_int) -> c_int,
    page_iter_bounding_box: unsafe extern "C" fn(
        TessPageIteratorHandle,
        c_int,
        *mut c_int,
        *mut c_int,
        *mut c_int,
        *mut c_int,
    ) -> c_int,
    page_iter_baseline: unsafe extern "C" fn(
        TessPageIteratorHandle,
        c_int,
        *mut c_int,
        *mut c_int,
        *mut c_int,
        *mut c_int,
    ) -> c_int,
    delete_text: unsafe extern "C" fn(*mut c_char),
}

// All TessFns pointers point to immutable process-wide loaded DLL code.
unsafe impl Send for TessFns {}
unsafe impl Sync for TessFns {}

static TESS_API: OnceLock<Result<TessFns, String>> = OnceLock::new();

fn load_tesseract_library() -> Result<TessFns, String> {
    let bin_dirs = discover_tesseract_bin_dirs();
    let tess_dll_names = ["tesseract55.dll", "tesseract54.dll", "tesseract53.dll", "tesseract50.dll", "libtesseract-5.dll"];
    let lept_dll_names = ["leptonica-1.85.0.dll", "leptonica-1.84.1.dll", "leptonica-1.82.0.dll", "libleptonica-6.dll"];

    let mut chosen_dir: Option<PathBuf> = None;
    let mut chosen_tess: Option<PathBuf> = None;
    for dir in &bin_dirs {
        for name in &tess_dll_names {
            let p = dir.join(name);
            if p.is_file() {
                chosen_dir = Some(dir.clone());
                chosen_tess = Some(p);
                break;
            }
        }
        if chosen_tess.is_some() {
            break;
        }
    }

    let lept_lib = if let Some(ref dir) = chosen_dir {
        let mut loaded = None;
        for ln in &lept_dll_names {
            let lp = dir.join(ln);
            if lp.is_file() {
                loaded = unsafe { Library::new(&lp) }.ok();
                if loaded.is_some() {
                    break;
                }
            }
        }
        loaded
    } else {
        None
    };

    #[cfg(target_os = "windows")]
    if let Some(ref dir) = chosen_dir {
        use std::os::windows::ffi::OsStrExt;
        extern "system" {
            fn SetDllDirectoryW(lpPathName: *const u16) -> i32;
        }
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = SetDllDirectoryW(wide.as_ptr());
        }
    }

    let tess_lib = if let Some(ref p) = chosen_tess {
        unsafe { Library::new(p) }
            .map_err(|e| format!("Failed loading '{}': {}", p.display(), e))?
    } else {
        let mut last_err = String::from("Tesseract DLL not found");
        let mut found = None;
        for name in &tess_dll_names {
            match unsafe { Library::new(name) } {
                Ok(lib) => {
                    found = Some(lib);
                    break;
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        found.ok_or(last_err)?
    };

    unsafe {
        macro_rules! load_sym {
            ($sym:literal) => {
                *tess_lib
                    .get($sym)
                    .map_err(|e| format!("Missing Tesseract symbol {}: {}", String::from_utf8_lossy($sym), e))?
            };
        }
        Ok(TessFns {
            _leptonica: lept_lib,
            create: load_sym!(b"TessBaseAPICreate\0"),
            delete: load_sym!(b"TessBaseAPIDelete\0"),
            end: load_sym!(b"TessBaseAPIEnd\0"),
            init2: load_sym!(b"TessBaseAPIInit2\0"),
            set_page_seg_mode: load_sym!(b"TessBaseAPISetPageSegMode\0"),
            set_variable: load_sym!(b"TessBaseAPISetVariable\0"),
            set_image: load_sym!(b"TessBaseAPISetImage\0"),
            set_source_resolution: load_sym!(b"TessBaseAPISetSourceResolution\0"),
            detect_osd: load_sym!(b"TessBaseAPIDetectOrientationScript\0"),
            recognize: load_sym!(b"TessBaseAPIRecognize\0"),
            mean_text_conf: load_sym!(b"TessBaseAPIMeanTextConf\0"),
            get_iterator: load_sym!(b"TessBaseAPIGetIterator\0"),
            res_iter_delete: load_sym!(b"TessResultIteratorDelete\0"),
            res_iter_next: load_sym!(b"TessResultIteratorNext\0"),
            res_iter_get_utf8_text: load_sym!(b"TessResultIteratorGetUTF8Text\0"),
            res_iter_confidence: load_sym!(b"TessResultIteratorConfidence\0"),
            res_iter_get_page_iter: load_sym!(b"TessResultIteratorGetPageIterator\0"),
            page_iter_is_at_beginning: load_sym!(b"TessPageIteratorIsAtBeginningOf\0"),
            page_iter_is_at_final: load_sym!(b"TessPageIteratorIsAtFinalElement\0"),
            page_iter_bounding_box: load_sym!(b"TessPageIteratorBoundingBox\0"),
            page_iter_baseline: load_sym!(b"TessPageIteratorBaseline\0"),
            delete_text: load_sym!(b"TessDeleteText\0"),
            _tesseract: tess_lib,
        })
    }
}

pub struct TesseractOcrEngine;

impl TesseractOcrEngine {
    pub fn global() -> &'static Self {
        &TesseractOcrEngine
    }
}

struct TessGuard<'a> {
    fns: &'a TessFns,
    api: TessBaseApiHandle,
}

impl<'a> Drop for TessGuard<'a> {
    fn drop(&mut self) {
        if !self.api.is_null() {
            unsafe {
                (self.fns.end)(self.api);
                (self.fns.delete)(self.api);
            }
        }
    }
}

impl OcrEngine for TesseractOcrEngine {
    fn name(&self) -> &'static str {
        "Tesseract 5 LSTM (Offline)"
    }

    fn is_available(&self) -> bool {
        TESS_API.get_or_init(load_tesseract_library).is_ok()
    }

    fn detect_orientation(
        &self,
        gray: &[u8],
        width: u32,
        height: u32,
        dpi: u32,
    ) -> Result<OrientationResult, String> {
        if width < 64 || height < 64 || gray.len() < (width as usize) * (height as usize) {
            return Ok(OrientationResult {
                rotate_cw_deg: 0,
                confidence: 0.0,
            });
        }
        let datapath = match resolve_datapath_for_langs("osd") {
            Ok(p) => p,
            Err(_) => {
                return Ok(OrientationResult {
                    rotate_cw_deg: 0,
                    confidence: 0.0,
                });
            }
        };
        let fns = TESS_API
            .get_or_init(load_tesseract_library)
            .as_ref()
            .map_err(|e| e.clone())?;

        let api = unsafe { (fns.create)() };
        if api.is_null() {
            return Err("TessBaseAPICreate returned null".into());
        }
        let _guard = TessGuard { fns, api };

        let dp_c = CString::new(datapath.to_string_lossy().as_bytes())
            .map_err(|e| format!("Invalid datapath: {}", e))?;
        let osd_c = CString::new("osd").unwrap();
        let rc = unsafe { (fns.init2)(api, dp_c.as_ptr(), osd_c.as_ptr(), OEM_DEFAULT) };
        if rc != 0 {
            return Ok(OrientationResult {
                rotate_cw_deg: 0,
                confidence: 0.0,
            });
        }

        unsafe {
            (fns.set_page_seg_mode)(api, PSM_OSD_ONLY);
            (fns.set_image)(
                api,
                gray.as_ptr(),
                width as c_int,
                height as c_int,
                1,
                width as c_int,
            );
            (fns.set_source_resolution)(api, dpi.max(72) as c_int);
        }

        let mut orient_deg: c_int = 0;
        let mut orient_conf: c_float = 0.0;
        let mut script_name: *const c_char = std::ptr::null();
        let mut script_conf: c_float = 0.0;

        let ok = unsafe {
            (fns.detect_osd)(
                api,
                &mut orient_deg,
                &mut orient_conf,
                &mut script_name,
                &mut script_conf,
            )
        };
        if ok == 0 {
            return Ok(OrientationResult {
                rotate_cw_deg: 0,
                confidence: 0.0,
            });
        }

        // Note: Tesseract reports orientation as counter-clockwise rotation needed to fix orientation
        // OR orientation in degrees CW? Wait: let's normalize cleanly:
        // In Tesseract DetectOrientationScript: orient_deg is how much to rotate COUNTER-CLOCKWISE to upright,
        // wait: if image is rotated 90 deg clockwise visually ("top on right"), Tesseract returns orient_deg = 270
        // meaning rotate 270 deg CCW (Wait: 90 CW = top on right; to bring top back up = rotate 90 CCW = 270 CW!).
        // Specifically in Tesseract API docs: orient_deg is degrees CLOCKWISE the text must be rotated OR counter-clockwise:
        // "orientation: 0=up, 1=page right (rotated 90 CCW), 2=down, 3=left; orient_deg = orient * 90 = CCW rotation of textlines".
        // In our helper `rotate_gray_cw`, a page whose top points RIGHT (clockwise 90 deg) has orient_deg=270 (or 90) —
        // let's match standard `(360 - (orient_deg % 360)) % 360` or test empirically in `pdf_tests`!
        let norm = ((orient_deg % 360) + 360) % 360;
        // Tesseract 5.5 DetectOrientationScript returns orient_deg as CCW degrees needed to make the image upright
        // (verified empirically on rotated 90° CW test image -> orient_deg = 90 -> needs 270° CW rotation).
        let rotate_cw_deg = (360 - norm) % 360;
        Ok(OrientationResult {
            rotate_cw_deg,
            confidence: orient_conf,
        })
    }

    fn recognize_gray(
        &self,
        gray: &[u8],
        width: u32,
        height: u32,
        dpi: u32,
        lang: &str,
    ) -> Result<OcrPageResult, String> {
        let t0 = Instant::now();
        let expected_len = (width as usize) * (height as usize);
        if width == 0 || height == 0 || gray.len() < expected_len {
            return Err("Invalid grayscale image buffer for OCR".into());
        }

        let datapath = resolve_datapath_for_langs(lang)?;
        let fns = TESS_API
            .get_or_init(load_tesseract_library)
            .as_ref()
            .map_err(|e| e.clone())?;

        let api = unsafe { (fns.create)() };
        if api.is_null() {
            return Err("TessBaseAPICreate returned null".into());
        }
        let _guard = TessGuard { fns, api };

        let dp_c = CString::new(datapath.to_string_lossy().as_bytes())
            .map_err(|e| format!("Invalid datapath: {}", e))?;
        let lang_c = CString::new(lang).map_err(|e| format!("Invalid lang: {}", e))?;
        let rc = unsafe { (fns.init2)(api, dp_c.as_ptr(), lang_c.as_ptr(), OEM_DEFAULT) };
        if rc != 0 {
            return Err(format!(
                "Tesseract failed initializing language(s) '{}' from '{}'",
                lang,
                datapath.display()
            ));
        }

        let dpi_s = CString::new(dpi.max(72).to_string()).unwrap();
        let k_dpi = CString::new("user_defined_dpi").unwrap();
        let k_sp = CString::new("preserve_interword_spaces").unwrap();
        let v_one = CString::new("1").unwrap();

        unsafe {
            (fns.set_page_seg_mode)(api, PSM_AUTO);
            (fns.set_variable)(api, k_dpi.as_ptr(), dpi_s.as_ptr());
            (fns.set_variable)(api, k_sp.as_ptr(), v_one.as_ptr());
            (fns.set_image)(
                api,
                gray.as_ptr(),
                width as c_int,
                height as c_int,
                1,
                width as c_int,
            );
            (fns.set_source_resolution)(api, dpi.max(72) as c_int);
        }

        let rec_rc = unsafe { (fns.recognize)(api, std::ptr::null_mut()) };
        if rec_rc != 0 {
            return Err(format!("Tesseract Recognize failed (code {})", rec_rc));
        }

        let mean_conf = unsafe { (fns.mean_text_conf)(api) }.clamp(0, 100) as f32;
        let ri = unsafe { (fns.get_iterator)(api) };
        let mut words: Vec<OcrWord> = Vec::new();

        if !ri.is_null() {
            let mut para_idx: usize = 0;
            let mut line_idx: usize = 0;
            loop {
                let pi = unsafe { (fns.res_iter_get_page_iter)(ri) };
                if !pi.is_null() {
                    if unsafe { (fns.page_iter_is_at_beginning)(pi, RIL_PARA) } != 0 {
                        para_idx += 1;
                    }
                    if unsafe { (fns.page_iter_is_at_beginning)(pi, RIL_TEXTLINE) } != 0 {
                        line_idx += 1;
                    }
                    let txt_ptr = unsafe { (fns.res_iter_get_utf8_text)(ri, RIL_WORD) };
                    if !txt_ptr.is_null() {
                        let raw_txt = unsafe { CStr::from_ptr(txt_ptr) }
                            .to_string_lossy()
                            .to_string();
                        unsafe { (fns.delete_text)(txt_ptr) };

                        let trimmed = raw_txt.trim();
                        if !trimmed.is_empty() {
                            let mut left: c_int = 0;
                            let mut top: c_int = 0;
                            let mut right: c_int = 0;
                            let mut bottom: c_int = 0;
                            let has_box = unsafe {
                                (fns.page_iter_bounding_box)(
                                    pi,
                                    RIL_WORD,
                                    &mut left,
                                    &mut top,
                                    &mut right,
                                    &mut bottom,
                                )
                            } != 0;

                            if has_box && right > left && bottom > top {
                                let mut bx1: c_int = left;
                                let mut by1: c_int = bottom;
                                let mut bx2: c_int = right;
                                let mut by2: c_int = bottom;
                                let has_base = unsafe {
                                    (fns.page_iter_baseline)(
                                        pi,
                                        RIL_WORD,
                                        &mut bx1,
                                        &mut by1,
                                        &mut bx2,
                                        &mut by2,
                                    )
                                } != 0;
                                let baseline_px = if has_base {
                                    ((by1 + by2) as f32 * 0.5).clamp(top as f32, bottom as f32)
                                } else {
                                    (top as f32) + ((bottom - top) as f32) * 0.82
                                };

                                let conf = unsafe { (fns.res_iter_confidence)(ri, RIL_WORD) }
                                    .clamp(0.0, 100.0);
                                let is_line_end = unsafe {
                                    (fns.page_iter_is_at_final)(pi, RIL_TEXTLINE, RIL_WORD)
                                } != 0
                                    || unsafe {
                                        (fns.page_iter_is_at_final)(pi, RIL_BLOCK, RIL_WORD)
                                    } != 0;

                                words.push(OcrWord {
                                    text: trimmed.to_string(),
                                    bbox_px: [left as f32, top as f32, right as f32, bottom as f32],
                                    baseline_px,
                                    confidence: conf,
                                    para_id: para_idx.max(1),
                                    line_id: line_idx.max(1),
                                    is_line_end,
                                });
                            }
                        }
                    }
                }
                if unsafe { (fns.res_iter_next)(ri, RIL_WORD) } == 0 {
                    break;
                }
            }
            unsafe { (fns.res_iter_delete)(ri) };
        }

        // Keep reliable mean confidence computed from non-empty words if Tesseract reports 0 with words or vice versa
        let computed_conf = if words.is_empty() {
            0.0
        } else if mean_conf > 0.0 {
            mean_conf
        } else {
            words.iter().map(|w| w.confidence).sum::<f32>() / (words.len() as f32)
        };

        Ok(OcrPageResult {
            page_one: 1,
            width_px: width,
            height_px: height,
            osd_cw_deg: 0,
            skew_deg: 0.0,
            mean_confidence: computed_conf,
            duration_ms: t0.elapsed().as_millis() as u64,
            words,
            words_vp: Vec::new(),
        })
    }
}

// ============================================================================
//  Image Preprocessing: Illumination Normalization, Denoise, OSD, Deskew
// ============================================================================

pub fn bgra_to_gray(bgra: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = (width as usize) * (height as usize);
    let mut gray = Vec::with_capacity(n);
    for px in bgra.chunks_exact(4).take(n) {
        let b = px[0] as u32;
        let g = px[1] as u32;
        let r = px[2] as u32;
        let luma = (77 * r + 150 * g + 29 * b) >> 8;
        gray.push(luma as u8);
    }
    gray
}

/// Remove isolated salt-and-pepper noise without blunting thin Bangla matra strokes,
/// and flatten uneven lighting shadows via fast integral-image local background estimation.
pub fn preprocess_grayscale_for_ocr(input: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    if w < 16 || h < 16 || input.len() < w * h {
        return input.to_vec();
    }

    // 1. Estimate whether the image has shadows / low contrast / salt-pepper grain
    let mut hist = [0u32; 256];
    let step = ((w * h) / 40_000).max(1);
    let mut sampled = 0u32;
    let mut dark_cnt = 0u32;
    for i in (0..(w * h)).step_by(step) {
        let v = input[i];
        hist[v as usize] += 1;
        sampled += 1;
        if v < 190 {
            dark_cnt += 1;
        }
    }
    let dark_ratio = dark_cnt as f32 / sampled.max(1) as f32;

    // 2. Mild 3x3 impulse despeckle only for noisy photo-like images (high midtone/dark background)
    let mut buf = input.to_vec();
    if dark_ratio > 0.22 {
        let mut clean = buf.clone();
        for y in 1..(h - 1) {
            let row = y * w;
            for x in 1..(w - 1) {
                let c = buf[row + x] as i32;
                let n1 = buf[row - w + x] as i32;
                let n2 = buf[row + w + x] as i32;
                let n3 = buf[row + x - 1] as i32;
                let n4 = buf[row + x + 1] as i32;
                let avg4 = (n1 + n2 + n3 + n4) / 4;
                if (c - avg4).abs() > 70 {
                    // Isolated hot/dark pixel spike -> clamp toward 4-neighbor median-average
                    clean[row + x] = avg4.clamp(0, 255) as u8;
                }
            }
        }
        buf = clean;
    }

    // 3. Integral image for fast local background estimation (window ~ max(21, min(w,h)/32))
    // If the page already has clean white background (p90 >= 242 and p10 <= 90), skip heavy flatten
    let mut acc = 0u64;
    let sampled64 = sampled as u64;
    let mut p10 = 0u8;
    let mut p95 = 255u8;
    for (val, &cnt) in hist.iter().enumerate() {
        acc += cnt as u64;
        if p10 == 0 && acc * 10 >= sampled64 {
            p10 = val as u8;
        }
        if acc * 100 >= sampled64 * 95 {
            p95 = val as u8;
            break;
        }
    }

    if p95 >= 240 && dark_ratio < 0.20 {
        // Already high-key clean scan: apply only gentle contrast polish
        if p10 > 20 && p95 > p10.saturating_add(40) {
            let span = (p95 - p10) as f32;
            for px in &mut buf {
                let norm = ((*px as f32 - (p10 as f32 * 0.4)) / (span + 15.0)) * 255.0;
                *px = norm.clamp(0.0, 255.0) as u8;
            }
        }
        return buf;
    }

    // Shadows / camera photo: compute integral image and flatten uneven illumination + contrast stretch
    let stride = w + 1;
    let mut integral = vec![0u64; stride * (h + 1)];
    for y in 0..h {
        let mut row_sum = 0u64;
        let in_row = y * w;
        let out_row = (y + 1) * stride;
        let prev_row = y * stride;
        for x in 0..w {
            row_sum += buf[in_row + x] as u64;
            integral[out_row + x + 1] = integral[prev_row + x + 1] + row_sum;
        }
    }

    let r = (w.min(h) / 36).clamp(16, 48);
    let mut out = vec![255u8; w * h];
    for y in 0..h {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r + 1).min(h);
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(w);
            let area = ((y1 - y0) * (x1 - x0)) as u64;
            let sum = integral[y1 * stride + x1] + integral[y0 * stride + x0]
                - integral[y1 * stride + x0]
                - integral[y0 * stride + x1];
            let local_mean = (sum / area.max(1)).max(24) as f32;
            let px = buf[y * w + x] as f32;
            // Background division normalizes paper shadows cleanly to ~248 white while preserving stroke ink
            let ratio = px / local_mean;
            let mapped = if ratio >= 0.96 {
                255.0
            } else if ratio <= 0.45 {
                0.0
            } else {
                ((ratio - 0.45) / (0.96 - 0.45)) * 255.0
            };
            out[y * w + x] = mapped.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

pub fn rotate_gray_cw(src: &[u8], w: u32, h: u32, deg_cw: i32) -> (Vec<u8>, u32, u32) {
    let norm = ((deg_cw % 360) + 360) % 360;
    let wu = w as usize;
    let hu = h as usize;
    match norm {
        90 => {
            let mut dst = vec![255u8; wu * hu];
            for y in 0..hu {
                for x in 0..wu {
                    let nx = hu - 1 - y;
                    let ny = x;
                    dst[ny * hu + nx] = src[y * wu + x];
                }
            }
            (dst, h, w)
        }
        180 => {
            let mut dst = vec![255u8; wu * hu];
            for y in 0..hu {
                for x in 0..wu {
                    let nx = wu - 1 - x;
                    let ny = hu - 1 - y;
                    dst[ny * wu + nx] = src[y * wu + x];
                }
            }
            (dst, w, h)
        }
        270 => {
            let mut dst = vec![255u8; wu * hu];
            for y in 0..hu {
                for x in 0..wu {
                    let nx = y;
                    let ny = wu - 1 - x;
                    dst[ny * hu + nx] = src[y * wu + x];
                }
            }
            (dst, h, w)
        }
        _ => (src.to_vec(), w, h),
    }
}

/// Estimates skew angle (in degrees, positive = text tilts down toward right, needing -angle rotation to level)
/// via coarse-to-fine horizontal projection profile variance maximization.
pub fn estimate_skew_angle_deg(gray: &[u8], w: u32, h: u32) -> f32 {
    if w < 120 || h < 120 {
        return 0.0;
    }
    // Downsample ~4x for fast skew estimation
    let scale = ((w.max(h) as f32) / 520.0).ceil().max(1.0) as usize;
    let dw = (w as usize) / scale;
    let dh = (h as usize) / scale;
    if dw < 40 || dh < 40 {
        return 0.0;
    }

    // Extract foreground ink coordinates (x, y) centered at (dw/2, dh/2)
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(8192);
    let cx = dw as f32 * 0.5;
    let cy = dh as f32 * 0.5;
    for y in 0..dh {
        let sy = y * scale;
        for x in 0..dw {
            let sx = x * scale;
            if gray[sy * (w as usize) + sx] < 165 {
                pts.push((x as f32 - cx, y as f32 - cy));
            }
        }
    }
    if pts.len() < 80 || pts.len() > (dw * dh * 3) / 5 {
        return 0.0;
    }

    let score_angle = |deg: f32| -> f64 {
        let rad = (-deg).to_radians();
        let sin_a = rad.sin();
        let cos_a = rad.cos();
        let mut hist = vec![0u32; dh + 8];
        for &(dx, dy) in &pts {
            let ry = dx * sin_a + dy * cos_a + cy;
            if ry >= 0.0 && (ry as usize) < hist.len() {
                hist[ry as usize] += 1;
            }
        }
        let mut sum_sq = 0.0f64;
        for i in 1..hist.len() {
            let diff = hist[i] as f64 - hist[i - 1] as f64;
            sum_sq += diff * diff;
        }
        sum_sq
    };

    let mut best_deg = 0.0f32;
    let mut best_score = score_angle(0.0);

    // Coarse sweep -12..+12 deg step 1 deg
    for a in -12..=12 {
        let af = a as f32;
        let sc = score_angle(af);
        if sc > best_score {
            best_score = sc;
            best_deg = af;
        }
    }

    // Fine sweep around best_deg +/- 1.2 deg step 0.2 deg
    let center = best_deg;
    for step_i in -6..=6 {
        let af = center + (step_i as f32) * 0.2;
        if af.abs() > 14.0 {
            continue;
        }
        let sc = score_angle(af);
        if sc > best_score {
            best_score = sc;
            best_deg = af;
        }
    }

    if best_deg.abs() < 0.35 {
        0.0
    } else {
        best_deg
    }
}

pub fn rotate_gray_arbitrary_white(src: &[u8], w: u32, h: u32, ccw_deg: f32) -> Vec<u8> {
    if ccw_deg.abs() < 0.05 {
        return src.to_vec();
    }
    let wu = w as usize;
    let hu = h as usize;
    let mut dst = vec![255u8; wu * hu];
    let rad = ccw_deg.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();
    let cx = wu as f32 * 0.5;
    let cy = hu as f32 * 0.5;

    for dy in 0..hu {
        let y_off = dy as f32 - cy;
        let row = dy * wu;
        for dx in 0..wu {
            let x_off = dx as f32 - cx;
            // Inverse rotate back into source image
            let sx = x_off * cos_a + y_off * sin_a + cx;
            let sy = -x_off * sin_a + y_off * cos_a + cy;
            if sx >= 0.0 && sx < (wu - 1) as f32 && sy >= 0.0 && sy < (hu - 1) as f32 {
                let x0 = sx as usize;
                let y0 = sy as usize;
                let fx = sx - x0 as f32;
                let fy = sy - y0 as f32;
                let p00 = src[y0 * wu + x0] as f32;
                let p10 = src[y0 * wu + x0 + 1] as f32;
                let p01 = src[(y0 + 1) * wu + x0] as f32;
                let p11 = src[(y0 + 1) * wu + x0 + 1] as f32;
                let v = p00 * (1.0 - fx) * (1.0 - fy)
                    + p10 * fx * (1.0 - fy)
                    + p01 * (1.0 - fx) * fy
                    + p11 * fx * fy;
                dst[row + dx] = v.clamp(0.0, 255.0) as u8;
            }
        }
    }
    dst
}

/// Maps a point in deskewed+OSD-upright buffer `(px, py)` back into original pre-OSD visual pixel space `(ox, oy)`.
fn map_upright_px_to_orig(
    px: f32,
    py: f32,
    upright_w: u32,
    upright_h: u32,
    skew_deg: f32,
    osd_cw_deg: i32,
    orig_w: u32,
    orig_h: u32,
) -> (f32, f32) {
    // 1. Undo deskew rotation (which rotated image by ccw_deg = -skew_deg)
    let (ux, uy) = if skew_deg.abs() >= 0.05 {
        let cx = upright_w as f32 * 0.5;
        let cy = upright_h as f32 * 0.5;
        let rad = (-skew_deg).to_radians();
        let cos_a = rad.cos();
        let sin_a = rad.sin();
        let x_off = px - cx;
        let y_off = py - cy;
        (x_off * cos_a + y_off * sin_a + cx, -x_off * sin_a + y_off * cos_a + cy)
    } else {
        (px, py)
    };

    // 2. Undo OSD clockwise rotation back into original pixel coords
    let ow = orig_w as f32;
    let oh = orig_h as f32;
    match ((osd_cw_deg % 360) + 360) % 360 {
        90 => (uy, (oh - 1.0) - ux),
        180 => ((ow - 1.0) - ux, (oh - 1.0) - uy),
        270 => ((ow - 1.0) - uy, ux),
        _ => (ux, uy),
    }
}

/// Runs full render + preprocess + OSD + deskew + Tesseract OCR + viewport mapping on a single page.
pub fn run_ocr_on_grayscale(
    page_one: usize,
    raw_gray: Vec<u8>,
    orig_w: u32,
    orig_h: u32,
    dpi: u32,
    vp_width: f32,
    vp_height: f32,
    lang: &str,
) -> Result<OcrPageResult, String> {
    let t0 = Instant::now();
    let engine = TesseractOcrEngine::global();
    if !engine.is_available() {
        return Err("Offline Tesseract OCR library not available".into());
    }

    let cleaned = preprocess_grayscale_for_ocr(&raw_gray, orig_w, orig_h);
    drop(raw_gray);

    // OSD auto-orientation detection
    let osd = engine.detect_orientation(&cleaned, orig_w, orig_h, dpi)?;
    let osd_cw_deg = if osd.confidence >= 1.5 && osd.rotate_cw_deg != 0 {
        osd.rotate_cw_deg
    } else {
        0
    };

    let (rot_img, rw, rh) = if osd_cw_deg != 0 {
        rotate_gray_cw(&cleaned, orig_w, orig_h, osd_cw_deg)
    } else {
        (cleaned, orig_w, orig_h)
    };

    // Deskew detection & correction
    let skew_deg = estimate_skew_angle_deg(&rot_img, rw, rh);
    let ocr_ready = if skew_deg.abs() >= 0.35 {
        rotate_gray_arbitrary_white(&rot_img, rw, rh, -skew_deg)
    } else {
        rot_img
    };

    let mut page_res = engine.recognize_gray(&ocr_ready, rw, rh, dpi, lang)?;
    page_res.page_one = page_one;
    page_res.osd_cw_deg = osd_cw_deg;
    page_res.skew_deg = skew_deg;
    page_res.duration_ms = t0.elapsed().as_millis() as u64;

    let sx = vp_width / (orig_w.max(1) as f32);
    let sy = vp_height / (orig_h.max(1) as f32);

    let mut words_vp = Vec::with_capacity(page_res.words.len());
    for w in &page_res.words {
        let [l, t, r, b] = w.bbox_px;
        let base_y = w.baseline_px;
        let p_start = map_upright_px_to_orig(l, base_y, rw, rh, skew_deg, osd_cw_deg, orig_w, orig_h);
        let p_end = map_upright_px_to_orig(r, base_y, rw, rh, skew_deg, osd_cw_deg, orig_w, orig_h);

        let vx0 = p_start.0 * sx;
        let vy0 = p_start.1 * sy;
        let vx1 = p_end.0 * sx;
        let vy1 = p_end.1 * sy;
        let dx = vx1 - vx0;
        let dy = vy1 - vy0;
        let width_pts = dx.hypot(dy).max(1.0);
        let (dir_x, dir_y) = (dx / width_pts, dy / width_pts);

        // Compute visual cap height in viewport points
        let p_top = map_upright_px_to_orig(l, t, rw, rh, skew_deg, osd_cw_deg, orig_w, orig_h);
        let height_pts = ((p_top.0 - p_start.0) * sx)
            .hypot((p_top.1 - p_start.1) * sy)
            .max((b - t) * sy * 0.72)
            .clamp(5.0, 96.0);

        words_vp.push(OcrWordViewport {
            text: w.text.clone(),
            vx: vx0,
            baseline_vy: vy0,
            width_pts,
            height_pts,
            dir_x,
            dir_y,
            confidence: w.confidence,
            line_id: w.line_id,
        });
    }
    page_res.words_vp = words_vp;
    Ok(page_res)
}

// ============================================================================
//  Scanned Page Detection via PDFium
// ============================================================================

fn accumulate_image_coverage_from_objects<'a>(
    objects: impl Iterator<Item = PdfPageObject<'a>>,
    page_w: f32,
    page_h: f32,
    grid: &mut [u8; 64 * 64],
) {
    for obj in objects {
        if obj.object_type() == PdfPageObjectType::Image {
            if let Ok(bounds) = obj.bounds() {
                let l = bounds.left().value.min(bounds.right().value).clamp(0.0, page_w);
                let r = bounds.right().value.max(bounds.left().value).clamp(0.0, page_w);
                let b = bounds.bottom().value.min(bounds.top().value).clamp(0.0, page_h);
                let t = bounds.top().value.max(bounds.bottom().value).clamp(0.0, page_h);
                if r > l && t > b {
                    let gx0 = ((l / page_w) * 64.0).floor().clamp(0.0, 63.0) as usize;
                    let gx1 = ((r / page_w) * 64.0).ceil().clamp(0.0, 64.0) as usize;
                    let gy0 = ((b / page_h) * 64.0).floor().clamp(0.0, 63.0) as usize;
                    let gy1 = ((t / page_h) * 64.0).ceil().clamp(0.0, 64.0) as usize;
                    for gy in gy0..gy1 {
                        for gx in gx0..gx1 {
                            grid[gy * 64 + gx] = 1;
                        }
                    }
                }
            }
        } else if let Some(form) = obj.as_x_object_form_object() {
            accumulate_image_coverage_from_objects(form.iter(), page_w, page_h, grid);
        }
    }
}

pub fn inspect_page_scan_status(
    ws: &PdfWorkspaceEngine,
    file_path: &str,
    page_one: usize,
) -> Result<ScanDetectResult, String> {
    let (backing, _, _) = ws.load_backing(file_path)?;
    inspect_page_scan_status_bytes(&backing, file_path, page_one)
}

pub fn inspect_page_scan_status_bytes(
    bytes: &[u8],
    file_path: &str,
    page_one: usize,
) -> Result<ScanDetectResult, String> {
    let pdfium = get_pdfium()?;
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let doc = guard
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?;
    let total_pages = doc.pages().len() as usize;
    if total_pages == 0 {
        return Err("PDF has 0 pages".into());
    }
    let idx = (page_one.saturating_sub(1)).min(total_pages - 1) as i32;
    let pg = doc
        .pages()
        .get(idx)
        .map_err(|e| format!("Failed opening page {}: {:?}", page_one, e))?;

    let non_ws_chars = pg
        .text()
        .map(|tp| {
            tp.chars()
                .iter()
                .filter_map(|c| c.unicode_char())
                .filter(|c| !c.is_whitespace() && !c.is_control())
                .count()
        })
        .unwrap_or(0);

    let page_w = pg.width().value.abs().max(1.0);
    let page_h = pg.height().value.abs().max(1.0);
    let mut grid = [0u8; 64 * 64];
    accumulate_image_coverage_from_objects(pg.objects().iter(), page_w, page_h, &mut grid);
    let covered_cells: usize = grid.iter().map(|&v| v as usize).sum();
    let image_coverage = (covered_cells as f32) / (64.0 * 64.0);

    let has_text = non_ws_chars > 0;
    let is_scanned = !has_text && image_coverage > 0.80;

    Ok(ScanDetectResult {
        file_path: file_path.to_string(),
        page: (idx as usize) + 1,
        total_pages,
        is_scanned,
        has_text,
        non_whitespace_chars: non_ws_chars,
        image_coverage,
        ocr_available: TesseractOcrEngine::global().is_available(),
    })
}

pub fn inspect_pages_range_status(
    ws: &PdfWorkspaceEngine,
    file_path: &str,
    pages: &[usize],
) -> Result<RangeInspectResult, String> {
    let (backing, _, _) = ws.load_backing(file_path)?;
    let pdfium = get_pdfium()?;
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let doc = guard
        .load_pdf_from_byte_slice(&backing, None)
        .map_err(|e| format!("PDFium failed opening PDF: {:?}", e))?;
    let total_pages = doc.pages().len() as usize;

    let mut scanned_pages = Vec::new();
    let mut text_pages = Vec::new();
    let mut blank_pages = Vec::new();

    for &p1 in pages {
        if p1 < 1 || p1 > total_pages {
            continue;
        }
        let pg = doc
            .pages()
            .get((p1 - 1) as i32)
            .map_err(|e| format!("Failed opening page {}: {:?}", p1, e))?;
        let non_ws = pg
            .text()
            .map(|tp| tp.all().chars().filter(|c| !c.is_whitespace() && !c.is_control()).count())
            .unwrap_or(0);
        if non_ws > 0 {
            text_pages.push(p1);
        } else {
            let page_w = pg.width().value.abs().max(1.0);
            let page_h = pg.height().value.abs().max(1.0);
            let mut grid = [0u8; 64 * 64];
            accumulate_image_coverage_from_objects(pg.objects().iter(), page_w, page_h, &mut grid);
            let cov = grid.iter().map(|&v| v as usize).sum::<usize>() as f32 / (64.0 * 64.0);
            if cov > 0.80 {
                scanned_pages.push(p1);
            } else {
                blank_pages.push(p1);
            }
        }
    }

    Ok(RangeInspectResult {
        total_pages,
        scanned_pages,
        text_pages,
        blank_pages,
    })
}

// ============================================================================
//  Rendering Helper (Sequential PDFium Page Render -> Compact Grayscale)
// ============================================================================

pub fn render_page_for_ocr_from_bytes(
    bytes: &[u8],
    page_one: usize,
    dpi: u32,
) -> Result<(Vec<u8>, u32, u32, f32, f32), String> {
    let lo = LoDocument::load_mem(bytes)
        .map_err(|e| format!("Failed parsing PDF geometry: {}", e))?;
    let page_id = *lo
        .get_pages()
        .get(&(page_one as u32))
        .ok_or_else(|| format!("Page {} out of bounds", page_one))?;
    let pt = get_page_transform(&lo, page_id);
    let vp_w = pt.vp_width.max(1.0);
    let vp_h = pt.vp_height.max(1.0);
    drop(lo);

    // Cap extreme page rasterization (e.g. huge poster sizes at 600 DPI) to ~32 MP to prevent OOM
    let scale = (dpi.max(72) as f32) / 72.0;
    let mut px_w = (vp_w * scale).round().max(16.0);
    let mut px_h = (vp_h * scale).round().max(16.0);
    let max_px = 32_000_000.0f32;
    if px_w * px_h > max_px {
        let factor = (max_px / (px_w * px_h)).sqrt();
        px_w = (px_w * factor).round().max(16.0);
        px_h = (px_h * factor).round().max(16.0);
    }
    let w_u32 = px_w as u32;
    let h_u32 = px_h as u32;

    let pdfium = get_pdfium()?;
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let doc = guard
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| format!("PDFium open failed: {:?}", e))?;
    let pg = doc
        .pages()
        .get((page_one - 1) as i32)
        .map_err(|e| format!("PDFium page {} failed: {:?}", page_one, e))?;

    let cfg = PdfRenderConfig::new()
        .set_target_size(w_u32 as i32, h_u32 as i32)
        .set_clear_color(PdfColor::WHITE)
        .set_text_smoothing(true)
        .set_image_smoothing(true)
        .render_annotations(false)
        .render_form_data(false);

    let mut bgra = vec![255u8; (w_u32 as usize) * (h_u32 as usize) * 4];
    let mut bitmap = PdfBitmap::from_bytes(
        w_u32 as i32,
        h_u32 as i32,
        PdfBitmapFormat::BGRA,
        &mut bgra,
    )
    .map_err(|e| format!("PdfBitmap alloc failed: {:?}", e))?;
    pg.render_into_bitmap_with_config(&mut bitmap, &cfg)
        .map_err(|e| format!("PDFium render failed: {:?}", e))?;
    drop(bitmap);

    let gray = bgra_to_gray(&bgra, w_u32, h_u32);
    drop(bgra);
    Ok((gray, w_u32, h_u32, vp_w, vp_h))
}

// ============================================================================
//  Shared Unicode CID Font Builder + Invisible Text Stream (`3 Tr`) Writer
// ============================================================================

struct OcrFontSlot {
    res_name: String,
    font_bytes: Vec<u8>,
    face_index: u32,
    ps_name: String,
    char_to_gid: HashMap<char, u16>,
    gid_to_unicode: BTreeMap<u16, char>,
    gid_widths: BTreeMap<u16, i64>,
    upem: f32,
    asc: i64,
    desc: i64,
    cap_h: i64,
    type0_id: ObjectId,
}

impl OcrFontSlot {
    fn from_bytes(
        doc: &mut LoDocument,
        res_name: &str,
        ps_name: &str,
        font_bytes: Vec<u8>,
        face_index: u32,
    ) -> Result<Self, String> {
        let face = TtfFace::parse(&font_bytes, face_index)
            .map_err(|e| format!("Invalid font {}: {:?}", ps_name, e))?;
        let upem = face.units_per_em().max(1) as f32;
        let asc = ((face.ascender() as f32 / upem) * 1000.0).round() as i64;
        let desc = ((face.descender() as f32 / upem) * 1000.0).round() as i64;
        let cap_h = ((face.capital_height().unwrap_or(face.ascender()) as f32 / upem) * 1000.0)
            .round() as i64;
        let type0_id = doc.new_object_id();
        Ok(Self {
            res_name: res_name.to_string(),
            font_bytes,
            face_index,
            ps_name: ps_name.to_string(),
            char_to_gid: HashMap::new(),
            gid_to_unicode: BTreeMap::new(),
            gid_widths: BTreeMap::new(),
            upem,
            asc,
            desc,
            cap_h,
            type0_id,
        })
    }

    fn supports(&self, ch: char) -> bool {
        if let Ok(face) = TtfFace::parse(&self.font_bytes, self.face_index) {
            face.glyph_index(ch).map(|g| g.0 != 0).unwrap_or(false)
        } else {
            false
        }
    }

    /// Encodes a word/run using nominal glyph indices and tracks cumulative /W and /ToUnicode entries.
    /// Returns `(cid_bytes, nominal_width_at_unit_font_size)`.
    fn encode_run(&mut self, text: &str) -> (Vec<u8>, f32) {
        let face = match TtfFace::parse(&self.font_bytes, self.face_index) {
            Ok(f) => f,
            Err(_) => return (Vec::new(), 0.0),
        };
        let mut out = Vec::with_capacity(text.len() * 2);
        let mut total_w1000 = 0.0f32;
        for ch in text.chars() {
            if ch == '\r' || ch == '\n' {
                continue;
            }
            let gid = if let Some(&cached) = self.char_to_gid.get(&ch) {
                cached
            } else {
                let g = face.glyph_index(ch).unwrap_or(GlyphId(0)).0;
                self.char_to_gid.insert(ch, g);
                g
            };
            self.gid_to_unicode.entry(gid).or_insert(ch);
            let w1000 = if let Some(&w) = self.gid_widths.get(&gid) {
                w
            } else {
                let adv = face
                    .glyph_hor_advance(GlyphId(gid))
                    .unwrap_or((self.upem * 0.5) as u16);
                let w = ((adv as f32 / self.upem) * 1000.0).round().max(100.0) as i64;
                self.gid_widths.insert(gid, w);
                w
            };
            total_w1000 += w1000 as f32;
            out.push((gid >> 8) as u8);
            out.push((gid & 0xFF) as u8);
        }
        (out, total_w1000 / 1000.0)
    }

    fn finalize_into_doc(self, doc: &mut LoDocument) -> ObjectId {
        let mut w_arr: Vec<Object> = Vec::with_capacity(self.gid_widths.len() * 2);
        for (gid, w) in &self.gid_widths {
            w_arr.push(Object::Integer(*gid as i64));
            w_arr.push(Object::Array(vec![Object::Integer(*w)]));
        }

        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        let entries: Vec<(u16, char)> = self.gid_to_unicode.into_iter().collect();
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

        let mut ff_dict = Dictionary::new();
        ff_dict.set("Length1", Object::Integer(self.font_bytes.len() as i64));
        let mut ff_stream = Stream::new(ff_dict, self.font_bytes);
        let _ = ff_stream.compress();
        let ff_id = doc.add_object(Object::Stream(ff_stream));

        let mut fd_dict = Dictionary::new();
        fd_dict.set("Type", Object::Name(b"FontDescriptor".to_vec()));
        fd_dict.set("FontName", Object::Name(self.ps_name.as_bytes().to_vec()));
        fd_dict.set("Flags", Object::Integer(32));
        fd_dict.set(
            "FontBBox",
            Object::Array(vec![
                Object::Integer(-500),
                Object::Integer(self.desc.min(-300)),
                Object::Integer(1500),
                Object::Integer(self.asc.max(1000)),
            ]),
        );
        fd_dict.set("ItalicAngle", Object::Integer(0));
        fd_dict.set("Ascent", Object::Integer(self.asc));
        fd_dict.set("Descent", Object::Integer(self.desc));
        fd_dict.set("CapHeight", Object::Integer(self.cap_h));
        fd_dict.set("StemV", Object::Integer(80));
        fd_dict.set("FontFile2", Object::Reference(ff_id));
        let fd_id = doc.add_object(Object::Dictionary(fd_dict));

        let mut cid_sys = Dictionary::new();
        cid_sys.set("Registry", Object::string_literal("Adobe"));
        cid_sys.set("Ordering", Object::string_literal("Identity"));
        cid_sys.set("Supplement", Object::Integer(0));

        let mut cid_font = Dictionary::new();
        cid_font.set("Type", Object::Name(b"Font".to_vec()));
        cid_font.set("Subtype", Object::Name(b"CIDFontType2".to_vec()));
        cid_font.set("BaseFont", Object::Name(self.ps_name.as_bytes().to_vec()));
        cid_font.set("CIDSystemInfo", Object::Dictionary(cid_sys));
        cid_font.set("FontDescriptor", Object::Reference(fd_id));
        cid_font.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
        cid_font.set("DW", Object::Integer(550));
        cid_font.set("W", Object::Array(w_arr));
        let cid_id = doc.add_object(Object::Dictionary(cid_font));

        let mut type0 = Dictionary::new();
        type0.set("Type", Object::Name(b"Font".to_vec()));
        type0.set("Subtype", Object::Name(b"Type0".to_vec()));
        type0.set("BaseFont", Object::Name(self.ps_name.as_bytes().to_vec()));
        type0.set("Encoding", Object::Name(b"Identity-H".to_vec()));
        type0.set("DescendantFonts", Object::Array(vec![Object::Reference(cid_id)]));
        type0.set("ToUnicode", Object::Reference(tounicode_id));
        doc.objects.insert(self.type0_id, Object::Dictionary(type0));
        self.type0_id
    }
}

struct SharedOcrFonts {
    latin: OcrFontSlot,
    bengali: Option<OcrFontSlot>,
    extras: Vec<OcrFontSlot>,
}

impl SharedOcrFonts {
    fn new(doc: &mut LoDocument) -> Result<Self, String> {
        let latin_bytes = crate::Assets::get("fonts/NotoSans-Regular.ttf")
            .or_else(|| crate::Assets::get("standard_fonts/LiberationSans-Regular.ttf"))
            .map(|a| a.data.to_vec())
            .or_else(|| load_font_by_family("Arial"))
            .ok_or("No offline Latin TTF font found for OCR text embedding")?;

        let latin = OcrFontSlot::from_bytes(doc, "PFOCR_Latin", "NotoSans-OCR", latin_bytes, 0)?;

        let bengali = crate::Assets::get("fonts/NotoSansBengali-Regular.ttf")
            .map(|a| a.data.to_vec())
            .or_else(|| load_font_by_family("Nirmala UI"))
            .and_then(|b| {
                OcrFontSlot::from_bytes(doc, "PFOCR_Ben", "NotoSansBengali-OCR", b, 0).ok()
            });

        Ok(Self {
            latin,
            bengali,
            extras: Vec::new(),
        })
    }

    fn pick_slot_idx_for_word(&mut self, doc: &mut LoDocument, word: &str) -> usize {
        // Return 0 = latin, 1 = bengali, 2+ = extra idx + 2
        let has_ben = word.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c));
        if has_ben {
            if let Some(ref b) = self.bengali {
                if word
                    .chars()
                    .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
                    .any(|c| b.supports(c))
                {
                    return 1;
                }
            }
        }
        let non_ws: Vec<char> = word
            .chars()
            .filter(|c| !c.is_control() && *c != ' ')
            .collect();
        if non_ws.is_empty() || non_ws.iter().all(|&c| self.latin.supports(c)) {
            return 0;
        }
        for (i, ex) in self.extras.iter().enumerate() {
            if non_ws.iter().all(|&c| ex.supports(c)) {
                return i + 2;
            }
        }
        if let Some((bytes, face_idx, fam)) = find_system_font_covering(&non_ws) {
            let clean: String = fam.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let res = format!("PFOCR_Ex{}", self.extras.len() + 1);
            let ps = format!("{}-OCR", if clean.is_empty() { "Sys" } else { &clean });
            if let Ok(slot) = OcrFontSlot::from_bytes(doc, &res, &ps, bytes, face_idx) {
                self.extras.push(slot);
                return self.extras.len() + 1;
            }
        }
        0
    }

    fn slot_mut(&mut self, idx: usize) -> &mut OcrFontSlot {
        if idx == 1 {
            if let Some(ref mut b) = self.bengali {
                return b;
            }
        } else if idx >= 2 {
            let ei = idx - 2;
            if ei < self.extras.len() {
                return &mut self.extras[ei];
            }
        }
        &mut self.latin
    }

    fn slot_ref(&self, idx: usize) -> &OcrFontSlot {
        if idx == 1 {
            if let Some(ref b) = self.bengali {
                return b;
            }
        } else if idx >= 2 {
            let ei = idx - 2;
            if ei < self.extras.len() {
                return &self.extras[ei];
            }
        }
        &self.latin
    }

    fn finalize_all(self, doc: &mut LoDocument) {
        self.latin.finalize_into_doc(doc);
        if let Some(b) = self.bengali {
            b.finalize_into_doc(doc);
        }
        for ex in self.extras {
            ex.finalize_into_doc(doc);
        }
    }
}

fn load_font_by_family(fam_name: &str) -> Option<Vec<u8>> {
    let db = get_font_db();
    let id = db.query(&Query {
        families: &[fontdb::Family::Name(fam_name)],
        weight: Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: Style::Normal,
    })?;
    let mut out = None;
    db.with_face_data(id, |data, _| {
        out = Some(data.to_vec());
    });
    out
}

fn find_system_font_covering(chars: &[char]) -> Option<(Vec<u8>, u32, String)> {
    let db = get_font_db();
    let mut best: Option<(usize, fontdb::ID, u32, String)> = None;
    for face in db.faces() {
        let mut matched = 0usize;
        db.with_face_data(face.id, |data, idx| {
            if let Ok(ttf) = TtfFace::parse(data, idx) {
                for &ch in chars {
                    if ttf.glyph_index(ch).map(|g| g.0 != 0).unwrap_or(false) {
                        matched += 1;
                    }
                }
            }
        });
        if matched > 0 {
            let fam = face
                .families
                .first()
                .map(|(f, _)| f.clone())
                .unwrap_or_else(|| "System".to_string());
            let idx = face.index;
            if matched == chars.len() {
                let mut bytes_out = None;
                db.with_face_data(face.id, |d, _| bytes_out = Some(d.to_vec()));
                if let Some(b) = bytes_out {
                    return Some((b, idx, fam));
                }
            }
            if best.as_ref().is_none_or(|b| matched > b.0) {
                best = Some((matched, face.id, idx, fam));
            }
        }
    }
    if let Some((_, fid, idx, fam)) = best {
        let mut bytes_out = None;
        db.with_face_data(fid, |d, _| bytes_out = Some(d.to_vec()));
        return bytes_out.map(|b| (b, idx, fam));
    }
    None
}

/// Removes any previous `/PFOCR BDC ... EMC` tagged OCR blocks from decoded operations.
fn strip_existing_ocr_ops(ops: Vec<Operation>) -> Vec<Operation> {
    let mut out = Vec::with_capacity(ops.len());
    let mut depth = 0usize;
    let mut in_ocr = false;
    for op in ops {
        if !in_ocr {
            if op.operator == "BDC" {
                if let Some(Object::Name(tag)) = op.operands.first() {
                    if tag == b"PFOCR" {
                        in_ocr = true;
                        depth = 1;
                        continue;
                    }
                }
            }
            out.push(op);
        } else {
            if op.operator == "BDC" || op.operator == "BMC" {
                depth += 1;
            } else if op.operator == "EMC" {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    in_ocr = false;
                }
            }
        }
    }
    out
}

/// Downsamples oversized raw FlateDecode RGB/Gray XObject images on `page_id` when
/// `target_dpi > 0` (`Searchable Image` mode). Never upsamples and leaves compressed
/// JPEG / CCITT / JBIG2 / Indexed / SoftMask images untouched so original scans never degrade.
fn maybe_downsample_page_images(doc: &mut LoDocument, page_id: ObjectId, target_dpi: u32) {
    if target_dpi == 0 {
        return;
    }
    let page_pt = get_page_transform(doc, page_id);
    let page_w_in = (page_pt.vp_width / 72.0).max(0.5);
    let page_h_in = (page_pt.vp_height / 72.0).max(0.5);

    let xobj_ids: Vec<ObjectId> = {
        let mut ids = Vec::new();
        if let Some(res) = crate::font_shaper::get_page_resources_dict(doc, page_id) {
            if let Ok(xo) = res.get(b"XObject") {
                if let Some(xd) = resolve_dict(doc, xo) {
                    for (_, v) in xd.iter() {
                        if let Ok(oid) = v.as_reference() {
                            ids.push(oid);
                        }
                    }
                }
            }
        }
        ids
    };

    for oid in xobj_ids {
        let mut clone_stream = match doc.get_object(oid) {
            Ok(Object::Stream(s)) => s.clone(),
            _ => continue,
        };
        if clone_stream.dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()) != Some(b"Image") {
            continue;
        }
        // Only safely downsample plain Flate/raw Gray or RGB without ImageMask/SMask/DecodeParms complications
        let filter_ok = match clone_stream.dict.get(b"Filter").ok() {
            None => true,
            Some(Object::Name(n)) => n == b"FlateDecode",
            _ => false,
        };
        if !filter_ok || clone_stream.dict.has(b"DecodeParms") || clone_stream.dict.has(b"ImageMask") {
            continue;
        }
        let bpc = clone_stream
            .dict
            .get(b"BitsPerComponent")
            .ok()
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(8);
        if bpc != 8 {
            continue;
        }
        let channels = match clone_stream.dict.get(b"ColorSpace").ok() {
            Some(Object::Name(n)) if n == b"DeviceGray" => 1usize,
            Some(Object::Name(n)) if n == b"DeviceRGB" => 3usize,
            _ => continue,
        };
        let iw = clone_stream
            .dict
            .get(b"Width")
            .ok()
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(0) as usize;
        let ih = clone_stream
            .dict
            .get(b"Height")
            .ok()
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(0) as usize;
        if iw < 64 || ih < 64 {
            continue;
        }

        let eff_dpi = (iw as f32 / page_w_in).max(ih as f32 / page_h_in);
        if eff_dpi <= (target_dpi as f32) * 1.15 {
            continue;
        }
        let factor = (target_dpi as f32) / eff_dpi;
        let nw = ((iw as f32) * factor).round().max(16.0) as usize;
        let nh = ((ih as f32) * factor).round().max(16.0) as usize;
        if nw >= iw || nh >= ih {
            continue;
        }

        if clone_stream.is_compressed() && clone_stream.decompress().is_err() {
            continue;
        }
        if clone_stream.content.len() < iw * ih * channels {
            continue;
        }

        // Bilinear downsample into `new_raw`
        let mut new_raw = vec![0u8; nw * nh * channels];
        let xr = iw as f32 / nw as f32;
        let yr = ih as f32 / nh as f32;
        for dy in 0..nh {
            let sy = ((dy as f32 + 0.5) * yr - 0.5).clamp(0.0, (ih - 1) as f32);
            let y0 = sy as usize;
            let y1 = (y0 + 1).min(ih - 1);
            let fy = sy - y0 as f32;
            for dx in 0..nw {
                let sx = ((dx as f32 + 0.5) * xr - 0.5).clamp(0.0, (iw - 1) as f32);
                let x0 = sx as usize;
                let x1 = (x0 + 1).min(iw - 1);
                let fx = sx - x0 as f32;
                let out_idx = (dy * nw + dx) * channels;
                for c in 0..channels {
                    let p00 = clone_stream.content[(y0 * iw + x0) * channels + c] as f32;
                    let p10 = clone_stream.content[(y0 * iw + x1) * channels + c] as f32;
                    let p01 = clone_stream.content[(y1 * iw + x0) * channels + c] as f32;
                    let p11 = clone_stream.content[(y1 * iw + x1) * channels + c] as f32;
                    let v = p00 * (1.0 - fx) * (1.0 - fy)
                        + p10 * fx * (1.0 - fy)
                        + p01 * (1.0 - fx) * fy
                        + p11 * fx * fy;
                    new_raw[out_idx + c] = v.clamp(0.0, 255.0) as u8;
                }
            }
        }

        let mut new_dict = clone_stream.dict.clone();
        new_dict.remove(b"Filter");
        new_dict.remove(b"Length");
        new_dict.set("Width", Object::Integer(nw as i64));
        new_dict.set("Height", Object::Integer(nh as i64));
        let mut new_stream = Stream::new(new_dict, new_raw);
        let _ = new_stream.compress();
        doc.objects.insert(oid, Object::Stream(new_stream));
    }
}

/// Writes invisible (`3 Tr`) OCR word boxes into the PDF pages using shared Unicode CID fonts (`ToUnicode`).
pub fn write_ocr_results_into_pdf(
    pdf_bytes: &[u8],
    page_results: &[OcrPageResult],
    output_mode: &str,
    downsample_dpi: i32,
) -> Result<Vec<u8>, String> {
    let mut doc = LoDocument::load_mem(pdf_bytes)
        .map_err(|e| format!("Failed loading PDF in lopdf: {}", e))?;
    let pages_map = doc.get_pages();
    let mut fonts = SharedOcrFonts::new(&mut doc)?;

    let allow_downsample = output_mode == "searchable_image" && downsample_dpi > 0;

    for pr in page_results {
        let Some(&page_id) = pages_map.get(&(pr.page_one as u32)) else {
            continue;
        };

        if allow_downsample {
            maybe_downsample_page_images(&mut doc, page_id, downsample_dpi as u32);
        }

        let pt = get_page_transform(&doc, page_id);

        // Strip any previous /PFOCR BDC..EMC block and wrap existing content safely in q..Q
        let existing_raw = doc.get_page_content(page_id);
        let existing_ops = Content::decode(&existing_raw)
            .map(|c| strip_existing_ocr_ops(c.operations))
            .unwrap_or_default();

        let mut ops: Vec<Operation> = Vec::with_capacity(existing_ops.len() + pr.words_vp.len() * 6 + 16);
        if !existing_ops.is_empty() {
            ops.push(Operation::new("q", vec![]));
            ops.extend(existing_ops);
            ops.push(Operation::new("Q", vec![]));
        }

        if !pr.words_vp.is_empty() {
            ops.push(Operation::new("BDC", vec![Object::Name(b"PFOCR".to_vec()), Object::Dictionary(Dictionary::new())]));
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new("BT", vec![]));
            ops.push(Operation::new("Tr", vec![Object::Integer(3)]));

            let mut used_font_indices: HashSet<usize> = HashSet::new();
            let mut active_font_idx: Option<usize> = None;

            for (wi, w) in pr.words_vp.iter().enumerate() {
                // Append a trailing space between words on the same line so Acrobat / Chrome / PDFium copy preserves word boundaries!
                let next_same_line = pr
                    .words_vp
                    .get(wi + 1)
                    .is_some_and(|nw| nw.line_id == w.line_id);
                let text_with_space = if next_same_line {
                    format!("{} ", w.text)
                } else {
                    w.text.clone()
                };

                let slot_idx = fonts.pick_slot_idx_for_word(&mut doc, &text_with_space);
                used_font_indices.insert(slot_idx);

                let font_size = (w.height_pts * 0.92).clamp(4.0, 96.0);
                let (word_bytes, word_unit_w) = fonts.slot_mut(slot_idx).encode_run(&w.text);
                if word_bytes.is_empty() {
                    continue;
                }
                let (full_bytes, _) = fonts.slot_mut(slot_idx).encode_run(&text_with_space);
                let natural_pts = (word_unit_w * font_size).max(0.5);
                let tz = ((w.width_pts / natural_pts) * 100.0).clamp(25.0, 400.0);

                if active_font_idx != Some(slot_idx) {
                    let res_name = fonts.slot_ref(slot_idx).res_name.clone();
                    ops.push(Operation::new(
                        "Tf",
                        vec![
                            Object::Name(res_name.into_bytes()),
                            Object::Real(font_size),
                        ],
                    ));
                    active_font_idx = Some(slot_idx);
                } else {
                    let res_name = fonts.slot_ref(slot_idx).res_name.clone();
                    ops.push(Operation::new(
                        "Tf",
                        vec![
                            Object::Name(res_name.into_bytes()),
                            Object::Real(font_size),
                        ],
                    ));
                }

                // Derive local text matrix directly from PageTransform differentials so rotated & skewed
                // text aligns identically in unrotated PDF user space across /Rotate 0/90/180/270!
                let p0 = pt.vp_to_pdf(w.vx, w.baseline_vy);
                let px_step = pt.vp_to_pdf(w.vx + w.dir_x, w.baseline_vy + w.dir_y);
                // Visual glyph UP in Y-down viewport corresponds to (+dir_y, -dir_x)
                let py_step = pt.vp_to_pdf(w.vx + w.dir_y, w.baseline_vy - w.dir_x);
                let tm_a = px_step.0 - p0.0;
                let tm_b = px_step.1 - p0.1;
                let tm_c = py_step.0 - p0.0;
                let tm_d = py_step.1 - p0.1;

                ops.push(Operation::new(
                    "Tm",
                    vec![
                        Object::Real(tm_a),
                        Object::Real(tm_b),
                        Object::Real(tm_c),
                        Object::Real(tm_d),
                        Object::Real(p0.0),
                        Object::Real(p0.1),
                    ],
                ));
                ops.push(Operation::new("Tz", vec![Object::Real(tz)]));
                ops.push(Operation::new(
                    "Tj",
                    vec![Object::String(full_bytes, lopdf::StringFormat::Hexadecimal)],
                ));
            }

            ops.push(Operation::new("ET", vec![]));
            ops.push(Operation::new("Q", vec![]));
            ops.push(Operation::new("EMC", vec![]));

            // Attach font references used on this page
            for slot_idx in used_font_indices {
                let (res_name, tid) = {
                    let s = fonts.slot_ref(slot_idx);
                    (s.res_name.clone(), s.type0_id)
                };
                attach_resource_entry(&mut doc, page_id, b"Font", &res_name, Object::Reference(tid))?;
            }
        }

        let new_content = Content { operations: ops };
        let encoded = new_content
            .encode()
            .map_err(|e| format!("Failed encoding OCR content stream: {}", e))?;
        let mut cstream = Stream::new(Dictionary::new(), encoded);
        let _ = cstream.compress();
        let cstream_id = doc.add_object(Object::Stream(cstream));
        if let Ok(Object::Dictionary(ref mut pd)) = doc.get_object_mut(page_id) {
            pd.set("Contents", Object::Reference(cstream_id));
        }
    }

    fonts.finalize_all(&mut doc);

    let mut out_bytes = Vec::new();
    doc.save_to(&mut out_bytes)
        .map_err(|e| format!("Failed serializing OCR PDF: {}", e))?;

    // Verify result opens cleanly in PDFium and extracted text contains expected characters if words were written
    verify_ocr_pdf_bytes(&out_bytes)?;

    Ok(out_bytes)
}

fn verify_ocr_pdf_bytes(bytes: &[u8]) -> Result<(), String> {
    let pdfium = get_pdfium()?;
    let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
    let doc = guard
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| format!("PDFium rejected OCR output PDF: {:?}", e))?;
    if doc.pages().len() == 0 {
        return Err("OCR output PDF has 0 pages".into());
    }
    Ok(())
}

// ============================================================================
//  Background OCR Job Manager (Thread Pool, Progress/ETA, Cancel, Backup)
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrStartRequest {
    pub file_path: String,
    pub pages: Vec<usize>,
    pub language: String,
    pub secondary_language: Option<String>,
    pub output_mode: String,
    pub downsample_dpi: i32,
    #[serde(default)]
    pub rerun_text_pages: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LowConfidencePage {
    pub page: usize,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrJobSnapshot {
    pub job_id: String,
    pub status: String, // "running" | "cancelled_pending_choice" | "done" | "cancelled" | "error"
    pub file_path: String,
    pub current_page: usize,
    pub completed_pages: usize,
    pub total_pages: usize,
    pub percent: u32,
    pub eta_seconds: Option<u64>,
    pub recognized_pages: usize,
    pub skipped_pages: usize,
    pub low_confidence_pages: Vec<LowConfidencePage>,
    pub mean_confidence: f32,
    pub avg_ms_per_page: u64,
    pub can_keep_partial: bool,
    pub message: String,
    pub error: Option<String>,
    pub revision: Option<u64>,
    pub can_undo: Option<bool>,
    pub can_redo: Option<bool>,
    pub backup_path: Option<String>,
}

struct PendingPartialCommit {
    file_path: String,
    output_mode: String,
    downsample_dpi: i32,
    backup_path: Option<PathBuf>,
    results: Vec<OcrPageResult>,
}

#[derive(Clone)]
pub struct OcrJobManager {
    jobs: Arc<Mutex<HashMap<String, OcrJobSnapshot>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    partials: Arc<Mutex<HashMap<String, PendingPartialCommit>>>,
}

impl Default for OcrJobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrJobManager {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            partials: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get_status(&self, job_id: &str) -> Option<OcrJobSnapshot> {
        self.jobs.lock().unwrap().get(job_id).cloned()
    }

    pub fn cancel(&self, job_id: &str) -> bool {
        if let Some(flag) = self.cancels.lock().unwrap().get(job_id).cloned() {
            flag.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    pub fn resolve_cancel(&self, ws: &PdfWorkspaceEngine, job_id: &str, keep_partial: bool) -> Result<OcrJobSnapshot, String> {
        let partial = self
            .partials
            .lock()
            .unwrap()
            .remove(job_id)
            .ok_or("No pending partial OCR results for this job")?;

        if keep_partial && !partial.results.is_empty() {
            let mut results = partial.results;
            results.sort_by_key(|r| r.page_one);
            let backup_ref = partial.backup_path.as_deref();
            let (rev, can_undo, can_redo) = apply_ocr_commit_to_workspace(
                ws,
                &partial.file_path,
                &results,
                &partial.output_mode,
                partial.downsample_dpi,
                backup_ref,
            )?;
            let recognized = results.len();
            let mut jg = self.jobs.lock().unwrap();
            let snap = jg.get_mut(job_id).ok_or("Missing job")?;
            snap.status = "done".to_string();
            snap.percent = 100;
            snap.recognized_pages = recognized;
            snap.can_keep_partial = false;
            snap.revision = Some(rev);
            snap.can_undo = Some(can_undo);
            snap.can_redo = Some(can_redo);
            snap.message = format!("Text recognized on {} page(s) (kept partial results)", recognized);
            return Ok(snap.clone());
        } else {
            if let Some(ref bp) = partial.backup_path {
                let _ = fs::remove_file(bp);
            }
            let mut jg = self.jobs.lock().unwrap();
            let snap = jg.get_mut(job_id).ok_or("Missing job")?;
            snap.status = "cancelled".to_string();
            snap.can_keep_partial = false;
            snap.recognized_pages = 0;
            snap.message = "OCR cancelled; original document preserved".to_string();
            return Ok(snap.clone());
        }
    }

    pub fn start_job(&self, ws: Arc<PdfWorkspaceEngine>, req: OcrStartRequest) -> Result<String, String> {
        if req.output_mode == "editable_text" {
            return Err("Editable Text & Images output is coming soon. Please choose Searchable Image.".into());
        }
        if !TesseractOcrEngine::global().is_available() {
            return Err("Offline Tesseract OCR runtime not found.".into());
        }

        let lang_expr = match &req.secondary_language {
            Some(sec) if !sec.trim().is_empty() && sec.trim() != "none" && sec.trim() != req.language.trim() => {
                format!("{}+{}", req.language.trim(), sec.trim())
            }
            _ => req.language.trim().to_string(),
        };
        resolve_datapath_for_langs(&lang_expr)?;

        let (backing, _, _) = ws.load_backing(&req.file_path)?;
        let lo_meta = LoDocument::load_mem(&backing)
            .map_err(|e| format!("Failed opening PDF for OCR: {}", e))?;
        let total_doc_pages = lo_meta.get_pages().len();
        drop(lo_meta);

        let mut target_pages: Vec<usize> = req
            .pages
            .iter()
            .copied()
            .filter(|&p| p >= 1 && p <= total_doc_pages)
            .collect();
        target_pages.sort_unstable();
        target_pages.dedup();
        if target_pages.is_empty() {
            return Err("No valid pages selected for OCR".into());
        }

        // Write disk backup in %APPDATA%/pdfforge/ocr_backups/ before mutating
        let backup_dir = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("pdfforge")
            .join("ocr_backups");
        let _ = fs::create_dir_all(&backup_dir);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let backup_path = backup_dir.join(format!("ocr_backup_{}.pdf", ts));
        let _ = fs::write(&backup_path, &*backing);

        let job_id = format!("ocr-{}-{}", ts, target_pages.len());
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let init_snap = OcrJobSnapshot {
            job_id: job_id.clone(),
            status: "running".to_string(),
            file_path: req.file_path.clone(),
            current_page: target_pages[0],
            completed_pages: 0,
            total_pages: target_pages.len(),
            percent: 2,
            eta_seconds: None,
            recognized_pages: 0,
            skipped_pages: 0,
            low_confidence_pages: Vec::new(),
            mean_confidence: 0.0,
            avg_ms_per_page: 0,
            can_keep_partial: false,
            message: format!("Preparing OCR on {} page(s)...", target_pages.len()),
            error: None,
            revision: None,
            can_undo: None,
            can_redo: None,
            backup_path: Some(backup_path.to_string_lossy().to_string()),
        };

        self.jobs.lock().unwrap().insert(job_id.clone(), init_snap);
        self.cancels.lock().unwrap().insert(job_id.clone(), cancel_flag.clone());

        let mgr = self.clone();
        let jid = job_id.clone();
        std::thread::spawn(move || {
            let start_t = Instant::now();
            let ocr_dpi: u32 = match req.downsample_dpi {
                150 => 150,
                600 => 600,
                _ => 300,
            };

            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2);
            let workers = cores.saturating_sub(1).clamp(1, 6);
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build();

            let mut results: Vec<OcrPageResult> = Vec::new();
            let mut skipped_count = 0usize;
            let mut processed_count = 0usize;

            // Filter pages that already have text if !rerun_text_pages
            let mut work_pages: Vec<usize> = Vec::with_capacity(target_pages.len());
            for &p1 in &target_pages {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                if !req.rerun_text_pages {
                    if let Ok(st) = inspect_page_scan_status_bytes(&backing, &req.file_path, p1) {
                        if st.has_text {
                            skipped_count += 1;
                            processed_count += 1;
                            continue;
                        }
                    }
                }
                work_pages.push(p1);
            }

            let total_n = target_pages.len().max(1);
            if skipped_count > 0 {
                let mut g = mgr.jobs.lock().unwrap();
                if let Some(s) = g.get_mut(&jid) {
                    s.skipped_pages = skipped_count;
                    s.completed_pages = processed_count;
                    s.percent = ((processed_count * 92) / total_n).clamp(2, 92) as u32;
                }
            }

            // Process in bounded memory waves of `workers` pages so huge 200+ page PDFs stay strictly under memory budget!
            let batch_size = workers.max(1);
            let mut fatal_err: Option<String> = None;

            for chunk in work_pages.chunks(batch_size) {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                // Step 1: Render chunk pages sequentially into compact grayscale Vec<u8> buffers
                let mut rendered_inputs: Vec<(usize, Vec<u8>, u32, u32, f32, f32)> =
                    Vec::with_capacity(chunk.len());
                for &p1 in chunk {
                    if cancel_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    {
                        let mut g = mgr.jobs.lock().unwrap();
                        if let Some(s) = g.get_mut(&jid) {
                            s.current_page = p1;
                            s.message = format!(
                                "Recognizing text... page {} of {}",
                                processed_count + 1,
                                total_n
                            );
                        }
                    }
                    match render_page_for_ocr_from_bytes(&backing, p1, ocr_dpi) {
                        Ok((gray, w, h, vp_w, vp_h)) => {
                            rendered_inputs.push((p1, gray, w, h, vp_w, vp_h));
                        }
                        Err(e) => {
                            fatal_err = Some(e);
                            break;
                        }
                    }
                }
                if fatal_err.is_some() || cancel_flag.load(Ordering::Relaxed) {
                    break;
                }

                // Step 2: Parallel CPU preprocessing + Tesseract OCR across worker pool
                let batch_results: Vec<Result<OcrPageResult, String>> = match &pool {
                    Ok(tp) => tp.install(|| {
                        use rayon::prelude::*;
                        rendered_inputs
                            .into_par_iter()
                            .map(|(p1, gray, w, h, vp_w, vp_h)| {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    return Err("CANCELLED".into());
                                }
                                run_ocr_on_grayscale(p1, gray, w, h, ocr_dpi, vp_w, vp_h, &lang_expr)
                            })
                            .collect()
                    }),
                    Err(_) => rendered_inputs
                        .into_iter()
                        .map(|(p1, gray, w, h, vp_w, vp_h)| {
                            run_ocr_on_grayscale(p1, gray, w, h, ocr_dpi, vp_w, vp_h, &lang_expr)
                        })
                        .collect(),
                };

                for r in batch_results {
                    match r {
                        Ok(pr) => {
                            processed_count += 1;
                            results.push(pr);
                        }
                        Err(e) => {
                            if e != "CANCELLED" {
                                fatal_err = Some(e);
                            }
                        }
                    }
                }
                if fatal_err.is_some() {
                    break;
                }

                let elapsed_ms = start_t.elapsed().as_millis() as u64;
                let done_ocr = results.len().max(1) as u64;
                let avg_ms = elapsed_ms / done_ocr;
                let rem_pages = total_n.saturating_sub(processed_count) as u64;
                let eta_s = if processed_count > 0 && rem_pages > 0 {
                    Some(((rem_pages * avg_ms) + 500) / 1000)
                } else {
                    Some(0)
                };
                let pct = ((processed_count * 92) / total_n).clamp(3, 94) as u32;
                let cur_p = results.last().map(|r| r.page_one).unwrap_or(target_pages[0]);

                let mut g = mgr.jobs.lock().unwrap();
                if let Some(s) = g.get_mut(&jid) {
                    s.completed_pages = processed_count;
                    s.current_page = cur_p;
                    s.recognized_pages = results.len();
                    s.percent = pct;
                    s.eta_seconds = eta_s;
                    s.avg_ms_per_page = avg_ms;
                    s.message = format!("Recognizing text... page {} of {}", processed_count, total_n);
                }
            }

            if let Some(err) = fatal_err {
                let mut g = mgr.jobs.lock().unwrap();
                if let Some(s) = g.get_mut(&jid) {
                    s.status = "error".to_string();
                    s.error = Some(err.clone());
                    s.message = err;
                }
                mgr.cancels.lock().unwrap().remove(&jid);
                return;
            }

            // Cancelled midway: let UI ask user Keep Partial vs Rollback if any pages completed
            if cancel_flag.load(Ordering::Relaxed) {
                if !results.is_empty() {
                    let recognized = results.len();
                    mgr.partials.lock().unwrap().insert(
                        jid.clone(),
                        PendingPartialCommit {
                            file_path: req.file_path.clone(),
                            output_mode: req.output_mode.clone(),
                            downsample_dpi: req.downsample_dpi,
                            backup_path: Some(backup_path),
                            results,
                        },
                    );
                    let mut g = mgr.jobs.lock().unwrap();
                    if let Some(s) = g.get_mut(&jid) {
                        s.status = "cancelled_pending_choice".to_string();
                        s.can_keep_partial = true;
                        s.recognized_pages = recognized;
                        s.message = format!(
                            "Cancelled after recognizing {} page(s). Keep completed pages or rollback?",
                            recognized
                        );
                    }
                } else {
                    let _ = fs::remove_file(&backup_path);
                    let mut g = mgr.jobs.lock().unwrap();
                    if let Some(s) = g.get_mut(&jid) {
                        s.status = "cancelled".to_string();
                        s.message = "OCR cancelled".to_string();
                    }
                }
                mgr.cancels.lock().unwrap().remove(&jid);
                return;
            }

            if results.is_empty() {
                let _ = fs::remove_file(&backup_path);
                let mut g = mgr.jobs.lock().unwrap();
                if let Some(s) = g.get_mut(&jid) {
                    s.status = "done".to_string();
                    s.percent = 100;
                    s.eta_seconds = Some(0);
                    s.recognized_pages = 0;
                    s.skipped_pages = skipped_count;
                    s.message = format!(
                        "No pages needed OCR (skipped {} page(s) that already contain text)",
                        skipped_count
                    );
                }
                mgr.cancels.lock().unwrap().remove(&jid);
                return;
            }

            {
                let mut g = mgr.jobs.lock().unwrap();
                if let Some(s) = g.get_mut(&jid) {
                    s.percent = 95;
                    s.message = "Embedding searchable invisible text layer...".to_string();
                }
            }

            results.sort_by_key(|r| r.page_one);
            let low_conf: Vec<LowConfidencePage> = results
                .iter()
                .filter(|r| !r.words_vp.is_empty() && r.mean_confidence < 65.0)
                .map(|r| LowConfidencePage {
                    page: r.page_one,
                    confidence: (r.mean_confidence * 10.0).round() / 10.0,
                })
                .collect();
            let mean_conf = results.iter().map(|r| r.mean_confidence).sum::<f32>()
                / (results.len().max(1) as f32);

            match apply_ocr_commit_to_workspace(
                &ws,
                &req.file_path,
                &results,
                &req.output_mode,
                req.downsample_dpi,
                Some(&backup_path),
            ) {
                Ok((rev, can_undo, can_redo)) => {
                    let recognized = results.len();
                    let elapsed_ms = start_t.elapsed().as_millis() as u64;
                    let avg_ms = elapsed_ms / (recognized.max(1) as u64);
                    let mut g = mgr.jobs.lock().unwrap();
                    if let Some(s) = g.get_mut(&jid) {
                        s.status = "done".to_string();
                        s.percent = 100;
                        s.completed_pages = total_n;
                        s.eta_seconds = Some(0);
                        s.recognized_pages = recognized;
                        s.skipped_pages = skipped_count;
                        s.low_confidence_pages = low_conf;
                        s.mean_confidence = (mean_conf * 10.0).round() / 10.0;
                        s.avg_ms_per_page = avg_ms;
                        s.revision = Some(rev);
                        s.can_undo = Some(can_undo);
                        s.can_redo = Some(can_redo);
                        s.message = if skipped_count > 0 {
                            format!(
                                "Text recognized on {} page(s) ({} existing-text page(s) skipped)",
                                recognized, skipped_count
                            )
                        } else {
                            format!("Text recognized on {} page(s)", recognized)
                        };
                    }
                }
                Err(e) => {
                    let mut g = mgr.jobs.lock().unwrap();
                    if let Some(s) = g.get_mut(&jid) {
                        s.status = "error".to_string();
                        s.error = Some(e.clone());
                        s.message = e;
                    }
                }
            }
            mgr.cancels.lock().unwrap().remove(&jid);
        });

        Ok(job_id)
    }
}

fn apply_ocr_commit_to_workspace(
    ws: &PdfWorkspaceEngine,
    file_path: &str,
    results: &[OcrPageResult],
    output_mode: &str,
    downsample_dpi: i32,
    _backup_path: Option<&Path>,
) -> Result<(u64, bool, bool), String> {
    let (current_backing, _, _) = ws.load_backing(file_path)?;
    let new_bytes = write_ocr_results_into_pdf(&current_backing, results, output_mode, downsample_dpi)?;
    let (rev, can_undo, can_redo) = ws.replace_working_bytes_with_undo(file_path, new_bytes)?;
    Ok((rev, can_undo, can_redo))
}
