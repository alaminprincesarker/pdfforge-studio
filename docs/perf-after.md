# PDFForge Studio — Step 7 Performance Report (After Optimization)

**Date:** 2026-11-24  
**Build:** `PDFForge-Rust.exe` v1.2.5 (`cargo build --release --offline`: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`; binary size `26.89 MB`)  
**Test Machine:** AMD Ryzen 5 7535HS (6C / 12T), 5.7 GB usable RAM (6,111 MB total), AMD Radeon Integrated Graphics (2 GB), Windows 11, 150% default DPI scaling (`dpr = 1.5`, viewport `1280×752`) — identical hardware and fixtures to [`docs/perf-before.md`](./perf-before.md).  
**Note on Adobe Acrobat Comparison:** Adobe Acrobat Pro / Reader is **not installed** on this test machine (only Microsoft Edge and Google Chrome are present), so direct side-by-side Acrobat measurements could not be captured on this host. All numbers below are real live measurements from `PDFForge-Rust.exe --perf-bench`.  
**Diagnostics & Correctness:** `0` JS errors / uncaught rejections in `%APPDATA%\pdfforge\startup_diag.log` across all 19 benchmark runs; `17 / 17` Rust unit and integration tests pass (`cargo test --offline`).

---

## 1. Startup Comparison (Before vs After — 5 Warm Launches)

| Metric | Before (Median) | After (Median) | After Range | Target | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **App start → Window visible** | 725.1 ms | **52.4 ms** | 42.2 – 66.2 ms | < 300 ms | **PASS (13.8× faster)** |
| **App start → UI ready** | 728.7 ms | **966.9 ms** | 904.3 – 1,014.5 ms | < 600 ms | **MISSED** *(see note)* |
| **JS boot (`uiBootMs`)** | 187.0 ms | **291.5 ms** | 242.8 – 298.5 ms | — | Measured |
| **Initial DOM nodes (Home)** | 1,608 | **1,611** | 1,611 | < 3,000 | **PASS** |
| **Startup Memory (JS Heap / Rust RSS)** | 7 MB / 37.9 MB | **9 MB / 38.9 MB** | 9 MB / 38.8–39.1 MB | Low | **PASS** |

> **Honest note on `uiReadyMs` (966.9 ms vs < 600 ms target):** Revealing the native dark-painted (`#1e1e1e`) window frame before attaching WebView2 cut time-to-visible-window from `725.1 ms` down to **`52.4 ms`** with zero white flash. However, Microsoft Edge WebView2's synchronous `WebViewBuilder::build(&window)` initialization with GPU flags on this 5.7 GB RAM laptop takes ~600–650 ms before JavaScript begins executing, plus ~243–298 ms for React hydration (`uiBootMs`), bringing total `uiReadyMs` to `904 – 1,015 ms` (whereas before, `windowVisibleMs` and `uiReadyMs` were both gated behind the same blocking `WebViewBuilder` + DLL extraction step).

---

## 2. Before vs After Across All 5 Test PDFs (`150%` DPI, `1280×752`)

### A. Cold Open (Empty Disk Cache) vs Before

| Test PDF | Open → 1st Page (Before → Cold) | Open → Visible Sharp (Before → Cold) | Scroll FPS (Before → Cold) | Frames > 50 ms (Before → Cold) | Max Frame (Before → Cold) | Zoom Visual / Sharp (Before → Cold) | Search 1st / 2nd (Before → Cold) | DOM Nodes (Before → Cold) | Rust RSS (Before → Cold) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **1. `1_text_10mb.pdf`** *(10.41 MB, 220 pg)* | 607.7 → **719.5 ms** | 6,746.0 → **1,189.6 ms** | 14 → **51 fps** | 42 → **0** | 134.0 → **33.8 ms** | 58.2/239.5 → **7.1 / 7.1 ms** | 1,954/1,768 → **1,739.9 / 5.8 ms** | 1,278 → **640** | 163.1 → **150.3 MB** |
| **2. `2_scanned_100mb.pdf`** *(104.10 MB, 56 pg)* | 641.1 → **588.2 ms** | 1,581.3 → **1,079.3 ms** | 24 → **51 fps** | 14 → **0** | 417.0 → **39.7 ms** | timeout → **5.9 / 5.9 ms** | — | 544 → **398** | 89.6 → 322.1 MB* |
| **3. `3_pages_1000.pdf`** *(0.66 MB, 1,000 pg)* | 1,244.7 → **417.2 ms** | 11,018.9 → **767.0 ms** | 9 → **37 fps** | 64 → **3** | 524.1 → **55.3 ms** | 168.0/721.8 → **5.7 / 5.7 ms** | 5,378/3,817 → **1,770.4 / 5.4 ms** | 3,495 → **516** | 112.3 → 148.5 MB |
| **4. `4_vector_map.pdf`** *(0.54 MB, 8 pg, ~144k paths)* | 256.5 → 586.1 ms *(267.7 ms canvas)* | 2,611.9 → **826.5 ms** | 56 → **50 fps** | 0 → **0** | 25.4 → **25.3 ms** | 17.8/48.6 → **9.2 / 9.2 ms** | — | 402 → **398** | 62.5 → 151.3 MB |
| **5. `5_many_images.pdf`** *(24.58 MB, 40 pg, 480 imgs)* | 381.0 → **345.8 ms** | 3,245.0 → **595.5 ms** | 40 → **53 fps** | 3 → **0** | 67.2 → **38.2 ms** | 11.1/11.1 → **4.3 / 4.3 ms** | — | 498 → **400** | **622.8 → 144.2 MB** |

*\*Note on cold `2_scanned_100mb.pdf` Rust RSS (322.1 MB): includes OS-mapped 104 MB mmap pages + Rust RAM LRU BMP render cache (`~160 MB` ceiling) during active rapid scroll; drops to `223.5 MB` upon tab close (`208.3 MB` warm) and never overflows or crashes.*

### B. Warm Disk-Cache Reopen vs Before

| Test PDF | Open → 1st Page (Before → Warm) | Open → Visible Sharp (Before → Warm) | Scroll FPS (Before → Warm) | Frames > 50 ms (Before → Warm) | Max Frame (Before → Warm) | Zoom Visual / Sharp (Before → Warm) | Search 1st / 2nd (Before → Warm) | DOM Nodes | Rust RSS / JS Heap |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **1. `1_text_10mb.pdf`** | 607.7 → **464.6 ms** | 6,746.0 → **662.9 ms** *(10.2×)* | 14 → **59 fps** *(4.2×)* | 42 → **0** | 134.0 → **25.1 ms** | 58.2/239.5 → **13.2 / 13.2 ms** | 1,954/1,768 → **58.0 / 11.5 ms** *(154×)* | **640** | 123.3 MB / 30 MB |
| **2. `2_scanned_100mb.pdf`** | 641.1 → **309.7 ms** *(2.1×)* | 1,581.3 → **495.0 ms** *(3.2×)* | 24 → **50 fps** *(2.1×)* | 14 → **0** | 417.0 → **33.4 ms** | timeout → **7.7 / 166.4 ms** | — | **398** | 208.3 MB / 11 MB |
| **3. `3_pages_1000.pdf`** | 1,244.7 → **238.9 ms** *(5.2×)* | 11,018.9 → **446.1 ms** *(24.7×)* | 9 → **53 fps** *(5.9×)* | 64 → **0** | 524.1 → **25.5 ms** | 168.0/721.8 → **5.8 / 5.8 ms** | 5,378/3,817 → **18.1 / 7.6 ms** *(502×)* | **516** | 112.3 MB / 31 MB |
| **4. `4_vector_map.pdf`** | 256.5 → 400.0 ms | 2,611.9 → **649.4 ms** *(4.0×)* | 56 → **60 fps** | 0 → **0** | 25.4 → **16.9 ms** | 17.8/48.6 → **14.9 / 202.2 ms** | — | **400** | 76.3 MB / 13 MB |
| **5. `5_many_images.pdf`** | 381.0 → **302.7 ms** | 3,245.0 → **488.2 ms** *(6.6×)* | 40 → **59 fps** | 3 → **0** | 67.2 → **33.4 ms** | 11.1/11.1 → **10.9 / 181.9 ms** | — | **400** | **110.6 MB** / 12 MB |

> **Honest note on First-Page Latency Targets:**
> - **Large / 100 MB+ file target (< 1 s):** **PASS** — `2_scanned_100mb.pdf` renders first page in **`588.2 ms` cold / `309.7 ms` warm**, and `3_pages_1000.pdf` in **`417.2 ms` cold / `238.9 ms` warm**.
> - **10 MB text PDF target (< 300 ms):** **PARTIAL (`464.6 ms` warm / `719.5 ms` cold)** — although initial visible-pages-sharp dropped **10.2×** (`6,746 ms → 662.9 ms`) and scroll jumped from `14 fps` to **`59 fps`**, the initial `pdfjs.getDocument()` range handshake on the `10.41 MB` file still adds ~200 ms before the first page mounts so `TextLayer` and PDF.js annotation hooks remain 100% available.

---

## 3. Multi-Tab Switch, 5-File Stress Test & Disk Cache

| Scenario | Before | After | Improvement |
| :--- | :---: | :---: | :---: |
| **Tab switch (`1000 pg → 10 MB`)** | 76.9 ms | **35.3 ms** | **2.2× faster** |
| **Tab switch (`10 MB → 1000 pg`)** | 133.3 ms | **26.2 ms** | **5.1× faster** |
| **DOM nodes with 2 tabs open** | **4,100** *(> 3,000)* | **595 – 640** | **85% reduction** |
| **Single-process 5-file stress test** | **CRASH (`0xc0000409`)** | **5 / 5 PASS (`53–60 fps`, `0–1` frame >50ms, peak RSS `310.3 MB`)** | **Crash eliminated** |
| **Post-close browser cleanup (2-tab / 5-file)** | Leaked `158–361 MB` | **`liveCanvases = 0`, `bitmapCacheMb = 0`, `domNodes = 1,611`, JS Heap `33–72 MB`** | Clean JS teardown |
| **Persistent Disk Cache (`%LOCALAPPDATA%\PDFForge\cache`)** | None | **279 cached page PNGs (`92.53 MB`) + 2 compressed search indexes (`0.19 MB`)** | Configurable limit + Settings **Clear Cache** |

---

## 4. Multi-DPI Scaling Verification (`100%`, `125%`, `150%`, `200%`)

Verified via `--bench-scale=<factor>` (Chromium `--force-device-scale-factor`) on both `1_text_10mb.pdf` and `3_pages_1000.pdf`:

| Windows Scale | Measured `dpr` | Viewport | `1_text_10mb.pdf` FPS / >50ms / Max Frame / Zoom / DOM | `3_pages_1000.pdf` FPS / >50ms / Max Frame / Zoom / DOM |
| :---: | :---: | :---: | :---: | :---: |
| **100%** | `1.0` | `1920×1128` | **60 fps** / `0` / `17.4 ms` / `6.6 ms` vis / `627` | **60 fps** / `0` / `16.9 ms` / `14.5 ms` vis / `462` |
| **125%** | `1.25` | `1536×903` | **55 fps** / `0` / `33.4 ms` / `14.0 ms` vis / `640` | **60 fps** / `0` / `16.9 ms` / `10.2 ms` vis / `535` |
| **150%** | `1.5` | `1280×752` | **60 fps** / `0` / `18.0 ms` / `13.1 ms` vis / `640` | **60 fps** / `0` / `17.0 ms` / `11.1 ms` vis / `595` |
| **200%** | `2.0` | `960×564` | **55 fps** / `0` / `33.6 ms` / `21.2 ms` vis / `640` | **58 fps** / `0` / `33.4 ms` / `13.8 ms` vis / `478` |

Across all 4 DPI scales (`100%` through `200%`), DPR capping (`<= 2`), 512×512 high-zoom tiling, and virtualized DOM kept scrolling at **55–60 FPS** with **0 frames over 50 ms** and **< 650 DOM nodes**.

---

## 5. Bottlenecks Found & Fixes Applied

| # | Bottleneck Found in Step 0 Profiling | Fix Implemented | Verified Impact |
| :-: | :--- | :--- | :--- |
| **1** | **Eager Acrobat `lopdf::Document::load_mem` on every visible page during passive scroll** caused `622.8 MB` RSS spikes and `0xc0000409` multi-file crashes. | Deferred `refreshAll` in `acrobat-pro.js`, skipped `getCachedObjects` in read mode & large-file `getAnnotations` during scroll, and capped `LoDocument::load_mem` in `pdf_layout.rs` to files `<= 2.5 MB` (reading geometry directly from PDFium on larger files). | `5_many_images.pdf` RSS **622.8 → 110.6–144.2 MB**; 5-file stress test **0 crashes**. |
| **2** | **Unvirtualized N-page DOM + N `IntersectionObserver`s + synchronous layout thrashing (`offsetTop`/`getBoundingClientRect`) in scroll handler** caused `3,495–4,100` DOM nodes and `9 FPS` / `524 ms` frames on 1,000-page PDFs. | Virtualized `b6` page list (`visWin` + spacer divs) and `h6` thumbnails (`thWin`), replaced scroll DOM reads with pure O(1)/O(N) arithmetic inside a single rAF loop, added passive scroll listeners + `contain: layout style paint; content-visibility: auto`, and deferred `TextLayer` until 220 ms after scroll settles. | DOM **3,495 → 516** (`4,100 → 640` 2-tab); 1,000-page scroll **9 → 53–60 FPS**, max frame **524 → 16.9–25.5 ms**. |
| **3** | **Slow Rust PDFium re-opening & default PNG Deflate on every `render/page` request** (`388–2,070 ms` per page on 100 MB PDFs) + main-thread PDF.js Canvas2D stalls on large dense text PDFs. | Added `encode_bgra_to_bmp_fast` (0.1 ms uncompressed BGRA BMP over local custom protocol), `load_pdf_from_file` direct IO, background PNG persistence to `%LOCALAPPDATA%\PDFForge\cache`, and automatic `probe_file_fast` routing (`>= 8 MB`, `>= 150 pages`, `>= 120` vectors, or `>= 8` images) to off-main-thread native PDFium. | `render/page` **388–2,070 ms → 13–51 ms cold / 0–5 ms cached**; `1_text_10mb.pdf` scroll **14 → 59 FPS** with `0` frames > 50 ms. |
| **4** | **Synchronous zoom double-render bug + missing progressive low-res paint & tiling** caused `58–168 ms` visual / `240–722 ms` sharp zoom lag and blank pages. | Removed synchronous `setRenderZoom(effectiveZoom)` in `a6`, applied instant GPU CSS `transform: scale()` preview with 130 ms debounce, progressive 0.42× / cached bitmap first paint, `OffscreenCanvas` + `createImageBitmap`, and 512×512 grid-snapped tiles at high zoom. | Zoom visual response **58–168 ms → 4.3–14.9 ms**; zero blank black/white screens. |
| **5** | **Blocking startup & unindexed repeat search** caused `725 ms` window open, `1.8–5.4 s` repeat search, and slow tab switches. | Set release `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`; revealed dark `#1e1e1e` native frame immediately; moved PDFium DLL extraction off startup; added WebView2 GPU rasterization/zero-copy flags; inlined critical dark CSS & lazy-loaded `acrobat-pro.js` / `ocr-ui.js` / 14 `@font-face` rules; preserved current page bitmap on tab suspend; added zlib-compressed per-file disk search index (`search_file_indexed`). | Window visible **725.1 → 52.4 ms**; tab switch **76.9–133.3 → 26.2–35.3 ms**; repeat search **1,768–3,817 → 5.4–11.5 ms**. |
| **6** | **Pre-existing OCR & rotated-page edge bugs uncovered during full verification.** | Fixed `u8` overflow in `ocr.rs` contrast polish, corrected Tesseract 5.5 OSD CCW→CW rotation conversion (`(360 - norm) % 360`), and replaced `PdfPageText::all()` (which clipped `/Rotate 90` pages) with `text_page.chars()` across OCR & search indexing. | All **17 / 17** Rust unit & integration tests pass cleanly. |
