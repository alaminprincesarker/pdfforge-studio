# PDFForge Studio (Native Rust Offline Desktop PDF Reader & Editor)

PDFForge is a 100% offline native Rust desktop PDF reader and editor for Windows 10/11 (x64) and macOS, built with `tao`, `wry` (WebView2/WKWebView), `rfd`, and `rust-embed` into a self-contained native executable (`PDFForge.exe` / `PDFForge-Rust.exe`).

## Documentation & Requirement Tracking
- [Master Requirements & Change Tracker](REQUIREMENTS.md) — Functional requirements checklist and fast "Where to Edit What" index
- [Architecture & Section Map](docs/ARCHITECTURE_AND_SECTIONS.md) — Rust backend modules, IPC routes, CSS blocks, and UI bundle symbol index
- [Features & Workflows Guide](docs/FEATURES_AND_WORKFLOWS.md) — End-to-end feature flows, shortcuts, fullscreen/scroll lifecycle, and packaging steps

## Pure Rust Project Structure
```
PDF Open and Edit/
├── Cargo.toml                 # Rust package manifest & release optimizations
├── Cargo.lock                 # Locked Rust crate dependencies
├── build.rs                   # Compiles app.rc resources & static WebView2 shim (wv2_shim.c)
├── REQUIREMENTS.md            # Functional requirements matrix & section quick-lookup
├── docs/
│   ├── ARCHITECTURE_AND_SECTIONS.md  # Module, IPC & frontend component map
│   └── FEATURES_AND_WORKFLOWS.md     # End-to-end feature specifications & shortcuts
├── src-rust/
│   ├── main.rs                # Native Rust windowing (tao), WebView2 (wry), drag-drop & custom protocol
│   ├── store.rs               # Native Rust AppData store (%APPDATA%\pdfforge)
│   ├── dialogs.rs             # Native Windows file & printer dialogs (rfd + PowerShell Get-Printer)
│   ├── pdf_engine.rs          # Native Rust binary PDF file reader, writer & CLI arg handler
│   ├── installer.rs           # Native Windows installer, uninstaller, Registry & .pdf icon registration
│   ├── app.rc                 # Win32 icon resources (app icon + PDF document icon)
│   ├── wv2_shim.c             # Static WebView2 loader shim (zero external DLL dependency)
│   ├── WebView2Loader.dll     # Embedded inside the Rust binary via include_bytes!
│   └── assets/                # Embedded UI bundle, PDF worker, CMap & font resources (rust-embed)
└── release/
    ├── PDFForge.exe
    ├── PDFForge-Rust.exe
    ├── PDFForge-Setup-1.0.0-Windows-x64.exe
    ├── Uninstall-PDFForge.exe
    ├── PDFForge-1.0.0-Windows-x64-Portable.zip
    └── PDFForge-1.0.0-macOS-Installer.zip
```

## Build & Run Commands (Pure Rust / Cargo)

### 1. Run in Release Mode
```powershell
cargo run --release
```

### 2. Build Standalone Release Binary
```powershell
cargo build --release
```
The compiled executable is output to `target\release\PDFForge-Rust.exe` and copied to `release\PDFForge.exe`.

---

## Developer & App Publishing Information
- **Application Name**: PDFForge Studio (`com.alamin.pdfforge.studio`)
- **Version**: `1.0.0` (Native Rust Edition)
- **Developer / Publisher**: **Md. Alamin**
- **Contact Email**: `alaminprincesarker@gmail.com`
- **Runtime Stack**: Native Rust (`tao` + `wry` WebView2/WKWebView), React 18 UI, PDF.js, pdf-lib
- **Privacy & License**: 100% Offline, Zero Cloud Telemetry, MIT Open License
- **Internal Updater**: `Help -> Check for Updates...` supports offline `.exe` upgrades, local/remote manifest checks, installation repair, and clean uninstall (`--uninstall` / `--uninstall-silent`) without Windows PCA compatibility warnings.
