// ============================================================================
// PDFForge Offline OCR UI Module (Acrobat-style Recognize Text & Scanned Alert)
// ============================================================================

const sessionPromptedFiles = new Set();
const scanCache = new Map(); // key: `${filePath}|${rev}|${page}` -> ScanDetectResult
let currentViewerCtx = null; // { tabId, filePath, currentPage, totalPages, revision, reloadFn, updateTabFn, showToastFn, getSettingsFn, updateSettingsFn }
let activeOverlayEl = null;
let cachedLangResp = null;

function escHtml(s) {
  return String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function normPathKey(fp) {
  return String(fp || "").trim().toLowerCase();
}

function getThemePalette() {
  const s = currentViewerCtx && currentViewerCtx.getSettingsFn ? currentViewerCtx.getSettingsFn() : {};
  const isLight = s && s.theme === "light";
  return isLight
    ? {
        isLight: true,
        backdrop: "rgba(15, 23, 42, 0.45)",
        surface: "#ffffff",
        surfaceAlt: "#f8fafc",
        inputBg: "#ffffff",
        border: "#cbd5e1",
        divider: "#e2e8f0",
        text: "#0f172a",
        muted: "#64748b",
        accent: "#2563eb",
        accentHover: "#1d4ed8",
        accentSoft: "rgba(37, 99, 235, 0.12)",
        btnSecondaryBg: "#f1f5f9",
        btnSecondaryText: "#1e293b",
        btnSecondaryHover: "#e2e8f0",
      }
    : {
        isLight: false,
        backdrop: "rgba(0, 0, 0, 0.66)",
        surface: "#222229",
        surfaceAlt: "#19191f",
        inputBg: "#16161b",
        border: "#363643",
        divider: "#2f2f3b",
        text: "#f1f5f9",
        muted: "#9ca3af",
        accent: "#3b82f6",
        accentHover: "#2563eb",
        accentSoft: "rgba(59, 130, 246, 0.16)",
        btnSecondaryBg: "#2e2e38",
        btnSecondaryText: "#e5e7eb",
        btnSecondaryHover: "#3b3b47",
      };
}

function closeActiveOverlay() {
  if (activeOverlayEl && activeOverlayEl.parentNode) {
    activeOverlayEl.parentNode.removeChild(activeOverlayEl);
  }
  activeOverlayEl = null;
}

function registerViewerContext(ctx) {
  currentViewerCtx = { ...(currentViewerCtx || {}), ...ctx };
}

function clearScanCacheForFile(filePath) {
  const norm = normPathKey(filePath);
  for (const k of Array.from(scanCache.keys())) {
    if (normPathKey(k.split("|")[0]) === norm) {
      scanCache.delete(k);
    }
  }
}

function getCachedScanStatus(filePath, page, rev) {
  if (!filePath || !page) return null;
  const r = rev || 0;
  const exact = scanCache.get(`${normPathKey(filePath)}|${r}|${page}`);
  if (exact) return exact;
  return scanCache.get(`${normPathKey(filePath)}|0|${page}`) || null;
}

async function checkScannedPage(filePath, page, rev) {
  if (!filePath || !page || !window.pdfforgeOcr || !window.pdfforgeOcr.detectScanned) {
    return null;
  }
  const key = `${normPathKey(filePath)}|${rev || 0}|${page}`;
  if (scanCache.has(key)) return scanCache.get(key);
  try {
    const res = await window.pdfforgeOcr.detectScanned(filePath, page);
    if (res && typeof res.isScanned === "boolean") {
      scanCache.set(key, res);
      return res;
    }
  } catch (_) {}
  return null;
}

async function onPageDisplayed(ctx) {
  registerViewerContext(ctx);
  if (!ctx || !ctx.filePath || !ctx.page) return;
  const filePath = ctx.filePath;
  const page = ctx.page;
  const rev = ctx.revision || 0;

  // Never block rendering; fetch scan classification asynchronously
  const res = await checkScannedPage(filePath, page, rev);
  if (!res || !res.isScanned || !res.ocrAvailable) return;

  // Respect user setting & once-per-file-per-session rule
  const norm = normPathKey(filePath);
  if (sessionPromptedFiles.has(norm)) return;

  let showAlert = true;
  try {
    if (currentViewerCtx && currentViewerCtx.getSettingsFn) {
      const s = currentViewerCtx.getSettingsFn();
      if (s && s.showScannedPageAlert === false) showAlert = false;
    } else if (window.electronAPI && window.electronAPI.getSettings) {
      const s = await window.electronAPI.getSettings();
      if (s && s.showScannedPageAlert === false) showAlert = false;
    }
  } catch (_) {}

  if (!showAlert) return;

  // Verify the user is still viewing the same file and no other modal is open
  if (activeOverlayEl) return;
  if (
    currentViewerCtx &&
    currentViewerCtx.filePath &&
    normPathKey(currentViewerCtx.filePath) !== norm
  ) {
    return;
  }

  sessionPromptedFiles.add(norm);
  showScannedPageAlertModal({
    filePath,
    page,
    totalPages: res.totalPages || ctx.totalPages || 1,
  });
}

function showScannedPageAlertModal({ filePath, page, totalPages }) {
  closeActiveOverlay();
  const pal = getThemePalette();

  const overlay = document.createElement("div");
  overlay.className = "pdfforge-modal-backdrop";
  overlay.setAttribute("data-pdfforge-modal", "scanned-alert");
  overlay.setAttribute("role", "dialog");
  overlay.setAttribute("aria-modal", "true");
  overlay.style.cssText = `
    position: fixed; inset: 0; z-index: 999999; display: flex; align-items: center; justify-content: center;
    background: ${pal.backdrop}; backdrop-filter: blur(3px); padding: 16px;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  `;

  overlay.innerHTML = `
    <div style="width: min(440px, 94vw); background: ${pal.surface}; color: ${pal.text}; border: 1px solid ${pal.border};
                border-radius: 10px; box-shadow: 0 24px 64px rgba(0,0,0,0.45); overflow: hidden; font-size: 13px;">
      <div style="display:flex; align-items:center; justify-content:space-between; padding: 13px 16px; border-bottom: 1px solid ${pal.divider};">
        <div style="display:flex; align-items:center; gap: 9px; font-weight: 600; font-size: 14px;">
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="${pal.accent}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="flex-shrink:0;">
            <circle cx="12" cy="12" r="10"></circle>
            <line x1="12" y1="16" x2="12" y2="12"></line>
            <line x1="12" y1="8" x2="12.01" y2="8"></line>
          </svg>
          <span>Scanned Page Alert</span>
        </div>
        <button type="button" data-ocr-close style="background:transparent; border:none; color:${pal.muted}; cursor:pointer; font-size:18px; line-height:1; padding:2px 6px; border-radius:4px;" title="Close">&times;</button>
      </div>
      <div style="padding: 16px; line-height: 1.55;">
        <p style="margin: 0 0 14px 0;">
          This page contains only an image. Would you like to run text recognition to make the text on this page accessible?
        </p>
        <label style="display:flex; align-items:center; gap:8px; cursor:pointer; color:${pal.muted}; font-size:12px; user-select:none;">
          <input type="checkbox" data-ocr-dont-show style="width:15px; height:15px; accent-color:${pal.accent}; cursor:pointer;" />
          <span>Do not show this message again</span>
        </label>
      </div>
      <div style="display:flex; justify-content:flex-end; gap:8px; padding: 12px 16px; background:${pal.surfaceAlt}; border-top:1px solid ${pal.divider};">
        <button type="button" data-ocr-no style="padding:6px 16px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.btnSecondaryText}; font-size:12.5px; font-weight:500; cursor:pointer;">No</button>
        <button type="button" data-ocr-yes style="padding:6px 18px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; font-size:12.5px; font-weight:600; cursor:pointer;">Yes</button>
      </div>
    </div>
  `;

  const persistDontShow = async () => {
    const chk = overlay.querySelector("[data-ocr-dont-show]");
    if (chk && chk.checked) {
      try {
        if (currentViewerCtx && currentViewerCtx.updateSettingsFn) {
          currentViewerCtx.updateSettingsFn({ showScannedPageAlert: false });
        }
        if (window.electronAPI && window.electronAPI.updateSettings) {
          await window.electronAPI.updateSettings({ showScannedPageAlert: false });
        }
      } catch (_) {}
    }
  };

  const cleanup = () => {
    window.removeEventListener("keydown", onKey);
    closeActiveOverlay();
  };

  const onKey = (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      persistDontShow();
      cleanup();
    }
  };
  window.addEventListener("keydown", onKey);

  overlay.querySelector("[data-ocr-close]").onclick = () => {
    persistDontShow();
    cleanup();
  };
  overlay.querySelector("[data-ocr-no]").onclick = () => {
    persistDontShow();
    cleanup();
  };
  overlay.querySelector("[data-ocr-yes]").onclick = async () => {
    await persistDontShow();
    cleanup();
    openRecognizeTextDialog({
      filePath,
      currentPage: page,
      totalPages,
      defaultPageScope: "current",
    });
  };
  overlay.addEventListener("mousedown", (ev) => {
    if (ev.target === overlay) {
      persistDontShow();
      cleanup();
    }
  });

  document.body.appendChild(overlay);
  activeOverlayEl = overlay;
  const yesBtn = overlay.querySelector("[data-ocr-yes]");
  if (yesBtn) setTimeout(() => yesBtn.focus(), 20);
}

async function fetchLanguages() {
  if (!window.pdfforgeOcr || !window.pdfforgeOcr.languages) {
    return { available: false, languages: [], engineError: "OCR bridge unavailable" };
  }
  try {
    const resp = await window.pdfforgeOcr.languages();
    cachedLangResp = resp;
    return resp;
  } catch (e) {
    return { available: false, languages: [], engineError: String(e) };
  }
}

async function openRecognizeTextDialog(opts = {}) {
  const ctx = currentViewerCtx || {};
  const filePath = opts.filePath || ctx.filePath;
  if (!filePath) {
    if (ctx.showToastFn) ctx.showToastFn("Open a PDF document first before running Recognize Text.", "info");
    return;
  }

  const currentPage = Math.max(1, Number(opts.currentPage || ctx.currentPage || 1));
  const totalPages = Math.max(1, Number(opts.totalPages || ctx.totalPages || 1));
  const defaultScope = opts.defaultPageScope || "all";

  let savedSettings = {
    language: "eng",
    secondaryLanguage: "",
    outputMode: "searchable_image",
    downsampleDpi: 300,
  };
  try {
    const s = ctx.getSettingsFn
      ? ctx.getSettingsFn()
      : window.electronAPI && window.electronAPI.getSettings
      ? await window.electronAPI.getSettings()
      : null;
    if (s && s.lastOcrSettings) {
      savedSettings = {
        language: s.lastOcrSettings.language || "eng",
        secondaryLanguage: s.lastOcrSettings.secondaryLanguage || "",
        outputMode: s.lastOcrSettings.outputMode || "searchable_image",
        downsampleDpi:
          typeof s.lastOcrSettings.downsampleDpi === "number" ? s.lastOcrSettings.downsampleDpi : 300,
      };
    }
  } catch (_) {}

  const langResp = await fetchLanguages();
  const langs = Array.isArray(langResp.languages) && langResp.languages.length
    ? langResp.languages
    : [{ code: "eng", name: "English (US)" }, { code: "ben", name: "Bangla (Bengali)" }];

  if (!langs.some((l) => l.code === savedSettings.language)) {
    savedSettings.language = langs[0].code;
  }

  closeActiveOverlay();
  const pal = getThemePalette();

  const overlay = document.createElement("div");
  overlay.className = "pdfforge-modal-backdrop";
  overlay.setAttribute("data-pdfforge-modal", "recognize-text");
  overlay.setAttribute("role", "dialog");
  overlay.setAttribute("aria-modal", "true");
  overlay.style.cssText = `
    position: fixed; inset: 0; z-index: 999999; display: flex; align-items: center; justify-content: center;
    background: ${pal.backdrop}; backdrop-filter: blur(3px); padding: 16px;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  `;

  overlay.innerHTML = `
    <div style="width: min(490px, 95vw); max-height: 92vh; overflow-y: auto; background: ${pal.surface}; color: ${pal.text};
                border: 1px solid ${pal.border}; border-radius: 10px; box-shadow: 0 24px 64px rgba(0,0,0,0.48); font-size: 13px;">
      <div style="display:flex; align-items:center; justify-content:space-between; padding: 13px 16px; border-bottom: 1px solid ${pal.divider};">
        <div style="display:flex; align-items:center; gap:8px; font-weight:600; font-size:14px;">
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="${pal.accent}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path>
            <polyline points="14 2 14 8 20 8"></polyline>
            <line x1="8" y1="13" x2="16" y2="13"></line>
            <line x1="8" y1="17" x2="13" y2="17"></line>
          </svg>
          <span>Recognize Text</span>
        </div>
        <button type="button" data-rt-close style="background:transparent; border:none; color:${pal.muted}; cursor:pointer; font-size:18px; line-height:1; padding:2px 6px; border-radius:4px;" title="Close">&times;</button>
      </div>

      <div data-rt-body style="padding: 15px 16px; display:flex; flex-direction:column; gap: 14px;">
        ${
          !langResp.available
            ? `<div style="padding:10px 12px; border-radius:6px; background:rgba(239,68,68,0.12); border:1px solid rgba(239,68,68,0.35); color:#f87171; font-size:12px;">
                 Offline OCR runtime could not be loaded: ${escHtml(langResp.engineError || "tesseract55.dll missing")}.
               </div>`
            : ""
        }

        <fieldset style="border: 1px solid ${pal.border}; border-radius: 7px; padding: 10px 12px; margin: 0;">
          <legend style="padding: 0 6px; font-size: 11.5px; font-weight: 600; color: ${pal.muted}; text-transform: uppercase; letter-spacing: 0.04em;">Pages</legend>
          <div style="display:flex; flex-direction:column; gap: 8px;">
            <label style="display:flex; align-items:center; gap:8px; cursor:pointer;">
              <input type="radio" name="rt-pages" value="all" ${defaultScope === "all" ? "checked" : ""} style="accent-color:${pal.accent};" />
              <span>All pages <span style="color:${pal.muted}; font-size:11.5px;">(1 - ${totalPages})</span></span>
            </label>
            <label style="display:flex; align-items:center; gap:8px; cursor:pointer;">
              <input type="radio" name="rt-pages" value="current" ${defaultScope === "current" ? "checked" : ""} style="accent-color:${pal.accent};" />
              <span>Current page <span style="color:${pal.muted}; font-size:11.5px;">(Page ${currentPage})</span></span>
            </label>
            <div style="display:flex; align-items:center; gap:8px; flex-wrap:wrap;">
              <label style="display:flex; align-items:center; gap:8px; cursor:pointer;">
                <input type="radio" name="rt-pages" value="range" ${defaultScope === "range" ? "checked" : ""} style="accent-color:${pal.accent};" />
                <span>From page</span>
              </label>
              <input type="number" data-rt-from min="1" max="${totalPages}" value="1" ${defaultScope !== "range" ? "disabled" : ""}
                style="width:66px; padding:4px 7px; border-radius:5px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12.5px;" />
              <span style="color:${pal.muted};">to</span>
              <input type="number" data-rt-to min="1" max="${totalPages}" value="${totalPages}" ${defaultScope !== "range" ? "disabled" : ""}
                style="width:66px; padding:4px 7px; border-radius:5px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12.5px;" />
            </div>
            <div data-rt-range-err style="display:none; color:#f87171; font-size:11.5px; padding-left:22px;"></div>
          </div>
        </fieldset>

        <fieldset style="border: 1px solid ${pal.border}; border-radius: 7px; padding: 10px 12px; margin: 0;">
          <legend style="padding: 0 6px; font-size: 11.5px; font-weight: 600; color: ${pal.muted}; text-transform: uppercase; letter-spacing: 0.04em;">Settings</legend>
          <div style="display:grid; grid-template-columns: 140px 1fr; gap: 10px 12px; align-items: start;">
            <label style="padding-top:6px; color:${pal.muted}; font-size:12.5px;">Document Language:</label>
            <div style="position:relative;">
              <button type="button" data-rt-lang-btn
                style="width:100%; display:flex; align-items:center; justify-content:space-between; padding:6px 10px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12.5px; cursor:pointer; text-align:left;">
                <span data-rt-lang-label></span>
                <span style="font-size:10px; color:${pal.muted};">&#9662;</span>
              </button>
              <div data-rt-lang-menu style="display:none; position:absolute; left:0; right:0; top:calc(100% + 4px); z-index:20; background:${pal.surface}; border:1px solid ${pal.border}; border-radius:7px; box-shadow:0 12px 32px rgba(0,0,0,0.45); overflow:hidden;">
                <div style="padding:6px; border-bottom:1px solid ${pal.divider};">
                  <input type="text" data-rt-lang-search placeholder="Search language..."
                    style="width:100%; box-sizing:border-box; padding:5px 8px; border-radius:5px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12px; outline:none;" />
                </div>
                <div data-rt-lang-list style="max-height:175px; overflow-y:auto;"></div>
              </div>
            </div>

            <label style="padding-top:6px; color:${pal.muted}; font-size:12.5px;">Secondary Language:</label>
            <select data-rt-sec-lang style="width:100%; padding:6px 8px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12.5px;">
              <option value="">None (Single language)</option>
              ${langs.map((l) => `<option value="${escHtml(l.code)}" ${savedSettings.secondaryLanguage === l.code ? "selected" : ""}>+ ${escHtml(l.name)} (${escHtml(l.code)})</option>`).join("")}
            </select>

            <label style="padding-top:6px; color:${pal.muted}; font-size:12.5px;">Output:</label>
            <div>
              <select data-rt-output style="width:100%; padding:6px 8px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12.5px;">
                <option value="searchable_image" ${savedSettings.outputMode === "searchable_image" ? "selected" : ""}>Searchable Image</option>
                <option value="searchable_image_exact" ${savedSettings.outputMode === "searchable_image_exact" ? "selected" : ""}>Searchable Image (Exact)</option>
                <option value="editable_text" ${savedSettings.outputMode === "editable_text" ? "selected" : ""}>Editable Text & Images (Coming Soon)</option>
              </select>
              <div data-rt-output-hint style="margin-top:4px; font-size:11px; color:${pal.muted};"></div>
            </div>

            <label style="padding-top:6px; color:${pal.muted}; font-size:12.5px;">Downsample To:</label>
            <select data-rt-dpi style="width:100%; padding:6px 8px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.inputBg}; color:${pal.text}; font-size:12.5px;">
              <option value="0" ${savedSettings.downsampleDpi === 0 ? "selected" : ""}>Off (Keep original image resolution)</option>
              <option value="150" ${savedSettings.downsampleDpi === 150 ? "selected" : ""}>150 dpi (Compact)</option>
              <option value="300" ${savedSettings.downsampleDpi === 300 ? "selected" : ""}>300 dpi (Recommended)</option>
              <option value="600" ${savedSettings.downsampleDpi === 600 ? "selected" : ""}>600 dpi (High Accuracy)</option>
            </select>
          </div>
        </fieldset>
      </div>

      <div data-rt-footer style="display:flex; justify-content:flex-end; align-items:center; gap:8px; padding:12px 16px; background:${pal.surfaceAlt}; border-top:1px solid ${pal.divider};">
        <button type="button" data-rt-cancel style="padding:6px 16px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.btnSecondaryText}; font-size:12.5px; font-weight:500; cursor:pointer;">Cancel</button>
        <button type="button" data-rt-ok style="padding:6px 20px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; font-size:12.5px; font-weight:600; cursor:pointer;">OK</button>
      </div>
    </div>
  `;

  document.body.appendChild(overlay);
  activeOverlayEl = overlay;

  let selectedLang = savedSettings.language;
  const fromInp = overlay.querySelector("[data-rt-from]");
  const toInp = overlay.querySelector("[data-rt-to]");
  const rangeErr = overlay.querySelector("[data-rt-range-err]");
  const langBtn = overlay.querySelector("[data-rt-lang-btn]");
  const langLabel = overlay.querySelector("[data-rt-lang-label]");
  const langMenu = overlay.querySelector("[data-rt-lang-menu]");
  const langSearch = overlay.querySelector("[data-rt-lang-search]");
  const langList = overlay.querySelector("[data-rt-lang-list]");
  const secLangSel = overlay.querySelector("[data-rt-sec-lang]");
  const outSel = overlay.querySelector("[data-rt-output]");
  const outHint = overlay.querySelector("[data-rt-output-hint]");
  const dpiSel = overlay.querySelector("[data-rt-dpi]");
  const okBtn = overlay.querySelector("[data-rt-ok]");
  const cancelBtn = overlay.querySelector("[data-rt-cancel]");

  const renderLangLabel = () => {
    const found = langs.find((l) => l.code === selectedLang);
    langLabel.textContent = found ? `${found.name} (${found.code})` : selectedLang;
  };
  const renderLangList = (filterText = "") => {
    const q = filterText.trim().toLowerCase();
    const filtered = langs.filter(
      (l) => !q || l.name.toLowerCase().includes(q) || l.code.toLowerCase().includes(q)
    );
    langList.innerHTML = filtered.length
      ? filtered
          .map(
            (l) => `
          <button type="button" data-code="${escHtml(l.code)}"
            style="width:100%; text-align:left; padding:6px 10px; border:none; background:${
              l.code === selectedLang ? pal.accentSoft : "transparent"
            }; color:${pal.text}; font-size:12px; cursor:pointer; display:flex; justify-content:space-between;">
            <span>${escHtml(l.name)}</span>
            <span style="color:${pal.muted}; font-size:11px;">${escHtml(l.code)}</span>
          </button>`
          )
          .join("")
      : `<div style="padding:10px; color:${pal.muted}; font-size:12px;">No matching traineddata language. Add .traineddata to tessdata folder.</div>`;
    langList.querySelectorAll("button[data-code]").forEach((b) => {
      b.onclick = () => {
        selectedLang = b.getAttribute("data-code");
        renderLangLabel();
        langMenu.style.display = "none";
      };
    });
  };
  renderLangLabel();
  renderLangList("");

  langBtn.onclick = (ev) => {
    ev.stopPropagation();
    const open = langMenu.style.display !== "none";
    langMenu.style.display = open ? "none" : "block";
    if (!open) {
      langSearch.value = "";
      renderLangList("");
      setTimeout(() => langSearch.focus(), 10);
    }
  };
  langSearch.oninput = () => renderLangList(langSearch.value);
  overlay.addEventListener("click", (ev) => {
    if (!langMenu.contains(ev.target) && ev.target !== langBtn) {
      langMenu.style.display = "none";
    }
  });

  const updateOutputHint = () => {
    const v = outSel.value;
    if (v === "editable_text") {
      outHint.textContent = "Coming soon — choose Searchable Image to embed invisible OCR text now.";
      outHint.style.color = "#f59e0b";
      okBtn.disabled = true;
      okBtn.style.opacity = "0.5";
    } else if (v === "searchable_image_exact") {
      outHint.textContent = "Keeps original pixels untouched and adds an invisible Unicode text layer.";
      outHint.style.color = pal.muted;
      okBtn.disabled = !langResp.available;
      okBtn.style.opacity = langResp.available ? "1" : "0.5";
    } else {
      outHint.textContent = "Preserves page appearance cleanly with an invisible searchable text layer.";
      outHint.style.color = pal.muted;
      okBtn.disabled = !langResp.available;
      okBtn.style.opacity = langResp.available ? "1" : "0.5";
    }
  };
  outSel.onchange = updateOutputHint;
  updateOutputHint();

  const validateScopeAndPages = () => {
    const scope = overlay.querySelector('input[name="rt-pages"]:checked').value;
    fromInp.disabled = scope !== "range";
    toInp.disabled = scope !== "range";
    rangeErr.style.display = "none";
    if (scope === "all") {
      return { valid: true, pages: Array.from({ length: totalPages }, (_, i) => i + 1) };
    }
    if (scope === "current") {
      return { valid: true, pages: [currentPage] };
    }
    const f = parseInt(fromInp.value, 10);
    const t = parseInt(toInp.value, 10);
    if (!Number.isInteger(f) || !Number.isInteger(t) || f < 1 || t > totalPages || f > t) {
      rangeErr.textContent = `Enter a valid page range between 1 and ${totalPages}.`;
      rangeErr.style.display = "block";
      return { valid: false, pages: [] };
    }
    const pages = [];
    for (let p = f; p <= t; p++) pages.push(p);
    return { valid: true, pages };
  };

  overlay.querySelectorAll('input[name="rt-pages"]').forEach((r) => {
    r.onchange = validateScopeAndPages;
  });
  fromInp.oninput = validateScopeAndPages;
  toInp.oninput = validateScopeAndPages;

  let activeJobId = null;
  let pollTimer = null;
  let running = false;

  const cleanup = () => {
    if (pollTimer) clearInterval(pollTimer);
    window.removeEventListener("keydown", onKey);
    closeActiveOverlay();
  };

  const onKey = (ev) => {
    if (ev.key === "Escape" && !running) {
      ev.preventDefault();
      cleanup();
    }
  };
  window.addEventListener("keydown", onKey);

  overlay.querySelector("[data-rt-close]").onclick = async () => {
    if (running && activeJobId) {
      await window.pdfforgeOcr.cancel(activeJobId).catch(() => {});
    } else {
      cleanup();
    }
  };
  cancelBtn.onclick = async () => {
    if (running && activeJobId) {
      cancelBtn.disabled = true;
      cancelBtn.textContent = "Stopping...";
      await window.pdfforgeOcr.cancel(activeJobId).catch(() => {});
    } else {
      cleanup();
    }
  };

  const showProgressAndRun = async (pages, rerunTextPages) => {
    running = true;
    const bodyEl = overlay.querySelector("[data-rt-body]");
    const footerEl = overlay.querySelector("[data-rt-footer]");

    const ocrReq = {
      filePath,
      pages,
      language: selectedLang,
      secondaryLanguage: secLangSel.value || null,
      outputMode: outSel.value,
      downsampleDpi: parseInt(dpiSel.value, 10) || 0,
      rerunTextPages: !!rerunTextPages,
    };

    // Remember settings immediately
    const lastOcrSettings = {
      language: ocrReq.language,
      secondaryLanguage: ocrReq.secondaryLanguage,
      outputMode: ocrReq.outputMode,
      downsampleDpi: ocrReq.downsampleDpi,
    };
    try {
      if (ctx.updateSettingsFn) ctx.updateSettingsFn({ lastOcrSettings });
      if (window.electronAPI && window.electronAPI.updateSettings) {
        window.electronAPI.updateSettings({ lastOcrSettings }).catch(() => {});
      }
    } catch (_) {}

    bodyEl.innerHTML = `
      <div style="display:flex; flex-direction:column; gap:10px; padding: 6px 0;">
        <div style="display:flex; justify-content:space-between; align-items:center;">
          <span data-ocr-status-msg style="font-weight:500;">Starting text recognition...</span>
          <span data-ocr-pct style="font-variant-numeric:tabular-nums; color:${pal.accent}; font-weight:600;">0%</span>
        </div>
        <div style="width:100%; height:8px; border-radius:999px; background:${pal.surfaceAlt}; border:1px solid ${pal.border}; overflow:hidden;">
          <div data-ocr-bar style="width:2%; height:100%; background:${pal.accent}; transition:width 0.25s ease;"></div>
        </div>
        <div data-ocr-eta style="font-size:11.5px; color:${pal.muted};">Preparing pages...</div>
      </div>
    `;
    footerEl.innerHTML = `
      <button type="button" data-rt-abort style="padding:6px 16px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.btnSecondaryText}; font-size:12.5px; font-weight:500; cursor:pointer;">Cancel</button>
    `;
    footerEl.querySelector("[data-rt-abort]").onclick = async (ev) => {
      ev.currentTarget.disabled = true;
      ev.currentTarget.textContent = "Stopping...";
      if (activeJobId) await window.pdfforgeOcr.cancel(activeJobId).catch(() => {});
    };

    const started = await window.pdfforgeOcr.start(ocrReq).catch((e) => ({ ok: false, error: String(e) }));
    if (!started || !started.ok || !started.jobId) {
      running = false;
      bodyEl.innerHTML = `<div style="color:#f87171;">OCR failed to start: ${escHtml((started && started.error) || "Unknown error")}</div>`;
      footerEl.innerHTML = `<button type="button" data-rt-done style="padding:6px 18px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; cursor:pointer;">Close</button>`;
      footerEl.querySelector("[data-rt-done]").onclick = cleanup;
      return;
    }

    activeJobId = started.jobId;
    const msgEl = bodyEl.querySelector("[data-ocr-status-msg]");
    const pctEl = bodyEl.querySelector("[data-ocr-pct]");
    const barEl = bodyEl.querySelector("[data-ocr-bar]");
    const etaEl = bodyEl.querySelector("[data-ocr-eta]");

    const finalizeUiFromSnap = async (snap) => {
      running = false;
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }

      if (snap.status === "cancelled_pending_choice") {
        bodyEl.innerHTML = `
          <div style="line-height:1.5;">
            <div style="font-weight:600; margin-bottom:6px;">Text recognition paused/cancelled</div>
            <div style="color:${pal.muted};">${escHtml(snap.message)}</div>
          </div>
        `;
        footerEl.innerHTML = `
          <button type="button" data-rt-rollback style="padding:6px 14px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.btnSecondaryText}; font-size:12.5px; cursor:pointer;">Rollback Changes</button>
          <button type="button" data-rt-keep style="padding:6px 16px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; font-size:12.5px; font-weight:600; cursor:pointer;">Keep Completed (${snap.recognizedPages})</button>
        `;
        footerEl.querySelector("[data-rt-rollback]").onclick = async () => {
          const fin = await window.pdfforgeOcr.resolveCancel(activeJobId, false);
          if (ctx.showToastFn) ctx.showToastFn(fin.message || "OCR cancelled", "info");
          cleanup();
        };
        footerEl.querySelector("[data-rt-keep]").onclick = async () => {
          const fin = await window.pdfforgeOcr.resolveCancel(activeJobId, true);
          await finalizeUiFromSnap(fin);
        };
        return;
      }

      if (snap.status === "done") {
        clearScanCacheForFile(filePath);
        if (snap.revision && window.__acrobatPro && window.__acrobatPro.bumpRevision) {
          window.__acrobatPro.bumpRevision(filePath, snap.revision);
        }
        if (ctx.updateTabFn && ctx.tabId && snap.recognizedPages > 0) {
          ctx.updateTabFn(ctx.tabId, {
            isDirty: true,
            canUndo: !!snap.canUndo,
            canRedo: !!snap.canRedo,
          });
        }
        if (ctx.reloadFn && snap.recognizedPages > 0) {
          await ctx.reloadFn();
        }
        if (ctx.showToastFn) {
          ctx.showToastFn(snap.message || `Text recognized on ${snap.recognizedPages} page(s)`, "success");
        }

        const lowList = Array.isArray(snap.lowConfidencePages) ? snap.lowConfidencePages : [];
        bodyEl.innerHTML = `
          <div style="display:flex; flex-direction:column; gap:10px;">
            <div style="display:flex; align-items:center; gap:9px; color:#34d399; font-weight:600; font-size:13.5px;">
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"></polyline></svg>
              <span>${escHtml(snap.message || `Text recognized on ${snap.recognizedPages} page(s)`)}</span>
            </div>
            <div style="display:grid; grid-template-columns: repeat(3, 1fr); gap:8px; background:${pal.surfaceAlt}; border:1px solid ${pal.divider}; border-radius:7px; padding:10px; font-size:12px;">
              <div><div style="color:${pal.muted}; font-size:11px;">Recognized</div><strong>${snap.recognizedPages} page(s)</strong></div>
              <div><div style="color:${pal.muted}; font-size:11px;">Mean Confidence</div><strong>${Math.round(snap.meanConfidence || 0)}%</strong></div>
              <div><div style="color:${pal.muted}; font-size:11px;">Avg Speed</div><strong>${snap.avgMsPerPage || 0} ms/pg</strong></div>
            </div>
            ${
              lowList.length
                ? `<div style="padding:8px 10px; border-radius:6px; background:rgba(245,158,11,0.12); border:1px solid rgba(245,158,11,0.3); font-size:12px; color:#fbbf24;">
                     Low confidence detected on page(s): ${lowList.map((x) => `${x.page} (${Math.round(x.confidence)}%)`).join(", ")}.
                   </div>`
                : ""
            }
            <div style="font-size:11.5px; color:${pal.muted};">
              Search (Ctrl+F), text selection, copy, and Edit Text now work on recognized pages. Use Save / Save As to persist to disk, or Ctrl+Z to undo.
            </div>
          </div>
        `;
        footerEl.innerHTML = `
          <button type="button" data-rt-done style="padding:6px 20px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; font-size:12.5px; font-weight:600; cursor:pointer;">Done</button>
        `;
        footerEl.querySelector("[data-rt-done]").onclick = cleanup;
        return;
      }

      if (snap.status === "cancelled") {
        if (ctx.showToastFn) ctx.showToastFn(snap.message || "OCR cancelled", "info");
        cleanup();
        return;
      }

      // error
      bodyEl.innerHTML = `<div style="color:#f87171;">${escHtml(snap.error || snap.message || "OCR failed")}</div>`;
      footerEl.innerHTML = `<button type="button" data-rt-done style="padding:6px 18px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; cursor:pointer;">Close</button>`;
      footerEl.querySelector("[data-rt-done]").onclick = cleanup;
    };

    pollTimer = setInterval(async () => {
      try {
        const snap = await window.pdfforgeOcr.status(activeJobId);
        if (!snap) return;
        if (snap.status === "running") {
          msgEl.textContent = snap.message || `Recognizing text... page ${snap.completedPages} of ${snap.totalPages}`;
          pctEl.textContent = `${snap.percent || 0}%`;
          barEl.style.width = `${Math.max(3, snap.percent || 0)}%`;
          const eta = typeof snap.etaSeconds === "number" && snap.completedPages > 0
            ? snap.etaSeconds > 60
              ? `About ${Math.ceil(snap.etaSeconds / 60)} min remaining (${snap.avgMsPerPage || 0} ms/page)`
              : `About ${snap.etaSeconds}s remaining (${snap.avgMsPerPage || 0} ms/page)`
            : "Processing first page wave...";
          etaEl.textContent = eta;
        } else {
          await finalizeUiFromSnap(snap);
        }
      } catch (_) {}
    }, 280);
  };

  okBtn.onclick = async () => {
    const chk = validateScopeAndPages();
    if (!chk.valid) return;
    okBtn.disabled = true;
    okBtn.textContent = "Checking...";
    try {
      const rangeInfo = await window.pdfforgeOcr.inspectRange(filePath, chk.pages);
      const textPages = (rangeInfo && rangeInfo.textPages) || [];
      if (textPages.length > 0 && textPages.length < chk.pages.length) {
        // Mixed document: some selected pages already have real text, some are scanned
        const bodyEl = overlay.querySelector("[data-rt-body]");
        const footerEl = overlay.querySelector("[data-rt-footer]");
        const previewList = textPages.slice(0, 6).join(", ") + (textPages.length > 6 ? `, ... (+${textPages.length - 6})` : "");
        const prevHtml = bodyEl.innerHTML;
        bodyEl.innerHTML = `
          <div style="line-height:1.55;">
            <div style="font-weight:600; margin-bottom:6px;">Existing text found on ${textPages.length} page(s)</div>
            <p style="margin:0; color:${pal.muted};">Page(s) ${escHtml(previewList)} already contain searchable text. Would you like to skip those pages (recommended) or re-run OCR on all selected pages?</p>
          </div>
        `;
        footerEl.innerHTML = `
          <button type="button" data-rt-skip-cancel style="padding:6px 14px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.btnSecondaryText}; font-size:12px; cursor:pointer;">Cancel</button>
          <button type="button" data-rt-rerun style="padding:6px 14px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.text}; font-size:12px; cursor:pointer;">Re-run All (${chk.pages.length})</button>
          <button type="button" data-rt-skip style="padding:6px 16px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; font-size:12px; font-weight:600; cursor:pointer;">Skip Existing Text</button>
        `;
        footerEl.querySelector("[data-rt-skip-cancel]").onclick = cleanup;
        footerEl.querySelector("[data-rt-rerun]").onclick = () => showProgressAndRun(chk.pages, true);
        footerEl.querySelector("[data-rt-skip]").onclick = () => showProgressAndRun(chk.pages, false);
        void prevHtml;
        return;
      } else if (textPages.length === chk.pages.length && textPages.length > 0) {
        const bodyEl = overlay.querySelector("[data-rt-body]");
        const footerEl = overlay.querySelector("[data-rt-footer]");
        bodyEl.innerHTML = `
          <div style="line-height:1.55;">
            <div style="font-weight:600; margin-bottom:6px;">Selected page(s) already contain text</div>
            <p style="margin:0; color:${pal.muted};">All ${textPages.length} selected page(s) already have extractable text. Do you want to re-run OCR and replace any existing OCR layer?</p>
          </div>
        `;
        footerEl.innerHTML = `
          <button type="button" data-rt-skip-cancel style="padding:6px 14px; border-radius:6px; border:1px solid ${pal.border}; background:${pal.btnSecondaryBg}; color:${pal.btnSecondaryText}; font-size:12px; cursor:pointer;">Cancel</button>
          <button type="button" data-rt-rerun style="padding:6px 16px; border-radius:6px; border:none; background:${pal.accent}; color:#fff; font-size:12px; font-weight:600; cursor:pointer;">Re-run OCR</button>
        `;
        footerEl.querySelector("[data-rt-skip-cancel]").onclick = cleanup;
        footerEl.querySelector("[data-rt-rerun]").onclick = () => showProgressAndRun(chk.pages, true);
        return;
      }
    } catch (_) {}

    await showProgressAndRun(chk.pages, false);
  };
}

export default {
  registerViewerContext,
  onPageDisplayed,
  checkScannedPage,
  getCachedScanStatus,
  clearScanCacheForFile,
  openRecognizeTextDialog,
  fetchLanguages,
};
