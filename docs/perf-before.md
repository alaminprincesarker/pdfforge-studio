# PDFForge Studio — Step 0 Baseline Performance Report (Before Optimization)

**Date:** 2026-11-24  
**Build:** `PDFForge-Rust.exe` v1.2.5 (`cargo build --release`, `opt-level = 3`, `lto = "thin"`)  
**Test Machine:** AMD Ryzen 5 7535HS (6C / 12T), 5.7 GB usable RAM (6,111 MB total), AMD Radeon Integrated Graphics (2 GB), Windows 11, 150% DPI scaling (`dpr = 1.5`, viewport `1280×752`).  
**Note on Adobe Acrobat:** Adobe Acrobat is not installed on this test machine (only Microsoft Edge and Google Chrome are present).

---

## 1. Startup Baseline (Release Build)

| Metric | Median (5 runs) | Range | Target |
| :--- | :---: | :---: | :---: |
| App start -> Window visible | **725.1 ms** | 630.1 – 1,080.9 ms (3,088.7 ms cold) | < 300 ms |
| App start -> UI ready | **728.7 ms** | 631.3 – 1,083.4 ms (3,091.3 ms cold) | < 600 ms |
| Boot JS execution (`uiBootMs`) | **187.0 ms** | 150.8 – 229.3 ms | — |
| Initial DOM nodes (Home) | **1,608** | 1,608 | < 3,000 |
| Startup memory (WebView2 JS / Rust RSS) | **7 MB / 37.9 MB** | 7 MB / 37.7–38.0 MB | Low |

---

## 2. Benchmark Results Across 5 Test PDFs (Before)

All 5 fixtures were generated deterministically in `%TEMP%\pdfforge_perf_suite\` and exercised end-to-end inside the live `PDFForge-Rust.exe` WebView2 application via `--perf-bench`:

| Test PDF | Size / Pages | Renderer | Open -> 1st Page | Open -> Visible Sharp | Scroll FPS | Long Frames (>16ms / >50ms) | Max Frame | Zoom (Visual / Sharp) | Search (1st / 2nd) | DOM Nodes | Live Canvases | Mem (JS Heap / Rust RSS) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **1. `1_text_10mb.pdf`** | 10.41 MB / 220 pg | `pdfjs` | **607.7 ms** | 6,746.0 ms | **14 fps** | 62 / 42 | **134.0 ms** | 58.2 / 239.5 ms | 1,954 / 1,768 ms | 1,278 | 4 | 123 MB / 163.1 MB |
| **2. `2_scanned_100mb.pdf`** | 104.10 MB / 56 pg | `pdfium` | **641.1 ms** | 1,581.3 ms | **24 fps** | 48 / 14 | **417.0 ms** | — *(timed out)* | — | 544 | 5 | 15 MB / 89.6 MB |
| **3. `3_pages_1000.pdf`** | 0.66 MB / 1,000 pg | `pdfium` | **1,244.7 ms** | 11,018.9 ms | **9 fps** | 64 / 64 | **524.1 ms** | 168.0 / 721.8 ms | 5,378 / 3,817 ms | **3,495** | 4 | 51 MB / 112.3 MB |
| **4. `4_vector_map.pdf`** | 0.54 MB / 8 pg (~144k paths) | `pdfjs` | **256.5 ms** | 2,611.9 ms | **56 fps** | 35 / 0 | 25.4 ms | 17.8 / 48.6 ms | — | 402 | 4 | 29 MB / 62.5 MB |
| **5. `5_many_images.pdf`** | 24.58 MB / 40 pg (480 imgs) | `pdfjs` | **381.0 ms** | 3,245.0 ms | **40 fps** | 47 / 3 | **67.2 ms** | 11.1 / 11.1 ms | — | 498 | 4 | 14 MB / **622.8 MB** |

### Multi-Tab Switch & Memory Release Baseline (`1_text_10mb.pdf` <-> `3_pages_1000.pdf`)
- **Tab switch latency:** `76.9 ms` (`1000pg -> 10MB`), `133.3 ms` (`10MB -> 1000pg`)
- **DOM nodes with 2 tabs open:** **4,100** (exceeds 3,000 budget)
- **Post-close memory leak observed:** After closing tabs, Rust RSS remained at **158.8 – 361.4 MB** (up from 37.9 MB at startup), and opening all 5 files sequentially in one process crashed with `0xc0000409` due to concurrent unthrottled `lopdf::Document::load_mem` parses.

---

## 3. Top 5 Slowest Bottlenecks Identified by Profiling

1. **Eager Acrobat Layout Parsing (`mountAcrobatPageSurface` -> `lopdf::Document::load_mem` on every visible page during scroll/read mode)**
   - *Evidence:* Rust RSS exploded from `37.7 MB` to **`622.8 MB`** on `5_many_images.pdf` (24.6 MB) and **`548 MB`** in debug / **`286 MB`** in release on `1_text_10mb.pdf`, and stayed elevated (`158–361 MB`) after closing tabs. Even in passive reading mode, every visible `PageView` spawned `/__rust_api__/acrobat/getLayout`, causing multiple background threads to parse the full PDF into `lopdf` ASTs concurrently.
2. **Unvirtualized N-Page DOM + N `IntersectionObserver`s + Synchronous Layout Thrashing in Scroll Handler**
   - *Evidence:* `3_pages_1000.pdf` created **3,495 DOM nodes** (4,100 with 2 tabs), ran at **9 FPS** with **64/64 frames > 50 ms** and **524.1 ms max frame**, and `1_text_10mb.pdf` ran at **14 FPS** (max frame `134 ms`). Root causes:
     - `b6` mounted all `N` `PageView` components and `N` separate `IntersectionObserver` instances (plus `h6` mounted `N` thumbnail components and `N` observers even when collapsed).
     - `b6`'s scroll handler ran binary search reading `el.offsetTop + el.offsetHeight` and looped calling `getBoundingClientRect()` on every scroll frame.
     - Global `window.addEventListener("wheel", ..., { passive: false, capture: true })` forced main-thread sync on every normal scroll wheel tick.
3. **Rust `pdfium-render` Re-Opening Entire PDF & Slow Default PNG Encoding on Every `render/page` Call**
   - *Evidence:* Rust spans on `2_scanned_100mb.pdf` measured **`388 – 2,070 ms`** per single `743×962` page render, and **`99 – 426 ms`** per page on `3_pages_1000.pdf`. Every request re-called `load_pdf_from_byte_slice(&backing, None)` under a single global mutex AND encoded PNG using default slow Deflate level instead of reusing open PDFium document handles, fast BMP/fast-PNG encoding, and an on-disk render cache.
4. **Synchronous Zoom Re-Render (Defeating Debounce) + Missing Progressive Low-Res First Paint**
   - *Evidence:* `a6` had `useEffect(() => setRenderZoom(effectiveZoom), [effectiveZoom])` which triggered immediate full canvas re-renders on every zoom tick, producing **168 ms visual / 721–1,001 ms sharp** zoom latency on `3_pages_1000.pdf` and **58–68 ms visual / 240–336 ms sharp** on `1_text_10mb.pdf`, with blank skeletons during initial page paint instead of instant low-res / disk-cached bitmaps + GPU `transform: scale()` preview.
5. **Eager Startup Blocking, Unindexed Full-Doc Search, and Non-Optimized Release/WebView2 Config**
   - *Evidence:* Startup took **630 – 1,081 ms** to window visible and **631 – 1,083 ms** to UI ready because `main.rs` synchronously wrote embedded DLLs before window creation, lacked WebView2 GPU rasterization/zero-copy flags, used `lto = "thin"` instead of `lto = "fat"` + `panic = "abort"`, and `index.html` eagerly loaded `acrobat-pro.js` (130 KB), `ocr-ui.js`, 2 external stylesheets, and 14 `@font-face` rules before first paint.
   - Repeated text searches took **1,954 -> 1,768 ms** (`10 MB text`) and **5,378 -> 3,817 ms** (`1,000 pages`) because there was no persistent per-file disk text search index.
