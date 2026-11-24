// PDFForge Studio — Built-in Performance Logger & Ctrl+Shift+P HUD Telemetry

const MB = 1024 * 1024;

const metrics = {
  appStartEpochMs: Date.now(),
  rustWindowVisibleMs: null,
  rustUiReadyMs: null,
  uiBootMs: null,
  fileOpens: [], // { fileName, filePath, openStartMs, firstPageMs, allSharpMs, numPages, fileSize, renderer }
  activeFileOpen: null,
  zoomSamples: [], // { ts, visualMs, sharpMs, fromZoom, toZoom }
  pendingZoom: null,
  tabSwitchSamples: [], // { ts, fromTab, toTab, switchMs }
  pendingTabSwitch: null,
  scrollFps: 60,
  frameCount: 0,
  longFrames16: 0,
  longFrames50: 0,
  maxFrameMs: 0,
  longTasks: [], // { startTime, duration }
  rustRssMb: 0,
  rustSpans: [],
  pageRenderTimes: [], // { page, ms, engine }
  hudOpen: false,
};

let hudEl = null;
let hudTimer = null;
let rafActive = false;
let lastFrameTs = 0;
let fpsWindowFrames = 0;
let fpsWindowStart = 0;
let trackingScroll = false;
let scrollIdleTimer = null;

function nowPerf() {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

// Observe Chromium Long Tasks (>50ms main-thread blocks)
try {
  if (typeof PerformanceObserver !== "undefined") {
    const po = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        metrics.longTasks.push({
          startTime: Math.round(entry.startTime),
          duration: Math.round(entry.duration * 10) / 10,
        });
        if (metrics.longTasks.length > 200) metrics.longTasks.shift();
      }
    });
    po.observe({ type: "longtask", buffered: true });
  }
} catch (_) {}

function stepFrame(ts) {
  if (!rafActive) return;
  if (lastFrameTs > 0) {
    const dt = ts - lastFrameTs;
    metrics.frameCount++;
    if (trackingScroll || metrics.hudOpen) {
      if (dt > 16.7) metrics.longFrames16++;
      if (dt > 50.0) metrics.longFrames50++;
      if (dt > metrics.maxFrameMs && dt < 5000) {
        metrics.maxFrameMs = Math.round(dt * 10) / 10;
      }
      fpsWindowFrames++;
      if (fpsWindowStart === 0) fpsWindowStart = ts;
      const elapsed = ts - fpsWindowStart;
      if (elapsed >= 500) {
        metrics.scrollFps = Math.min(240, Math.round((fpsWindowFrames * 1000) / elapsed));
        fpsWindowFrames = 0;
        fpsWindowStart = ts;
      }
    }
  }
  lastFrameTs = ts;
  if (trackingScroll || metrics.hudOpen) {
    requestAnimationFrame(stepFrame);
  } else {
    rafActive = false;
    lastFrameTs = 0;
  }
}

function ensureFrameLoop() {
  if (!rafActive) {
    rafActive = true;
    lastFrameTs = 0;
    fpsWindowFrames = 0;
    fpsWindowStart = 0;
    requestAnimationFrame(stepFrame);
  }
}

export function markScrollActivity() {
  trackingScroll = true;
  ensureFrameLoop();
  if (scrollIdleTimer) clearTimeout(scrollIdleTimer);
  scrollIdleTimer = setTimeout(() => {
    trackingScroll = false;
  }, 600);
}

export function resetFrameStats() {
  metrics.frameCount = 0;
  metrics.longFrames16 = 0;
  metrics.longFrames50 = 0;
  metrics.maxFrameMs = 0;
  metrics.scrollFps = 60;
  fpsWindowFrames = 0;
  fpsWindowStart = 0;
}

export function markUiReady() {
  if (metrics.uiBootMs == null) {
    metrics.uiBootMs = Math.round(nowPerf() * 10) / 10;
  }
  fetch("/__rust_api__/perf/uiReady", {
    method: "POST",
    body: JSON.stringify({ uiBootMs: metrics.uiBootMs }),
  })
    .then((r) => r.json())
    .then((d) => {
      if (d && typeof d.windowVisibleMs === "number") metrics.rustWindowVisibleMs = d.windowVisibleMs;
      if (d && typeof d.uiReadyMs === "number") metrics.rustUiReadyMs = d.uiReadyMs;
    })
    .catch(() => {});
}

export function beginFileOpen(filePath, fileName) {
  const rec = {
    filePath: filePath || "",
    fileName: fileName || String(filePath || "").split(/[/\\]/).pop() || "document.pdf",
    openStartPerf: nowPerf(),
    firstPageMs: null,
    allSharpMs: null,
    numPages: 0,
    fileSize: 0,
    renderer: "pdfjs",
  };
  metrics.activeFileOpen = rec;
  metrics.fileOpens.push(rec);
  if (metrics.fileOpens.length > 30) metrics.fileOpens.shift();
  return rec;
}

export function markFileMetadata(filePath, { numPages, fileSize, renderer } = {}) {
  const rec = metrics.activeFileOpen;
  if (!rec || (filePath && rec.filePath && rec.filePath !== filePath)) return;
  if (typeof numPages === "number") rec.numPages = numPages;
  if (typeof fileSize === "number") rec.fileSize = fileSize;
  if (renderer) rec.renderer = renderer;
}

export function markPageRendered(filePath, page, durationMs, engine = "pdfjs", isSharp = true) {
  const t = nowPerf();
  metrics.pageRenderTimes.push({
    page,
    ms: Math.round((durationMs || 0) * 10) / 10,
    engine,
    sharp: !!isSharp,
    ts: Math.round(t),
  });
  if (metrics.pageRenderTimes.length > 300) metrics.pageRenderTimes.shift();

  const rec = metrics.activeFileOpen;
  if (rec && (!filePath || !rec.filePath || rec.filePath === filePath)) {
    const dt = Math.round((t - rec.openStartPerf) * 10) / 10;
    if (rec.firstPageMs == null) {
      rec.firstPageMs = dt;
    }
    if (isSharp && !rec.initialOpenSettled) {
      rec.allSharpMs = dt;
    }
    if (engine) rec.renderer = engine;
  }
}

export function beginZoom(fromZoom, toZoom) {
  metrics.pendingZoom = {
    startPerf: nowPerf(),
    fromZoom,
    toZoom,
    visualMs: null,
    sharpMs: null,
  };
  requestAnimationFrame(() => {
    if (metrics.pendingZoom && metrics.pendingZoom.visualMs == null) {
      metrics.pendingZoom.visualMs = Math.round((nowPerf() - metrics.pendingZoom.startPerf) * 10) / 10;
    }
  });
}

export function markZoomSharp() {
  if (!metrics.pendingZoom) return;
  const pz = metrics.pendingZoom;
  pz.sharpMs = Math.round((nowPerf() - pz.startPerf) * 10) / 10;
  metrics.zoomSamples.push({
    ts: Date.now(),
    fromZoom: pz.fromZoom,
    toZoom: pz.toZoom,
    visualMs: pz.visualMs != null ? pz.visualMs : pz.sharpMs,
    sharpMs: pz.sharpMs,
  });
  if (metrics.zoomSamples.length > 60) metrics.zoomSamples.shift();
  metrics.pendingZoom = null;
}

export function beginTabSwitch(fromTab, toTab) {
  if (fromTab === toTab) return;
  const start = nowPerf();
  metrics.pendingTabSwitch = { fromTab, toTab, startPerf: start };
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      if (metrics.pendingTabSwitch && metrics.pendingTabSwitch.toTab === toTab) {
        const dt = Math.round((nowPerf() - metrics.pendingTabSwitch.startPerf) * 10) / 10;
        metrics.tabSwitchSamples.push({ ts: Date.now(), fromTab, toTab, switchMs: dt });
        if (metrics.tabSwitchSamples.length > 40) metrics.tabSwitchSamples.shift();
        metrics.pendingTabSwitch = null;
      }
    });
  });
}

export async function refreshRustStats() {
  try {
    const res = await fetch("/__rust_api__/perf/metrics", { method: "POST", body: "{}" });
    if (res.ok) {
      const data = await res.json();
      if (typeof data.windowVisibleMs === "number") metrics.rustWindowVisibleMs = data.windowVisibleMs;
      if (typeof data.uiReadyMs === "number") metrics.rustUiReadyMs = data.uiReadyMs;
      if (typeof data.rustRssMb === "number") metrics.rustRssMb = data.rustRssMb;
      if (Array.isArray(data.spans)) metrics.rustSpans = data.spans;
      return data;
    }
  } catch (_) {}
  return null;
}

export function getSnapshot() {
  const perfStats =
    window.__pdfforgePerf && typeof window.__pdfforgePerf.getMemoryStats === "function"
      ? window.__pdfforgePerf.getMemoryStats()
      : {};
  let jsHeapMb = perfStats.jsHeapMb ?? null;
  if (jsHeapMb == null && typeof performance !== "undefined" && performance.memory) {
    jsHeapMb = Math.round((performance.memory.usedJSHeapSize / MB) * 10) / 10;
  }
  const domNodes = typeof document !== "undefined" ? document.getElementsByTagName("*").length : 0;
  const liveCanvases =
    typeof document !== "undefined"
      ? Array.from(document.getElementsByTagName("canvas")).filter((c) => c.width > 0 && c.height > 0).length
      : perfStats.liveCanvases || 0;
  const lastOpen = metrics.fileOpens[metrics.fileOpens.length - 1] || null;
  const lastZoom = metrics.zoomSamples[metrics.zoomSamples.length - 1] || null;
  const lastTab = metrics.tabSwitchSamples[metrics.tabSwitchSamples.length - 1] || null;

  return {
    windowVisibleMs: metrics.rustWindowVisibleMs,
    uiReadyMs: metrics.rustUiReadyMs ?? metrics.uiBootMs,
    uiBootMs: metrics.uiBootMs,
    lastFileOpen: lastOpen
      ? {
          fileName: lastOpen.fileName,
          numPages: lastOpen.numPages,
          fileSizeMb: Math.round((lastOpen.fileSize / MB) * 100) / 100,
          firstPageMs: lastOpen.firstPageMs,
          allSharpMs: lastOpen.allSharpMs,
          renderer: lastOpen.renderer,
        }
      : null,
    scrollFps: metrics.scrollFps,
    longFrames16: metrics.longFrames16,
    longFrames50: metrics.longFrames50,
    maxFrameMs: metrics.maxFrameMs,
    lastZoom,
    lastTabSwitch: lastTab,
    jsHeapMb,
    bitmapCacheMb: perfStats.bitmapCacheMb ?? 0,
    rustRssMb: metrics.rustRssMb,
    liveCanvases,
    domNodes,
    longTaskCount: metrics.longTasks.length,
    topLongTasks: metrics.longTasks
      .slice()
      .sort((a, b) => b.duration - a.duration)
      .slice(0, 5),
    recentPageRenders: metrics.pageRenderTimes.slice(-10),
    rustSpans: metrics.rustSpans.slice(-10),
  };
}

function renderHud() {
  if (!hudEl || !metrics.hudOpen) return;
  const snap = getSnapshot();
  const fo = snap.lastFileOpen;
  const lz = snap.lastZoom;
  const lt = snap.lastTabSwitch;
  hudEl.querySelector("[data-perf-body]").innerHTML = `
    <div style="display:grid;grid-template-columns:1fr auto;gap:4px 12px;font-family:Consolas,monospace;font-size:11px;line-height:1.45;">
      <span style="color:#9ca3af;">App &rarr; Window Visible</span><strong style="color:#60a5fa;">${snap.windowVisibleMs != null ? snap.windowVisibleMs + " ms" : "—"}</strong>
      <span style="color:#9ca3af;">App &rarr; UI Ready</span><strong style="color:#60a5fa;">${snap.uiReadyMs != null ? snap.uiReadyMs + " ms" : "—"}</strong>
      <span style="color:#9ca3af;">Open &rarr; First Page</span><strong style="color:#34d399;">${fo && fo.firstPageMs != null ? fo.firstPageMs + " ms" : "—"}</strong>
      <span style="color:#9ca3af;">Open &rarr; Visible Sharp</span><strong style="color:#34d399;">${fo && fo.allSharpMs != null ? fo.allSharpMs + " ms" : "—"}</strong>
      <span style="color:#9ca3af;">Active Renderer</span><strong>${fo ? fo.renderer : "—"}</strong>
      <span style="color:#9ca3af;">Scroll FPS / Max Frame</span><strong style="color:${snap.scrollFps >= 55 ? "#34d399" : "#f59e0b"};">${snap.scrollFps} fps / ${snap.maxFrameMs} ms</strong>
      <span style="color:#9ca3af;">Long Frames (&gt;16ms / &gt;50ms)</span><strong>${snap.longFrames16} / ${snap.longFrames50}</strong>
      <span style="color:#9ca3af;">Zoom (CSS / Sharp)</span><strong>${lz ? `${lz.visualMs}ms / ${lz.sharpMs}ms` : "—"}</strong>
      <span style="color:#9ca3af;">Tab Switch</span><strong>${lt ? lt.switchMs + " ms" : "—"}</strong>
      <span style="color:#9ca3af;">WebView2 JS Heap / Cache</span><strong>${snap.jsHeapMb != null ? snap.jsHeapMb + " MB" : "—"} / ${snap.bitmapCacheMb} MB</strong>
      <span style="color:#9ca3af;">Rust Process RSS</span><strong>${snap.rustRssMb} MB</strong>
      <span style="color:#9ca3af;">Live Canvases / DOM Nodes</span><strong style="color:${snap.domNodes <= 3000 ? "#34d399" : "#f87171"};">${snap.liveCanvases} / ${snap.domNodes}</strong>
    </div>
  `;
}

export function toggleHud(force) {
  const next = typeof force === "boolean" ? force : !metrics.hudOpen;
  metrics.hudOpen = next;
  if (next) {
    ensureFrameLoop();
    if (!hudEl) {
      hudEl = document.createElement("div");
      hudEl.id = "pdfforge-perf-hud";
      hudEl.style.cssText =
        "position:fixed;top:38px;right:12px;z-index:999990;width:320px;background:rgba(18,18,24,0.94);border:1px solid #3c3c4c;border-radius:8px;box-shadow:0 8px 24px rgba(0,0,0,0.45);color:#f3f4f6;padding:10px 12px;user-select:text;";
      hudEl.innerHTML = `
        <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:8px;padding-bottom:6px;border-bottom:1px solid #2e2e3a;">
          <span style="font-size:11.5px;font-weight:700;color:#60a5fa;letter-spacing:0.03em;">PERF MONITOR (Ctrl+Shift+P)</span>
          <div style="display:flex;gap:6px;">
            <button type="button" data-perf-reset style="background:#2b2b36;border:1px solid #3f3f4e;color:#d1d5db;border-radius:4px;font-size:10px;padding:2px 6px;cursor:pointer;">Reset</button>
            <button type="button" data-perf-copy style="background:#2563eb;border:none;color:#fff;border-radius:4px;font-size:10px;padding:2px 6px;cursor:pointer;">Copy JSON</button>
            <button type="button" data-perf-close style="background:transparent;border:none;color:#9ca3af;font-size:14px;cursor:pointer;line-height:1;">&times;</button>
          </div>
        </div>
        <div data-perf-body></div>
      `;
      hudEl.querySelector("[data-perf-reset]").onclick = () => {
        resetFrameStats();
        renderHud();
      };
      hudEl.querySelector("[data-perf-copy]").onclick = () => {
        const txt = JSON.stringify(getSnapshot(), null, 2);
        navigator.clipboard && navigator.clipboard.writeText(txt).catch(() => {});
      };
      hudEl.querySelector("[data-perf-close]").onclick = () => toggleHud(false);
      document.body.appendChild(hudEl);
    }
    hudEl.style.display = "block";
    refreshRustStats().then(renderHud);
    renderHud();
    if (hudTimer) clearInterval(hudTimer);
    hudTimer = setInterval(() => {
      refreshRustStats().then(renderHud);
      renderHud();
    }, 500);
  } else {
    if (hudTimer) {
      clearInterval(hudTimer);
      hudTimer = null;
    }
    if (hudEl) hudEl.style.display = "none";
  }
}

if (typeof window !== "undefined") {
  window.addEventListener("keydown", (ev) => {
    if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && ev.key.toLowerCase() === "p") {
      ev.preventDefault();
      ev.stopPropagation();
      toggleHud();
    }
  });
  // Track scroll activity globally on viewer containers passively
  window.addEventListener(
    "scroll",
    () => {
      markScrollActivity();
    },
    { passive: true, capture: true }
  );
  if (location.search.includes("perf=1")) {
    setTimeout(() => toggleHud(true), 200);
  }
}

const PerfLogger = {
  markUiReady,
  beginFileOpen,
  markFileMetadata,
  markPageRendered,
  beginZoom,
  markZoomSharp,
  beginTabSwitch,
  markScrollActivity,
  resetFrameStats,
  refreshRustStats,
  getSnapshot,
  toggleHud,
};

if (typeof window !== "undefined") {
  window.__pdfforgePerfLog = PerfLogger;

  // Automated CLI benchmark harness (--perf-bench <output.json> <pdf1> ...)
  async function runAutomatedBench() {
    const cfg = window.__pdfforgePerfBench;
    if (!cfg || !Array.isArray(cfg.files) || !cfg.outputPath) return;

    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const waitFor = async (pred, timeoutMs = 20000, intervalMs = 25) => {
      const deadline = nowPerf() + timeoutMs;
      while (nowPerf() < deadline) {
        try {
          const val = pred();
          if (val) return val;
        } catch (_) {}
        await sleep(intervalMs);
      }
      return null;
    };

    // Wait for React + qt store mount
    await waitFor(() => window.qt && typeof window.qt.getState === "function", 15000, 30);
    await sleep(350);
    const startupRust = (await refreshRustStats()) || {};
    const startupSnap = getSnapshot();

    const report = {
      timestamp: new Date().toISOString(),
      dpr: window.devicePixelRatio || 1,
      viewport: { width: window.innerWidth, height: window.innerHeight },
      startup: {
        windowVisibleMs: startupRust.windowVisibleMs ?? startupSnap.windowVisibleMs,
        uiReadyMs: startupRust.uiReadyMs ?? startupSnap.uiReadyMs,
        uiBootMs: startupSnap.uiBootMs,
        rustRssMb: startupRust.rustRssMb ?? startupSnap.rustRssMb,
        jsHeapMb: startupSnap.jsHeapMb,
        domNodes: startupSnap.domNodes,
      },
      files: [],
      tabSwitch: [],
      postClose: null,
    };

    const openedTabIds = [];
    for (let fi = 0; fi < cfg.files.length; fi++) {
      const fp = cfg.files[fi];
      const fname = fp.split(/[/\\]/).pop() || `file_${fi + 1}.pdf`;
      const tabId = `bench-${fi}-${Date.now()}`;
      openedTabIds.push({ tabId, fp, fname });

      beginFileOpen(fp, fname);
      const tOpenStart = nowPerf();
      window.qt.getState().addTab({
        id: tabId,
        filePath: fp,
        fileName: fname,
        currentPage: 1,
        totalPages: 1,
        zoom: 0,
        fitMode: "fit-page",
      });

      // Wait for first page canvas with non-zero bitmap dimensions in active viewer
      let firstPaintedMs = null;
      await waitFor(() => {
        const sc = document.querySelector(`[data-pdfforge-viewer-scroll="1"][data-tab-id="${tabId}"]`);
        if (!sc) return false;
        const p1 = sc.querySelector('[data-page-number="1"] canvas');
        if (p1 && p1.width > 0 && p1.height > 0) {
          if (firstPaintedMs == null) firstPaintedMs = Math.round((nowPerf() - tOpenStart) * 10) / 10;
          return true;
        }
        return false;
      }, 25000, 15);

      // Give visible neighbors time to sharpen, then freeze initial open metrics before scroll/zoom
      await sleep(750);
      const rec = metrics.activeFileOpen;
      if (rec) rec.initialOpenSettled = true;
      const initialFirstPageMs = rec && rec.firstPageMs != null ? rec.firstPageMs : firstPaintedMs;
      const initialAllSharpMs = rec && rec.allSharpMs != null ? rec.allSharpMs : firstPaintedMs;
      const afterOpenRust = (await refreshRustStats()) || {};
      const afterOpenSnap = getSnapshot();

      // Scroll benchmark (60 frames of smooth scrolling across viewer)
      const sc = document.querySelector(`[data-pdfforge-viewer-scroll="1"][data-tab-id="${tabId}"]`);
      resetFrameStats();
      trackingScroll = true;
      ensureFrameLoop();
      const scrollFrameDts = [];
      if (sc && sc.scrollHeight > sc.clientHeight) {
        const totalFrames = 65;
        const maxScroll = Math.min(sc.scrollHeight - sc.clientHeight, 26000);
        const stepPx = Math.max(120, Math.round(maxScroll / totalFrames));
        let prevTs = nowPerf();
        for (let k = 0; k < totalFrames; k++) {
          await new Promise((res) =>
            requestAnimationFrame((ts) => {
              const dt = ts - prevTs;
              if (k > 0) scrollFrameDts.push(dt);
              prevTs = ts;
              sc.scrollTop = Math.min(sc.scrollHeight - sc.clientHeight, sc.scrollTop + stepPx);
              res();
            })
          );
        }
      }
      await sleep(200);
      trackingScroll = false;

      const avgDt = scrollFrameDts.length
        ? scrollFrameDts.reduce((a, b) => a + b, 0) / scrollFrameDts.length
        : 16.67;
      const scrollFpsMeas = Math.min(120, Math.round(1000 / Math.max(1, avgDt)));
      const scrollLong16 = scrollFrameDts.filter((d) => d > 16.7).length;
      const scrollLong50 = scrollFrameDts.filter((d) => d > 50.0).length;
      const scrollMaxMs = scrollFrameDts.length
        ? Math.round(Math.max(...scrollFrameDts) * 10) / 10
        : 0;

      // Zoom benchmark (Ctrl+Wheel zoom in + zoom out)
      const zoomBeforeCount = metrics.zoomSamples.length;
      if (sc) {
        sc.scrollTop = 0;
        await sleep(180);
        const rect = sc.getBoundingClientRect();
        const cx = rect.left + rect.width * 0.5;
        const cy = rect.top + rect.height * 0.5;
        for (let wTick = 0; wTick < 3; wTick++) {
          window.dispatchEvent(
            new WheelEvent("wheel", {
              deltaY: -120,
              ctrlKey: true,
              clientX: cx,
              clientY: cy,
              bubbles: true,
              cancelable: true,
            })
          );
          await sleep(25);
        }
        await waitFor(() => metrics.zoomSamples.length > zoomBeforeCount, 3000, 20);
        await sleep(250);
      }
      const zoomSample =
        metrics.zoomSamples.length > zoomBeforeCount
          ? metrics.zoomSamples[metrics.zoomSamples.length - 1]
          : null;

      // Measure second-pass reopen / search benchmark on 10MB text & 1000-page PDFs
      let search1Ms = null;
      let search2Ms = null;
      if (
        (fname.includes("text_10mb") || fname.includes("pages_1000")) &&
        window.__pdfforgePerf &&
        typeof window.__pdfforgePerf.searchInDocumentChunked === "function"
      ) {
        const tabRec = window.__pdfforgePerf.getTabRuntime
          ? window.__pdfforgePerf.getTabRuntime(tabId)
          : null;
        if (tabRec && tabRec.pdfDoc) {
          const qStr = fname.includes("pages_1000") ? "SECTION_KEY_0500" : "Performance Report";
          const s0 = nowPerf();
          await window.__pdfforgePerf.searchInDocumentChunked(tabRec.pdfDoc, qStr, false, {
            filePath: fp,
          });
          search1Ms = Math.round((nowPerf() - s0) * 10) / 10;
          const s1 = nowPerf();
          await window.__pdfforgePerf.searchInDocumentChunked(tabRec.pdfDoc, qStr, false, {
            filePath: fp,
          });
          search2Ms = Math.round((nowPerf() - s1) * 10) / 10;
        }
      }

      const endRust = (await refreshRustStats()) || {};
      const endSnap = getSnapshot();

      report.files.push({
        fileName: fname,
        numPages: rec ? rec.numPages : 0,
        fileSizeMb: rec ? Math.round((rec.fileSize / MB) * 100) / 100 : 0,
        renderer: rec ? rec.renderer : "pdfjs",
        firstPageMs: initialFirstPageMs,
        firstCanvasMs: firstPaintedMs,
        allSharpMs: initialAllSharpMs,
        scrollFps: scrollFpsMeas,
        scrollLongFrames16: scrollLong16,
        scrollLongFrames50: scrollLong50,
        scrollMaxFrameMs: scrollMaxMs,
        zoomVisualMs: zoomSample ? zoomSample.visualMs : null,
        zoomSharpMs: zoomSample ? zoomSample.sharpMs : null,
        search1Ms,
        search2Ms,
        domNodes: afterOpenSnap.domNodes,
        liveCanvases: endSnap.liveCanvases,
        jsHeapMb: endSnap.jsHeapMb,
        rustRssMb: endRust.rustRssMb ?? afterOpenRust.rustRssMb ?? 0,
        bitmapCacheMb: endSnap.bitmapCacheMb,
        topLongTasks: endSnap.topLongTasks,
        rustSpans: (endRust.spans || []).slice(-6),
      });

      // Keep at most 2 tabs open at once while moving through suite; test tab switching on tabs 0 & 1
      if (fi === 1 && openedTabIds.length >= 2) {
        const t0Id = openedTabIds[0].tabId;
        const t1Id = openedTabIds[1].tabId;
        for (const [fromId, toId] of [
          [t1Id, t0Id],
          [t0Id, t1Id],
        ]) {
          const ts0 = nowPerf();
          beginTabSwitch(fromId, toId);
          window.qt.getState().setActiveTabId(toId);
          await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
          const dt = Math.round((nowPerf() - ts0) * 10) / 10;
          report.tabSwitch.push({ fromId, toId, switchMs: dt });
          await sleep(250);
        }
      }
      if (fi >= 1) {
        // Close oldest tab so memory stays realistic across 5-file run
        const old = openedTabIds.shift();
        if (old) window.qt.getState().closeTab(old.tabId);
        await sleep(200);
      }
    }

    // Close all remaining tabs and check post-close memory
    for (const rem of openedTabIds) {
      window.qt.getState().closeTab(rem.tabId);
    }
    await sleep(600);
    const postRust = (await refreshRustStats()) || {};
    const postSnap = getSnapshot();
    report.postClose = {
      jsHeapMb: postSnap.jsHeapMb,
      rustRssMb: postRust.rustRssMb ?? postSnap.rustRssMb,
      liveCanvases: postSnap.liveCanvases,
      domNodes: postSnap.domNodes,
      bitmapCacheMb: postSnap.bitmapCacheMb,
    };

    await fetch("/__rust_api__/perf/benchDone", {
      method: "POST",
      body: JSON.stringify({ outputPath: cfg.outputPath, report }),
    }).catch(() => {});
  }

  if (window.__pdfforgePerfBench) {
    setTimeout(() => {
      runAutomatedBench().catch((err) => {
        fetch("/__rust_api__/perf/benchDone", {
          method: "POST",
          body: JSON.stringify({
            outputPath: window.__pdfforgePerfBench.outputPath,
            report: { error: String(err && err.stack ? err.stack : err) },
          }),
        }).catch(() => {});
      });
    }, 100);
  }
}

export default PerfLogger;
