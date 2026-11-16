// PDFForge Studio — Acrobat Pro DC Document Engine Frontend Layer
// Communicates cleanly with Rust (pdfium-render + lopdf + rustybuzz + ttf-parser + fontdb + unicode-bidi)
// via window.pdfAcrobat (/ __rust_api__/acrobat/*). 100% offline.

const layoutCache = new Map(); // key: `${filePath}::${rev}::${page}` -> Promise<PageLayout>
const objectsCache = new Map(); // key: `${filePath}::${rev}::${page}` -> Promise<PageObjects>
const docRevisions = new Map(); // filePath -> number
const pageControllers = new Map(); // `${filePath}::${page}` -> controller
let systemFontsPromise = null;

const HIGHLIGHT_STORAGE_KEY = "pdfforge_last_highlight_color";

export function getSavedHighlightColor() {
  try {
    const v = localStorage.getItem(HIGHLIGHT_STORAGE_KEY);
    if (v && /^#[0-9a-f]{6}$/i.test(v)) return v;
  } catch {}
  return "#facc15";
}

export function setSavedHighlightColor(hex) {
  try {
    if (hex && /^#[0-9a-f]{6}$/i.test(hex)) {
      localStorage.setItem(HIGHLIGHT_STORAGE_KEY, hex);
    }
  } catch {}
}

// Single authoritative selection state object shared across Viewer, Top Toolbar, Shortcuts, and Fullscreen Bar
const emptySelection = () => ({
  active: false,
  filePath: null,
  page: 1,
  startPage: 1,
  endPage: 1,
  startChar: null,
  endChar: null,
  text: "",
  rects: [],
  pages: [],
});

let selectionState = emptySelection();
window.__acrobatSelectionState = selectionState;

function emitSelectionChange() {
  window.__acrobatSelectionState = selectionState;
  try {
    window.dispatchEvent(new CustomEvent("pdfforge-selectionchange", { detail: selectionState }));
  } catch {}
  try {
    document.dispatchEvent(new Event("selectionchange"));
  } catch {}
}

export function getSelectionState() {
  return selectionState;
}

export function clearSelection(filePathFilter = null) {
  if (filePathFilter && selectionState.filePath && selectionState.filePath !== filePathFilter) return;
  if (!selectionState.active) return;
  selectionState = emptySelection();
  emitSelectionChange();
  for (const ctrl of pageControllers.values()) {
    ctrl.renderSelectionOnly();
  }
}

function getRev(filePath) {
  return docRevisions.get(filePath) || 1;
}

export function bumpRevision(filePath, rev, page) {
  const next = typeof rev === "number" ? rev : getRev(filePath) + 1;
  docRevisions.set(filePath, next);
  for (const k of Array.from(layoutCache.keys())) {
    if (k.startsWith(`${filePath}::`) && (!page || k.endsWith(`::${page}`))) {
      layoutCache.delete(k);
    }
  }
  for (const k of Array.from(objectsCache.keys())) {
    if (k.startsWith(`${filePath}::`) && (!page || k.endsWith(`::${page}`))) {
      objectsCache.delete(k);
    }
  }
}

export function getCachedLayout(filePath, page) {
  if (!window.pdfAcrobat || !filePath) return Promise.resolve(null);
  const key = `${filePath}::${getRev(filePath)}::${page}`;
  if (!layoutCache.has(key)) {
    const p = window.pdfAcrobat.getPageLayout(filePath, page).catch(() => null);
    layoutCache.set(key, p);
  }
  return layoutCache.get(key);
}

export function getCachedObjects(filePath, page) {
  if (!window.pdfAcrobat || !filePath) return Promise.resolve(null);
  const key = `${filePath}::${getRev(filePath)}::${page}`;
  if (!objectsCache.has(key)) {
    const p = window.pdfAcrobat.getObjects(filePath, page).catch(() => null);
    objectsCache.set(key, p);
  }
  return objectsCache.get(key);
}

export function getSystemFonts() {
  if (!systemFontsPromise) {
    systemFontsPromise = window.pdfAcrobat
      ? window.pdfAcrobat
          .listSystemFonts()
          .then((list) => {
            if (!Array.isArray(list)) return [];
            return list.map((it) =>
              typeof it === "string"
                ? { name: it, cssFamily: `'${it}', sans-serif`, category: "Font", isBundled: false }
                : it
            );
          })
          .catch(() => [
            { name: "Noto Sans", cssFamily: "'Noto Sans', sans-serif", category: "Sans-Serif", isBundled: true },
            { name: "Noto Serif", cssFamily: "'Noto Serif', serif", category: "Serif", isBundled: true },
            { name: "Noto Sans Bengali", cssFamily: "'Noto Sans Bengali', 'Nirmala UI', sans-serif", category: "Bengali", isBundled: true },
            { name: "Roboto", cssFamily: "'Roboto', sans-serif", category: "Sans-Serif", isBundled: true },
          ])
      : Promise.resolve([
          { name: "Noto Sans", cssFamily: "'Noto Sans', sans-serif", category: "Sans-Serif", isBundled: true },
          { name: "Noto Serif", cssFamily: "'Noto Serif', serif", category: "Serif", isBundled: true },
          { name: "Noto Sans Bengali", cssFamily: "'Noto Sans Bengali', 'Nirmala UI', sans-serif", category: "Bengali", isBundled: true },
        ]);
  }
  return systemFontsPromise;
}

function createEl(tag, styles = {}, attrs = {}) {
  const el = document.createElement(tag);
  Object.assign(el.style, styles);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "className") el.className = v;
    else if (k === "textContent") el.textContent = v;
    else el.setAttribute(k, String(v));
  }
  return el;
}

function createSvgEl(tag, attrs = {}) {
  const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs)) {
    el.setAttribute(k, String(v));
  }
  return el;
}

// =========================================================================
// Unified Desktop Context Menu Engine (Icons, Shortcuts, Submenus, Keyboard)
// =========================================================================
let activeMenuCleanup = null;

export function closeContextMenu() {
  if (activeMenuCleanup) {
    activeMenuCleanup();
    activeMenuCleanup = null;
  }
}

/**
 * Shows a viewport-clamped native-feeling dark menu supporting:
 * - { separator: true }
 * - { label, icon, shortcut, disabled, checked, danger, action, submenu: [...] }
 * Closes automatically on Escape, click outside, scroll, and window resize.
 * Supports ArrowUp/ArrowDown/ArrowRight/ArrowLeft/Enter keyboard navigation.
 */
export function showContextMenu(clientX, clientY, items) {
  closeContextMenu();
  if (!Array.isArray(items) || !items.length) return;

  const overlay = createEl(
    "div",
    {
      position: "fixed",
      inset: "0",
      zIndex: "1000050",
      pointerEvents: "none",
    },
    { "data-acrobat-contextmenu": "1" }
  );

  let activeSubEl = null;
  let subFocusIdx = -1;

  function buildPanel(panelItems, anchorX, anchorY, isSub = false) {
    const panel = createEl("div", {
      position: "fixed",
      minWidth: "228px",
      maxWidth: "300px",
      maxHeight: "min(520px, 86vh)",
      overflowY: "auto",
      backgroundColor: "#1e1e26",
      border: "1px solid #3c3c4b",
      borderRadius: "8px",
      padding: "5px 0",
      boxShadow: "0 16px 38px rgba(0,0,0,0.65)",
      color: "#e5e7eb",
      fontSize: "12px",
      fontFamily: "Inter, 'Segoe UI', sans-serif",
      pointerEvents: "auto",
      userSelect: "none",
      zIndex: isSub ? "1000052" : "1000051",
    });

    const rowButtons = [];
    let focusedIdx = -1;

    const setFocus = (nextIdx) => {
      if (!rowButtons.length) return;
      rowButtons.forEach((b) => {
        b.el.style.backgroundColor = "transparent";
        b.el.style.color = b.item.danger ? "#fca5a5" : b.item.disabled ? "#6b7280" : "#e5e7eb";
      });
      focusedIdx = nextIdx;
      const cur = rowButtons[focusedIdx];
      if (cur && !cur.item.disabled) {
        cur.el.style.backgroundColor = cur.item.danger ? "#dc2626" : "#2f6bff";
        cur.el.style.color = "#ffffff";
      }
    };

    const openSub = (rowObj) => {
      if (activeSubEl) {
        activeSubEl.remove();
        activeSubEl = null;
      }
      if (!rowObj || !rowObj.item.submenu || !rowObj.item.submenu.length) return;
      const rc = rowObj.el.getBoundingClientRect();
      activeSubEl = buildPanel(rowObj.item.submenu, rc.right - 2, rc.top - 4, true);
      overlay.appendChild(activeSubEl);
      positionPanel(activeSubEl, rc.right - 2, rc.top - 4, rc.left);
    };

    for (const it of panelItems) {
      if (it.separator) {
        panel.appendChild(
          createEl("div", {
            height: "1px",
            backgroundColor: "#333340",
            margin: "4px 8px",
          })
        );
        continue;
      }

      const btn = createEl("button", {
        width: "100%",
        padding: "5px 12px",
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        gap: "10px",
        background: "transparent",
        border: "none",
        textAlign: "left",
        cursor: it.disabled ? "not-allowed" : "pointer",
        color: it.danger ? "#fca5a5" : it.disabled ? "#6b7280" : "#e5e7eb",
        fontSize: "12px",
        opacity: it.disabled ? "0.48" : "1",
      });
      btn.type = "button";
      btn.disabled = !!it.disabled;

      const left = createEl("span", {
        display: "flex",
        alignItems: "center",
        gap: "8px",
        minWidth: "0",
        flex: "1",
      });
      const iconBox = createEl("span", {
        width: "15px",
        textAlign: "center",
        flexShrink: "0",
        fontSize: "12px",
        opacity: "0.9",
      });
      iconBox.textContent = it.checked ? "✓" : it.icon || "";
      const lbl = createEl("span", {
        overflow: "hidden",
        textOverflow: "ellipsis",
        whiteSpace: "nowrap",
      });
      lbl.textContent = it.label || "";
      left.appendChild(iconBox);
      left.appendChild(lbl);

      const right = createEl("span", {
        fontSize: "10.5px",
        opacity: "0.65",
        flexShrink: "0",
      });
      right.textContent = it.submenu ? "▶" : it.shortcut || "";

      btn.appendChild(left);
      btn.appendChild(right);

      const entry = { el: btn, item: it, idx: rowButtons.length };
      rowButtons.push(entry);

      btn.addEventListener("pointerenter", () => {
        if (!it.disabled) setFocus(entry.idx);
        if (it.submenu && !it.disabled) openSub(entry);
        else if (!isSub && activeSubEl) {
          activeSubEl.remove();
          activeSubEl = null;
        }
      });

      btn.addEventListener("mousedown", (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
      });

      btn.addEventListener("click", (ev) => {
        ev.stopPropagation();
        if (it.disabled) return;
        if (it.submenu) {
          openSub(entry);
          return;
        }
        closeContextMenu();
        it.action && it.action();
      });

      panel.appendChild(btn);
    }

    panel._rowButtons = rowButtons;
    panel._getFocus = () => focusedIdx;
    panel._setFocus = setFocus;
    panel._openSub = openSub;
    return panel;
  }

  function positionPanel(panel, x, y, fallbackRightX = null) {
    const vw = window.innerWidth || 1280;
    const vh = window.innerHeight || 800;
    const pw = panel.offsetWidth || 235;
    const ph = panel.offsetHeight || 280;
    let left = x;
    let top = y;
    if (left + pw > vw - 8) {
      left = fallbackRightX !== null ? Math.max(8, fallbackRightX - pw + 2) : Math.max(8, vw - pw - 8);
    }
    if (top + ph > vh - 8) {
      top = Math.max(8, vh - ph - 8);
    }
    panel.style.left = `${Math.max(8, left)}px`;
    panel.style.top = `${Math.max(8, top)}px`;
  }

  const mainPanel = buildPanel(items, clientX, clientY, false);
  overlay.appendChild(mainPanel);
  document.body.appendChild(overlay);
  positionPanel(mainPanel, clientX, clientY);

  const onDownOutside = (ev) => {
    if (!overlay.contains(ev.target)) closeContextMenu();
  };
  const onDismiss = () => closeContextMenu();
  const onKey = (ev) => {
    const targetPanel = activeSubEl || mainPanel;
    const rows = targetPanel._rowButtons || [];
    const enabled = rows.filter((r) => !r.item.disabled);
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      if (activeSubEl) {
        activeSubEl.remove();
        activeSubEl = null;
      } else {
        closeContextMenu();
      }
      return;
    }
    if (ev.key === "ArrowDown" && enabled.length) {
      ev.preventDefault();
      const cur = targetPanel._getFocus();
      const pos = enabled.findIndex((r) => r.idx === cur);
      const next = enabled[(pos + 1) % enabled.length];
      targetPanel._setFocus(next.idx);
    } else if (ev.key === "ArrowUp" && enabled.length) {
      ev.preventDefault();
      const cur = targetPanel._getFocus();
      const pos = enabled.findIndex((r) => r.idx === cur);
      const prev = enabled[(pos - 1 + enabled.length) % enabled.length];
      targetPanel._setFocus(prev.idx);
    } else if (ev.key === "ArrowRight") {
      const cur = mainPanel._getFocus();
      const row = rows[cur];
      if (row && row.item.submenu && !activeSubEl) {
        ev.preventDefault();
        mainPanel._openSub(row);
      }
    } else if (ev.key === "ArrowLeft" && activeSubEl) {
      ev.preventDefault();
      activeSubEl.remove();
      activeSubEl = null;
    } else if (ev.key === "Enter") {
      const cur = targetPanel._getFocus();
      const row = rows[cur];
      if (row && !row.item.disabled) {
        ev.preventDefault();
        if (row.item.submenu) {
          targetPanel._openSub(row);
        } else {
          closeContextMenu();
          row.item.action && row.item.action();
        }
      }
    }
  };

  window.addEventListener("mousedown", onDownOutside, true);
  window.addEventListener("scroll", onDismiss, true);
  window.addEventListener("resize", onDismiss);
  window.addEventListener("blur", onDismiss);
  window.addEventListener("keydown", onKey, true);

  activeMenuCleanup = () => {
    window.removeEventListener("mousedown", onDownOutside, true);
    window.removeEventListener("scroll", onDismiss, true);
    window.removeEventListener("resize", onDismiss);
    window.removeEventListener("blur", onDismiss);
    window.removeEventListener("keydown", onKey, true);
    overlay.remove();
  };
}

window.__acrobatShowContextMenu = showContextMenu;
window.__acrobatCloseContextMenu = closeContextMenu;

// =========================================================================
// Searchable Live-Preview Font Picker Popup (Offline Bundled + Windows Fonts)
// =========================================================================
function createSearchableFontPicker(currentFont, fontsList, onSelect) {
  const wrap = createEl("div", { position: "relative", display: "inline-block" });

  const trigger = createEl("button", {
    background: "#13131a",
    color: "#ffffff",
    border: "1px solid #3b3b49",
    borderRadius: "4px",
    padding: "3px 8px",
    width: "145px",
    textAlign: "left",
    fontSize: "11px",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: "4px",
    cursor: "pointer",
  });
  trigger.type = "button";
  const lbl = createEl("span", {
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
    fontFamily: currentFont || "Noto Sans",
  });
  lbl.textContent = currentFont || "Noto Sans";
  const caret = createEl("span", { opacity: "0.7", fontSize: "9px" });
  caret.textContent = "▼";
  trigger.appendChild(lbl);
  trigger.appendChild(caret);

  let drop = null;
  const closeDrop = () => {
    if (drop) {
      drop.remove();
      drop = null;
    }
    window.removeEventListener("mousedown", onOut, true);
  };
  const onOut = (ev) => {
    if (drop && !drop.contains(ev.target) && !trigger.contains(ev.target)) {
      closeDrop();
    }
  };

  trigger.addEventListener("click", (ev) => {
    ev.stopPropagation();
    if (drop) {
      closeDrop();
      return;
    }
    drop = createEl("div", {
      position: "fixed",
      width: "268px",
      maxHeight: "310px",
      backgroundColor: "#1b1b24",
      border: "1px solid #3f3f52",
      borderRadius: "8px",
      boxShadow: "0 16px 38px rgba(0,0,0,0.65)",
      zIndex: "1000040",
      display: "flex",
      flexDirection: "column",
      overflow: "hidden",
    });

    const rc = trigger.getBoundingClientRect();
    const topPos = rc.bottom + 314 > window.innerHeight ? Math.max(8, rc.top - 314) : rc.bottom + 4;
    drop.style.left = `${Math.max(8, Math.min(window.innerWidth - 276, rc.left))}px`;
    drop.style.top = `${topPos}px`;

    const searchBox = createEl("input", {
      margin: "6px",
      padding: "5px 8px",
      background: "#121218",
      color: "#fff",
      border: "1px solid #3b3b49",
      borderRadius: "5px",
      fontSize: "11.5px",
      outline: "none",
    });
    searchBox.type = "text";
    searchBox.placeholder = "Search bundled & system fonts...";

    const listWrap = createEl("div", {
      flex: "1",
      overflowY: "auto",
      padding: "0 4px 6px 4px",
    });

    const renderList = (queryStr) => {
      listWrap.innerHTML = "";
      const q = (queryStr || "").trim().toLowerCase();
      const filtered = fontsList.filter((f) => !q || f.name.toLowerCase().includes(q) || f.category.toLowerCase().includes(q));
      let lastGroup = "";
      for (const f of filtered.slice(0, 120)) {
        const grp = f.isBundled ? "Bundled Offline Fonts" : f.category || "System Fonts";
        if (grp !== lastGroup) {
          lastGroup = grp;
          const hdr = createEl("div", {
            fontSize: "9.5px",
            fontWeight: "700",
            textTransform: "uppercase",
            letterSpacing: "0.05em",
            color: "#9ca3af",
            padding: "5px 8px 2px 8px",
          });
          hdr.textContent = grp;
          listWrap.appendChild(hdr);
        }
        const row = createEl("button", {
          width: "100%",
          padding: "5px 8px",
          borderRadius: "5px",
          border: "none",
          background: f.name === lbl.textContent ? "rgba(47,107,255,0.28)" : "transparent",
          color: "#f3f4f6",
          textAlign: "left",
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: "8px",
        });
        row.type = "button";
        const nm = createEl("span", {
          fontFamily: f.cssFamily || `'${f.name}', sans-serif`,
          fontSize: "12.5px",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        });
        nm.textContent = f.name;
        const preview = createEl("span", {
          fontFamily: f.cssFamily || `'${f.name}', sans-serif`,
          fontSize: "11px",
          opacity: "0.68",
          flexShrink: "0",
        });
        preview.textContent = f.name.includes("Bengali") ? "অ আ ক" : "Aa Bb";
        row.appendChild(nm);
        row.appendChild(preview);
        row.addEventListener("pointerenter", () => {
          row.style.background = "#2f6bff";
        });
        row.addEventListener("pointerleave", () => {
          row.style.background = f.name === lbl.textContent ? "rgba(47,107,255,0.28)" : "transparent";
        });
        row.addEventListener("click", (e2) => {
          e2.stopPropagation();
          lbl.textContent = f.name;
          lbl.style.fontFamily = f.cssFamily || `'${f.name}', sans-serif`;
          closeDrop();
          onSelect(f);
        });
        listWrap.appendChild(row);
      }
      if (!filtered.length) {
        const empty = createEl("div", { padding: "10px", color: "#9ca3af", fontSize: "11px", textAlign: "center" });
        empty.textContent = "No matching font found";
        listWrap.appendChild(empty);
      }
    };

    searchBox.addEventListener("input", () => renderList(searchBox.value));
    drop.appendChild(searchBox);
    drop.appendChild(listWrap);
    document.body.appendChild(drop);
    renderList("");
    searchBox.focus();
    window.addEventListener("mousedown", onOut, true);
  });

  wrap.appendChild(trigger);
  wrap._setDisplay = (name, cssFam) => {
    lbl.textContent = name;
    if (cssFam) lbl.style.fontFamily = cssFam;
  };
  return wrap;
}

window.__acrobatBuildFontPicker = createSearchableFontPicker;
window.__acrobatGetSystemFonts = getSystemFonts;

function localFindChar(layout, x, y) {
  if (!layout || !Array.isArray(layout.chars) || layout.chars.length === 0) return null;
  for (let i = 0; i < layout.chars.length; i++) {
    const c = layout.chars[i];
    const [bx, by, bw, bh] = c.bbox;
    if (x >= bx - 2 && x <= bx + bw + 2 && y >= by - 2.5 && y <= by + bh + 2.5) {
      return c.index;
    }
  }
  let best = null;
  let bestDist = Infinity;
  for (let i = 0; i < layout.chars.length; i++) {
    const c = layout.chars[i];
    const cx = c.bbox[0] + c.bbox[2] * 0.5;
    const cy = c.bbox[1] + c.bbox[3] * 0.5;
    const dy = Math.abs(cy - y);
    const dx = Math.abs(cx - x);
    const d = dy * 2.4 + dx;
    if (d < bestDist) {
      bestDist = d;
      best = c.index;
    }
  }
  return best;
}

function localSelectionRects(layout, startChar, endChar) {
  if (!layout || !layout.chars || !layout.chars.length) return { text: "", rects: [] };
  const lo = Math.max(0, Math.min(startChar, endChar));
  const hi = Math.min(layout.chars.length - 1, Math.max(startChar, endChar));
  const rects = [];
  let text = "";
  for (const blk of layout.blocks || []) {
    if (blk.endChar < lo || blk.startChar > hi) continue;
    for (const ln of blk.lines || []) {
      if (ln.endChar < lo || ln.startChar > hi) continue;
      if (text) text += "\n";
      const l0 = Math.max(lo, ln.startChar);
      const l1 = Math.min(hi, ln.endChar);
      let cur = null;
      for (let i = l0; i <= l1; i++) {
        const ch = layout.chars[i];
        if (!ch) continue;
        text += ch.unicode;
        const [cx, cy, cw, chh] = ch.bbox;
        if (cw <= 0.1 || chh <= 0.1) continue;
        if (!cur) {
          cur = [cx, cy, cw, chh];
        } else {
          const rRight = cur[0] + cur[2];
          const sameY = Math.abs(cur[1] - cy) < Math.max(cur[3], chh) * 0.65;
          const closeX = Math.abs(cx - rRight) < Math.max(ch.fontSize, 8) * 1.4 || cx <= rRight;
          if (sameY && closeX) {
            const nx = Math.min(cur[0], cx);
            const ny = Math.min(cur[1], cy);
            const nx2 = Math.max(rRight, cx + cw);
            const ny2 = Math.max(cur[1] + cur[3], cy + chh);
            cur = [nx, ny, nx2 - nx, ny2 - ny];
          } else {
            rects.push(cur);
            cur = [cx, cy, cw, chh];
          }
        }
      }
      if (cur) rects.push(cur);
    }
  }
  return { text: text.trimEnd(), rects };
}

async function computeRangeState(filePath, aPage, aChar, fPage, fChar, exactRust = false) {
  let loPage = aPage, loChar = aChar, hiPage = fPage, hiChar = fChar;
  if (aPage > fPage || (aPage === fPage && aChar > fChar)) {
    loPage = fPage; loChar = fChar; hiPage = aPage; hiChar = aChar;
  }

  const pagesOut = [];
  const textParts = [];
  for (let pg = loPage; pg <= hiPage; pg++) {
    const ctrl = pageControllers.get(`${filePath}::${pg}`);
    const layout = ctrl ? ctrl.getLayout() : await getCachedLayout(filePath, pg);
    if (!layout || !layout.chars || !layout.chars.length) continue;
    const sIdx = pg === loPage ? loChar : 0;
    const eIdx = pg === hiPage ? hiChar : layout.chars.length - 1;
    let res;
    if (exactRust && window.pdfAcrobat) {
      res = await window.pdfAcrobat.getSelectionRects(filePath, pg, sIdx, eIdx).catch(() => null);
    }
    if (!res) res = localSelectionRects(layout, sIdx, eIdx);
    if (res && (res.rects.length || res.text)) {
      pagesOut.push({
        page: pg,
        startChar: Math.min(sIdx, eIdx),
        endChar: Math.max(sIdx, eIdx),
        text: res.text || "",
        rects: res.rects || [],
      });
      if (res.text) textParts.push(res.text);
    }
  }

  if (!pagesOut.length) return emptySelection();
  return {
    active: true,
    filePath,
    page: loPage,
    startPage: loPage,
    endPage: hiPage,
    startChar: loChar,
    endChar: hiChar,
    text: textParts.join("\n"),
    rects: pagesOut[0].rects,
    pages: pagesOut,
  };
}

function updateAllSelectionLayersForFile(filePath) {
  for (const [k, ctrl] of pageControllers.entries()) {
    if (!filePath || k.startsWith(`${filePath}::`)) {
      ctrl.renderSelectionOnly();
    }
  }
}

export async function applySelectionAction(actionType, colorOverride) {
  const sel = selectionState;
  if (actionType === "editText") {
    if (sel.active && sel.filePath) {
      const ctrl = pageControllers.get(`${sel.filePath}::${sel.page}`);
      if (ctrl && ctrl.openEditBlockForChar(sel.startChar)) {
        return true;
      }
    }
    return false;
  }

  if (!sel.active || !sel.filePath || !sel.pages.length) return false;

  if (actionType === "copy") {
    if (navigator.clipboard && sel.text) {
      navigator.clipboard.writeText(sel.text).catch(() => {});
      const ctrl = pageControllers.get(`${sel.filePath}::${sel.page}`);
      ctrl && ctrl.toast && ctrl.toast("Copied selected text", "success");
    }
    return true;
  }

  if (actionType === "copyQuote") {
    if (navigator.clipboard && sel.text) {
      navigator.clipboard.writeText(`"${sel.text}"`).catch(() => {});
      const ctrl = pageControllers.get(`${sel.filePath}::${sel.page}`);
      ctrl && ctrl.toast && ctrl.toast("Copied selection as quote", "success");
    }
    return true;
  }

  if (actionType === "highlight" || actionType === "underline" || actionType === "strikethrough") {
    let color = colorOverride;
    if (actionType === "highlight") {
      color = color || getSavedHighlightColor();
      setSavedHighlightColor(color);
    } else if (actionType === "underline") {
      color = color || "#10b981";
    } else {
      color = color || "#ef4444";
    }

    const items = sel.pages
      .filter((pg) => pg.rects && pg.rects.length > 0)
      .map((pg) => ({
        page: pg.page,
        rects: pg.rects.map(([x, y, width, height]) => ({ x, y, width, height })),
        text: pg.text || "",
      }));

    const firstCtrl = pageControllers.get(`${sel.filePath}::${sel.pages[0].page}`);
    if (firstCtrl && items.length) {
      firstCtrl.applyMarkupBatch(actionType, items, color);
      return true;
    }
  }
  return false;
}

window.__acrobatApplySelectionAction = applySelectionAction;

// =========================================================================
// Page Surface Controller: Selection + Edit Text Boxes + Editable SVG Shapes
// =========================================================================
export function mountAcrobatPageSurface(opts) {
  const {
    container,
    filePath,
    page,
    getZoom,
    getRotation,
    getTool,
    getActiveColor,
    getStrokeWidth,
    getFontFamily,
    getFontSize,
    getSearchQuery,
    onCreateMarkupBatch,
    onCreateMarkup,
    onRefreshPdf,
    onToast,
  } = opts;

  if (!container || !filePath || !window.pdfAcrobat) return null;

  const ctrlKey = `${filePath}::${page}`;
  if (pageControllers.has(ctrlKey)) {
    try {
      pageControllers.get(ctrlKey).destroy();
    } catch {}
  }

  let destroyed = false;
  let layoutData = null;
  let objectsData = null;
  let selectedShape = null;
  let activeTextEditor = null;

  const root = createEl(
    "div",
    {
      position: "absolute",
      inset: "0",
      pointerEvents: "none",
      zIndex: "17",
      overflow: "visible",
    },
    { "data-acrobat-surface": String(page) }
  );

  const svgLayer = createSvgEl("svg", {
    width: "100%",
    height: "100%",
    style: "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;z-index:17;overflow:visible;",
  });

  const selLayer = createEl("div", {
    position: "absolute",
    inset: "0",
    pointerEvents: "none",
    zIndex: "18",
  });

  const shapeHudLayer = createEl("div", {
    position: "absolute",
    inset: "0",
    pointerEvents: "none",
    zIndex: "36",
  });

  const editTextLayer = createEl("div", {
    position: "absolute",
    inset: "0",
    pointerEvents: "none",
    zIndex: "37",
  });

  const hitLayer = createEl("div", {
    position: "absolute",
    inset: "0",
    pointerEvents: "none",
    zIndex: "19",
  });

  root.appendChild(svgLayer);
  root.appendChild(selLayer);
  root.appendChild(hitLayer);
  root.appendChild(shapeHudLayer);
  root.appendChild(editTextLayer);
  container.appendChild(root);

  function curScale() {
    return Math.max(0.1, (getZoom() || 100) / 100);
  }

  function clientToPdf(clientX, clientY) {
    const r = container.getBoundingClientRect();
    const sc = curScale();
    return [(clientX - r.left) / sc, (clientY - r.top) / sc];
  }

  function syncPointerModes() {
    const tool = getTool();
    const isSel = tool === "select";
    const isEditMode = tool === "editText" || tool === "pageEdit" || tool === "editShape" || tool === "shape_vec";
    const isDrawShape = ["vec_rect", "vec_ellipse", "vec_line", "vec_arrow", "vec_freehand"].includes(tool);

    if (isSel || isEditMode || isDrawShape) {
      hitLayer.style.pointerEvents = "auto";
      hitLayer.style.cursor = isSel
        ? "text"
        : isDrawShape
        ? "crosshair"
        : tool === "editText" || tool === "pageEdit"
        ? "text"
        : "default";
    } else {
      hitLayer.style.pointerEvents = "none";
    }
  }

  function renderSelectionOnly() {
    selLayer.innerHTML = "";
    const sc = curScale();

    const qInfo = getSearchQuery ? getSearchQuery() : { query: "", caseSensitive: false };
    if (qInfo && qInfo.query && qInfo.query.trim() && layoutData && layoutData.chars && layoutData.chars.length) {
      const needle = qInfo.caseSensitive ? qInfo.query.trim() : qInfo.query.trim().toLowerCase();
      const hay = layoutData.chars
        .map((c) => (qInfo.caseSensitive ? c.unicode : c.unicode.toLowerCase()))
        .join("");
      let pos = 0;
      while ((pos = hay.indexOf(needle, pos)) !== -1) {
        const res = localSelectionRects(layoutData, pos, pos + needle.length - 1);
        for (const [rx, ry, rw, rh] of res.rects) {
          selLayer.appendChild(
            createEl("div", {
              position: "absolute",
              left: `${rx * sc}px`,
              top: `${ry * sc}px`,
              width: `${Math.max(2, rw * sc)}px`,
              height: `${Math.max(4, rh * sc)}px`,
              backgroundColor: "rgba(250, 204, 21, 0.42)",
              border: "1px solid rgba(217, 119, 6, 0.7)",
              borderRadius: "2px",
              pointerEvents: "none",
            })
          );
        }
        pos += Math.max(1, needle.length);
      }
    }

    if (!selectionState.active || selectionState.filePath !== filePath) return;
    const pgEntry = (selectionState.pages || []).find((p) => p.page === page);
    if (!pgEntry || !pgEntry.rects || !pgEntry.rects.length) return;

    for (const [rx, ry, rw, rh] of pgEntry.rects) {
      selLayer.appendChild(
        createEl("div", {
          position: "absolute",
          left: `${rx * sc}px`,
          top: `${ry * sc}px`,
          width: `${Math.max(2, rw * sc)}px`,
          height: `${Math.max(4, rh * sc)}px`,
          backgroundColor: "rgba(47, 107, 255, 0.30)",
          borderRadius: "2px",
          pointerEvents: "none",
          mixBlendMode: "multiply",
        })
      );
    }
  }

  function renderShapeOverlays() {
    svgLayer.innerHTML = "";
    shapeHudLayer.innerHTML = "";
    if (!objectsData || !objectsData.pathObjects) return;

    const pt = objectsData.pageTransform;
    if (pt) {
      svgLayer.setAttribute("viewBox", `0 0 ${pt.vpWidth} ${pt.vpHeight}`);
    }

    const tool = getTool();
    const showEditOutlines = tool === "editShape" || tool === "pageEdit" || selectedShape !== null;

    if (showEditOutlines) {
      for (const shp of objectsData.pathObjects) {
        if (shp.isPageBackground) continue;
        const isSel = selectedShape && selectedShape.id === shp.id;
        const pathEl = createSvgEl("path", {
          d: shp.svgPath,
          fill: isSel ? "rgba(47, 107, 255, 0.14)" : "transparent",
          stroke: isSel ? "#2f6bff" : "rgba(59, 130, 246, 0.45)",
          "stroke-width": isSel ? "2" : "1",
          "stroke-dasharray": isSel ? "none" : "4 2",
          style: "pointer-events:auto;cursor:move;",
        });
        pathEl.addEventListener("pointerdown", (ev) => {
          ev.stopPropagation();
          selectShapeObject(shp);
        });
        svgLayer.appendChild(pathEl);
      }
    }

    if (selectedShape) {
      renderSelectedShapeHud();
    }
  }

  function renderSelectedShapeHud() {
    shapeHudLayer.innerHTML = "";
    if (!selectedShape) return;
    const sc = curScale();
    const [bx, by, bw, bh] = selectedShape.bboxVp;
    const rot = selectedShape.rotationDeg || 0;

    const frame = createEl("div", {
      position: "absolute",
      left: `${bx * sc}px`,
      top: `${by * sc}px`,
      width: `${Math.max(12, bw * sc)}px`,
      height: `${Math.max(12, bh * sc)}px`,
      border: "1.5px solid #2f6bff",
      backgroundColor: "rgba(47, 107, 255, 0.08)",
      transform: `rotate(${rot}deg)`,
      transformOrigin: "center center",
      pointerEvents: "auto",
      cursor: "move",
      boxSizing: "border-box",
    });

    frame.addEventListener("pointerdown", (ev) => {
      if (ev.target !== frame) return;
      ev.stopPropagation();
      ev.preventDefault();
      const startX = ev.clientX;
      const startY = ev.clientY;
      const origBbox = [...selectedShape.bboxVp];
      const onMv = (e2) => {
        selectedShape.bboxVp = [
          origBbox[0] + (e2.clientX - startX) / curScale(),
          origBbox[1] + (e2.clientY - startY) / curScale(),
          origBbox[2],
          origBbox[3],
        ];
        renderSelectedShapeHud();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });

    const seHandle = createEl("div", {
      position: "absolute",
      right: "-5px",
      bottom: "-5px",
      width: "10px",
      height: "10px",
      backgroundColor: "#2f6bff",
      border: "1.5px solid #fff",
      borderRadius: "2px",
      cursor: "se-resize",
    });
    seHandle.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const sx = ev.clientX;
      const sy = ev.clientY;
      const orig = [...selectedShape.bboxVp];
      const onMv = (e2) => {
        selectedShape.bboxVp = [
          orig[0],
          orig[1],
          Math.max(8, orig[2] + (e2.clientX - sx) / curScale()),
          Math.max(8, orig[3] + (e2.clientY - sy) / curScale()),
        ];
        renderSelectedShapeHud();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });
    frame.appendChild(seHandle);

    const rotHandle = createEl(
      "div",
      {
        position: "absolute",
        top: "-22px",
        left: "50%",
        transform: "translateX(-50%)",
        width: "14px",
        height: "14px",
        borderRadius: "9999px",
        backgroundColor: "#10b981",
        border: "2px solid #fff",
        cursor: "grab",
      },
      { title: "Drag to Rotate Shape" }
    );
    rotHandle.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const rc = frame.getBoundingClientRect();
      const cx = rc.left + rc.width / 2;
      const cy = rc.top + rc.height / 2;
      const onMv = (e2) => {
        const deg = Math.round((Math.atan2(e2.clientY - cy, e2.clientX - cx) * 180) / Math.PI + 90);
        selectedShape.rotationDeg = deg;
        renderSelectedShapeHud();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });
    frame.appendChild(rotHandle);

    shapeHudLayer.appendChild(frame);

    const bar = createEl(
      "div",
      {
        position: "absolute",
        left: `${Math.max(8, Math.min(container.clientWidth - 320, bx * sc))}px`,
        top: `${Math.max(8, (by + bh) * sc + 10)}px`,
        backgroundColor: "#1e1e26",
        border: "1px solid #3f3f50",
        borderRadius: "8px",
        padding: "5px 8px",
        display: "flex",
        alignItems: "center",
        gap: "6px",
        fontSize: "11px",
        color: "#e5e7eb",
        pointerEvents: "auto",
        boxShadow: "0 10px 24px rgba(0,0,0,0.45)",
      },
      { "data-acrobat-ui": "1" }
    );

    const fillInp = createEl(
      "input",
      { width: "22px", height: "22px", border: "none", background: "transparent", cursor: "pointer" },
      { type: "color", value: selectedShape.fillColor || "#3b82f6", title: "Fill Color" }
    );
    fillInp.addEventListener("change", (ev) => {
      selectedShape.fillColor = ev.target.value;
    });

    const strokeInp = createEl(
      "input",
      { width: "22px", height: "22px", border: "none", background: "transparent", cursor: "pointer" },
      { type: "color", value: selectedShape.strokeColor || "#111827", title: "Stroke Color" }
    );
    strokeInp.addEventListener("change", (ev) => {
      selectedShape.strokeColor = ev.target.value;
    });

    const lwInp = createEl(
      "input",
      {
        width: "42px",
        background: "#13131a",
        border: "1px solid #3c3c4a",
        borderRadius: "4px",
        color: "#fff",
        padding: "2px 4px",
        fontSize: "11px",
      },
      { type: "number", min: "0.5", max: "24", step: "0.5", value: String(selectedShape.strokeWidth || 2), title: "Stroke Width" }
    );
    lwInp.addEventListener("change", (ev) => {
      selectedShape.strokeWidth = Math.max(0.5, Number(ev.target.value) || 1);
    });

    const mkAction = (label, bg, onClick) => {
      const b = createEl("button", {
        background: bg,
        color: "#fff",
        border: "none",
        borderRadius: "5px",
        padding: "3px 8px",
        cursor: "pointer",
        fontWeight: "600",
        fontSize: "11px",
      });
      b.textContent = label;
      b.addEventListener("click", async (ev) => {
        ev.stopPropagation();
        await onClick();
      });
      return b;
    };

    bar.appendChild(document.createTextNode(`${selectedShape.shapeKind} `));
    bar.appendChild(fillInp);
    bar.appendChild(strokeInp);
    bar.appendChild(lwInp);
    bar.appendChild(mkAction("Apply", "#2f6bff", () => commitSelectedShape("update")));
    bar.appendChild(mkAction("Delete", "#dc2626", () => commitSelectedShape("delete")));
    bar.appendChild(
      mkAction("✕", "#383846", async () => {
        selectedShape = null;
        renderShapeOverlays();
      })
    );
    shapeHudLayer.appendChild(bar);
  }

  function selectShapeObject(shp) {
    selectedShape = {
      ...shp,
      origBboxVp: [...shp.bboxVp],
      origSegmentsVp: shp.segmentsVp,
      rotationDeg: 0,
    };
    renderShapeOverlays();
  }

  async function commitSelectedShape(action) {
    if (!selectedShape || !window.pdfAcrobat) return;
    const payload = {
      filePath,
      page,
      action,
      shapeKind: selectedShape.shapeKind,
      opStart: selectedShape.opStart,
      opEnd: selectedShape.opEnd,
      bboxVp: selectedShape.bboxVp,
      origBboxVp: selectedShape.origBboxVp,
      origSegmentsVp: selectedShape.origSegmentsVp,
      pointsVp: null,
      rotationDeg: selectedShape.rotationDeg || 0,
      fillColor: selectedShape.fillColor,
      strokeColor: selectedShape.strokeColor,
      fillOpacity: selectedShape.fillOpacity ?? 1,
      strokeOpacity: selectedShape.strokeOpacity ?? 1,
      strokeWidth: selectedShape.strokeWidth ?? 2,
    };
    const res = await window.pdfAcrobat.commitShapeEdit(payload);
    if (res && res.ok) {
      bumpRevision(filePath, res.revision, page);
      selectedShape = null;
      await refreshAll();
      onRefreshPdf && onRefreshPdf();
      onToast && onToast(action === "delete" ? "Removed PDF vector shape" : "Updated PDF vector shape", "success");
    } else {
      onToast && onToast((res && res.error) || "Shape update failed", "error");
    }
  }

  async function openEditTextBox(block) {
    if (activeTextEditor) activeTextEditor();
    editTextLayer.innerHTML = "";
    const sc = curScale();

    const state = {
      text: block.text,
      origBboxVp: [...block.bbox],
      newBboxVp: [...block.bbox],
      originalFont: block.fontName || "Noto Sans",
      fontFamily: block.fontName || (getFontFamily && getFontFamily()) || "Noto Sans",
      fontSize: Math.round((block.fontSize || 12) * 10) / 10,
      color: block.color || "#111827",
      bold: !!block.bold,
      italic: !!block.italic,
      align: block.alignment || "left",
      lineSpacing: block.lineSpacing || 1.2,
      bullets: !!block.hasBullets,
      notice: !block.hasEmbeddedFont
        ? `Font "${block.fontName}" is not embedded; offline substitute will be embedded on save.`
        : block.isSubsetFont
        ? `Font "${block.fontName}" is an embedded subset; missing characters use closest offline match.`
        : null,
    };

    const sysFonts = await getSystemFonts();

    const wrap = createEl(
      "div",
      {
        position: "absolute",
        left: `${state.newBboxVp[0] * sc}px`,
        top: `${state.newBboxVp[1] * sc}px`,
        width: `${Math.max(120, state.newBboxVp[2] * sc + 16)}px`,
        minHeight: `${Math.max(28, state.newBboxVp[3] * sc + 8)}px`,
        backgroundColor: "rgba(255,255,255,0.97)",
        border: "1.5px solid #2f6bff",
        boxShadow: "0 8px 28px rgba(0,0,0,0.35)",
        borderRadius: "3px",
        pointerEvents: "auto",
        boxSizing: "border-box",
      },
      { "data-acrobat-ui": "1", "data-acrobat-text-editor": "1" }
    );

    const ta = createEl("textarea", {
      width: "100%",
      height: `${Math.max(26, state.newBboxVp[3] * sc + 4)}px`,
      border: "none",
      outline: "none",
      resize: "both",
      background: "transparent",
      padding: "2px 4px",
      margin: "0",
      overflow: "hidden",
      color: state.color,
      fontFamily: `'${state.fontFamily}', 'Noto Sans', 'Noto Sans Bengali', sans-serif`,
      fontSize: `${state.fontSize * sc}px`,
      fontWeight: state.bold ? "700" : "400",
      fontStyle: state.italic ? "italic" : "normal",
      lineHeight: String(state.lineSpacing),
      textAlign: state.align,
      direction: block.isRtl ? "rtl" : "ltr",
    });
    ta.value = state.text;

    const noticeBanner = createEl("div", {
      position: "absolute",
      left: "0",
      bottom: "100%",
      marginBottom: "4px",
      backgroundColor: "#fef3c7",
      color: "#92400e",
      border: "1px solid #f59e0b",
      borderRadius: "4px",
      padding: "2px 7px",
      fontSize: "10.5px",
      whiteSpace: "nowrap",
      maxWidth: "460px",
      overflow: "hidden",
      textOverflow: "ellipsis",
      display: state.notice ? "block" : "none",
    });
    if (state.notice) noticeBanner.textContent = state.notice;

    const toolbar = createEl("div", {
      position: "absolute",
      left: "0",
      top: "calc(100% + 6px)",
      backgroundColor: "#1f1f27",
      border: "1px solid #3f3f50",
      borderRadius: "8px",
      padding: "6px 8px",
      display: "flex",
      flexWrap: "wrap",
      alignItems: "center",
      gap: "5px",
      width: "420px",
      color: "#f3f4f6",
      fontSize: "11px",
      boxShadow: "0 12px 30px rgba(0,0,0,0.5)",
      zIndex: "50",
    });

    const reflowPreview = async () => {
      state.text = ta.value;
      const bw = wrap.offsetWidth / curScale();
      state.newBboxVp[2] = Math.max(40, bw);
      try {
        const res = await window.pdfAcrobat.shapeReflowBlock({
          filePath,
          page,
          text: state.text,
          originalFont: state.originalFont,
          fontFamily: state.fontFamily,
          fontSize: state.fontSize,
          maxWidth: state.newBboxVp[2],
          bold: state.bold,
          italic: state.italic,
          align: state.align,
          lineSpacing: state.lineSpacing,
          bullets: state.bullets,
        });
        if (res) {
          state.newBboxVp[3] = Math.max(state.fontSize * 1.3, res.totalHeight);
          ta.style.height = `${Math.max(24, state.newBboxVp[3] * curScale() + 6)}px`;
          if (res.notice) {
            noticeBanner.textContent = res.notice;
            noticeBanner.style.display = "block";
          } else {
            noticeBanner.style.display = "none";
          }
        }
      } catch {}
    };

    const fontPicker = createSearchableFontPicker(state.fontFamily, sysFonts, (picked) => {
      state.fontFamily = picked.name;
      ta.style.fontFamily = picked.cssFamily || `'${picked.name}', sans-serif`;
      reflowPreview();
    });

    const sizeInp = createEl(
      "input",
      { width: "46px", background: "#13131a", color: "#fff", border: "1px solid #3b3b49", borderRadius: "4px", padding: "2px 4px" },
      { type: "number", min: "6", max: "96", step: "0.5", value: String(state.fontSize), title: "Font Size (pt)" }
    );
    const colInp = createEl(
      "input",
      { width: "22px", height: "22px", border: "none", background: "transparent", cursor: "pointer" },
      { type: "color", value: state.color, title: "Text Color" }
    );

    const mkTog = (lbl, active, onClk) => {
      const b = createEl("button", {
        background: active ? "#2f6bff" : "#2c2c38",
        color: "#fff",
        border: "1px solid rgba(255,255,255,0.1)",
        borderRadius: "4px",
        padding: "2px 7px",
        cursor: "pointer",
        fontSize: "11px",
        fontWeight: "600",
      });
      b.textContent = lbl;
      b.addEventListener("click", onClk);
      return b;
    };

    sizeInp.addEventListener("change", () => {
      state.fontSize = Math.max(6, Number(sizeInp.value) || 12);
      ta.style.fontSize = `${state.fontSize * curScale()}px`;
      reflowPreview();
    });
    colInp.addEventListener("change", () => {
      state.color = colInp.value;
      ta.style.color = state.color;
    });
    ta.addEventListener("input", reflowPreview);

    const bBtn = mkTog("B", state.bold, () => {
      state.bold = !state.bold;
      bBtn.style.background = state.bold ? "#2f6bff" : "#2c2c38";
      ta.style.fontWeight = state.bold ? "700" : "400";
      reflowPreview();
    });
    const iBtn = mkTog("I", state.italic, () => {
      state.italic = !state.italic;
      iBtn.style.background = state.italic ? "#2f6bff" : "#2c2c38";
      ta.style.fontStyle = state.italic ? "italic" : "normal";
      reflowPreview();
    });
    const bulBtn = mkTog("•", state.bullets, () => {
      state.bullets = !state.bullets;
      bulBtn.style.background = state.bullets ? "#2f6bff" : "#2c2c38";
      reflowPreview();
    });

    const alignBtn = mkTog(state.align[0].toUpperCase(), false, () => {
      const order = ["left", "center", "right", "justify"];
      state.align = order[(order.indexOf(state.align) + 1) % order.length];
      alignBtn.textContent = state.align[0].toUpperCase();
      ta.style.textAlign = state.align;
      reflowPreview();
    });
    alignBtn.title = "Cycle Alignment (Left/Center/Right/Justify)";

    const moveGrip = createEl("div", {
      position: "absolute",
      top: "-10px",
      right: "8px",
      background: "#2f6bff",
      color: "#fff",
      fontSize: "9px",
      padding: "0 6px",
      borderRadius: "4px",
      cursor: "move",
      userSelect: "none",
    });
    moveGrip.textContent = "Drag Block";
    moveGrip.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const sx = ev.clientX;
      const sy = ev.clientY;
      const origX = state.newBboxVp[0];
      const origY = state.newBboxVp[1];
      const onMv = (e2) => {
        state.newBboxVp[0] = origX + (e2.clientX - sx) / curScale();
        state.newBboxVp[1] = origY + (e2.clientY - sy) / curScale();
        wrap.style.left = `${state.newBboxVp[0] * curScale()}px`;
        wrap.style.top = `${state.newBboxVp[1] * curScale()}px`;
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });

    const closeEditor = () => {
      editTextLayer.innerHTML = "";
      activeTextEditor = null;
    };
    activeTextEditor = closeEditor;

    const doDelete = async () => {
      const res = await window.pdfAcrobat.commitEditText({
        filePath,
        page,
        origBboxVp: state.origBboxVp,
        newBboxVp: state.newBboxVp,
        text: "",
        originalFont: state.originalFont,
        fontFamily: state.fontFamily,
        fontSize: state.fontSize,
        color: state.color,
        bold: state.bold,
        italic: state.italic,
        align: state.align,
        lineSpacing: state.lineSpacing,
        bullets: state.bullets,
        bgColor: "#ffffff",
        deleteBlock: true,
      });
      closeEditor();
      if (res && res.ok) {
        bumpRevision(filePath, res.revision, page);
        await refreshAll();
        onRefreshPdf && onRefreshPdf();
        onToast && onToast("Removed text block from PDF", "info");
      }
    };

    const applyBtn = mkTog("Apply", true, async () => {
      state.text = ta.value;
      const res = await window.pdfAcrobat.commitEditText({
        filePath,
        page,
        origBboxVp: state.origBboxVp,
        newBboxVp: state.newBboxVp,
        text: state.text,
        originalFont: state.originalFont,
        fontFamily: state.fontFamily,
        fontSize: state.fontSize,
        color: state.color,
        bold: state.bold,
        italic: state.italic,
        align: state.align,
        lineSpacing: state.lineSpacing,
        bullets: state.bullets,
        bgColor: "#ffffff",
        deleteBlock: false,
      });
      closeEditor();
      if (res && res.ok) {
        bumpRevision(filePath, res.revision, page);
        await refreshAll();
        onRefreshPdf && onRefreshPdf();
        onToast && onToast(res.notice || "Text block updated in PDF stream", "success");
      } else {
        onToast && onToast((res && res.error) || "Failed editing text block", "error");
      }
    });

    const delBtn = mkTog("Delete", false, doDelete);
    delBtn.style.background = "#dc2626";
    const cancelBtn = mkTog("Cancel", false, closeEditor);

    // Context menu specifically for Text in Edit Mode
    ta.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      const sStart = ta.selectionStart || 0;
      const sEnd = ta.selectionEnd || 0;
      const hasSelText = sEnd > sStart;
      const quickFonts = sysFonts.slice(0, 12).map((f) => ({
        label: f.name,
        checked: state.fontFamily === f.name,
        action: () => {
          state.fontFamily = f.name;
          ta.style.fontFamily = f.cssFamily;
          fontPicker._setDisplay(f.name, f.cssFamily);
          reflowPreview();
        },
      }));
      const quickSizes = [9, 10, 11, 12, 14, 16, 18, 24, 32].map((sz) => ({
        label: `${sz} pt`,
        checked: Math.round(state.fontSize) === sz,
        action: () => {
          state.fontSize = sz;
          sizeInp.value = String(sz);
          ta.style.fontSize = `${sz * curScale()}px`;
          reflowPreview();
        },
      }));
      showContextMenu(ev.clientX, ev.clientY, [
        {
          label: "Cut",
          icon: "✂",
          shortcut: "Ctrl+X",
          disabled: !hasSelText,
          action: () => {
            const sel = ta.value.slice(sStart, sEnd);
            navigator.clipboard && navigator.clipboard.writeText(sel);
            ta.setRangeText("", sStart, sEnd, "start");
            reflowPreview();
          },
        },
        {
          label: "Copy",
          icon: "📋",
          shortcut: "Ctrl+C",
          disabled: !hasSelText,
          action: () => {
            navigator.clipboard && navigator.clipboard.writeText(ta.value.slice(sStart, sEnd));
          },
        },
        {
          label: "Paste",
          icon: "📄",
          shortcut: "Ctrl+V",
          action: async () => {
            try {
              const clip = await navigator.clipboard.readText();
              ta.setRangeText(clip || "", sStart, sEnd, "end");
              reflowPreview();
            } catch {}
          },
        },
        { separator: true },
        { label: "Font", icon: "A", submenu: quickFonts },
        { label: "Font Size", icon: "↕", submenu: quickSizes },
        {
          label: "Bold",
          icon: "B",
          checked: state.bold,
          action: () => bBtn.click(),
        },
        {
          label: "Italic",
          icon: "I",
          checked: state.italic,
          action: () => iBtn.click(),
        },
        { separator: true },
        { label: "Apply Changes", icon: "✓", action: () => applyBtn.click() },
        { label: "Delete Block", icon: "🗑", danger: true, shortcut: "Del", action: doDelete },
      ]);
    });

    toolbar.appendChild(fontPicker);
    toolbar.appendChild(sizeInp);
    toolbar.appendChild(colInp);
    toolbar.appendChild(bBtn);
    toolbar.appendChild(iBtn);
    toolbar.appendChild(bulBtn);
    toolbar.appendChild(alignBtn);
    toolbar.appendChild(applyBtn);
    toolbar.appendChild(delBtn);
    toolbar.appendChild(cancelBtn);

    wrap.appendChild(noticeBanner);
    wrap.appendChild(moveGrip);
    wrap.appendChild(ta);
    wrap.appendChild(toolbar);
    editTextLayer.appendChild(wrap);
    ta.focus();
  }

  function findPageControllerAtClient(clientX, clientY) {
    for (const [k, ctrl] of pageControllers.entries()) {
      if (!k.startsWith(`${filePath}::`)) continue;
      const rc = ctrl.getRect();
      if (clientX >= rc.left - 40 && clientX <= rc.right + 40 && clientY >= rc.top - 12 && clientY <= rc.bottom + 12) {
        return ctrl;
      }
    }
    return null;
  }

  hitLayer.addEventListener("pointerdown", async (ev) => {
    if (ev.button !== 0) return;
    const tool = getTool();
    const [px, py] = clientToPdf(ev.clientX, ev.clientY);

    if (["vec_rect", "vec_ellipse", "vec_line", "vec_arrow", "vec_freehand"].includes(tool)) {
      ev.preventDefault();
      ev.stopPropagation();
      const shapeKind = tool.replace("vec_", "");
      const startPt = [px, py];
      const pts = [[px, py]];
      const previewSvg = createSvgEl("svg", {
        width: "100%",
        height: "100%",
        viewBox: objectsData?.pageTransform
          ? `0 0 ${objectsData.pageTransform.vpWidth} ${objectsData.pageTransform.vpHeight}`
          : `0 0 612 792`,
        style: "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;",
      });
      const color = (getActiveColor && getActiveColor()) || "#2f6bff";
      const lw = (getStrokeWidth && getStrokeWidth()) || 2;
      const previewShape = createSvgEl("path", {
        d: "",
        fill: shapeKind === "rectangle" || shapeKind === "ellipse" ? "rgba(47,107,255,0.16)" : "none",
        stroke: color,
        "stroke-width": String(lw),
      });
      previewSvg.appendChild(previewShape);
      shapeHudLayer.appendChild(previewSvg);

      const onMv = (e2) => {
        const [nx, ny] = clientToPdf(e2.clientX, e2.clientY);
        if (shapeKind === "freehand") {
          pts.push([nx, ny]);
          previewShape.setAttribute(
            "d",
            pts.map((p, i) => `${i === 0 ? "M" : "L"}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(" ")
          );
        } else if (shapeKind === "line" || shapeKind === "arrow") {
          pts[1] = [nx, ny];
          previewShape.setAttribute(
            "d",
            `M${startPt[0].toFixed(1)},${startPt[1].toFixed(1)} L${nx.toFixed(1)},${ny.toFixed(1)}`
          );
        } else if (shapeKind === "rectangle") {
          const rx = Math.min(startPt[0], nx),
            ry = Math.min(startPt[1], ny),
            rw = Math.abs(nx - startPt[0]),
            rh = Math.abs(ny - startPt[1]);
          previewShape.setAttribute(
            "d",
            `M${rx},${ry} L${rx + rw},${ry} L${rx + rw},${ry + rh} L${rx},${ry + rh} Z`
          );
        } else if (shapeKind === "ellipse") {
          const rx = Math.abs(nx - startPt[0]) / 2,
            ry = Math.abs(ny - startPt[1]) / 2,
            cx = (startPt[0] + nx) / 2,
            cy = (startPt[1] + ny) / 2;
          previewShape.setAttribute(
            "d",
            `M${cx - rx},${cy} A${rx},${ry} 0 1,0 ${cx + rx},${cy} A${rx},${ry} 0 1,0 ${cx - rx},${cy} Z`
          );
        }
      };

      const onUp = async (e2) => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
        previewSvg.remove();
        const [ex, ey] = clientToPdf(e2.clientX, e2.clientY);
        const bx = Math.min(startPt[0], ex),
          by = Math.min(startPt[1], ey),
          bw = Math.max(4, Math.abs(ex - startPt[0])),
          bh = Math.max(4, Math.abs(ey - startPt[1]));
        if (bw < 5 && bh < 5 && pts.length < 3) return;
        const res = await window.pdfAcrobat.commitShapeEdit({
          filePath,
          page,
          action: "add",
          shapeKind,
          opStart: null,
          opEnd: null,
          bboxVp: [bx, by, bw, bh],
          origBboxVp: null,
          origSegmentsVp: null,
          pointsVp: shapeKind === "freehand" ? pts : [[startPt[0], startPt[1]], [ex, ey]],
          rotationDeg: 0,
          fillColor: shapeKind === "rectangle" || shapeKind === "ellipse" ? color : null,
          strokeColor: color,
          fillOpacity: shapeKind === "rectangle" || shapeKind === "ellipse" ? 0.25 : 1,
          strokeOpacity: 1,
          strokeWidth: lw,
        });
        if (res && res.ok) {
          bumpRevision(filePath, res.revision, page);
          await refreshAll();
          onRefreshPdf && onRefreshPdf();
          onToast && onToast(`Added PDF ${shapeKind} shape`, "success");
        }
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
      return;
    }

    if (tool === "editText" || tool === "pageEdit") {
      ev.preventDefault();
      ev.stopPropagation();
      const hit = await window.pdfAcrobat.hitTest(filePath, page, px, py).catch(() => null);
      if (hit && hit.blockId && layoutData) {
        const blk = (layoutData.blocks || []).find((b) => b.id === hit.blockId);
        if (blk) {
          openEditTextBox(blk);
          return;
        }
      }
      if (hit && hit.shapeId && objectsData) {
        const shp = (objectsData.pathObjects || []).find((s) => s.id === hit.shapeId);
        if (shp) selectShapeObject(shp);
      }
      return;
    }

    // Pure Acrobat Reading-Order Selection (NO floating popup)
    ev.preventDefault();
    ev.stopPropagation();
    const clickDetail = ev.detail || 1;
    const hit = await window.pdfAcrobat.hitTest(filePath, page, px, py).catch(() => null);
    if (!hit || hit.charIndex === null || hit.charIndex === undefined) {
      clearSelection(filePath);
      return;
    }

    if (clickDetail >= 2) {
      const rng =
        clickDetail === 2
          ? hit.wordRange
          : clickDetail === 3
          ? hit.lineRange
          : hit.blockRange;
      if (rng && rng.length === 2) {
        selectionState = await computeRangeState(filePath, page, rng[0], page, rng[1], true);
        emitSelectionChange();
        updateAllSelectionLayersForFile(filePath);
      }
      return;
    }

    const anchorPage = page;
    const anchorChar = hit.charIndex;
    let focusPage = page;
    let focusChar = anchorChar;
    let dragged = false;

    const startClientX = ev.clientX;
    const startClientY = ev.clientY;
    let rafPending = false;

    const onMove = (e2) => {
      if (!dragged && Math.hypot(e2.clientX - startClientX, e2.clientY - startClientY) < 3) return;
      dragged = true;
      const targetCtrl = findPageControllerAtClient(e2.clientX, e2.clientY) || api;
      const targetPage = targetCtrl.getPage();
      const [nx, ny] = targetCtrl.mapClientToPdf(e2.clientX, e2.clientY);
      const nextChar = localFindChar(targetCtrl.getLayout(), nx, ny);
      if (nextChar !== null && (nextChar !== focusChar || targetPage !== focusPage)) {
        focusPage = targetPage;
        focusChar = nextChar;
        if (!rafPending) {
          rafPending = true;
          requestAnimationFrame(async () => {
            rafPending = false;
            selectionState = await computeRangeState(filePath, anchorPage, anchorChar, focusPage, focusChar, false);
            emitSelectionChange();
            updateAllSelectionLayersForFile(filePath);
          });
        }
      }
    };

    const onUp = async () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      if (!dragged) {
        clearSelection(filePath);
        return;
      }
      selectionState = await computeRangeState(filePath, anchorPage, anchorChar, focusPage, focusChar, true);
      emitSelectionChange();
      updateAllSelectionLayersForFile(filePath);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  });

  const onKeyDown = async (ev) => {
    if (destroyed) return;
    const tag = ev.target && ev.target.tagName;
    if (["INPUT", "TEXTAREA", "SELECT"].includes(tag) || (ev.target && ev.target.isContentEditable)) return;

    if ((ev.ctrlKey || ev.metaKey) && !ev.shiftKey && !ev.altKey) {
      const k = ev.key.toLowerCase();
      if (k === "c" && selectionState.active && selectionState.filePath === filePath && selectionState.page === page) {
        ev.preventDefault();
        ev.stopPropagation();
        await applySelectionAction("copy");
      } else if (k === "a") {
        const ctrls = Array.from(pageControllers.entries())
          .filter(([key, ctrl]) => key.startsWith(`${filePath}::`) && ctrl.getLayout()?.chars?.length)
          .sort((a, b) => a[1].getPage() - b[1].getPage());
        if (ctrls.length && ctrls[0][1].getPage() === page) {
          const firstCtrl = ctrls[0][1];
          const lastCtrl = ctrls[ctrls.length - 1][1];
          const lastLen = lastCtrl.getLayout().chars.length;
          if (lastLen > 0) {
            ev.preventDefault();
            ev.stopPropagation();
            selectionState = await computeRangeState(
              filePath,
              firstCtrl.getPage(),
              0,
              lastCtrl.getPage(),
              lastLen - 1,
              false
            );
            emitSelectionChange();
            updateAllSelectionLayersForFile(filePath);
          }
        }
      }
    } else if (ev.altKey && !ev.ctrlKey && !ev.metaKey && !ev.shiftKey && selectionState.active && selectionState.filePath === filePath && selectionState.page === page) {
      const k = ev.key.toLowerCase();
      if (k === "h") {
        ev.preventDefault();
        await applySelectionAction("highlight");
      } else if (k === "u") {
        ev.preventDefault();
        await applySelectionAction("underline");
      } else if (k === "s") {
        ev.preventDefault();
        await applySelectionAction("strikethrough");
      }
    }
  };
  window.addEventListener("keydown", onKeyDown);

  async function refreshAll() {
    if (destroyed) return;
    syncPointerModes();
    const [lay, objs] = await Promise.all([
      getCachedLayout(filePath, page),
      getCachedObjects(filePath, page),
    ]);
    if (destroyed) return;
    layoutData = lay;
    objectsData = objs;
    if (selectionState.active && selectionState.filePath === filePath && page >= selectionState.startPage && page <= selectionState.endPage) {
      selectionState = await computeRangeState(
        filePath,
        selectionState.startPage,
        selectionState.startChar,
        selectionState.endPage,
        selectionState.endChar,
        true
      );
      emitSelectionChange();
    }
    renderSelectionOnly();
    renderShapeOverlays();
  }

  refreshAll();

  const api = {
    getPage: () => page,
    getRect: () => container.getBoundingClientRect(),
    getLayout: () => layoutData,
    getObjects: () => objectsData,
    mapClientToPdf: clientToPdf,
    toast: onToast,
    renderSelectionOnly,
    applyMarkupBatch: (tp, items, col) => {
      if (onCreateMarkupBatch) {
        onCreateMarkupBatch(tp, items, col);
      } else if (onCreateMarkup) {
        for (const it of items) {
          onCreateMarkup(tp, it.rects, it.text, col, it.page);
        }
      }
    },
    refreshLayoutOnly: () => {
      syncPointerModes();
      renderSelectionOnly();
      renderShapeOverlays();
    },
    reloadFromRust: refreshAll,
    openEditBlockAtPoint: async (clientX, clientY) => {
      const [px, py] = clientToPdf(clientX, clientY);
      const hit = await window.pdfAcrobat.hitTest(filePath, page, px, py).catch(() => null);
      if (hit && hit.blockId && layoutData) {
        const blk = (layoutData.blocks || []).find((b) => b.id === hit.blockId);
        if (blk) {
          openEditTextBox(blk);
          return true;
        }
      }
      return false;
    },
    openEditBlockForChar: (charIdx) => {
      if (!layoutData || !layoutData.blocks) return false;
      const blk = layoutData.blocks.find((b) => charIdx >= b.startChar && charIdx <= b.endChar);
      if (blk) {
        openEditTextBox(blk);
        return true;
      }
      return false;
    },
    hitShapeAtClient: (clientX, clientY) => {
      if (!objectsData || !objectsData.pathObjects) return null;
      const [px, py] = clientToPdf(clientX, clientY);
      return objectsData.pathObjects
        .slice()
        .reverse()
        .find((s) => !s.isPageBackground && px >= s.bboxVp[0] - 4 && px <= s.bboxVp[0] + s.bboxVp[2] + 4 && py >= s.bboxVp[1] - 4 && py <= s.bboxVp[1] + s.bboxVp[3] + 4) || null;
    },
    hitImageAtClient: (clientX, clientY) => {
      if (!objectsData || !objectsData.imageObjects) return null;
      const [px, py] = clientToPdf(clientX, clientY);
      return objectsData.imageObjects
        .slice()
        .reverse()
        .find((im) => px >= im.bboxVp[0] && px <= im.bboxVp[0] + im.bboxVp[2] && py >= im.bboxVp[1] && py <= im.bboxVp[1] + im.bboxVp[3]) || null;
    },
    selectAndEditShape: (shp) => {
      if (shp) selectShapeObject(shp);
    },
    deletePdfShape: async (shp) => {
      if (!shp || !window.pdfAcrobat) return;
      const res = await window.pdfAcrobat.commitShapeEdit({
        filePath,
        page,
        action: "delete",
        shapeKind: shp.shapeKind,
        opStart: shp.opStart,
        opEnd: shp.opEnd,
        bboxVp: shp.bboxVp,
        origBboxVp: shp.bboxVp,
        origSegmentsVp: shp.segmentsVp,
        pointsVp: null,
        rotationDeg: 0,
        fillColor: shp.fillColor,
        strokeColor: shp.strokeColor,
        fillOpacity: shp.fillOpacity ?? 1,
        strokeOpacity: shp.strokeOpacity ?? 1,
        strokeWidth: shp.strokeWidth ?? 2,
      });
      if (res && res.ok) {
        bumpRevision(filePath, res.revision, page);
        await refreshAll();
        onRefreshPdf && onRefreshPdf();
        onToast && onToast("Removed vector shape", "info");
      }
    },
    getSelectedText: () => (selectionState.active && selectionState.filePath === filePath ? selectionState.text : ""),
    destroy: () => {
      destroyed = true;
      window.removeEventListener("keydown", onKeyDown);
      if (activeTextEditor) activeTextEditor();
      root.remove();
      pageControllers.delete(ctrlKey);
    },
  };
  pageControllers.set(ctrlKey, api);
  return api;
}

export function notifyAllPageSurfaces(filePath) {
  for (const [k, ctrl] of pageControllers.entries()) {
    if (!filePath || k.startsWith(`${filePath}::`)) {
      ctrl.refreshLayoutOnly();
    }
  }
}

export function getControllerForPage(filePath, page) {
  return pageControllers.get(`${filePath}::${page}`) || null;
}
