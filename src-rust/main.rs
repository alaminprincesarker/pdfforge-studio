#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dialogs;
mod font_shaper;
mod installer;
mod pdf_annots;
mod pdf_engine;
mod pdf_layout;
mod pdf_objects;
#[cfg(test)]
mod pdf_tests;
mod single_instance;
mod store;

use http::{header::CONTENT_TYPE, Response, StatusCode};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use std::borrow::Cow;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::{Fullscreen, WindowBuilder},
};
use wry::{RequestAsyncResponder, WebContext, WebViewBuilder};
#[cfg(target_os = "windows")]
use wry::WebViewBuilderExtWindows;

use crate::pdf_layout::PdfWorkspaceEngine;
use crate::pdf_objects::{commit_edit_text_block, commit_shape_operation, EditTextCommitRequest, ShapeCommitRequest};
use crate::store::{AppDataStore, FileState, PartialAppSettings, RecentFile, SessionState};

#[derive(RustEmbed)]
#[folder = "src-rust/assets/"]
struct Assets;

pub(crate) const WEBVIEW2_LOADER_BYTES: &[u8] = include_bytes!("WebView2Loader.dll");
#[cfg(target_os = "windows")]
pub(crate) const PDFIUM_DLL_BYTES: &[u8] = include_bytes!("pdfium.dll");
const ICON_RGBA_BYTES: &[u8] = include_bytes!("icon_rgba.bin");

#[cfg(target_os = "windows")]
fn ensure_pdfium_dll_present(app_data_dir: &std::path::Path) {
    let _ = fs::create_dir_all(app_data_dir);
    let dll_path = app_data_dir.join("pdfium.dll");
    if !dll_path.exists() {
        let _ = fs::write(&dll_path, PDFIUM_DLL_BYTES);
    }
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let local_dll = exe_dir.join("pdfium.dll");
            if !local_dll.exists() {
                let _ = fs::write(&local_dll, PDFIUM_DLL_BYTES);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn ensure_webview2_loader_loaded(app_data_dir: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn LoadLibraryW(lpLibFileName: *const u16) -> *mut core::ffi::c_void;
    }

    let _ = fs::create_dir_all(app_data_dir);
    let dll_path = app_data_dir.join("WebView2Loader.dll");
    if !dll_path.exists() {
        let _ = fs::write(&dll_path, WEBVIEW2_LOADER_BYTES);
    }

    // Also try placing a copy next to the executable if writable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let local_dll = exe_dir.join("WebView2Loader.dll");
            if !local_dll.exists() {
                let _ = fs::write(&local_dll, WEBVIEW2_LOADER_BYTES);
            }
        }
    }

    let wide: Vec<u16> = dll_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = LoadLibraryW(wide.as_ptr());
    }
}

#[cfg(target_os = "windows")]
mod win32_frame {
    use core::ffi::c_void;
    use tao::platform::windows::WindowExtWindows;

    const DWMWA_TRANSITIONS_FORCEDISABLED: u32 = 3;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xFFFFFFFE;
    const DWMWCP_DEFAULT: i32 = 0;
    const DWMWCP_DONOTROUND: i32 = 1;
    const GCLP_HBRBACKGROUND: i32 = -10;
    const SW_HIDE: i32 = 0;

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            dwAttribute: u32,
            pvAttribute: *const c_void,
            cbAttribute: u32,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateSolidBrush(color: u32) -> isize;
    }

    extern "system" {
        fn SetClassLongPtrW(hwnd: isize, nIndex: i32, dwNewLong: isize) -> isize;
        fn ShowWindow(hwnd: isize, nCmdShow: i32) -> i32;
    }

    pub fn apply_frameless_appearance(window: &tao::window::Window, sharp_corners: bool) {
        let hwnd = window.hwnd() as isize;
        if hwnd == 0 {
            return;
        }
        unsafe {
            let dark: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const i32 as *const c_void,
                4,
            );
            let border_none: u32 = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border_none as *const u32 as *const c_void,
                4,
            );
            let corner_pref: i32 = if sharp_corners {
                DWMWCP_DONOTROUND
            } else {
                DWMWCP_DEFAULT
            };
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner_pref as *const i32 as *const c_void,
                4,
            );
            // #1e1e1e in BGR COLORREF so non-client/background never flashes white or classic gray
            let dark_brush = CreateSolidBrush(0x001E1E1E);
            if dark_brush != 0 {
                let _ = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, dark_brush);
            }
        }
    }

    pub fn set_transitions_disabled(window: &tao::window::Window, disabled: bool) {
        let hwnd = window.hwnd() as isize;
        if hwnd == 0 {
            return;
        }
        let val: i32 = if disabled { 1 } else { 0 };
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                &val as *const i32 as *const c_void,
                4,
            );
        }
    }

    pub fn hide_instantly_before_close(window: &tao::window::Window) {
        let hwnd = window.hwnd() as isize;
        if hwnd == 0 {
            return;
        }
        unsafe {
            let val: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                &val as *const i32 as *const c_void,
                4,
            );
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[derive(Debug, Clone)]
enum UserEvent {
    ShowMainWindow,
    Minimize,
    Maximize,
    Unmaximize,
    Close,
    ClosePromptActive,
    ConfirmClose(bool, Option<SessionState>),
    CloseTimeoutFallback,
    ToggleFullScreen,
    SetFullScreen(bool),
    DragWindow,
    DragEnter,
    DragLeave,
    DropFiles(Vec<String>),
    OpenFiles(Vec<String>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathPayload {
    file_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveDialogPayload {
    default_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenMultipleDialogPayload {
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveFileStatePayload {
    file_path: String,
    state: FileState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSignaturePayload {
    id: String,
    base64_png: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteSignaturePayload {
    id: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MailtoPayload {
    subject: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IpcWindowCommand {
    cmd: String,
    value: Option<bool>,
    proceed: Option<bool>,
    session: Option<SessionState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectPrintPayload {
    printer_name: String,
    #[serde(default = "default_print_copies")]
    copies: u16,
    #[serde(default)]
    orientation: String,
    #[serde(default)]
    paper_size: String,
    #[serde(default = "default_duplex_mode")]
    duplex: String,
    #[serde(default = "default_scale_mode")]
    scale_mode: String,
    #[serde(default = "default_custom_scale")]
    custom_scale: f32,
    output_pdf_path: Option<String>,
    pages: Vec<String>,
}

fn default_print_copies() -> u16 {
    1
}

fn default_duplex_mode() -> String {
    "simplex".to_string()
}

fn default_scale_mode() -> String {
    "fit".to_string()
}

fn default_custom_scale() -> f32 {
    100.0
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatPagePayload {
    file_path: String,
    page: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatHitPayload {
    file_path: String,
    page: usize,
    x: f32,
    y: f32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatSelectionPayload {
    file_path: String,
    page: usize,
    start_char: usize,
    end_char: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatShapeReflowPayload {
    file_path: String,
    page: usize,
    text: String,
    original_font: String,
    font_family: String,
    font_size: f32,
    max_width: f32,
    bold: bool,
    italic: bool,
    align: String,
    line_spacing: f32,
    bullets: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatEditTextPayload {
    file_path: String,
    #[serde(flatten)]
    req: EditTextCommitRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatShapeCommitPayload {
    file_path: String,
    #[serde(flatten)]
    req: ShapeCommitRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatImageCommitPayload {
    file_path: String,
    #[serde(flatten)]
    req: pdf_objects::ImageCommitRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatAnnotationCommitPayload {
    file_path: String,
    #[serde(flatten)]
    req: pdf_annots::CommitAnnotationRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatFormFieldPayload {
    file_path: String,
    #[serde(flatten)]
    req: pdf_annots::UpdateFormFieldRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatRotatePagesPayload {
    file_path: String,
    pages: Vec<usize>,
    delta_deg: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatRedactPayload {
    file_path: String,
    page: usize,
    rects_vp: Vec<[f32; 4]>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatDocPropsPayload {
    file_path: String,
    #[serde(flatten)]
    props: pdf_annots::DocPropertiesDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatBookmarksPayload {
    file_path: String,
    bookmarks: Vec<pdf_annots::BookmarkDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatPageLabelsPayload {
    file_path: String,
    style: String,
    prefix: String,
    start_num: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatCropPagesPayload {
    file_path: String,
    pages: Vec<usize>,
    margins: [f32; 4],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatPageOpPayload {
    file_path: String,
    #[serde(flatten)]
    req: pdf_annots::PageOpsRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatExtractPagesPayload {
    file_path: String,
    pages: Vec<usize>,
    output_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatCombinePayload {
    paths: Vec<String>,
    output_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcrobatSaveVerifiedPayload {
    file_path: String,
    target_path: Option<String>,
    #[serde(default)]
    incremental: bool,
    #[serde(default = "default_compact_save")]
    compact: bool,
}

fn default_compact_save() -> bool {
    true
}

fn guess_mime_type(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if lower.ends_with(".js") || lower.ends_with(".mjs") {
        "text/javascript; charset=utf-8"
    } else if lower.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".ico") {
        "image/x-icon"
    } else if lower.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if lower.ends_with(".woff2") {
        "font/woff2"
    } else if lower.ends_with(".woff") {
        "font/woff"
    } else if lower.ends_with(".ttf") {
        "font/ttf"
    } else if lower.ends_with(".bcmap") {
        "application/octet-stream"
    } else {
        "application/octet-stream"
    }
}

fn json_response<T: serde::Serialize>(status: StatusCode, body: &T) -> Response<Cow<'static, [u8]>> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "*")
        .header("Access-Control-Expose-Headers", "X-File-Name, X-File-Size")
        .body(Cow::Owned(bytes))
        .unwrap()
}

fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn binary_response(
    status: StatusCode,
    bytes: Vec<u8>,
    file_name: &str,
    size: u64,
) -> Response<Cow<'static, [u8]>> {
    let encoded_name = url_encode(file_name);
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/octet-stream")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Expose-Headers", "X-File-Name, X-File-Size")
        .header("X-File-Name", encoded_name)
        .header("X-File-Size", size.to_string())
        .body(Cow::Owned(bytes))
        .unwrap()
}

const INIT_BRIDGE_SCRIPT: &str = r#"
(function () {
  if (window.electronAPI) return;

  window.__pdfforgeState = {
    isMaximized: true,
    isFullScreen: false,
    maxListeners: new Set(),
    fsListeners: new Set(),
    openFileListeners: new Set(),
    menuActionListeners: new Set(),
    pendingOpenFile: null,
    pendingOpenFiles: [],
  };

  window.__pdfforgeNotifyMaximized = function (val) {
    window.__pdfforgeState.isMaximized = !!val;
    window.__pdfforgeState.maxListeners.forEach(function (cb) {
      try { cb(!!val); } catch (e) {}
    });
  };

  window.__pdfforgeNotifyFullScreen = function (val) {
    window.__pdfforgeState.isFullScreen = !!val;
    window.__pdfforgeState.fsListeners.forEach(function (cb) {
      try { cb(!!val); } catch (e) {}
    });
  };

  window.__pdfforgeNotifyOpenFile = function (filePath) {
    if (!filePath) return;
    if (window.__pdfforgeState.openFileListeners.size === 0) {
      window.__pdfforgeState.pendingOpenFile = filePath;
      if (!window.__pdfforgeState.pendingOpenFiles.includes(filePath)) {
        window.__pdfforgeState.pendingOpenFiles.push(filePath);
      }
    } else {
      window.__pdfforgeState.openFileListeners.forEach(function (cb) {
        try { cb(filePath); } catch (e) {}
      });
    }
  };

  window.__pdfforgeNotifyOpenFiles = function (paths) {
    if (!Array.isArray(paths) || paths.length === 0) return;
    for (var i = 0; i < paths.length; i++) {
      window.__pdfforgeNotifyOpenFile(paths[i]);
    }
  };

  async function apiPostJson(endpoint, payload) {
    const res = await fetch('/__rust_api__' + endpoint, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload || {}),
    });
    return await res.json();
  }

  function sendWindowCmd(cmd, value) {
    if (window.ipc && typeof window.ipc.postMessage === 'function') {
      window.ipc.postMessage(JSON.stringify({ cmd: cmd, value: value }));
    }
  }

  window.electronAPI = {
    // Window controls
    minimizeWindow: async function () {
      sendWindowCmd('minimize');
    },
    maximizeWindow: async function () {
      sendWindowCmd('maximize');
    },
    unmaximizeWindow: async function () {
      sendWindowCmd('unmaximize');
    },
    closeWindow: async function () {
      if (typeof window.__pdfforgeRequestClose === 'function') {
        window.__pdfforgeRequestClose();
      } else {
        sendWindowCmd('close');
      }
    },
    confirmClose: function (proceed) {
      sendWindowCmd('confirmClose', !!proceed);
    },
    startDragWindow: function () {
      sendWindowCmd('dragWindow');
    },
    isWindowMaximized: async function () {
      return !!window.__pdfforgeState.isMaximized;
    },
    printDocument: async function () {
      window.dispatchEvent(new CustomEvent('pdfforge-open-print'));
    },
    listPrinters: async function () {
      return await apiPostJson('/print/listPrinters', {});
    },
    printDirect: async function (opts) {
      return await apiPostJson('/print/direct', opts || {});
    },
    toggleFullScreen: async function () {
      const next = !window.__pdfforgeState.isFullScreen;
      window.__pdfforgeState.isFullScreen = next;
      sendWindowCmd('setFullScreen', next);
      return next;
    },
    setFullScreen: async function (next) {
      const flag = !!next;
      window.__pdfforgeState.isFullScreen = flag;
      sendWindowCmd('setFullScreen', flag);
      return flag;
    },
    isFullScreen: async function () {
      return !!window.__pdfforgeState.isFullScreen;
    },
    onFullScreenChange: function (callback) {
      window.__pdfforgeState.fsListeners.add(callback);
      return function () {
        window.__pdfforgeState.fsListeners.delete(callback);
      };
    },
    onWindowStateChange: function (callback) {
      window.__pdfforgeState.maxListeners.add(callback);
      return function () {
        window.__pdfforgeState.maxListeners.delete(callback);
      };
    },

    // File operations
    openFileDialog: async function () {
      return await apiPostJson('/dialog/openFile', {});
    },
    openMultipleFilesDialog: async function (title) {
      return await apiPostJson('/dialog/openMultipleFiles', title ? { title: title } : {});
    },
    getPathForFile: function (file) {
      return (file && file.path) || '';
    },
    resolveDroppedFile: async function (file) {
      if (!file) return '';
      if (file.path) return file.path;
      const buf = await file.arrayBuffer();
      const res = await fetch(
        '/__rust_api__/file/cacheDroppedFile?name=' + encodeURIComponent(file.name || 'Dropped.pdf'),
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/octet-stream' },
          body: buf,
        }
      );
      const data = await res.json();
      return data.filePath || '';
    },
    readFile: async function (filePath) {
      const res = await fetch('/__rust_api__/file/read', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ filePath: filePath }),
      });
      if (!res.ok) {
        const errText = await res.text();
        throw new Error(errText || 'Failed to read PDF file');
      }
      const buffer = await res.arrayBuffer();
      const rawName = res.headers.get('X-File-Name') || 'document.pdf';
      const fileName = decodeURIComponent(rawName);
      const sizeHeader = res.headers.get('X-File-Size');
      const size = sizeHeader ? Number(sizeHeader) : buffer.byteLength;
      return {
        data: buffer,
        fileName: fileName,
        size: size,
      };
    },
    saveFile: async function (filePath, data) {
      let payload = data;
      if (data && data.buffer instanceof ArrayBuffer && typeof data.byteOffset === 'number') {
        payload = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
      }
      const res = await fetch(
        '/__rust_api__/file/save?path=' + encodeURIComponent(filePath),
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/octet-stream' },
          body: payload,
        }
      );
      return await res.json();
    },
    saveFileDialog: async function (defaultName) {
      return await apiPostJson('/dialog/saveFileDialog', { defaultName: defaultName });
    },
    showInFolder: async function (filePath) {
      await apiPostJson('/file/showInFolder', { filePath: filePath });
    },

    // Offline Store
    getRecentFiles: async function () {
      return await apiPostJson('/store/getRecentFiles', {});
    },
    addRecentFile: async function (file) {
      return await apiPostJson('/store/addRecentFile', file);
    },
    removeRecentFile: async function (filePath) {
      return await apiPostJson('/store/removeRecentFile', { filePath: filePath });
    },
    clearRecentFiles: async function () {
      await apiPostJson('/store/clearRecentFiles', {});
    },
    toggleFavoriteFile: async function (filePath) {
      return await apiPostJson('/store/toggleFavoriteFile', { filePath: filePath });
    },
    getSettings: async function () {
      return await apiPostJson('/store/getSettings', {});
    },
    updateSettings: async function (settings) {
      return await apiPostJson('/store/updateSettings', settings);
    },
    getFileState: async function (filePath) {
      return await apiPostJson('/store/getFileState', { filePath: filePath });
    },
    saveFileState: async function (filePath, state) {
      await apiPostJson('/store/saveFileState', { filePath: filePath, state: state });
    },
    saveSession: async function (session) {
      await apiPostJson('/store/saveSession', session || {});
    },
    getSession: async function () {
      return await apiPostJson('/store/getSession', {});
    },
    notifyWindowReady: function () {
      sendWindowCmd('windowReady');
    },
    listSignatures: async function () {
      return await apiPostJson('/signatures/list', {});
    },
    saveSignature: async function (id, base64Png) {
      return await apiPostJson('/signatures/save', { id: id, base64Png: base64Png });
    },
    deleteSignature: async function (id) {
      return await apiPostJson('/signatures/delete', { id: id });
    },

    // System & App Info
    getAppVersion: async function () {
      try {
        const info = await apiPostJson('/app/getAppInfo', {});
        if (info && info.version) return info.version;
      } catch (e) {}
      return '1.2.3';
    },
    appReady: async function () {
      return await apiPostJson('/app/ready', {});
    },
    getAppInfo: async function () {
      return await apiPostJson('/app/getAppInfo', {});
    },
    openMailto: async function (subject, to) {
      if (subject && typeof subject === 'object') {
        await apiPostJson('/app/openMailto', subject);
      } else {
        await apiPostJson('/app/openMailto', { subject: subject, to: to });
      }
    },
    openExternalUrl: async function (url) {
      return await apiPostJson('/app/openExternalUrl', { url: url });
    },
    pickUpdatePackage: async function () {
      return await apiPostJson('/app/pickUpdatePackage', {});
    },
    applyUpdatePackage: async function (filePath) {
      return await apiPostJson('/app/applyUpdatePackage', { filePath: filePath });
    },
    checkForUpdates: async function (manifestUrl) {
      return await apiPostJson('/app/checkForUpdates', { manifestUrl: manifestUrl || '' });
    },
    downloadAndApplyUpdate: async function (downloadUrl) {
      return await apiPostJson('/app/downloadAndApplyUpdate', { downloadUrl: downloadUrl || '' });
    },
    repairInstallation: async function () {
      return await apiPostJson('/app/repairInstallation', {});
    },
    uninstallApp: async function (opts) {
      return await apiPostJson('/app/uninstallApp', opts || {});
    },
    quitApp: async function () {
      sendWindowCmd('close');
    },
    openNewWindow: async function (filePath) {
      return await apiPostJson('/app/openNewWindow', { filePath: filePath || '' });
    },

    // Acrobat Pro Offline Document Model, Shaping & Stream Editing
    getPageLayout: async function (filePath, page) {
      return await apiPostJson('/acrobat/getPageLayout', { filePath: filePath, page: page });
    },
    hitTest: async function (filePath, page, x, y) {
      return await apiPostJson('/acrobat/hitTest', { filePath: filePath, page: page, x: x, y: y });
    },
    getSelectionRects: async function (filePath, page, startChar, endChar) {
      return await apiPostJson('/acrobat/getSelectionRects', {
        filePath: filePath,
        page: page,
        startChar: startChar,
        endChar: endChar,
      });
    },
    getObjects: async function (filePath, page) {
      return await apiPostJson('/acrobat/getObjects', { filePath: filePath, page: page });
    },
    shapeReflowBlock: async function (payload) {
      return await apiPostJson('/acrobat/shapeReflow', payload);
    },
    commitEditText: async function (payload) {
      return await apiPostJson('/acrobat/commitEditText', payload);
    },
    commitShapeEdit: async function (payload) {
      return await apiPostJson('/acrobat/commitShape', payload);
    },
    undoPdfEdit: async function (filePath) {
      return await apiPostJson('/acrobat/undo', { filePath: filePath });
    },
    redoPdfEdit: async function (filePath) {
      return await apiPostJson('/acrobat/redo', { filePath: filePath });
    },
    getWorkingPdf: async function (filePath) {
      const res = await fetch('/__rust_api__/acrobat/getWorkingPdf', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ filePath: filePath }),
      });
      if (!res.ok) throw new Error('Failed to fetch working PDF bytes');
      return await res.arrayBuffer();
    },
    listSystemFonts: async function () {
      return await apiPostJson('/acrobat/listFonts', {});
    },

    // Listeners
    onFileOpenRequest: function (callback) {
      window.__pdfforgeState.openFileListeners.add(callback);
      const pendingList = Array.isArray(window.__pdfforgeState.pendingOpenFiles)
        ? window.__pdfforgeState.pendingOpenFiles.slice()
        : [];
      if (
        window.__pdfforgeState.pendingOpenFile &&
        !pendingList.includes(window.__pdfforgeState.pendingOpenFile)
      ) {
        pendingList.unshift(window.__pdfforgeState.pendingOpenFile);
      }
      window.__pdfforgeState.pendingOpenFile = null;
      window.__pdfforgeState.pendingOpenFiles = [];
      if (pendingList.length > 0) {
        setTimeout(function () {
          for (var i = 0; i < pendingList.length; i++) {
            try { callback(pendingList[i]); } catch (e) {}
          }
        }, 150);
      }
      // Flush any multi-click / IPC files queued in Rust before React mounted
      apiPostJson('/app/ready', {}).then(function (res) {
        if (res && Array.isArray(res.pendingFiles) && res.pendingFiles.length > 0) {
          window.__pdfforgeNotifyOpenFiles(res.pendingFiles);
        }
      }).catch(function () {});
      return function () {
        window.__pdfforgeState.openFileListeners.delete(callback);
      };
    },
    onMenuAction: function (callback) {
      window.__pdfforgeState.menuActionListeners.add(callback);
      return function () {
        window.__pdfforgeState.menuActionListeners.delete(callback);
      };
    },
  };

  // Prevent WebView2 from zooming the whole app UI on touchpad pinch;
  // ViewerCanvas handles Ctrl+Wheel / touchpad pinch directly on the PDF document.
  // Use capture:true with ONLY preventDefault() (never stopPropagation()) so React still receives all events.
  window.addEventListener(
    'wheel',
    function (e) {
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
      }
    },
    { passive: false, capture: true }
  );

  // Desktop software behavior: suppress default browser context menu outside text inputs.
  window.addEventListener(
    'contextmenu',
    function (e) {
      if (e.defaultPrevented) return;
      var t = e.target;
      var tag = t && t.tagName ? t.tagName.toUpperCase() : '';
      if (tag === 'INPUT' || tag === 'TEXTAREA' || (t && t.isContentEditable)) return;
      e.preventDefault();
    },
    { capture: false }
  );

  // Desktop accelerator protection: prevent browser devtools/view-source/reload keys
  // and ensure Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+PageUp / Ctrl+PageDown stay app-handled.
  // Also block native WebView2 browser zoom hotkeys (Ctrl+Plus/Minus/0) while letting React zoom the PDF.
  window.addEventListener(
    'keydown',
    function (e) {
      var k = (e.key || '').toLowerCase();
      if (
        e.key === 'F12' ||
        e.key === 'F5' ||
        ((e.ctrlKey || e.metaKey) && e.shiftKey && (k === 'i' || k === 'j' || k === 'c')) ||
        ((e.ctrlKey || e.metaKey) && !e.shiftKey && (k === 'u' || k === 'r'))
      ) {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey && (k === '=' || k === '+' || k === '-' || k === '_' || k === '0')) {
        e.preventDefault();
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key === 'Tab' || e.key === 'PageUp' || e.key === 'PageDown')) {
        window.dispatchEvent(new CustomEvent('pdfforge-tab-nav-key', {
          detail: { key: e.key, shiftKey: !!e.shiftKey }
        }));
        e.preventDefault();
        e.stopPropagation();
        if (typeof e.stopImmediatePropagation === 'function') e.stopImmediatePropagation();
      }
    },
    { capture: true }
  );

  window.pdfAcrobat = {
    getPageLayout: function (fp, page) { return window.electronAPI.getPageLayout(fp, page); },
    hitTest: function (fp, page, x, y) { return window.electronAPI.hitTest(fp, page, x, y); },
    getSelectionRects: function (fp, page, s, e) { return window.electronAPI.getSelectionRects(fp, page, s, e); },
    getObjects: function (fp, page) { return window.electronAPI.getObjects(fp, page); },
    shapeReflowBlock: function (p) { return window.electronAPI.shapeReflowBlock(p); },
    commitEditText: function (p) { return window.electronAPI.commitEditText(p); },
    commitShapeEdit: function (p) { return window.electronAPI.commitShapeEdit(p); },
    commitImageEdit: function (p) { return apiPostJson('/acrobat/commitImage', p); },
    getAnnotations: function (fp, page) { return apiPostJson('/acrobat/getAnnotations', { filePath: fp, page: page }); },
    commitAnnotation: function (p) { return apiPostJson('/acrobat/commitAnnotation', p); },
    updateFormField: function (p) { return apiPostJson('/acrobat/updateFormField', p); },
    rotatePages: function (fp, pages, deltaDeg) { return apiPostJson('/acrobat/rotatePages', { filePath: fp, pages: pages, deltaDeg: deltaDeg }); },
    applyRedaction: function (fp, page, rectsVp) { return apiPostJson('/acrobat/applyRedaction', { filePath: fp, page: page, rectsVp: rectsVp }); },
    getDocProperties: function (fp) { return apiPostJson('/acrobat/getDocProperties', { filePath: fp }); },
    setDocProperties: function (p) { return apiPostJson('/acrobat/setDocProperties', p); },
    listBookmarks: function (fp) { return apiPostJson('/acrobat/listBookmarks', { filePath: fp }); },
    writeBookmarks: function (fp, bms) { return apiPostJson('/acrobat/writeBookmarks', { filePath: fp, bookmarks: bms }); },
    setPageLabels: function (p) { return apiPostJson('/acrobat/setPageLabels', p); },
    cropPages: function (p) { return apiPostJson('/acrobat/cropPages', p); },
    pageOp: function (p) { return apiPostJson('/acrobat/pageOp', p); },
    extractPages: function (p) { return apiPostJson('/acrobat/extractPages', p); },
    combinePdfs: function (p) { return apiPostJson('/acrobat/combinePdfs', p); },
    saveVerified: function (p) { return apiPostJson('/acrobat/saveVerified', p); },
    verifyPdf: function (fp) { return apiPostJson('/acrobat/verifyPdf', { filePath: fp }); },
    undo: function (fp) { return window.electronAPI.undoPdfEdit(fp); },
    redo: function (fp) { return window.electronAPI.redoPdfEdit(fp); },
    getWorkingPdf: function (fp) { return window.electronAPI.getWorkingPdf(fp); },
    listSystemFonts: function () { return window.electronAPI.listSystemFonts(); }
  };

  window.addEventListener('error', function (e) {
    fetch('/__rust_api__/diag', {
      method: 'POST',
      body: 'JS_ERROR: ' + (e.message || '') + ' at ' + (e.filename || '') + ':' + (e.lineno || '') + ':' + (e.colno || '') + ' stack=' + (e.error && e.error.stack ? e.error.stack : ''),
    }).catch(function () {});
  });
  window.addEventListener('unhandledrejection', function (e) {
    fetch('/__rust_api__/diag', {
      method: 'POST',
      body: 'UNHANDLED_REJECTION: ' + String(e.reason && (e.reason.stack || e.reason.message || e.reason)),
    }).catch(function () {});
  });
  const origConsoleError = console.error;
  console.error = function () {
    try {
      const msg = Array.from(arguments).map(function (a) { return a && a.stack ? a.stack : String(a); }).join(' ');
      fetch('/__rust_api__/diag', { method: 'POST', body: 'CONSOLE_ERROR: ' + msg }).catch(function () {});
    } catch (e) {}
    origConsoleError.apply(console, arguments);
  };
})();
"#;

fn handle_api_request(
    uri_path: &str,
    query: Option<&str>,
    body: Vec<u8>,
    store: Arc<Mutex<AppDataStore>>,
    pending_boot_open_files: Arc<Mutex<Vec<String>>>,
    webview_ready: Arc<AtomicBool>,
    pdf_workspace: PdfWorkspaceEngine,
    responder: RequestAsyncResponder,
) {
    match uri_path {
        "/__rust_api__/dialog/openFile" => {
            thread::spawn(move || {
                let res = dialogs::open_file_dialog();
                responder.respond(json_response(StatusCode::OK, &res));
            });
        }
        "/__rust_api__/dialog/openMultipleFiles" => {
            let payload: Option<OpenMultipleDialogPayload> = serde_json::from_slice(&body).ok();
            let title = payload.and_then(|p| p.title);
            thread::spawn(move || {
                let res = dialogs::open_multiple_files_dialog_titled(title.as_deref());
                responder.respond(json_response(StatusCode::OK, &res));
            });
        }
        "/__rust_api__/dialog/saveFileDialog" => {
            let payload: Option<SaveDialogPayload> = serde_json::from_slice(&body).ok();
            let default_name = payload.and_then(|p| p.default_name);
            thread::spawn(move || {
                let res = dialogs::save_file_dialog(default_name.as_deref());
                responder.respond(json_response(StatusCode::OK, &res));
            });
        }
        "/__rust_api__/file/read" => {
            let payload: Result<PathPayload, _> = serde_json::from_slice(&body);
            match payload {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || match ws.load_or_sync_from_disk(&p.file_path) {
                        Ok(bytes) => {
                            let file_name = std::path::Path::new(&p.file_path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "document.pdf".to_string());
                            let sz = bytes.len() as u64;
                            responder.respond(binary_response(
                                StatusCode::OK,
                                bytes,
                                &file_name,
                                sz,
                            ));
                        }
                        Err(err) => {
                            responder.respond(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "error": err }),
                            ));
                        }
                    });
                }
                Err(err) => {
                    responder.respond(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": err.to_string() }),
                    ));
                }
            }
        }
        "/__rust_api__/file/save" => {
            let file_path = query
                .and_then(|q| {
                    q.split('&')
                        .find_map(|pair| pair.strip_prefix("path="))
                        .map(url_decode)
                })
                .unwrap_or_default();

            if file_path.is_empty() {
                responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "success": false, "error": "Missing file path" }),
                ));
                return;
            }

            let ws = pdf_workspace.clone();
            thread::spawn(move || {
                let res = pdf_engine::save_pdf_file(&file_path, &body);
                if res.success {
                    ws.register_bytes_for_path(&file_path, body);
                }
                responder.respond(json_response(StatusCode::OK, &res));
            });
        }
        "/__rust_api__/file/cacheDroppedFile" => {
            let file_name = query
                .and_then(|q| {
                    q.split('&')
                        .find_map(|pair| pair.strip_prefix("name="))
                        .map(url_decode)
                })
                .unwrap_or_else(|| "Dropped_Document.pdf".to_string());

            let safe_name: String = file_name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ' ' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();

            let cache_dir = {
                let guard = store.lock().unwrap();
                guard.user_data_dir().join("autosave")
            };
            let _ = fs::create_dir_all(&cache_dir);
            let target_path = cache_dir.join(safe_name);
            let res_path = if fs::write(&target_path, &body).is_ok() {
                target_path.to_string_lossy().to_string()
            } else {
                String::new()
            };
            responder.respond(json_response(
                StatusCode::OK,
                &json!({ "filePath": res_path }),
            ));
        }
        "/__rust_api__/file/showInFolder" => {
            if let Ok(p) = serde_json::from_slice::<PathPayload>(&body) {
                dialogs::show_in_folder(&p.file_path);
            }
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": true })));
        }
        "/__rust_api__/store/getRecentFiles" => {
            let list = store.lock().unwrap().get_recent_files();
            responder.respond(json_response(StatusCode::OK, &list));
        }
        "/__rust_api__/store/addRecentFile" => {
            if let Ok(file) = serde_json::from_slice::<RecentFile>(&body) {
                let list = store.lock().unwrap().add_recent_file(file);
                responder.respond(json_response(StatusCode::OK, &list));
            } else {
                let list = store.lock().unwrap().get_recent_files();
                responder.respond(json_response(StatusCode::OK, &list));
            }
        }
        "/__rust_api__/store/removeRecentFile" => {
            if let Ok(p) = serde_json::from_slice::<PathPayload>(&body) {
                let list = store.lock().unwrap().remove_recent_file(&p.file_path);
                responder.respond(json_response(StatusCode::OK, &list));
            } else {
                let list = store.lock().unwrap().get_recent_files();
                responder.respond(json_response(StatusCode::OK, &list));
            }
        }
        "/__rust_api__/store/clearRecentFiles" => {
            store.lock().unwrap().clear_recent_files();
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": true })));
        }
        "/__rust_api__/store/toggleFavoriteFile" => {
            if let Ok(p) = serde_json::from_slice::<PathPayload>(&body) {
                let list = store.lock().unwrap().toggle_favorite_file(&p.file_path);
                responder.respond(json_response(StatusCode::OK, &list));
            } else {
                let list = store.lock().unwrap().get_recent_files();
                responder.respond(json_response(StatusCode::OK, &list));
            }
        }
        "/__rust_api__/store/getSettings" => {
            let settings = store.lock().unwrap().get_settings();
            responder.respond(json_response(StatusCode::OK, &settings));
        }
        "/__rust_api__/store/updateSettings" => {
            if let Ok(partial) = serde_json::from_slice::<PartialAppSettings>(&body) {
                let updated = store.lock().unwrap().update_settings(partial);
                responder.respond(json_response(StatusCode::OK, &updated));
            } else {
                let settings = store.lock().unwrap().get_settings();
                responder.respond(json_response(StatusCode::OK, &settings));
            }
        }
        "/__rust_api__/store/getFileState" => {
            if let Ok(p) = serde_json::from_slice::<PathPayload>(&body) {
                let state = store.lock().unwrap().get_file_state(&p.file_path);
                responder.respond(json_response(StatusCode::OK, &state));
            } else {
                responder.respond(json_response(StatusCode::OK, &Option::<FileState>::None));
            }
        }
        "/__rust_api__/store/saveFileState" => {
            if let Ok(p) = serde_json::from_slice::<SaveFileStatePayload>(&body) {
                store.lock().unwrap().save_file_state(&p.file_path, p.state);
            }
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": true })));
        }
        "/__rust_api__/store/saveSession" => {
            if let Ok(session) = serde_json::from_slice::<SessionState>(&body) {
                store.lock().unwrap().save_session(&session);
            }
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": true })));
        }
        "/__rust_api__/store/getSession" => {
            let session = store.lock().unwrap().get_session();
            responder.respond(json_response(StatusCode::OK, &session));
        }
        "/__rust_api__/signatures/list" => {
            let list = store.lock().unwrap().list_signatures();
            responder.respond(json_response(StatusCode::OK, &list));
        }
        "/__rust_api__/signatures/save" => {
            if let Ok(p) = serde_json::from_slice::<SaveSignaturePayload>(&body) {
                let ok = store.lock().unwrap().save_signature(&p.id, &p.base64_png);
                responder.respond(json_response(StatusCode::OK, &ok));
            } else {
                responder.respond(json_response(StatusCode::OK, &false));
            }
        }
        "/__rust_api__/signatures/delete" => {
            if let Ok(p) = serde_json::from_slice::<DeleteSignaturePayload>(&body) {
                let ok = store.lock().unwrap().delete_signature(&p.id);
                responder.respond(json_response(StatusCode::OK, &ok));
            } else {
                responder.respond(json_response(StatusCode::OK, &false));
            }
        }
        "/__rust_api__/app/openMailto" => {
            let payload = serde_json::from_slice::<serde_json::Value>(&body).ok();
            let subj = payload
                .as_ref()
                .and_then(|v| v.get("subject"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let to = payload
                .as_ref()
                .and_then(|v| v.get("to"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            dialogs::open_mailto_to(to.as_deref(), subj.as_deref());
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": true })));
        }
        "/__rust_api__/app/getAppInfo" => {
            let exe_path = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let install_dir = installer::get_default_install_dir();
            let is_installed = install_dir.join("PDFForge.exe").exists();
            responder.respond(json_response(
                StatusCode::OK,
                &json!({
                    "name": "PDFForge Studio",
                    "version": env!("CARGO_PKG_VERSION"),
                    "developer": "Md. Alamin",
                    "email": "alaminprincesarker@gmail.com",
                    "publisher": "Md. Alamin (PDFForge Studio)",
                    "packageId": "com.alamin.pdfforge.studio",
                    "repository": "https://github.com/alaminprincesarker/pdfforge-studio",
                    "website": "https://alaminprincesarker.github.io/pdfforge-studio/",
                    "updateManifestUrl": "https://alaminprincesarker.github.io/pdfforge-studio/update.json",
                    "stack": "Rust (tao + wry WebView2) + React 18 + PDF.js + pdf-lib",
                    "license": "MIT Open License",
                    "platform": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                    "exePath": exe_path,
                    "installDir": install_dir.to_string_lossy().to_string(),
                    "isInstalled": is_installed
                }),
            ));
        }
        "/__rust_api__/app/openExternalUrl" => {
            let url = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            let ok = dialogs::open_external_url(&url);
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": ok })));
        }
        "/__rust_api__/app/pickUpdatePackage" => {
            thread::spawn(move || {
                let res = dialogs::pick_update_package();
                responder.respond(json_response(StatusCode::OK, &res));
            });
        }
        "/__rust_api__/app/applyUpdatePackage" => {
            let file_path = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("filePath").and_then(|f| f.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            match installer::apply_update_package(&file_path) {
                Ok(()) => {
                    responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": true, "updating": true }),
                    ));
                    thread::spawn(|| {
                        thread::sleep(std::time::Duration::from_millis(450));
                        std::process::exit(0);
                    });
                }
                Err(e) => {
                    responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": false, "error": e }),
                    ));
                }
            }
        }
        "/__rust_api__/app/checkForUpdates" => {
            let manifest_url = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("manifestUrl").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            thread::spawn(move || {
                let res = installer::check_release_manifest(&manifest_url);
                responder.respond(json_response(StatusCode::OK, &res));
            });
        }
        "/__rust_api__/app/downloadAndApplyUpdate" => {
            let download_url = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("downloadUrl").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            thread::spawn(move || match installer::download_and_apply_update(&download_url) {
                Ok(()) => {
                    responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": true, "updating": true }),
                    ));
                    thread::sleep(std::time::Duration::from_millis(450));
                    std::process::exit(0);
                }
                Err(e) => {
                    responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": false, "error": e }),
                    ));
                }
            });
        }
        "/__rust_api__/app/repairInstallation" => {
            thread::spawn(move || {
                match installer::repair_installation() {
                    Ok(path) => responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": true, "installPath": path }),
                    )),
                    Err(e) => responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": false, "error": e }),
                    )),
                }
            });
        }
        "/__rust_api__/app/uninstallApp" => {
            let delete_user_data = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("deleteUserData").and_then(|b| b.as_bool()))
                .unwrap_or(false);
            let res = installer::uninstall_app_internally(delete_user_data);
            let ok = res.is_ok();
            let err_msg = res.err().unwrap_or_default();
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": ok, "error": err_msg })));
            if ok {
                thread::spawn(|| {
                    thread::sleep(std::time::Duration::from_millis(600));
                    std::process::exit(0);
                });
            }
        }
        "/__rust_api__/print/listPrinters" => {
            thread::spawn(move || {
                let printers = dialogs::list_printers();
                responder.respond(json_response(StatusCode::OK, &printers));
            });
        }
        "/__rust_api__/print/direct" => {
            let payload: Result<DirectPrintPayload, _> = serde_json::from_slice(&body);
            match payload {
                Ok(p) => {
                    thread::spawn(move || {
                        match dialogs::print_images_direct(
                            &p.printer_name,
                            p.copies,
                            &p.orientation,
                            &p.paper_size,
                            &p.duplex,
                            &p.scale_mode,
                            p.custom_scale,
                            p.output_pdf_path.as_deref(),
                            &p.pages,
                        ) {
                            Ok(()) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true }),
                            )),
                            Err(err) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": err }),
                            )),
                        }
                    });
                }
                Err(e) => {
                    responder.respond(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "ok": false, "error": format!("Invalid print request: {}", e) }),
                    ));
                }
            }
        }
        "/__rust_api__/app/ready" => {
            webview_ready.store(true, Ordering::SeqCst);
            let drained: Vec<String> = {
                let mut q = pending_boot_open_files.lock().unwrap();
                std::mem::take(&mut *q)
            };
            responder.respond(json_response(
                StatusCode::OK,
                &json!({ "ok": true, "pendingFiles": drained }),
            ));
        }
        "/__rust_api__/acrobat/getPageLayout" => {
            match serde_json::from_slice::<AcrobatPagePayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || match ws.get_page_layout(&p.file_path, p.page) {
                        Ok(layout) => responder.respond(json_response(StatusCode::OK, &*layout)),
                        Err(e) => responder.respond(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        )),
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/hitTest" => {
            match serde_json::from_slice::<AcrobatHitPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || match ws.hit_test(&p.file_path, p.page, p.x, p.y) {
                        Ok(res) => responder.respond(json_response(StatusCode::OK, &res)),
                        Err(e) => responder.respond(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        )),
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/getSelectionRects" => {
            match serde_json::from_slice::<AcrobatSelectionPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.get_selection_rects(&p.file_path, p.page, p.start_char, p.end_char)
                        {
                            Ok(res) => responder.respond(json_response(StatusCode::OK, &res)),
                            Err(e) => responder.respond(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/getObjects" => {
            match serde_json::from_slice::<AcrobatPagePayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || match ws.get_objects(&p.file_path, p.page) {
                        Ok(objs) => responder.respond(json_response(StatusCode::OK, &*objs)),
                        Err(e) => responder.respond(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        )),
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/shapeReflow" => {
            match serde_json::from_slice::<AcrobatShapeReflowPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        let bytes_opt = ws.get_working_bytes(&p.file_path).ok();
                        let lo_opt = bytes_opt
                            .as_ref()
                            .and_then(|b| lopdf::Document::load_mem(b).ok());
                        let page_id_opt = lo_opt
                            .as_ref()
                            .and_then(|d| d.get_pages().get(&(p.page as u32)).copied());
                        let res = font_shaper::shape_and_reflow_block(
                            lo_opt.as_ref(),
                            page_id_opt,
                            &p.text,
                            &p.original_font,
                            &p.font_family,
                            p.font_size,
                            p.max_width,
                            p.bold,
                            p.italic,
                            &p.align,
                            p.line_spacing,
                            p.bullets,
                        );
                        responder.respond(json_response(StatusCode::OK, &res));
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/commitEditText" => {
            match serde_json::from_slice::<AcrobatEditTextPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_mutation(&p.file_path, p.req.page, |doc| {
                            commit_edit_text_block(doc, &p.req)
                        }) {
                            Ok((rev, can_undo, can_redo, notice)) => responder.respond(
                                json_response(
                                    StatusCode::OK,
                                    &json!({
                                        "ok": true,
                                        "revision": rev,
                                        "canUndo": can_undo,
                                        "canRedo": can_redo,
                                        "notice": notice
                                    }),
                                ),
                            ),
                            Err(e) => responder.respond(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/commitShape" => {
            match serde_json::from_slice::<AcrobatShapeCommitPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_mutation(&p.file_path, p.req.page, |doc| {
                            commit_shape_operation(doc, &p.req).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({
                                    "ok": true,
                                    "revision": rev,
                                    "canUndo": can_undo,
                                    "canRedo": can_redo
                                }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/undo" => match serde_json::from_slice::<PathPayload>(&body) {
            Ok(p) => {
                let ws = pdf_workspace.clone();
                thread::spawn(move || match ws.undo(&p.file_path) {
                    Ok((_, rev, can_undo, can_redo)) => responder.respond(json_response(
                        StatusCode::OK,
                        &json!({
                            "ok": true,
                            "revision": rev,
                            "canUndo": can_undo,
                            "canRedo": can_redo
                        }),
                    )),
                    Err(e) => responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": false, "error": e }),
                    )),
                });
            }
            Err(e) => responder.respond(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "ok": false, "error": e.to_string() }),
            )),
        },
        "/__rust_api__/acrobat/redo" => match serde_json::from_slice::<PathPayload>(&body) {
            Ok(p) => {
                let ws = pdf_workspace.clone();
                thread::spawn(move || match ws.redo(&p.file_path) {
                    Ok((_, rev, can_undo, can_redo)) => responder.respond(json_response(
                        StatusCode::OK,
                        &json!({
                            "ok": true,
                            "revision": rev,
                            "canUndo": can_undo,
                            "canRedo": can_redo
                        }),
                    )),
                    Err(e) => responder.respond(json_response(
                        StatusCode::OK,
                        &json!({ "ok": false, "error": e }),
                    )),
                });
            }
            Err(e) => responder.respond(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "ok": false, "error": e.to_string() }),
            )),
        },
        "/__rust_api__/acrobat/getWorkingPdf" => {
            match serde_json::from_slice::<PathPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || match ws.get_working_bytes(&p.file_path) {
                        Ok(bytes) => {
                            let sz = bytes.len() as u64;
                            responder.respond(binary_response(
                                StatusCode::OK,
                                bytes,
                                "working.pdf",
                                sz,
                            ));
                        }
                        Err(e) => responder.respond(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        )),
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/listFonts" => {
            thread::spawn(move || {
                let catalog = font_shaper::list_font_catalog();
                responder.respond(json_response(StatusCode::OK, &catalog));
            });
        }
        "/__rust_api__/acrobat/commitImage" => {
            match serde_json::from_slice::<AcrobatImageCommitPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_mutation(&p.file_path, p.req.page, |doc| {
                            pdf_objects::commit_image_operation(doc, &p.req).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({
                                    "ok": true,
                                    "revision": rev,
                                    "canUndo": can_undo,
                                    "canRedo": can_redo
                                }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/getAnnotations" => {
            match serde_json::from_slice::<AcrobatPagePayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        let out = ws.get_working_bytes(&p.file_path).and_then(|bytes| {
                            let lo = lopdf::Document::load_mem(&bytes)
                                .map_err(|e| format!("Failed parsing PDF: {}", e))?;
                            pdf_annots::extract_page_annotations(&lo, p.page)
                        });
                        match out {
                            Ok(list) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": true, "annotations": list }))),
                            Err(e) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": false, "error": e }))),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/commitAnnotation" => {
            match serde_json::from_slice::<AcrobatAnnotationCommitPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_mutation(&p.file_path, p.req.page, |doc| {
                            pdf_annots::commit_annotation_op(doc, &p.req).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({
                                    "ok": true,
                                    "revision": rev,
                                    "canUndo": can_undo,
                                    "canRedo": can_redo
                                }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/updateFormField" => {
            match serde_json::from_slice::<AcrobatFormFieldPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_mutation(&p.file_path, p.req.page, |doc| {
                            pdf_annots::update_form_field(doc, &p.req).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/rotatePages" => {
            match serde_json::from_slice::<AcrobatRotatePagesPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_doc_mutation(&p.file_path, |doc| {
                            pdf_annots::rotate_pages(doc, &p.pages, p.delta_deg).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/applyRedaction" => {
            match serde_json::from_slice::<AcrobatRedactPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_mutation(&p.file_path, p.page, |doc| {
                            pdf_annots::apply_redactions(doc, p.page, &p.rects_vp).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/getDocProperties" => {
            match serde_json::from_slice::<PathPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        let out = ws.get_working_bytes(&p.file_path).and_then(|bytes| {
                            let lo = lopdf::Document::load_mem(&bytes)
                                .map_err(|e| format!("Failed parsing PDF: {}", e))?;
                            Ok(pdf_annots::get_doc_properties(&lo))
                        });
                        match out {
                            Ok(props) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": true, "properties": props }))),
                            Err(e) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": false, "error": e }))),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/setDocProperties" => {
            match serde_json::from_slice::<AcrobatDocPropsPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_doc_mutation(&p.file_path, |doc| {
                            pdf_annots::set_doc_properties(doc, &p.props).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/listBookmarks" => {
            match serde_json::from_slice::<PathPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        let out = ws.get_working_bytes(&p.file_path).and_then(|bytes| {
                            let lo = lopdf::Document::load_mem(&bytes)
                                .map_err(|e| format!("Failed parsing PDF: {}", e))?;
                            Ok(pdf_annots::list_bookmarks(&lo))
                        });
                        match out {
                            Ok(bms) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": true, "bookmarks": bms }))),
                            Err(e) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": false, "error": e }))),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/writeBookmarks" => {
            match serde_json::from_slice::<AcrobatBookmarksPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_doc_mutation(&p.file_path, |doc| {
                            pdf_annots::write_bookmarks(doc, &p.bookmarks).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/setPageLabels" => {
            match serde_json::from_slice::<AcrobatPageLabelsPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_doc_mutation(&p.file_path, |doc| {
                            pdf_annots::set_page_labels(doc, &p.style, &p.prefix, p.start_num)
                                .map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/cropPages" => {
            match serde_json::from_slice::<AcrobatCropPagesPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_doc_mutation(&p.file_path, |doc| {
                            pdf_annots::crop_pages_real(doc, &p.pages, p.margins).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/pageOp" => {
            match serde_json::from_slice::<AcrobatPageOpPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        match ws.push_doc_mutation(&p.file_path, |doc| {
                            pdf_annots::apply_page_operation(doc, &p.req).map(|_| None)
                        }) {
                            Ok((rev, can_undo, can_redo, _)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "canUndo": can_undo, "canRedo": can_redo }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/extractPages" => {
            match serde_json::from_slice::<AcrobatExtractPagesPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        let out = ws.get_working_bytes(&p.file_path).and_then(|bytes| {
                            let lo = lopdf::Document::load_mem(&bytes)
                                .map_err(|e| format!("Failed parsing PDF: {}", e))?;
                            let extracted = pdf_annots::extract_pages_to_bytes(&lo, &p.pages)?;
                            pdf_annots::save_pdf_verified(&extracted, None, &p.output_path, false, true)?;
                            Ok(())
                        });
                        match out {
                            Ok(()) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": true }))),
                            Err(e) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": false, "error": e }))),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/combinePdfs" => {
            match serde_json::from_slice::<AcrobatCombinePayload>(&body) {
                Ok(p) => {
                    thread::spawn(move || {
                        let res = pdf_annots::combine_pdfs_to_bytes(&p.paths).and_then(|bytes| {
                            pdf_annots::save_pdf_verified(&bytes, None, &p.output_path, false, true)?;
                            Ok(())
                        });
                        match res {
                            Ok(()) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": true }))),
                            Err(e) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": false, "error": e }))),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/saveVerified" => {
            match serde_json::from_slice::<AcrobatSaveVerifiedPayload>(&body) {
                Ok(p) => {
                    let ws = pdf_workspace.clone();
                    thread::spawn(move || {
                        let target = p.target_path.unwrap_or_else(|| p.file_path.clone());
                        match ws.save_verified_to_disk(&p.file_path, &target, p.incremental, p.compact) {
                            Ok((rev, used_inc)) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "revision": rev, "incremental": used_inc, "filePath": target }),
                            )),
                            Err(e) => responder.respond(json_response(
                                StatusCode::OK,
                                &json!({ "ok": false, "error": e }),
                            )),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/acrobat/verifyPdf" => {
            match serde_json::from_slice::<PathPayload>(&body) {
                Ok(p) => {
                    thread::spawn(move || {
                        let out = (|| -> Result<serde_json::Value, String> {
                            let bytes = fs::read(&p.file_path)
                                .map_err(|e| format!("Failed reading '{}': {}", p.file_path, e))?;
                            let lo = lopdf::Document::load_mem(&bytes)
                                .map_err(|e| format!("lopdf parse failed: {}", e))?;
                            let pdfium = pdf_layout::get_pdfium()?;
                            let guard = pdfium.lock().unwrap_or_else(|e| e.into_inner());
                            let pdoc = guard
                                .load_pdf_from_byte_slice(&bytes, None)
                                .map_err(|e| format!("PDFium load failed: {:?}", e))?;
                            let mut rotations = Vec::new();
                            let mut annot_counts = Vec::new();
                            let mut text_samples = Vec::new();
                            for (idx, pg) in pdoc.pages().iter().enumerate() {
                                let lo_pages = lo.get_pages();
                                let rot = lo_pages
                                    .get(&((idx + 1) as u32))
                                    .map(|pid| pdf_objects::get_page_transform(&lo, *pid).rotation)
                                    .unwrap_or(0);
                                rotations.push(rot);
                                let anns = pdf_annots::extract_page_annotations(&lo, idx + 1)
                                    .map(|v| v.len())
                                    .unwrap_or(0);
                                annot_counts.push(anns);
                                let txt = pg
                                    .text()
                                    .map(|t| {
                                        t.chars()
                                            .iter()
                                            .filter_map(|c| c.unicode_char())
                                            .take(120)
                                            .collect::<String>()
                                    })
                                    .unwrap_or_default();
                                text_samples.push(txt);
                            }
                            let props = pdf_annots::get_doc_properties(&lo);
                            let bookmarks = pdf_annots::list_bookmarks(&lo);
                            Ok(json!({
                                "ok": true,
                                "pageCount": pdoc.pages().len(),
                                "rotations": rotations,
                                "annotCounts": annot_counts,
                                "textSamples": text_samples,
                                "properties": props,
                                "bookmarkCount": bookmarks.len()
                            }))
                        })();
                        match out {
                            Ok(v) => responder.respond(json_response(StatusCode::OK, &v)),
                            Err(e) => responder.respond(json_response(StatusCode::OK, &json!({ "ok": false, "error": e }))),
                        }
                    });
                }
                Err(e) => responder.respond(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "ok": false, "error": e.to_string() }),
                )),
            }
        }
        "/__rust_api__/app/openNewWindow" => {
            let file_path = serde_json::from_slice::<PathPayload>(&body)
                .map(|p| p.file_path)
                .unwrap_or_default();
            let mut ok = false;
            if let Ok(exe) = std::env::current_exe() {
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("--new-window");
                if !file_path.trim().is_empty() {
                    cmd.arg(file_path.trim());
                }
                ok = cmd.spawn().is_ok();
            }
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": ok })));
        }
        "/__rust_api__/diag" => {
            let msg = String::from_utf8_lossy(&body).to_string();
            let log_path = store.lock().unwrap().user_data_dir().join("startup_diag.log");
            let prev = fs::read_to_string(&log_path).unwrap_or_default();
            let _ = fs::write(&log_path, format!("{}{}\n", prev, msg));
            responder.respond(json_response(StatusCode::OK, &json!({ "ok": true })));
        }
        _ => {
            responder.respond(json_response(
                StatusCode::NOT_FOUND,
                &json!({ "error": "Unknown API endpoint" }),
            ));
        }
    }
}

fn main() {
    if installer::check_and_run_installer_mode() {
        return;
    }

    let store = Arc::new(Mutex::new(AppDataStore::new()));
    let user_data_dir = store.lock().unwrap().user_data_dir().clone();
    let cli_pdfs = pdf_engine::parse_cli_pdf_args();
    let pending_boot_open_files: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let webview_ready: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // Enforce single-window architecture: clicking/opening additional PDF files forwards them
    // into the running window to open as new tabs, then exits the secondary launcher process.
    let mut instance_guard = match single_instance::try_acquire_or_forward(&user_data_dir, &cli_pdfs) {
        Some(guard) => guard,
        None => return,
    };

    #[cfg(target_os = "windows")]
    {
        ensure_webview2_loader_loaded(&user_data_dir);
        ensure_pdfium_dll_present(&user_data_dir);
    }
    let pdf_workspace = PdfWorkspaceEngine::new();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let proxy_for_single = proxy.clone();
    single_instance::start_listener(&mut instance_guard, move |paths| {
        let _ = proxy_for_single.send_event(UserEvent::OpenFiles(paths));
    });

    let app_icon = tao::window::Icon::from_rgba(ICON_RGBA_BYTES.to_vec(), 64, 64).ok();

    let window = WindowBuilder::new()
        .with_title("PDFForge")
        .with_window_icon(app_icon)
        .with_inner_size(LogicalSize::new(1360.0, 860.0))
        .with_min_inner_size(LogicalSize::new(920.0, 600.0))
        .with_maximized(true)
        .with_decorations(false)
        .with_visible(false)
        .build(&event_loop)
        .expect("Failed to create native Rust window");

    single_instance::register_window(&instance_guard, &window);
    #[cfg(target_os = "windows")]
    win32_frame::apply_frameless_appearance(&window, true);

    let web_data_dir = store.lock().unwrap().user_data_dir().join("webview2");
    let mut web_context = WebContext::new(Some(web_data_dir));

    let store_for_protocol = Arc::clone(&store);
    let pending_boot_for_protocol = Arc::clone(&pending_boot_open_files);
    let ready_for_protocol = Arc::clone(&webview_ready);
    let workspace_for_protocol = pdf_workspace.clone();
    let proxy_for_ipc = proxy.clone();
    let proxy_for_drop = proxy.clone();
    let proxy_for_close = proxy.clone();

    let mut init_script = INIT_BRIDGE_SCRIPT.to_string();
    if !cli_pdfs.is_empty() {
        if let Ok(escaped) = serde_json::to_string(&cli_pdfs) {
            init_script.push_str(&format!(
                "\nwindow.__pdfforgeNotifyOpenFiles({});\n",
                escaped
            ));
        }
    }

    let builder = WebViewBuilder::with_web_context(&mut web_context)
        .with_background_color((30, 30, 30, 255))
        .with_hotkeys_zoom(true)
        .with_initialization_script(&init_script);
    #[cfg(not(debug_assertions))]
    let builder = builder.with_devtools(false);
    #[cfg(target_os = "windows")]
    let builder = builder.with_browser_accelerator_keys(false);
    let webview = builder
        .with_drag_drop_handler(move |event| {
            match event {
                wry::DragDropEvent::Enter { paths, .. } => {
                    let has_pdf = paths.iter().any(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("pdf"))
                            .unwrap_or(false)
                    });
                    if has_pdf {
                        let _ = proxy_for_drop.send_event(UserEvent::DragEnter);
                    }
                }
                wry::DragDropEvent::Leave => {
                    let _ = proxy_for_drop.send_event(UserEvent::DragLeave);
                }
                wry::DragDropEvent::Drop { paths, .. } => {
                    let pdf_paths: Vec<String> = paths
                        .into_iter()
                        .filter(|p| {
                            p.extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.eq_ignore_ascii_case("pdf"))
                                .unwrap_or(false)
                        })
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    let _ = proxy_for_drop.send_event(UserEvent::DropFiles(pdf_paths));
                }
                _ => {}
            }
            true
        })
        .with_asynchronous_custom_protocol("pdfforge".into(), move |_webview_id, request, responder| {
            let uri = request.uri();
            let path = uri.path();
            let query = uri.query();

            if path.starts_with("/__rust_api__/") {
                let body = request.body().clone();
                handle_api_request(
                    path,
                    query,
                    body,
                    Arc::clone(&store_for_protocol),
                    Arc::clone(&pending_boot_for_protocol),
                    Arc::clone(&ready_for_protocol),
                    workspace_for_protocol.clone(),
                    responder,
                );
                return;
            }

            let clean_path = path.trim_start_matches('/');
            let asset_key = if clean_path.is_empty() {
                "index.html"
            } else {
                clean_path
            };

            if let Some(asset) = Assets::get(asset_key) {
                let mime = guess_mime_type(asset_key);
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, mime)
                    .header("Access-Control-Allow-Origin", "*")
                    .body(asset.data)
                    .unwrap();
                responder.respond(resp);
            } else if let Some(index) = Assets::get("index.html") {
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/html; charset=utf-8")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(index.data)
                    .unwrap();
                responder.respond(resp);
            } else {
                let resp = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Cow::Borrowed(&b"Not Found"[..]))
                    .unwrap();
                responder.respond(resp);
            }
        })
        .with_ipc_handler(move |req| {
            let body = req.body();
            if let Ok(parsed) = serde_json::from_str::<IpcWindowCommand>(body) {
                match parsed.cmd.as_str() {
                    "windowReady" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::ShowMainWindow);
                    }
                    "minimize" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::Minimize);
                    }
                    "maximize" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::Maximize);
                    }
                    "unmaximize" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::Unmaximize);
                    }
                    "close" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::Close);
                    }
                    "closePromptActive" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::ClosePromptActive);
                    }
                    "confirmClose" => {
                        let proceed = parsed.proceed.or(parsed.value).unwrap_or(false);
                        let _ = proxy_for_ipc.send_event(UserEvent::ConfirmClose(proceed, parsed.session));
                    }
                    "toggleFullScreen" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::ToggleFullScreen);
                    }
                    "setFullScreen" => {
                        let next = parsed.value.unwrap_or(false);
                        let _ = proxy_for_ipc.send_event(UserEvent::SetFullScreen(next));
                    }
                    "dragWindow" => {
                        let _ = proxy_for_ipc.send_event(UserEvent::DragWindow);
                    }
                    _ => {}
                }
            }
        })
        .with_new_window_req_handler(|url| {
            if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                dialogs::open_external_url(&url);
            }
            false
        })
        .with_url("pdfforge://localhost/index.html")
        .build(&window)
        .expect("Failed to initialize Rust WebView2");

    let mut last_maximized = window.is_maximized();
    let mut last_fullscreen = window.fullscreen().is_some();
    let mut was_maximized_before_fs = true;
    let mut close_pending = false;
    let mut awaiting_user_close_prompt = false;
    let mut window_shown = false;

    // Fallback safety timer: guarantee the window becomes visible quickly even if WebView2 init is slow
    let proxy_for_show = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(220));
        let _ = proxy_for_show.send_event(UserEvent::ShowMainWindow);
    });

    let sync_webview_bounds = |win: &tao::window::Window, wv: &wry::WebView| {
        let size = win.inner_size();
        let _ = wv.set_bounds(wry::Rect {
            position: tao::dpi::PhysicalPosition::new(0, 0).into(),
            size: tao::dpi::PhysicalSize::new(size.width, size.height).into(),
        });
        let _ = wv.zoom(1.0);
    };

    let store_for_close = Arc::clone(&store);

    event_loop.run(move |event, _, control_flow| {
        let _keep_instance_guard = &instance_guard;
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if !close_pending {
                    close_pending = true;
                    awaiting_user_close_prompt = false;
                    let _ = webview.evaluate_script(
                        "window.__pdfforgeRequestClose && window.__pdfforgeRequestClose();",
                    );
                    let p_close = proxy_for_close.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        let _ = p_close.send_event(UserEvent::CloseTimeoutFallback);
                    });
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                sync_webview_bounds(&window, &webview);
                let is_max = window.is_maximized();
                if is_max != last_maximized {
                    last_maximized = is_max;
                    let _ = webview.evaluate_script(&format!(
                        "window.__pdfforgeNotifyMaximized && window.__pdfforgeNotifyMaximized({});",
                        is_max
                    ));
                }
                let is_fs = window.fullscreen().is_some();
                #[cfg(target_os = "windows")]
                win32_frame::apply_frameless_appearance(&window, is_fs || is_max);
                if is_fs != last_fullscreen {
                    last_fullscreen = is_fs;
                    let _ = webview.evaluate_script(&format!(
                        "window.__pdfforgeNotifyFullScreen && window.__pdfforgeNotifyFullScreen({});",
                        is_fs
                    ));
                }
            }
            Event::UserEvent(user_event) => match user_event {
                UserEvent::ShowMainWindow => {
                    if !window_shown {
                        window_shown = true;
                        sync_webview_bounds(&window, &webview);
                        window.set_visible(true);
                        window.set_focus();
                        let _ = webview.focus();
                    }
                }
                UserEvent::Minimize => {
                    window.set_minimized(true);
                }
                UserEvent::Maximize => {
                    window.set_maximized(true);
                    last_maximized = true;
                    #[cfg(target_os = "windows")]
                    win32_frame::apply_frameless_appearance(&window, true);
                    sync_webview_bounds(&window, &webview);
                    let _ = webview.evaluate_script(
                        "window.__pdfforgeNotifyMaximized && window.__pdfforgeNotifyMaximized(true);",
                    );
                }
                UserEvent::Unmaximize => {
                    window.set_maximized(false);
                    last_maximized = false;
                    #[cfg(target_os = "windows")]
                    win32_frame::apply_frameless_appearance(&window, window.fullscreen().is_some());
                    sync_webview_bounds(&window, &webview);
                    let _ = webview.evaluate_script(
                        "window.__pdfforgeNotifyMaximized && window.__pdfforgeNotifyMaximized(false);",
                    );
                }
                UserEvent::Close => {
                    if !close_pending {
                        close_pending = true;
                        awaiting_user_close_prompt = false;
                        let _ = webview.evaluate_script(
                            "window.__pdfforgeRequestClose && window.__pdfforgeRequestClose();",
                        );
                        let p_close = proxy_for_close.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            let _ = p_close.send_event(UserEvent::CloseTimeoutFallback);
                        });
                    }
                }
                UserEvent::ClosePromptActive => {
                    awaiting_user_close_prompt = true;
                }
                UserEvent::ConfirmClose(proceed, session_opt) => {
                    if proceed {
                        if let Some(session) = session_opt {
                            store_for_close.lock().unwrap().save_session(&session);
                        }
                        #[cfg(target_os = "windows")]
                        win32_frame::hide_instantly_before_close(&window);
                        window.set_visible(false);
                        *control_flow = ControlFlow::Exit;
                    } else {
                        close_pending = false;
                        awaiting_user_close_prompt = false;
                    }
                }
                UserEvent::CloseTimeoutFallback => {
                    if close_pending && !awaiting_user_close_prompt {
                        #[cfg(target_os = "windows")]
                        win32_frame::hide_instantly_before_close(&window);
                        window.set_visible(false);
                        *control_flow = ControlFlow::Exit;
                    }
                }
                fs_ev @ (UserEvent::ToggleFullScreen | UserEvent::SetFullScreen(_)) => {
                    let next = match fs_ev {
                        UserEvent::SetFullScreen(val) => val,
                        _ => window.fullscreen().is_none(),
                    };
                    #[cfg(target_os = "windows")]
                    win32_frame::set_transitions_disabled(&window, true);
                    if next {
                        if !last_fullscreen {
                            was_maximized_before_fs = window.is_maximized();
                        }
                        if was_maximized_before_fs {
                            window.set_maximized(false);
                        }
                        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
                    } else {
                        window.set_fullscreen(None);
                        if was_maximized_before_fs {
                            window.set_maximized(true);
                        }
                    }
                    last_fullscreen = next;
                    let now_max = window.is_maximized();
                    #[cfg(target_os = "windows")]
                    {
                        win32_frame::apply_frameless_appearance(&window, next || now_max);
                        win32_frame::set_transitions_disabled(&window, false);
                    }
                    sync_webview_bounds(&window, &webview);
                    window.set_focus();
                    let _ = webview.focus();
                    window.request_redraw();
                    let _ = webview.evaluate_script(&format!(
                        "window.__pdfforgeState && (window.__pdfforgeState.isFullScreen = {fs}, window.__pdfforgeState.isMaximized = {mx}); \
                         window.__pdfforgeNotifyFullScreen && window.__pdfforgeNotifyFullScreen({fs}); \
                         window.__pdfforgeNotifyMaximized && window.__pdfforgeNotifyMaximized({mx}); \
                         window.dispatchEvent(new Event('resize')); \
                         setTimeout(function(){{ window.dispatchEvent(new Event('resize')); }}, 80);",
                        fs = next,
                        mx = now_max
                    ));
                }
                UserEvent::DragWindow => {
                    let _ = window.drag_window();
                }
                UserEvent::DragEnter => {
                    let _ = webview.evaluate_script(
                        "window.__pdfforgeDragEnter && window.__pdfforgeDragEnter();",
                    );
                }
                UserEvent::DragLeave => {
                    let _ = webview.evaluate_script(
                        "window.__pdfforgeDragLeave && window.__pdfforgeDragLeave();",
                    );
                }
                UserEvent::DropFiles(paths) => {
                    if let Ok(json_paths) = serde_json::to_string(&paths) {
                        let _ = webview.evaluate_script(&format!(
                            "window.__pdfforgeDropFiles && window.__pdfforgeDropFiles({});",
                            json_paths
                        ));
                    } else {
                        let _ = webview.evaluate_script(
                            "window.__pdfforgeDragLeave && window.__pdfforgeDragLeave();",
                        );
                    }
                }
                UserEvent::OpenFiles(paths) => {
                    single_instance::bring_window_to_front(&window);
                    let _ = webview.focus();
                    let pdfs: Vec<String> = paths
                        .into_iter()
                        .filter(|p| !p.trim().is_empty() && p.to_lowercase().ends_with(".pdf"))
                        .collect();
                    if !pdfs.is_empty() {
                        if webview_ready.load(Ordering::SeqCst) {
                            if let Ok(json_paths) = serde_json::to_string(&pdfs) {
                                let _ = webview.evaluate_script(&format!(
                                    "window.__pdfforgeNotifyOpenFiles && window.__pdfforgeNotifyOpenFiles({});",
                                    json_paths
                                ));
                            }
                        } else {
                            let mut q = pending_boot_open_files.lock().unwrap();
                            for p in &pdfs {
                                if !q.iter().any(|x| x.eq_ignore_ascii_case(p)) {
                                    q.push(p.clone());
                                }
                            }
                        }
                    }
                }
            },
            #[cfg(target_os = "macos")]
            Event::Opened { urls } => {
                single_instance::bring_window_to_front(&window);
                let _ = webview.focus();
                let pdfs: Vec<String> = urls
                    .into_iter()
                    .filter_map(|u| u.to_file_path().ok())
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("pdf"))
                            .unwrap_or(false)
                    })
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                if !pdfs.is_empty() {
                    if webview_ready.load(Ordering::SeqCst) {
                        if let Ok(json_paths) = serde_json::to_string(&pdfs) {
                            let _ = webview.evaluate_script(&format!(
                                "window.__pdfforgeNotifyOpenFiles && window.__pdfforgeNotifyOpenFiles({});",
                                json_paths
                            ));
                        }
                    } else {
                        let mut q = pending_boot_open_files.lock().unwrap();
                        for p in &pdfs {
                            if !q.iter().any(|x| x.eq_ignore_ascii_case(p)) {
                                q.push(p.clone());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    });
}

