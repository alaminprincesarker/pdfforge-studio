// PDFForge Studio — Streaming, Virtualization, LRU Caching, Worker Pool & Memory Monitor

const MB = 1024 * 1024;
const MAX_CANVAS_PIXELS = 16_000_000;
const SUSPEND_IDLE_MS = 30_000;

const state = {
  config: {
    memoryBudgetMb: 300,
    maxActiveDocs: 2,
    renderQuality: "standard",
    reduceMemoryUsage: false,
  },
  activeTabId: "home",
  tabRecords: new Map(), // tabId -> { tabId, filePath, lastUsedAt, suspended, loadingTask, pdfDoc, transport, numPages, fileSize, heavyMode, suspendCb }
  workers: [],
  workerNextIdx: 0,
  bitmapCache: new Map(), // key -> { key, tabId, rev, page, bitmap, width, height, bytes }
  bitmapBytes: 0,
  thumbCache: new Map(), // key -> { key, tabId, page, bitmap, width, height, bytes }
  thumbBytes: 0,
  thumbBudgetBytes: 12 * MB,
  liveCanvases: new Set(),
  renderQueue: [],
  activeRenders: 0,
  memoryPressure: false,
  bufferRadius: 2,
  pressureListeners: new Set(),
  statsListeners: new Set(),
  fileMeta: new Map(), // filePath -> { fileHash, numPages, page1Width, page1Height, isLinearized, preferPdfium }
  enginePerf: new Map(), // tabId -> { pdfjsAvg, pdfiumAvg, preferPdfium }
  lastScrollAt: 0,
};

export function markScrollingNow() {
  state.lastScrollAt = Date.now();
}

export function isRecentlyScrolling(windowMs = 240) {
  return Date.now() - state.lastScrollAt < windowMs;
}

function notifyPressure() {
  const detail = {
    high: state.memoryPressure,
    bufferRadius: state.bufferRadius,
    message: "",
  };
  state.pressureListeners.forEach((cb) => {
    try {
      cb(detail);
    } catch {}
  });
  try {
    window.dispatchEvent(new CustomEvent("pdfforge-memory-pressure", { detail }));
  } catch {}
}

export function onMemoryPressure(cb) {
  state.pressureListeners.add(cb);
  return () => state.pressureListeners.delete(cb);
}

export function getBufferRadius() {
  if (state.config.reduceMemoryUsage || state.memoryPressure) return 1;
  return state.bufferRadius;
}

export function setConfig(partial) {
  if (!partial || typeof partial !== "object") return;
  if (typeof partial.memoryBudgetMb === "number" && partial.memoryBudgetMb >= 64) {
    state.config.memoryBudgetMb = Math.min(2048, Math.max(100, Math.round(partial.memoryBudgetMb)));
  }
  if (typeof partial.maxActiveDocs === "number" && partial.maxActiveDocs >= 1) {
    state.config.maxActiveDocs = Math.min(6, Math.max(1, Math.round(partial.maxActiveDocs)));
  }
  if (partial.renderQuality === "high" || partial.renderQuality === "standard") {
    state.config.renderQuality = partial.renderQuality;
  }
  if (typeof partial.reduceMemoryUsage === "boolean") {
    state.config.reduceMemoryUsage = partial.reduceMemoryUsage;
  }
  enforceMemoryBudget();
}

export function getConfig() {
  return { ...state.config };
}

// --- Shared PDF.js Worker Pool (2..4 workers by CPU cores) ---
export function getSharedWorker() {
  const pdfjs = window.__pdfforgePdfjs;
  if (!pdfjs || typeof pdfjs.PDFWorker !== "function") return undefined;
  state.workers = state.workers.filter((w) => w && !w.destroyed);
  const cores = typeof navigator !== "undefined" ? navigator.hardwareConcurrency || 4 : 4;
  const targetCount = cores >= 4 ? 2 : 1;
  if (state.workers.length < targetCount) {
    try {
      const w = new pdfjs.PDFWorker({ name: `pdfforge-pool-${state.workers.length + 1}` });
      state.workers.push(w);
      return w;
    } catch {
      return state.workers[0];
    }
  }
  const idx = state.workerNextIdx % state.workers.length;
  state.workerNextIdx++;
  return state.workers[idx];
}

// --- HTTP Range PDFDataRangeTransport using custom protocol pdfforge://localhost/file/<id> ---
export async function createStreamingSource(filePath, optsOrCb) {
  const onProgress = typeof optsOrCb === "function" ? optsOrCb : optsOrCb && optsOrCb.onProgress;
  const signal = typeof optsOrCb === "object" && optsOrCb ? optsOrCb.signal : null;
  if (!window.electronAPI || typeof window.electronAPI.registerFileStream !== "function") {
    return null;
  }
  const reg = await window.electronAPI.registerFileStream(filePath);
  if (!reg || !reg.ok) {
    throw new Error((reg && reg.error) || "Failed registering PDF file stream");
  }
  if (signal && signal.aborted) {
    throw new DOMException("Aborted", "AbortError");
  }
  const { url, size, disableAutoFetch, heavyHint, fileHash, numPages, page1Width, page1Height, isLinearized, preferPdfium } = reg;
  if (!size || size <= 0) {
    throw new Error("PDF file is empty or unreadable (0 bytes).");
  }
  state.fileMeta.set(filePath, {
    fileHash: fileHash || "",
    numPages: numPages || 0,
    page1Width: page1Width || 0,
    page1Height: page1Height || 0,
    isLinearized: !!isLinearized,
    preferPdfium: !!preferPdfium,
    size,
    revision: reg.revision || 1,
  });
  const chunkBytes = Math.min(65536, size);
  const headCtrl = new AbortController();
  const onAbortHead = () => headCtrl.abort();
  if (signal) signal.addEventListener("abort", onAbortHead, { once: true });
  let initialData;
  try {
    const res = await fetch(url, {
      method: "GET",
      headers: { Range: `bytes=0-${chunkBytes - 1}` },
      signal: headCtrl.signal,
    });
    if (!res.ok && res.status !== 206) {
      throw new Error(`Stream read failed (${res.status})`);
    }
    initialData = new Uint8Array(await res.arrayBuffer());
  } finally {
    if (signal) signal.removeEventListener("abort", onAbortHead);
  }
  const pdfjs = window.__pdfforgePdfjs;
  if (!pdfjs || typeof pdfjs.PDFDataRangeTransport !== "function") {
    return { fallbackUrl: url, reg };
  }

  class RangeTransport extends pdfjs.PDFDataRangeTransport {
    constructor(length, initial) {
      super(length, initial, length <= initial.byteLength, reg.fileName || "document.pdf");
      this._aborted = false;
      this._controllers = new Set();
      this._loaded = initial.byteLength;
    }
    requestDataRange(begin, end) {
      if (this._aborted || (signal && signal.aborted)) return;
      const ctrl = new AbortController();
      this._controllers.add(ctrl);
      const endIncl = Math.max(begin, Math.min(this.length - 1, end - 1));
      fetch(url, {
        method: "GET",
        headers: { Range: `bytes=${begin}-${endIncl}` },
        signal: ctrl.signal,
      })
        .then((r) => {
          if (!r.ok && r.status !== 206) throw new Error(`HTTP ${r.status}`);
          return r.arrayBuffer();
        })
        .then((buf) => {
          if (this._aborted || (signal && signal.aborted)) return;
          const chunk = new Uint8Array(buf);
          this._loaded = Math.min(this.length, this._loaded + chunk.byteLength);
          this.onDataRange(begin, chunk);
          this.onDataProgress(this._loaded, this.length);
          if (typeof onProgress === "function") {
            try {
              onProgress({ loaded: this._loaded, total: this.length });
            } catch {}
          }
        })
        .catch(() => {})
        .finally(() => {
          this._controllers.delete(ctrl);
        });
    }
    abort() {
      this._aborted = true;
      for (const c of this._controllers) {
        try {
          c.abort();
        } catch {}
      }
      this._controllers.clear();
    }
  }

  const transport = new RangeTransport(size, initialData);
  if (signal) {
    signal.addEventListener("abort", () => transport.abort(), { once: true });
  }
  return {
    transport,
    url,
    size,
    disableAutoFetch: !!disableAutoFetch,
    heavyHint: !!heavyHint,
    preferPdfium: !!preferPdfium,
    isLinearized: !!isLinearized,
    fileHash: fileHash || "",
    numPages: numPages || 0,
    page1Width: page1Width || 0,
    page1Height: page1Height || 0,
    reg,
  };
}

export function getFileMeta(filePath) {
  return state.fileMeta.get(filePath) || null;
}

export function shouldUsePdfium(tabId, filePath, fallbackHeavy = false) {
  const ep = state.enginePerf.get(tabId);
  if (ep && typeof ep.preferPdfium === "boolean") return ep.preferPdfium;
  const meta = filePath ? state.fileMeta.get(filePath) : null;
  if (meta && meta.preferPdfium) return true;
  return !!fallbackHeavy;
}

export function recordEngineTiming(tabId, engine, durationMs) {
  if (!tabId || !Number.isFinite(durationMs)) return;
  let ep = state.enginePerf.get(tabId);
  if (!ep) {
    ep = { pdfjsAvg: null, pdfiumAvg: null, preferPdfium: null, samples: 0 };
    state.enginePerf.set(tabId, ep);
  }
  ep.samples++;
  if (engine === "pdfjs") {
    // Skip very first cold worker init sample before judging steady-state PDF.js vs PDFium speed
    if (ep.samples === 1) return;
    ep.pdfjsAvg = ep.pdfjsAvg == null ? durationMs : ep.pdfjsAvg * 0.6 + durationMs * 0.4;
    // Automatically escalate heavy vector/image pages to Rust PDFium if steady-state PDF.js stalls > 280ms
    if (ep.samples >= 3 && ep.pdfjsAvg > 280) {
      ep.preferPdfium = true;
    }
  } else if (engine === "pdfium") {
    ep.pdfiumAvg = ep.pdfiumAvg == null ? durationMs : ep.pdfiumAvg * 0.6 + durationMs * 0.4;
  }
}

export async function getDiskCachedBitmap(filePath, rev, page, widthPx, rotation = 0, isThumb = false) {
  if (!filePath || !window.electronAPI || typeof window.electronAPI.getDiskRenderBlob !== "function") {
    return null;
  }
  const meta = state.fileMeta.get(filePath);
  if (!meta || !meta.fileHash) return null;
  const zoomBucket = Math.round((Math.max(16, widthPx) + 8) / 16) * 16;
  try {
    const blob = await window.electronAPI.getDiskRenderBlob({
      fileHash: meta.fileHash,
      revision: rev || meta.revision || 1,
      page,
      zoomBucket,
      rotation: ((rotation % 360) + 360) % 360,
      isThumb: !!isThumb,
    });
    if (!blob || blob.size < 24) return null;
    return await createImageBitmap(blob);
  } catch {
    return null;
  }
}

export function putDiskCachedBitmap(filePath, rev, page, canvasOrBmp, widthPx, rotation = 0, isThumb = false) {
  if (!filePath || !canvasOrBmp || !window.electronAPI || typeof window.electronAPI.putDiskRenderBlob !== "function") {
    return;
  }
  const meta = state.fileMeta.get(filePath);
  if (!meta || !meta.fileHash) return;
  const zoomBucket = Math.round((Math.max(16, widthPx) + 8) / 16) * 16;
  const sendBlob = (blob) => {
    if (!blob || blob.size < 24) return;
    window.electronAPI.putDiskRenderBlob(
      {
        fileHash: meta.fileHash,
        revision: rev || meta.revision || 1,
        page,
        zoomBucket,
        rotation: ((rotation % 360) + 360) % 360,
        isThumb: !!isThumb,
      },
      blob
    );
  };
  try {
    if (typeof OffscreenCanvas !== "undefined" && canvasOrBmp instanceof ImageBitmap) {
      const oc = new OffscreenCanvas(canvasOrBmp.width, canvasOrBmp.height);
      const octx = oc.getContext("2d", { alpha: false });
      if (octx) {
        octx.drawImage(canvasOrBmp, 0, 0);
        oc.convertToBlob({ type: "image/png" }).then(sendBlob).catch(() => {});
        return;
      }
    }
    if (typeof canvasOrBmp.toBlob === "function") {
      canvasOrBmp.toBlob(sendBlob, "image/png");
    }
  } catch {}
}

// --- LRU Bitmap Cache (Global across tabs, default 300 MB) ---
function budgetBytes() {
  const mb = state.config.reduceMemoryUsage
    ? Math.min(state.config.memoryBudgetMb, 180)
    : state.config.memoryBudgetMb;
  return Math.max(64, mb) * MB;
}

export function getCachedBitmap(key) {
  const item = state.bitmapCache.get(key);
  if (!item) return null;
  // Promote to MRU
  state.bitmapCache.delete(key);
  state.bitmapCache.set(key, item);
  return item;
}

export function getAnyPageBitmap(tabId, page) {
  let latest = null;
  for (const item of state.bitmapCache.values()) {
    if (item.tabId === tabId && item.page === page) {
      latest = item;
    }
  }
  return latest;
}

export function putCachedBitmap(key, tabId, page, bitmap, width, height) {
  if (!bitmap || width <= 0 || height <= 0) return;
  const bytes = width * height * 4;
  const limit = budgetBytes();
  if (bytes > limit * 0.45) {
    try {
      bitmap.close && bitmap.close();
    } catch {}
    return;
  }
  const existing = state.bitmapCache.get(key);
  if (existing) {
    state.bitmapBytes = Math.max(0, state.bitmapBytes - existing.bytes);
    try {
      existing.bitmap && existing.bitmap.close && existing.bitmap.close();
    } catch {}
    state.bitmapCache.delete(key);
  }
  state.bitmapCache.set(key, { key, tabId, page, bitmap, width, height, bytes });
  state.bitmapBytes += bytes;
  evictBitmapsToLimit(limit);
}

function evictBitmapsToLimit(limit) {
  // Evict inactive tabs first, then LRU overall
  if (state.bitmapBytes <= limit) return;
  for (const [k, item] of state.bitmapCache.entries()) {
    if (state.bitmapBytes <= limit) break;
    if (item.tabId !== state.activeTabId) {
      state.bitmapCache.delete(k);
      state.bitmapBytes = Math.max(0, state.bitmapBytes - item.bytes);
      try {
        item.bitmap && item.bitmap.close && item.bitmap.close();
      } catch {}
    }
  }
  while (state.bitmapBytes > limit && state.bitmapCache.size > 0) {
    const firstKey = state.bitmapCache.keys().next().value;
    const item = state.bitmapCache.get(firstKey);
    state.bitmapCache.delete(firstKey);
    if (item) {
      state.bitmapBytes = Math.max(0, state.bitmapBytes - item.bytes);
      try {
        item.bitmap && item.bitmap.close && item.bitmap.close();
      } catch {}
    }
  }
}

export function evictTabBitmaps(tabId, keepPage = null) {
  for (const [k, item] of state.bitmapCache.entries()) {
    if (item.tabId === tabId) {
      if (keepPage != null && item.page === keepPage) continue;
      state.bitmapCache.delete(k);
      state.bitmapBytes = Math.max(0, state.bitmapBytes - item.bytes);
      try {
        item.bitmap && item.bitmap.close && item.bitmap.close();
      } catch {}
    }
  }
}

// --- Small Thumbnail LRU Cache ---
export function getCachedThumb(key) {
  const item = state.thumbCache.get(key);
  if (!item) return null;
  state.thumbCache.delete(key);
  state.thumbCache.set(key, item);
  return item;
}

export function putCachedThumb(key, tabId, page, bitmap, width, height) {
  if (!bitmap || width <= 0 || height <= 0) return;
  const bytes = width * height * 4;
  const existing = state.thumbCache.get(key);
  if (existing) {
    state.thumbBytes = Math.max(0, state.thumbBytes - existing.bytes);
    try {
      existing.bitmap && existing.bitmap.close && existing.bitmap.close();
    } catch {}
    state.thumbCache.delete(key);
  }
  state.thumbCache.set(key, { key, tabId, page, bitmap, width, height, bytes });
  state.thumbBytes += bytes;
  while (state.thumbCache.size > 72 || state.thumbBytes > state.thumbBudgetBytes) {
    const fk = state.thumbCache.keys().next().value;
    if (!fk) break;
    const item = state.thumbCache.get(fk);
    state.thumbCache.delete(fk);
    if (item) {
      state.thumbBytes = Math.max(0, state.thumbBytes - item.bytes);
      try {
        item.bitmap && item.bitmap.close && item.bitmap.close();
      } catch {}
    }
  }
}

export function clearThumbsForTab(tabId) {
  for (const [k, item] of state.thumbCache.entries()) {
    if (!tabId || item.tabId === tabId) {
      state.thumbCache.delete(k);
      state.thumbBytes = Math.max(0, state.thumbBytes - item.bytes);
      try {
        item.bitmap && item.bitmap.close && item.bitmap.close();
      } catch {}
    }
  }
}

// --- Canvas Cap & Live Canvas Registry ---
export function registerCanvas(canvas) {
  if (canvas) state.liveCanvases.add(canvas);
}

export function unregisterCanvas(canvas) {
  if (!canvas) return;
  state.liveCanvases.delete(canvas);
  try {
    canvas.width = 0;
    canvas.height = 0;
  } catch {}
}

export function computeRenderBudgetScale(baseW, baseH, desiredZoomScale) {
  const rawDpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
  let maxDpr = state.config.renderQuality === "high" ? 2 : 1.5;
  if (state.config.reduceMemoryUsage || state.memoryPressure) {
    maxDpr = 1;
  }
  const dpr = Math.min(maxDpr, Math.max(1, rawDpr));
  let pixelScale = desiredZoomScale * dpr;
  const maxDim = state.config.reduceMemoryUsage ? 3072 : 4096;
  const wPx = baseW * pixelScale;
  const hPx = baseH * pixelScale;
  if (wPx > maxDim || hPx > maxDim) {
    pixelScale = Math.min(pixelScale, maxDim / Math.max(1, baseW, baseH));
  }
  const totalPx = baseW * pixelScale * (baseH * pixelScale);
  if (totalPx > MAX_CANVAS_PIXELS) {
    pixelScale *= Math.sqrt(MAX_CANVAS_PIXELS / totalPx);
  }
  return { dpr, pixelScale, clampedMaxPixels: MAX_CANVAS_PIXELS };
}

// --- Priority Render Queue ---
function pumpQueue() {
  const maxConc = state.config.reduceMemoryUsage ? 1 : 2;
  while (state.activeRenders < maxConc && state.renderQueue.length > 0) {
    state.renderQueue.sort((a, b) => a.priority - b.priority || a.seq - b.seq);
    const job = state.renderQueue.shift();
    if (!job || job.cancelled) continue;
    state.activeRenders++;
    Promise.resolve()
      .then(() => job.run())
      .catch(() => {})
      .finally(() => {
        state.activeRenders = Math.max(0, state.activeRenders - 1);
        pumpQueue();
      });
  }
}

let queueSeq = 1;
export function enqueueRender({ tabId, page, priority = 1, run, onCancel }) {
  const entry = {
    id: queueSeq++,
    seq: queueSeq,
    tabId,
    page,
    priority,
    run,
    onCancel,
    cancelled: false,
  };
  state.renderQueue.push(entry);
  pumpQueue();
  return () => {
    entry.cancelled = true;
    if (typeof entry.onCancel === "function") {
      try {
        entry.onCancel();
      } catch {}
    }
  };
}

export function cancelQueuedRenders(tabId, predicate) {
  state.renderQueue = state.renderQueue.filter((item) => {
    if (item.tabId === tabId && (!predicate || predicate(item))) {
      item.cancelled = true;
      if (typeof item.onCancel === "function") {
        try {
          item.onCancel();
        } catch {}
      }
      return false;
    }
    return true;
  });
}

// --- Tab Lifecycle, Active Doc Limit (Default 2) & 30s Idle Suspension ---
export function setActiveTab(tabId) {
  state.activeTabId = tabId;
  const rec = state.tabRecords.get(tabId);
  if (rec) {
    rec.lastUsedAt = Date.now();
    rec.suspended = false;
  }
  enforceActiveDocLimit();
}

export function registerTabLifecycle(tabId, filePath, opts = {}) {
  const prev = state.tabRecords.get(tabId) || {};
  state.tabRecords.set(tabId, {
    ...prev,
    tabId,
    filePath,
    lastUsedAt: Date.now(),
    suspended: false,
    ...opts,
  });
  enforceActiveDocLimit();
}

export function bindTabRuntime(tabId, { loadingTask, pdfDoc, transport, worker, numPages, fileSize, heavyMode, suspendCb }) {
  const rec = state.tabRecords.get(tabId) || { tabId, lastUsedAt: Date.now() };
  rec.loadingTask = loadingTask || rec.loadingTask || null;
  rec.pdfDoc = pdfDoc || rec.pdfDoc || null;
  rec.transport = transport || rec.transport || null;
  if (worker || (loadingTask && loadingTask._worker)) {
    rec.worker = worker || (loadingTask && loadingTask._worker) || rec.worker || null;
  }
  if (typeof numPages === "number") rec.numPages = numPages;
  if (typeof fileSize === "number") rec.fileSize = fileSize;
  if (typeof heavyMode === "boolean") rec.heavyMode = heavyMode;
  if (typeof suspendCb === "function") rec.suspendCb = suspendCb;
  rec.suspended = false;
  rec.lastUsedAt = Date.now();
  state.tabRecords.set(tabId, rec);
  enforceActiveDocLimit();
}

export async function releaseTabRuntime(tabId, isClose = false, keepPage = null) {
  cancelQueuedRenders(tabId);
  evictTabBitmaps(tabId, isClose ? null : keepPage);
  clearThumbsForTab(tabId);
  if (isClose) state.enginePerf.delete(tabId);
  const rec = state.tabRecords.get(tabId);
  if (!rec) return;
  const docWorker = rec.worker || (rec.loadingTask && rec.loadingTask._worker) || null;
  try {
    if (rec.transport && typeof rec.transport.abort === "function") rec.transport.abort();
  } catch {}
  try {
    if (rec.loadingTask && typeof rec.loadingTask.destroy === "function") await rec.loadingTask.destroy();
  } catch {}
  try {
    if (rec.pdfDoc && typeof rec.pdfDoc.destroy === "function") await rec.pdfDoc.destroy();
  } catch {}
  rec.transport = null;
  rec.loadingTask = null;
  rec.pdfDoc = null;
  rec.worker = null;

  if (window.__acrobatPro && typeof window.__acrobatPro.evictFileCaches === "function" && rec.filePath) {
    window.__acrobatPro.evictFileCaches(rec.filePath);
  }
  if (isClose) {
    state.tabRecords.delete(tabId);
    if (rec.filePath && window.electronAPI && window.electronAPI.unregisterFileStream) {
      window.electronAPI.unregisterFileStream(rec.filePath).catch(() => {});
    }
  } else {
    rec.suspended = true;
    if (rec.filePath && window.electronAPI && window.electronAPI.suspendFileStream) {
      window.electronAPI.suspendFileStream(rec.filePath).catch(() => {});
    }
  }

  // Tear down worker if no active tab is still bound to it (reclaims worker ChunkedStream buffers)
  if (docWorker && !docWorker.destroyed) {
    let inUse = false;
    for (const r of state.tabRecords.values()) {
      if (!r.suspended && r.pdfDoc && (r.worker === docWorker || (r.loadingTask && r.loadingTask._worker === docWorker))) {
        inUse = true;
        break;
      }
    }
    if (!inUse && (isClose || state.config.reduceMemoryUsage || state.memoryPressure || (rec.fileSize || 0) > 20 * MB)) {
      state.workers = state.workers.filter((w) => w !== docWorker);
      try {
        docWorker.destroy && docWorker.destroy();
      } catch {}
    }
  }
}

export function enforceActiveDocLimit() {
  const maxActive = state.config.reduceMemoryUsage ? 1 : Math.max(1, state.config.maxActiveDocs || 2);
  const liveTabs = Array.from(state.tabRecords.values()).filter(
    (r) => !r.suspended && (r.pdfDoc || r.loadingTask)
  );
  if (liveTabs.length <= maxActive) return;

  // Sort inactive tabs LRU first
  const inactive = liveTabs
    .filter((r) => r.tabId !== state.activeTabId)
    .sort((a, b) => (a.lastUsedAt || 0) - (b.lastUsedAt || 0));

  let excess = liveTabs.length - maxActive;
  for (const rec of inactive) {
    if (excess <= 0) break;
    excess--;
    if (typeof rec.suspendCb === "function") {
      try {
        rec.suspendCb(rec.tabId);
      } catch {}
    } else {
      releaseTabRuntime(rec.tabId, false);
    }
  }
}

// --- Global Memory Monitor ---
export function getMemoryStats() {
  let jsHeapMb = null;
  try {
    if (typeof performance !== "undefined" && performance.memory) {
      jsHeapMb = Math.round(performance.memory.usedJSHeapSize / MB);
    }
  } catch {}
  return {
    jsHeapMb,
    bitmapCacheMb: Math.round(state.bitmapBytes / MB),
    thumbCacheMb: Math.round(state.thumbBytes / MB),
    liveCanvases: state.liveCanvases.size,
    budgetMb: state.config.memoryBudgetMb,
    activeDocs: Array.from(state.tabRecords.values()).filter((r) => !r.suspended && r.pdfDoc).length,
    highPressure: state.memoryPressure,
  };
}

export function enforceMemoryBudget() {
  const limit = budgetBytes();
  evictBitmapsToLimit(limit);

  let jsUsed = 0;
  try {
    if (typeof performance !== "undefined" && performance.memory) {
      jsUsed = performance.memory.usedJSHeapSize || 0;
    }
  } catch {}

  const high =
    state.config.reduceMemoryUsage ||
    state.bitmapBytes > limit * 0.88 ||
    state.liveCanvases.size > 14 ||
    (jsUsed > 0 && jsUsed > limit * 1.4);

  if (high) {
    // Suspend LRU inactive tabs and trim bitmap cache cleanly
    const inactive = Array.from(state.tabRecords.values())
      .filter((r) => r.tabId !== state.activeTabId && !r.suspended && r.pdfDoc)
      .sort((a, b) => (a.lastUsedAt || 0) - (b.lastUsedAt || 0));
    for (const rec of inactive) {
      if (typeof rec.suspendCb === "function") {
        try {
          rec.suspendCb(rec.tabId);
        } catch {}
      } else {
        releaseTabRuntime(rec.tabId, false);
      }
    }
    evictBitmapsToLimit(Math.floor(limit * 0.65));
    state.bufferRadius = 1;
  } else {
    state.bufferRadius = 2;
  }

  if (high !== state.memoryPressure) {
    state.memoryPressure = high;
    notifyPressure();
  }
}

// Periodic background memory + idle tab suspension monitor (every 5s)
if (typeof window !== "undefined") {
  setInterval(() => {
    const now = Date.now();
    for (const rec of state.tabRecords.values()) {
      if (
        rec.tabId !== state.activeTabId &&
        !rec.suspended &&
        rec.pdfDoc &&
        now - (rec.lastUsedAt || now) >= SUSPEND_IDLE_MS
      ) {
        if (typeof rec.suspendCb === "function") {
          try {
            rec.suspendCb(rec.tabId);
          } catch {}
        } else {
          releaseTabRuntime(rec.tabId, false);
        }
      }
    }
    enforceMemoryBudget();
  }, 5000);
}

// --- Chunked Non-Blocking Idle Search + Persistent Rust Disk Index ---
export function searchInDocumentChunked(pdfDoc, query, caseSensitive = false, opts = {}) {
  const { onProgress, onResults, signal, filePath } = opts;
  const term = String(query || "").trim();
  if (!pdfDoc || !term) {
    return Promise.resolve([]);
  }
  const totalPages = pdfDoc.numPages || 0;

  // Prefer Rust persistent disk-indexed search (zero main-thread blocking, <10ms on repeat searches!)
  let resolvedPath = filePath || "";
  if (!resolvedPath) {
    for (const rec of state.tabRecords.values()) {
      if (rec.pdfDoc === pdfDoc && rec.filePath) {
        resolvedPath = rec.filePath;
        break;
      }
    }
  }
  if (
    resolvedPath &&
    !resolvedPath.startsWith("cache:") &&
    window.electronAPI &&
    typeof window.electronAPI.searchIndexed === "function"
  ) {
    return window.electronAPI
      .searchIndexed(resolvedPath, term, !!caseSensitive)
      .then((res) => {
        if (signal && signal.aborted) return [];
        if (res && res.ok && Array.isArray(res.hits)) {
          const hits = res.hits;
          if (typeof onProgress === "function") {
            try { onProgress(res.totalPages || totalPages, res.totalPages || totalPages, hits); } catch {}
          }
          if (typeof onResults === "function") {
            try { onResults(hits); } catch {}
          }
          return hits;
        }
        return _searchPdfjsChunked(pdfDoc, term, caseSensitive, opts);
      })
      .catch(() => _searchPdfjsChunked(pdfDoc, term, caseSensitive, opts));
  }
  return _searchPdfjsChunked(pdfDoc, term, caseSensitive, opts);
}

function _searchPdfjsChunked(pdfDoc, term, caseSensitive = false, opts = {}) {
  const { onProgress, onResults, signal } = opts;
  const needle = caseSensitive ? term : term.toLowerCase();
  const totalPages = pdfDoc.numPages || 0;
  const results = [];
  let nextPage = 1;

  const scheduleIdle =
    typeof window !== "undefined" && typeof window.requestIdleCallback === "function"
      ? (fn) => window.requestIdleCallback(fn, { timeout: 20 })
      : (fn) => setTimeout(() => fn({ timeRemaining: () => 16 }), 0);

  return new Promise((resolve) => {
    const runSlice = async () => {
      if (signal && signal.aborted) {
        resolve(results);
        return;
      }
      const batchEnd = Math.min(totalPages, nextPage + 15);
      const pageNums = [];
      while (nextPage <= batchEnd) pageNums.push(nextPage++);

      // Process in micro-batches of 4 concurrent getPage/getTextContent calls cleanly under 50ms per slice
      for (let i = 0; i < pageNums.length; i += 4) {
        if (signal && signal.aborted) {
          resolve(results);
          return;
        }
        const sub = pageNums.slice(i, i + 4);
        const subMatches = await Promise.all(
          sub.map(async (pgNo) => {
            try {
              const pg = await pdfDoc.getPage(pgNo);
              if (signal && signal.aborted) {
                try { pg.cleanup && pg.cleanup(); } catch {}
                return null;
              }
              const txtContent = await pg.getTextContent();
              const joined = (txtContent.items || []).map((it) => it.str || "").join(" ");
              const hay = caseSensitive ? joined : joined.toLowerCase();
              let count = 0;
              let pos = hay.indexOf(needle);
              while (pos !== -1) {
                count++;
                pos = hay.indexOf(needle, pos + needle.length);
              }
              try { pg.cleanup && pg.cleanup(); } catch {}
              return count > 0 ? { pageNumber: pgNo, matchCount: count } : null;
            } catch {
              return null;
            }
          })
        );
        for (const m of subMatches) {
          if (m) results.push(m);
        }
      }

      if (typeof onProgress === "function") {
        try {
          onProgress(batchEnd, totalPages, results);
        } catch {}
      }

      if (nextPage <= totalPages) {
        scheduleIdle(runSlice);
      } else {
        if (typeof onResults === "function") {
          try {
            onResults(results);
          } catch {}
        }
        resolve(results);
      }
    };

    scheduleIdle(runSlice);
  });
}

export async function runBackgroundJob(payload, opts = {}) {
  const { onProgress, signal } = opts;
  const api = window.electronAPI;
  if (!api || typeof api.startBackgroundJob !== "function") {
    throw new Error("Background job engine unavailable");
  }
  const startRes = await api.startBackgroundJob(payload);
  if (!startRes || !startRes.ok || !startRes.jobId) {
    throw new Error((startRes && startRes.error) || "Failed starting background job");
  }
  const jobId = startRes.jobId;
  let aborted = false;
  const onAbort = () => {
    aborted = true;
    api.cancelBackgroundJob && api.cancelBackgroundJob(jobId).catch(() => {});
  };
  if (signal) {
    if (signal.aborted) {
      onAbort();
      throw new DOMException("Cancelled", "AbortError");
    }
    signal.addEventListener("abort", onAbort, { once: true });
  }
  try {
    while (true) {
      const snap = await api.getBackgroundJobStatus(jobId);
      if (typeof onProgress === "function" && snap) {
        try {
          onProgress(snap);
        } catch {}
      }
      if (!snap || snap.status === "done") return snap;
      if (snap.status === "cancelled" || aborted) {
        throw new DOMException("Cancelled", "AbortError");
      }
      if (snap.status === "error") {
        throw new Error(snap.error || snap.message || "Background job failed");
      }
      await new Promise((r) => setTimeout(r, 120));
    }
  } finally {
    if (signal) signal.removeEventListener("abort", onAbort);
  }
}

export function getTabRuntime(tabId) {
  return state.tabRecords.get(tabId) || null;
}

const PerfAPI = {
  setConfig,
  getConfig,
  onMemoryPressure,
  getBufferRadius,
  getSharedWorker,
  createStreamingSource,
  getFileMeta,
  shouldUsePdfium,
  recordEngineTiming,
  getDiskCachedBitmap,
  putDiskCachedBitmap,
  markScrollingNow,
  isRecentlyScrolling,
  getCachedBitmap,
  getAnyPageBitmap,
  putCachedBitmap,
  evictTabBitmaps,
  getCachedThumb,
  putCachedThumb,
  clearThumbsForTab,
  registerCanvas,
  unregisterCanvas,
  computeRenderBudgetScale,
  enqueueRender,
  cancelQueuedRenders,
  setActiveTab,
  registerTabLifecycle,
  bindTabRuntime,
  getTabRuntime,
  releaseTabRuntime,
  enforceActiveDocLimit,
  enforceMemoryBudget,
  getMemoryStats,
  searchInDocumentChunked,
  runBackgroundJob,
};

if (typeof window !== "undefined") {
  window.__pdfforgePerf = PerfAPI;
}

export default PerfAPI;
