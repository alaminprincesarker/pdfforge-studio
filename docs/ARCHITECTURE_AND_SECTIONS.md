# PDFForge Studio — Architecture & Code Section Map

This document maps every file, Rust module, custom protocol endpoint, CSS rule block, and frontend JS component/utility in the **pure Rust + static embedded UI** codebase.

---

## 1. Repository File Structure

| Path | Role & Purpose |
|---|---|
| `Cargo.toml` | Package metadata (`pdfforge` v`1.0.0`, binary `PDFForge-Rust`), crates (`tao` 0.30, `wry` 0.47, `rfd` 0.15, `serde`/`serde_json` 1.0, `base64` 0.22, `rust-embed` 8.5, `mime_guess` 2.0, `http` 1.1), `[profile.release]` (`opt-level = 3`, `lto = "thin"`, `strip = true`, `codegen-units = 1`). |
| `build.rs` | Compiles Windows resource script (`src-rust/app.rc` via `windres`) and C loader shim (`src-rust/wv2_shim.c` via `gcc` + `ar` -> `libWebView2Loader.dll.a`), replacing `webview2-com-sys` import libs so the final `.exe` has **zero** static DLL dependency on `WebView2Loader.dll`. Also sets `cargo:rerun-if-changed` watches on assets and icons. |
| `src-rust/main.rs` | Entry point: checks `installer::check_and_run_installer_mode()`, extracts & loads `WebView2Loader.dll`, builds borderless maximized `tao` window (`with_maximized(true)`, `with_decorations(false)`), injects `INIT_BRIDGE_SCRIPT`, serves `pdfforge://localhost/` via `rust-embed`, routes `/__rust_api__/*` IPC calls, manages `sync_webview_bounds`, fullscreen/maximize restoration, drag-and-drop, and CLI `.pdf` argument opening. |
| `src-rust/store.rs` | `AppDataStore` managing JSON/binary persistence in `%APPDATA%\pdfforge\`: `settings.json`, `recent-files.json`, `file-states.json`, `signatures/*.png`, `autosave/`. |
| `src-rust/dialogs.rs` | Native OS dialogs (`rfd`: `open_file_dialog`, `open_multiple_files_dialog`, `save_file_dialog`), Explorer reveal (`show_in_folder`), default mail client (`open_mailto`), and hidden PowerShell `Get-Printer` discovery (`list_printers`). |
| `src-rust/pdf_engine.rs` | Native binary PDF file reader (`read_pdf_file`), atomic write (`save_pdf_file`), and CLI `.pdf` path canonicalization (`parse_cli_pdf_arg`, stripping `\\?\`). |
| `src-rust/installer.rs` | Self-contained Windows installer/uninstaller triggered by `--install-silent`, `--install`, `--uninstall`, or executable name (`*setup*` / `uninstall*`). Installs to `%LOCALAPPDATA%\Programs\PDFForge`, writes Registry Control Panel keys (`HKCU\...\Uninstall\PDFForge`), registers `.pdf` ProgID (`PDFForge.Document`) + icons, creates Desktop & Start Menu `.lnk` shortcuts, and notifies Explorer shell (`SHChangeNotify`). |
| `src-rust/app.rc` | Win32 resource manifest embedding `icon.ico` (index `0`, App Icon) and `pdf_doc.ico` (index `1`, PDF Document Icon). |
| `src-rust/wv2_shim.c` | Minimal C runtime shim resolving the 5 CoreWebView2 loader entrypoints via `GetModuleHandleW(L"WebView2Loader.dll")` / `LoadLibraryW`. |
| `src-rust/assets/index.html` | Single-page HTML shell loading `./assets/index-11jbxaY0.js` and `./assets/index-ClXAvMiX.css`. |
| `src-rust/assets/pdf.worker.min.mjs` | Embedded offline PDF.js Web Worker for non-blocking background PDF parsing & rendering. |
| `src-rust/assets/cmaps/` & `standard_fonts/` | Offline CJK CMaps (`.bcmap`) and standard Type1/TrueType PDF fallback fonts (Foxit + Liberation Sans). |

---

## 2. Rust <-> WebView IPC & Custom Protocol (`src-rust/main.rs`)

### 2.1 Custom Protocol (`pdfforge://localhost/...`)
- Static assets in `src-rust/assets/` are embedded into `Assets` (`#[derive(RustEmbed)]`) and served with detected MIME types (`guess_mime_type`).
- API calls are posted to `/__rust_api__/*` via `fetch` inside `INIT_BRIDGE_SCRIPT`, bridged to `window.electronAPI`:

| Endpoint / Window Command | Backing Rust Module | Purpose |
|---|---|---|
| `window.ipc.postMessage({cmd, value})` | `main.rs` (`UserEvent`) | `minimize`, `maximize`, `unmaximize`, `close`, `toggleFullScreen`, `setFullScreen(bool)`, `dragWindow` |
| `/__rust_api__/dialog/openFile`, `openMultipleFiles`, `saveFile` | `dialogs.rs` | Native open/multi-open/save file dialogs |
| `/__rust_api__/file/read` | `pdf_engine.rs` | Streams raw PDF bytes + `X-File-Name` / `X-File-Size` headers on worker thread |
| `/__rust_api__/file/save` | `pdf_engine.rs` | Saves edited `pdf-lib` binary PDF buffer back to disk |
| `/__rust_api__/file/cacheDroppedFile` | `main.rs` + `store.rs` | Persists HTML5-dropped PDF bytes into `%APPDATA%\pdfforge\autosave\` so full viewer tabs work identically to disk files |
| `/__rust_api__/file/showInFolder` | `dialogs.rs` | Launches `explorer.exe /select,<path>` |
| `/__rust_api__/store/*` | `store.rs` | `getRecentFiles`, `addRecentFile`, `removeRecentFile`, `clearRecentFiles`, `toggleFavoriteFile`, `getSettings`, `updateSettings`, `getFileState`, `saveFileState` |
| `/__rust_api__/signatures/*` | `store.rs` | `list`, `save`, `delete` base64 PNG signatures |
| `/__rust_api__/printers/list` | `dialogs.rs` | Lists installed Windows printers + `Microsoft Print to PDF` / `Save as PDF (PDFForge)` |
| `/__rust_api__/system/mailto` | `dialogs.rs` | Launches default mail client with subject |

### 2.2 Host-to-JS Window Push Hooks (`INIT_BRIDGE_SCRIPT`)
- `window.__pdfforgeNotifyMaximized(bool)` -> updates `window.__pdfforgeState.isMaximized` & Zustand `setIsMaximized`
- `window.__pdfforgeNotifyFullScreen(bool)` -> updates `window.__pdfforgeState.isFullScreen` & Zustand `setIsFullScreen`
- `window.__pdfforgeDragEnter()` / `__pdfforgeDragLeave()` / `__pdfforgeDropFiles(paths)` -> native Tao file drop overlay & tab opener
- `window.__pdfforgeOpenFile(path)` -> CLI `.pdf` argument opener on launch / file association

---

## 3. Frontend CSS Section Map (`src-rust/assets/assets/index-ClXAvMiX.css`)

- **Base Tailwind Bundle**: Theme variables (`--bg-viewer`, `bg-titlebar`, `bg-toolbar`, `accent`), responsive grids, and utility classes.
- **Custom Tail Utilities & PDF Text Selection Block** (end of file):
  - `.no-scrollbar` — hides thin secondary scrollbars where desired
  - `.z-25`, `.z-35`, `.z-60`, `.rounded-xs`, `.shadow-xs`, `.backdrop-blur-xs`, `.min-h-0`, `.overscroll-contain` — layout helpers ensuring scrollable flex containers shrink properly without clipping or outer-body bleed
  - `.textLayer`, `.textLayer :is(span,br)`, `.textLayer .endOfContent` — transparent selectable PDF text overlay positioned over rendered `<canvas>`
  - `.textLayer.tool-non-text` — disables accidental text selection when drawing ink/shapes/hand-panning
  - `.textLayer ::selection`, `.textLayer.highlight-active ::selection`, `.textLayer.underline-active ::selection`, `.textLayer.strikethrough-active ::selection` — contextual selection colors per active annotation tool

---

## 4. Frontend JS Bundle Symbol Directory (`src-rust/assets/assets/index-11jbxaY0.js`)

Because the production frontend is pre-compiled into a static minified ES module, every UI feature maps to stable top-level identifiers:

### 4.1 Global Store & PDF Engine
| Symbol | Approx. Offset | Purpose |
|---|---|---|
| `xs` | `9172` | **PDF Engine Service** (PDF.js + `pdf-lib`): `loadDocument`, `renderPageToCanvas`, `renderTextLayer`, `extractEditablePageContent`, `savePdfWithEditsAndAnnotations`, reorder/rotate/split/merge/watermark/crop helpers. |
| `qt` | `185305` | **Zustand App Store**: `activeTabId`, `openTabs`, `recentFiles`, `settings`, `isMaximized`, `isFullScreen`, `setIsFullScreen`, `toggleFullScreen`, `isToolbarCollapsed`, `activeTool`, `activeColor`, `activeShape`, `shapeStyle`, `drawAsHighlighter`, `strokeWidth`, `fontSize`, `fontFamily`, `editPageContentMode`, `rightPanelOpen`, `rightPanelTab`, `openEditPdfPanel`, `pageColorMode`, `searchBarOpen`, `toast`. |

### 4.2 Shell, Title Bar, Tabs & Navigation Bars
| Symbol | Offset | Role & Owned UI |
|---|---|---|
| `C6` | `1260405` | **Root App Shell**: Initializes settings/window state, mounts native & HTML5 drag-and-drop listeners, handles global shortcuts (`F11`, `Ctrl+L`, `Esc`, `Ctrl+\`, `Ctrl+F`, `Ctrl+S`, `Ctrl+Shift+S`, `Ctrl+P`, `Ctrl+=`/`-`/`0`), renders `GR`, `XR`, `QR`, active views (`JR`, `t6`, tabs of `b6`), drop overlay, toasts, and modals (`w6`, `S6`, `k6`). |
| `GR` | `189344` | **Custom Title Bar**: Window menus (`File`, `Edit`, `View`, `Window`, `Help`), document dirty title indicator, native Minimize / Maximize-Restore / Close buttons, toolbar collapse toggle, and Full Screen button (`Ctrl+L` / `F11`) routed through `qt.getState().toggleFullScreen()`. |
| `XR` | `197450` | **Tab Strip**: Multi-document tabs, internal drag-reorder (`application/x-pdfforge-internal`), close tab, quick Open (`+`), Home/Tools switching. |
| `QR` | `205524` | **Primary Action Toolbar**: Save/Print, Undo/Redo, Find (`Ctrl+F`), Page Prev/Input/Next, Select / Hand / Marquee Zoom tools, Zoom Out/Dropdown/Custom%/In, Fit Width, Fit Page, **Continuous Vertical Scrolling toggle (`viewMode: continuous <-> single`)**, Two-Page Spread toggle (`two-page <-> continuous`), Rotate CW, Page Color Mode (Normal/Night/Sepia), Read Out Loud, Highlighter, Underline, Strikethrough, Add Text, Add Image, Draw/Ink Brush, Shapes (`rectangle`/`circle`/`arrow`/`line` + Outline/`Highlighter Fill`/`Solid Tint` + `Highlighter Freehand Brush`), Signature, Color/Font/Stroke dropdown, single non-wrapping **Edit PDF** right-panel toggle, Export (`KR`). Secondary Edit tools (`Edit Page`, `Sticky Note`, `Stamp`, `Redact`, `Eraser`) reside in Right Bar `g6`. |
| `c6` | `~1102k` | **Full Screen Floating Quick-Nav Pill**: Rendered **only in Full Screen mode** (`s && c6`); auto-hides after 1.8s idle, reveals on bottom hover (`y >= innerHeight - 92`), shows thin subtle bottom handle when hidden. Controls Prev/Next Page, page jump input, Zoom Out/%/In, Fit Page, Fit Width, Continuous Vertical Scrolling toggle, Two-Page Spread toggle, Rotate CW, Full Screen Exit. |

### 4.3 Document Viewer, Page Renderer & Editing Overlays
| Symbol | Offset | Role & Owned UI |
|---|---|---|
| `b6` | `1215138` | **Document Tab Controller & Scroll Viewport (`ref={x}`, `ref={w}`)**: Loads PDF bytes via Rust API, restores/debounces `%APPDATA%` file state, manages continuous/two-page/single scroll (`Oe`, `Me`, scroll-spy with `!s && ee !== "single"` guard), pinch/Ctrl+wheel/marquee zoom (`ne`, `floatZoomRef`), keyboard nav, fullscreen `wasFsRef` presentation & scroll restoration (`presNavRef` for slide wheel/swipe when `s=true`; `restoreScroll` to `L.current` on exit), live page content scanning (`scanCurrentPage`), and hosts sidebars (`h6`, `g6`), HUD (`c6`), search (`u6`), password prompt (`l6`), organize (`v6`), print (`y6`), signature (`x6`), and batch tools (`s5`). |
| `a6` / `o6` | `1088998` | **Memoized Single PDF Page Surface (`#page-container-{n}`)**: Lazy `IntersectionObserver` rendering, debounced crisp `<canvas>` redraw on zoom (`renderZoom`), aligned `.textLayer` span rendering, marquee zoom overlay, drag-text selection floating action bar (Highlight + color dots, Underline, Strikethrough, Highlight+Note, Copy, Dismiss), drag-box area highlighter, and `n6` overlay mount. |
| `n6` | `1059045` | **Interactive Annotation & Page-Content Overlay**: Renders/drags/resizes/edits notes, textboxes, drawings, highlight rects, shapes (outline/highlighter/filled), placed images, signatures, detected editable PDF text blocks (`pageTextEdit` overlays), and replaced/hidden page images (`pageImageEdit` overlays). |

### 4.4 Sidebars, Home, Tools & Feature Modals
| Symbol | Offset | Role & Owned UI |
|---|---|---|
| `h6` | `1111378` | **Left Sidebar**: Collapsible Page Thumbnails, Document Outline (Bookmarks), and Embedded Attachments panels. |
| `g6` | `1117132` | **Right Sidebar ("Edit PDF & Properties")**: Contextual tool settings (color, stroke/opacity, font size/family/bold/italic), live `pageEdit` section ("Edit Page Text & Images" — scans active page, lists editable text blocks & replaceable/hideable images), Quick Edit Tools shortcuts, and Document Annotations list. |
| `u6` | `1107064` | **In-Document Search Bar (`Ctrl+F`)**: Whole-document text search, match counter, case-sensitive toggle, Prev/Next jump. |
| `l6` | `1100487` | **Password Prompt Modal**: Prompts for user password on encrypted PDFs. |
| `JR` | `226994` | **Home Screen**: Recent & Favorite PDFs filter/search, quick Open, Samples/drag-drop target. |
| `t6` | `1046971` | **Tools Center**: Grid launching Edit PDF, Comment, Fill & Sign, Organize Pages, Combine, Split, Header/Footer, Watermark, Crop, Export, Compress, Protect, OCR. |
| `v6` / `m6` | `1155747` / `1152958` | **Organize Pages Modal**: Responsive grid, pointer drag-and-drop + swap/move controls (`swapFrom`/`swapTo`), clockwise/CCW rotate, duplicate, delete, insert pages from another PDF, thumbnail worker cache (`m6`), and commit to file via `pdf-lib`. |
| `y6` | `1183966` | **Print Studio Modal**: System printer selector, copies, grayscale, save ink/toner, page range, Size/Poster/Multiple/Booklet sizing, orientation, Comments & Forms options, live page preview slider, export-to-PDF or OS print. |
| `x6` | `1175217` | **Signature Creation Modal**: Draw / Type / Upload signatures, color pickers, persistent `%APPDATA%` signature library. |
| `s5` | `1017164` | **Document Utilities Modal**: Batch tools (`combine`/`merge`, `split`, `header-footer`, `watermark`, `crop`, `export`, `compress`, `protect`, `ocr`). |
| `w6`, `U6`, `S6`, `k6` | `1244854`.. | **System Modals**: About PDFForge (`w6` — Rust stack, Publisher `Md. Alamin`, `alaminprincesarker@gmail.com`, app publishing metadata), Internal Update & Maintenance (`U6` — offline `.exe` upgrade, local/optional manifest check, repair install, confirmed uninstall), Offline Help Guide (`S6`), and Preferences (`k6`). |

