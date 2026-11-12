# PDFForge Studio — Features, State Flows & Packaging Guide

> Detailed operational reference explaining how each user-facing workflow runs end-to-end across Rust and the UI bundle.

---

## 1. Window Modes, Full Screen Presentation & Scrolling Restoration

### 1.1 State Ownership
- **Rust (`src-rust/main.rs`)**: Owns OS window placement (`was_maximized_before_fs`, `last_fullscreen`), calls `window.set_maximized(...)` / `window.set_fullscreen(...)`, resizes WebView2 client bounds (`sync_webview_bounds`), restores input focus (`window.set_focus()` + `webview.focus()`), and pushes `__pdfforgeNotifyFullScreen` & `__pdfforgeNotifyMaximized`.
- **Store (`qt`)**: Holds `isFullScreen`, `isMaximized`, `setIsFullScreen`, `toggleFullScreen`. Calls `window.electronAPI.setFullScreen(next)` directly so React and Win32 never drift out of sync.
- **Document Tab (`b6`)**: Holds local `viewMode` (`ee`: `"continuous"` | `"two-page"` | `"single"`) synchronized with `openTabs[active].viewMode`.

### 1.2 Entering Full Screen (`Ctrl+L` / `F11` / TitleBar / `c6`)
1. `qt.toggleFullScreen()` sets `isFullScreen = true` and notifies Rust (`SetFullScreen(true)`).
2. Rust saves `was_maximized_before_fs`, un-maximizes if needed (avoiding Win32 borderless placement glitches), sets `Fullscreen::Borderless(None)`, syncs WebView bounds, focuses WebView2, and emits resize events.
3. In `b6`, `isFullScreen` (`s=true`) isolates the active slide `(ee==="single" || s) && $ !== I` **without overwriting `ee`**, resets `scrollTop = 0`, starts the 3-second Presentation Banner auto-hide timer (`showPresBanner`), and starts the 1.8-second bottom HUD auto-hide timer (`showFsBar`).
4. `presNavRef` attaches capture-phase `wheel` and touch swipe listeners so scrolling/swiping up/down switches slides when at the top/bottom of the slide, while still allowing internal vertical/horizontal pan scrolling when zoomed in (`canScroll(d)`).

### 1.3 Exiting Full Screen -> Normal Window Restoration
1. Pressing `Esc`, `Ctrl+L`, `F11`, or clicking `Exit` / `c6` Toggle routes through `qt.toggleFullScreen()` -> Rust `SetFullScreen(false)`.
2. Rust clears `window.set_fullscreen(None)`, immediately restores `window.set_maximized(true)` if `was_maximized_before_fs` was true, calls `sync_webview_bounds`, restores OS & WebView2 focus (`window.set_focus()` + `webview.focus()`), requests redraw, and pushes BOTH `__pdfforgeNotifyFullScreen(false)` and `__pdfforgeNotifyMaximized` plus `resize` events.
3. In `b6`:
   - `presNavRef` cleanup removes all fullscreen capture-phase `wheel` / `touch*` listeners immediately.
   - Because `ee` was preserved (`"continuous"` or `"two-page"`), all pages re-mount immediately.
   - `wasFsRef` triggers `restoreScroll()` on the next 2 animation frames and again at `130ms` (after Win32 DWM finishes re-maximizing and `GR`/`XR`/`QR`/`c6` remount): sets `x.current.style.overflow = "auto"`, focuses `x.current`, and calls `scrollIntoView` to keep the user on their exact active slide (`L.current`) instead of resetting to Page 1.
   - Scroll-spy (`!s && ee !== "single"`) resumes updating `currentPage` automatically during normal scrolling.
   - Normal-mode container `w` uses `minWidth: "100%", width: "max-content"` (for Continuous/Single modes) so zoomed pages scroll horizontally without left-edge clipping (`paddingBottom: "20px"`). Note: the bottom floating pill `c6` is rendered **only in Full Screen mode** so Normal Window stays uncluttered over document text.

### 1.4 Keyboard Shortcuts Summary
| Shortcut | Action | Handler |
|---|---|---|
| `Ctrl+O` | Open one or multiple PDF documents into tabs (`__pdfforgePickAndOpenPDFs`) | `GR`, `C6` |
| `Ctrl+W` | Close active document tab | `GR`, `C6` |
| `Ctrl+S` / `Ctrl+Shift+S` | Save / Save As | `C6` |
| `Ctrl+P` | Open Print Studio Modal | `C6` -> `b6` |
| `Ctrl+F` | Find in Document (`u6`) | `C6` |
| `Ctrl+Z` / `Ctrl+Y` (`Ctrl+Shift+Z`) | Undo / Redo annotations & edits | `b6` |
| `Ctrl+C` | Copy selected PDF text to clipboard | `b6` / `a6` popup |
| `Ctrl + =` / `Ctrl + -` / `Ctrl + 0` | Zoom In / Zoom Out / Actual Size (`100%`) | `C6` |
| `Touchpad Pinch` / `Ctrl + Wheel` | Smooth cursor-anchored zoom across all pages | `b6` (`ne`, `floatZoomRef`) |
| `Ctrl + \` | Toggle Toolbar collapse (Reader mode) | `C6` |
| `Ctrl+L` / `F11` / `Esc` (in FS) | Enter / Exit Full Screen Presentation Mode | `C6` |
| `Space` (hold + drag) / Hand Tool | Drag-to-pan canvas | `b6` (`dr`, `ty`, `ie`) |
| `Arrow Keys` / `PgUp` / `PgDn` / `Home` / `End` | Page / slide navigation | `b6` |

---

## 2. Zoom Engine (`10% – 800%`)

1. All zoom adjustments (Toolbar `QR`, Floating Bar `c6`, Shortcuts `Ctrl+/-`, Marquee Zoom Tool, Touchpad Pinch `Ctrl+Wheel`, Two-Finger Touch/Pointer Pinch) update `tab.zoom` in `qt` (`t(e, {zoom: be})`) and sync `H.current` + `floatZoomRef.current`.
2. Cursor-anchored scroll compensation in `ne(zoom, clientX, clientY)` preserves focal point ratios across re-render.
3. Every rendered page component (`a6`) derives its pixel dimensions directly from `tab.zoom` and applies debounced high-DPI `<canvas>` sharpness redraw (`renderZoom` after `75ms`) + CSS `--scale-factor` updates on `.textLayer`.
4. Exiting or entering Full Screen preserves the active document zoom across every page/slide.

---

## 3. Text Selection, Highlighting & Drawing Shapes

- **Text Selection (`a6` + `index-ClXAvMiX.css`)**:
  - `.textLayer` spans align with rendered PDF geometry (`--scale-factor`).
  - Only active when `activeTool` is `select`, `highlight`, `underline`, or `strikethrough`; other tools apply `.tool-non-text` (`user-select: none; pointer-events: none`).
  - Releasing drag-selection displays a contextual floating pill above the selection (`Highlight` + swatch colors, `Underline`, `Strikethrough`, `Highlight & Note`, `Copy`, `Dismiss`), or immediately commits when using `highlight` / `underline` / `strikethrough` tool.
- **Shape & Highlighter Drawing (`QR` + `n6` + `g6`)**:
  - **Highlighter tool**: works on selected text lines AND rectangular drag-box highlights anywhere on scanned/image areas.
  - **Freehand Ink / Brush**: toggle between opaque Pen and `Highlighter Freehand Brush` (`drawAsHighlighter`).
  - **Shapes**: `Rectangle`, `Circle / Oval`, `Arrow`, `Straight Line` with 3 style modes (`Outline Border`, `Highlighter Fill (Transparent)`, `Solid Tint Fill`), layered above background elements so shapes & ink highlight overlays never vanish under opaque shapes.

---

## 4. Edit PDF Right Panel (`g6`) & Live Text/Image Replacement

- Opening `Edit PDF` (from `Home`/`Tools`, `Edit -> Edit PDF Options`, or `QR` **Edit PDF**) calls `openEditPdfPanel()`, opening the right sidebar (`g6`). Secondary tools (`Edit Page`, `Sticky Note`, `Stamp`, `Redact`, `Eraser`, color swatches, font formatting, Acrobat tools) stay organized in `g6` so the top toolbar `QR` stays clean on a single row.
- Entering `Edit Page` (`pageEdit` tab / rail icon in `g6`) runs `scanCurrentPage()` via `xs.extractEditablePageContent()` and shows dashed overlays on editable text/images (`editPgMode && rpOpen && rpTab === "pageEdit"`).
- Clicking any detected text block creates/selects a `pageTextEdit` annotation (`origX`, `origY`, `origWidth`, `origHeight`, `text`, `fontSize`, `fontFamily`, `bold`, `italic`, `color`, `bgColor`).
- Exiting `Edit Page` (`Exit Edit Page` button, collapsing the Right Bar, switching tabs, or picking `Select`/`Hand`/`Highlight`/etc.) clears `editPageContentMode`, resets `activeTool` to `"select"`, automatically prunes any untouched `pageTextEdit` clicks (`text === origText && !deleted`), removes all dashed selection boxes, and restores full `.textLayer` PDF text selection (`pointer-events: none` on edited overlays when outside Edit panels).
- Clicking Replace/Hide on any detected image opens a file picker for PNG/JPG/WebP replacement (`pageImageEdit`) or masks it out, and `Save` / `Save As` bakes all changes cleanly via `pdf-lib`.

---

## 5. Build, Verification & Production Release Packaging

### 5.1 Toolchain Environment
- Rust Cargo (`$env:USERPROFILE\.cargo\bin\cargo.exe`)
- MinGW-w64 (`C:\Program Files\CodeBlocks\MinGW\bin`: `windres`, `gcc`, `ar`, `objdump`)
- Node.js (used strictly as an offline validator: `node --input-type=module --check`)

### 5.2 Build & Package Workflow
1. Validate JS bundle syntax:
   ```powershell
   Get-Content "src-rust\assets\assets\index-11jbxaY0.js" -Raw | node --input-type=module --check
   ```
2. Compile optimized native release binary:
   ```powershell
   $env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\CodeBlocks\MinGW\bin;$env:PATH"
   cargo build --release
   ```
3. Verify zero static `WebView2Loader.dll` import table dependency:
   ```powershell
   objdump -p target\release\PDFForge-Rust.exe | Select-String "DLL Name:"
   ```
4. Update `release\` executables & installers (`PDFForge.exe`, `PDFForge-Rust.exe`, `PDFForge-Setup-1.0.0-Windows-x64.exe`, `Uninstall-PDFForge.exe`), touch old timestamps on `WebView2Loader.dll`, and package `PDFForge-1.0.0-Windows-x64-Portable.zip` and `PDFForge-1.0.0-macOS-Installer.zip` using `tar -a -c -f`.
5. Verify uninstaller (`Uninstall-PDFForge.exe --uninstall-silent` deletes `%LOCALAPPDATA%\Programs\PDFForge`, shortcuts, and registry entries cleanly with exit code 0), then run silent install (`PDFForge-Setup-1.0.0-Windows-x64.exe --install-silent`), verify Control Panel registration (`Md. Alamin (PDFForge Studio)`), `.pdf` file associations, shortcuts, and launch stability.

### 5.3 App Publishing & Maintenance Notes
- **Publisher**: Md. Alamin (`alaminprincesarker@gmail.com`)
- **Package ID**: `com.alamin.pdfforge.studio`
- **Stack**: Rust (`tao` + `wry` WebView2 / WKWebView), React 18 UI, PDF.js, pdf-lib
- **Manifest**: `src-rust/app.manifest` embeds Win10/11 `<compatibility>`, `<trustInfo level="asInvoker">`, PerMonitorV2 DPI awareness, and long-path support so Windows Smart App / PCA compatibility checks recognize native install/uninstall lifecycle cleanly.

