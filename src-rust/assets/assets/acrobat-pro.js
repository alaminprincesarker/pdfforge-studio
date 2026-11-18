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

export function closeContextMenu(immediate = false) {
  if (activeMenuCleanup) {
    const fn = activeMenuCleanup;
    activeMenuCleanup = null;
    fn(immediate);
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
  closeContextMenu(true);
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
  let subHoverTimer = null;
  const clearSubTimer = () => {
    if (subHoverTimer) {
      clearTimeout(subHoverTimer);
      subHoverTimer = null;
    }
  };

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
      clearSubTimer();
      if (activeSubEl) {
        activeSubEl.remove();
        activeSubEl = null;
      }
      if (!rowObj || !rowObj.item.submenu || !rowObj.item.submenu.length) return;
      const rc = rowObj.el.getBoundingClientRect();
      activeSubEl = buildPanel(rowObj.item.submenu, rc.right - 2, rc.top - 4, true);
      overlay.appendChild(activeSubEl);
      const origin = positionPanel(activeSubEl, rc.right - 2, rc.top - 4, rc.left);
      if (window.__pdfforgeMotion) {
        window.__pdfforgeMotion.animateEnter(activeSubEl, {
          duration: 80,
          fromTransform: "scale(0.98)",
          origin,
        });
      }
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
        if (it.submenu && !it.disabled) {
          clearSubTimer();
          subHoverTimer = setTimeout(() => {
            openSub(entry);
          }, 150);
        } else if (!isSub && activeSubEl) {
          clearSubTimer();
          subHoverTimer = setTimeout(() => {
            if (activeSubEl) {
              activeSubEl.remove();
              activeSubEl = null;
            }
          }, 150);
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
    let ox = "left";
    let oy = "top";
    if (left + pw > vw - 8) {
      left = fallbackRightX !== null ? Math.max(8, fallbackRightX - pw + 2) : Math.max(8, vw - pw - 8);
      ox = "right";
    }
    if (top + ph > vh - 8) {
      top = Math.max(8, vh - ph - 8);
      oy = "bottom";
    }
    panel.style.left = `${Math.max(8, left)}px`;
    panel.style.top = `${Math.max(8, top)}px`;
    return `${oy} ${ox}`;
  }

  const mainPanel = buildPanel(items, clientX, clientY, false);
  overlay.appendChild(mainPanel);
  document.body.appendChild(overlay);
  const origin = positionPanel(mainPanel, clientX, clientY);
  if (window.__pdfforgeMotion) {
    window.__pdfforgeMotion.animateEnter(mainPanel, {
      duration: 70,
      fromTransform: "scale(0.97)",
      origin,
    });
  }

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

  activeMenuCleanup = (immediate = false) => {
    clearSubTimer();
    window.removeEventListener("mousedown", onDownOutside, true);
    window.removeEventListener("scroll", onDismiss, true);
    window.removeEventListener("resize", onDismiss);
    window.removeEventListener("blur", onDismiss);
    window.removeEventListener("keydown", onKey, true);
    if (!immediate && window.__pdfforgeMotion) {
      overlay.style.pointerEvents = "none";
      if (activeSubEl) {
        window.__pdfforgeMotion.animateExit(activeSubEl, { duration: 55, toTransform: "scale(0.98)" });
      }
      window.__pdfforgeMotion.animateExit(mainPanel, {
        duration: 55,
        toTransform: "scale(0.97)",
        origin,
        onDone: () => overlay.remove(),
      });
    } else {
      overlay.remove();
    }
  };
}

window.__acrobatShowContextMenu = showContextMenu;
window.__acrobatCloseContextMenu = closeContextMenu;

// =========================================================================
// Searchable Live-Preview Font Picker Popup (Offline Bundled + Windows Fonts)
// =========================================================================
function normalizeFontName(raw) {
  if (!raw) return "Noto Sans";
  let s = String(raw).replace(/^[A-Z]{6}\+/, "").trim();
  s = s.replace(/(?:PS)?(?:-(?:Bold|Italic|Oblique|Regular|Medium|Light|SemiBold|Black))+(?:MT)?$/i, "").replace(/MT$/i, "").trim();
  const low = s.toLowerCase();
  if (low === "timesnewroman" || low === "times-roman" || low === "times") return "Times New Roman";
  if (low === "helvetica" || low === "arialmt" || low === "arial") return "Arial";
  if (low === "couriernew" || low === "courier") return "Courier New";
  return s || "Noto Sans";
}

function createSearchableFontPicker(currentFont, fontsList, onSelect) {
  const wrap = createEl("div", { position: "relative", display: "inline-block" });

  const cleanInit = normalizeFontName(currentFont);
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
    fontFamily: `'${cleanInit}', sans-serif`,
  });
  lbl.textContent = cleanInit;
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

  trigger.addEventListener("mousedown", (ev) => {
    ev.stopPropagation();
  });
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
// Image Helper & Catmull-Rom Ink Smoothing Helpers
// =========================================================================
export function catmullRomScreenPath(ctx, pts, baseWidth, hasPressure) {
  if (!pts || pts.length === 0) return;
  if (pts.length === 1) {
    ctx.beginPath();
    ctx.arc(pts[0].x, pts[0].y, Math.max(0.8, (baseWidth * (pts[0].p || 0.7)) / 2), 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  if (!hasPressure) {
    ctx.lineWidth = Math.max(0.8, baseWidth);
    ctx.beginPath();
    ctx.moveTo(pts[0].x, pts[0].y);
    if (pts.length === 2) {
      ctx.lineTo(pts[1].x, pts[1].y);
    } else {
      for (let i = 0; i < pts.length - 1; i++) {
        const p0 = pts[Math.max(0, i - 1)];
        const p1 = pts[i];
        const p2 = pts[i + 1];
        const p3 = pts[Math.min(pts.length - 1, i + 2)];
        const cp1x = p1.x + (p2.x - p0.x) / 6;
        const cp1y = p1.y + (p2.y - p0.y) / 6;
        const cp2x = p2.x - (p3.x - p1.x) / 6;
        const cp2y = p2.y - (p3.y - p1.y) / 6;
        ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, p2.x, p2.y);
      }
    }
    ctx.stroke();
    return;
  }
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[Math.max(0, i - 1)];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[Math.min(pts.length - 1, i + 2)];
    const cp1x = p1.x + (p2.x - p0.x) / 6;
    const cp1y = p1.y + (p2.y - p0.y) / 6;
    const cp2x = p2.x - (p3.x - p1.x) / 6;
    const cp2y = p2.y - (p3.y - p1.y) / 6;
    const avgP = ((p1.p || 0.7) + (p2.p || 0.7)) * 0.5;
    ctx.lineWidth = Math.max(0.8, baseWidth * (0.45 + avgP * 0.85));
    ctx.beginPath();
    ctx.moveTo(p1.x, p1.y);
    ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, p2.x, p2.y);
    ctx.stroke();
  }
}

export async function dataUrlToImagePayload(dataUrl) {
  if (!dataUrl || typeof dataUrl !== "string") return {};
  const lower = dataUrl.toLowerCase();
  if (lower.startsWith("data:image/jpeg;base64,") || lower.startsWith("data:image/jpg;base64,")) {
    const dims = await new Promise((resolve) => {
      const img = new Image();
      img.onload = () => resolve({ w: img.naturalWidth || 200, h: img.naturalHeight || 200 });
      img.onerror = () => resolve({ w: 200, h: 200 });
      img.src = dataUrl;
    });
    return {
      imageDataUrl: dataUrl,
      rgbaBase64: null,
      pixelWidth: dims.w,
      pixelHeight: dims.h,
    };
  }
  return await new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      const w = Math.max(1, img.naturalWidth || 200);
      const h = Math.max(1, img.naturalHeight || 200);
      const cvs = document.createElement("canvas");
      cvs.width = w;
      cvs.height = h;
      const ctx = cvs.getContext("2d");
      ctx.drawImage(img, 0, 0, w, h);
      const raw = ctx.getImageData(0, 0, w, h).data;
      let bin = "";
      const chunk = 0x8000;
      for (let i = 0; i < raw.length; i += chunk) {
        bin += String.fromCharCode.apply(null, raw.subarray(i, i + chunk));
      }
      resolve({
        imageDataUrl: null,
        rgbaBase64: btoa(bin),
        pixelWidth: w,
        pixelHeight: h,
      });
    };
    img.onerror = () => resolve({ imageDataUrl: dataUrl });
    img.src = dataUrl;
  });
}

window.__acrobatDataUrlToImagePayload = dataUrlToImagePayload;

// =========================================================================
// Page Surface Controller: Selection + Edit PDF + Ink + Shapes + Forms
// =========================================================================
export function mountAcrobatPageSurface(opts) {
  const {
    container,
    filePath,
    page,
    getZoom,
    getRotation,
    getTool,
    getActiveShape,
    getShapeStyle,
    getActiveColor,
    getStrokeWidth,
    getOpacity,
    getDrawAsHighlighter,
    getFontFamily,
    getFontSize,
    getSearchQuery,
    onCreateMarkupBatch,
    onCreateMarkup,
    onRefreshPdf,
    onMutationState,
    onExitEditMode,
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
  let annotsData = [];
  let selectedShape = null;
  let selectedImage = null;
  let selectedAnnot = null;
  let activeTextEditor = null;
  let polyPoints = []; // Multi-click polygon / polyline preview

  const isEditTool = (t) =>
    t === "editText" || t === "pageEdit" || t === "editPdf" || t === "editShape" || t === "addTextBox";
  let lastEditModeState = isEditTool(getTool());
  const isTextMarkupTool = (t) => t === "highlight" || t === "underline" || t === "strikethrough";
  const isInkTool = (t) => t === "draw" || t === "ink" || t === "highlighter_ink" || t === "vec_freehand";
  const isShapeAnnotTool = (t) => t === "shape";
  const isVecTool = (t) => ["vec_rect", "vec_ellipse", "vec_line", "vec_arrow"].includes(t);

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

  const inkCanvas = createEl("canvas", {
    position: "absolute",
    inset: "0",
    width: "100%",
    height: "100%",
    pointerEvents: "none",
    zIndex: "20",
  });

  const selLayer = createEl("div", {
    position: "absolute",
    inset: "0",
    pointerEvents: "none",
    zIndex: "18",
  });

  const formLayer = createEl("div", {
    position: "absolute",
    inset: "0",
    pointerEvents: "none",
    zIndex: "22",
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
  root.appendChild(inkCanvas);
  root.appendChild(formLayer);
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

  function notifyMutation(res) {
    if (res && onMutationState) {
      onMutationState({
        isDirty: true,
        canUndo: !!res.canUndo,
        canRedo: !!res.canRedo,
        revision: res.revision,
      });
    }
  }

  function syncCanvasSize() {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, container.clientWidth || container.offsetWidth || 1);
    const h = Math.max(1, container.clientHeight || container.offsetHeight || 1);
    if (inkCanvas.width !== Math.round(w * dpr) || inkCanvas.height !== Math.round(h * dpr)) {
      inkCanvas.width = Math.round(w * dpr);
      inkCanvas.height = Math.round(h * dpr);
    }
    const ctx = inkCanvas.getContext("2d");
    if (ctx) ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return ctx;
  }

  function clearInkCanvas() {
    const ctx = inkCanvas.getContext("2d");
    if (ctx) {
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, inkCanvas.width, inkCanvas.height);
      ctx.restore();
    }
  }

  function syncPointerModes() {
    const tool = getTool();
    const isSel = tool === "select";
    const isMarkup = isTextMarkupTool(tool);
    const isEdit = isEditTool(tool);
    const isInk = isInkTool(tool);
    const isEraser = tool === "eraser";
    const isShape = isShapeAnnotTool(tool) || isVecTool(tool);

    if (isSel || isMarkup || isEdit || isInk || isEraser || isShape) {
      hitLayer.style.pointerEvents = "auto";
      hitLayer.style.touchAction = isInk || isEraser || isShape ? "none" : "auto";
      hitLayer.style.cursor =
        isSel || isMarkup
          ? "text"
          : isInk || isShape
          ? "crosshair"
          : isEraser
          ? "cell"
          : tool === "addTextBox"
          ? "crosshair"
          : "default";
    } else {
      hitLayer.style.pointerEvents = "none";
      hitLayer.style.touchAction = "auto";
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

  function renderFormWidgets() {
    formLayer.innerHTML = "";
    const widgets = (annotsData || []).filter((a) => a.subtype === "Widget");
    if (!widgets.length) return;
    const sc = curScale();
    for (const w of widgets) {
      const [bx, by, bw, bh] = w.bboxVp || [0, 0, 80, 18];
      const ftype = w.fieldType || "Tx";
      if (ftype === "Btn") {
        const checked = w.fieldValue && w.fieldValue !== "Off" && w.fieldValue !== "No";
        const cb = createEl(
          "input",
          {
            position: "absolute",
            left: `${bx * sc}px`,
            top: `${by * sc}px`,
            width: `${Math.max(14, bw * sc)}px`,
            height: `${Math.max(14, bh * sc)}px`,
            margin: "0",
            cursor: "pointer",
            pointerEvents: "auto",
            accentColor: "#2f6bff",
          },
          { type: "checkbox", title: w.fieldName || "Checkbox" }
        );
        cb.checked = !!checked;
        cb.addEventListener("change", async (ev) => {
          ev.stopPropagation();
          const val = cb.checked ? "Yes" : "Off";
          const res = await window.pdfAcrobat.updateFormField({
            filePath,
            page,
            fieldName: w.fieldName || w.id,
            value: val,
          });
          if (res && res.ok) {
            bumpRevision(filePath, res.revision, page);
            notifyMutation(res);
            await refreshAll();
            onRefreshPdf && onRefreshPdf();
          }
        });
        formLayer.appendChild(cb);
      } else if (ftype === "Ch" && Array.isArray(w.fieldOptions) && w.fieldOptions.length) {
        const selEl = createEl(
          "select",
          {
            position: "absolute",
            left: `${bx * sc}px`,
            top: `${by * sc}px`,
            width: `${Math.max(30, bw * sc)}px`,
            height: `${Math.max(16, bh * sc)}px`,
            fontSize: `${Math.max(10, Math.min(14, bh * sc * 0.65))}px`,
            border: "1px solid rgba(47,107,255,0.45)",
            background: "rgba(255,255,255,0.92)",
            color: "#111827",
            pointerEvents: "auto",
            borderRadius: "2px",
          },
          { title: w.fieldName || "Choice Field" }
        );
        for (const opt of w.fieldOptions) {
          const oEl = document.createElement("option");
          oEl.value = opt;
          oEl.textContent = opt;
          if (opt === w.fieldValue) oEl.selected = true;
          selEl.appendChild(oEl);
        }
        selEl.addEventListener("change", async (ev) => {
          ev.stopPropagation();
          const res = await window.pdfAcrobat.updateFormField({
            filePath,
            page,
            fieldName: w.fieldName || w.id,
            value: selEl.value,
          });
          if (res && res.ok) {
            bumpRevision(filePath, res.revision, page);
            notifyMutation(res);
            await refreshAll();
            onRefreshPdf && onRefreshPdf();
          }
        });
        formLayer.appendChild(selEl);
      } else {
        const inp = createEl(
          "input",
          {
            position: "absolute",
            left: `${bx * sc}px`,
            top: `${by * sc}px`,
            width: `${Math.max(24, bw * sc)}px`,
            height: `${Math.max(14, bh * sc)}px`,
            fontSize: `${Math.max(9, (w.fontSize || 11) * sc)}px`,
            border: "1px solid rgba(47,107,255,0.35)",
            background: "rgba(239,246,255,0.35)",
            color: "#111827",
            padding: "0 3px",
            boxSizing: "border-box",
            pointerEvents: "auto",
            borderRadius: "2px",
          },
          { type: "text", value: w.fieldValue || "", title: w.fieldName || "Form Text Field" }
        );
        inp.addEventListener("focus", () => {
          inp.style.background = "rgba(255,255,255,0.96)";
          inp.style.borderColor = "#2f6bff";
        });
        inp.addEventListener("blur", async () => {
          inp.style.background = "rgba(239,246,255,0.35)";
          if (inp.value === (w.fieldValue || "")) return;
          const res = await window.pdfAcrobat.updateFormField({
            filePath,
            page,
            fieldName: w.fieldName || w.id,
            value: inp.value,
          });
          if (res && res.ok) {
            w.fieldValue = inp.value;
            bumpRevision(filePath, res.revision, page);
            notifyMutation(res);
            onRefreshPdf && onRefreshPdf();
            onToast && onToast(`Updated PDF form field "${w.fieldName || w.id}"`, "success");
          }
        });
        inp.addEventListener("keydown", (ev) => {
          if (ev.key === "Enter") {
            ev.preventDefault();
            inp.blur();
          }
        });
        formLayer.appendChild(inp);
      }
    }
  }

  function renderShapeOverlays() {
    svgLayer.innerHTML = "";
    shapeHudLayer.innerHTML = "";

    const pt = objectsData?.pageTransform || layoutData?.pageTransform;
    if (pt) {
      svgLayer.setAttribute("viewBox", `0 0 ${pt.vpWidth} ${pt.vpHeight}`);
    }

    const tool = getTool();
    const inEditMode = isEditTool(tool);
    if (inEditMode && !lastEditModeState && window.__pdfforgeMotion) {
      window.__pdfforgeMotion.animateEnter(svgLayer, { duration: 65 });
    }
    lastEditModeState = inEditMode;

    // 1. Unified Edit PDF Outlines: Text Blocks (blue), Images (emerald), Vector Paths (purple)
    if (inEditMode && layoutData && Array.isArray(layoutData.blocks)) {
      for (const blk of layoutData.blocks) {
        if (!blk.bbox || blk.bbox[2] < 2 || blk.bbox[3] < 2) continue;
        const missing = !blk.hasEmbeddedFont || blk.isSubsetFont;
        const rectEl = createSvgEl("rect", {
          x: blk.bbox[0],
          y: blk.bbox[1],
          width: blk.bbox[2],
          height: blk.bbox[3],
          fill: "rgba(47, 107, 255, 0.03)",
          stroke: missing ? "rgba(245, 158, 11, 0.75)" : "rgba(59, 130, 246, 0.55)",
          "stroke-width": "1",
          "stroke-dasharray": "4 2",
          rx: "2",
          style: "pointer-events:auto;cursor:text;",
        });
        const titleEl = createSvgEl("title");
        titleEl.textContent = missing
          ? `Edit Text Block (${blk.fontName} — substitute notice on edit)`
          : `Edit Text Block (${blk.fontName})`;
        rectEl.appendChild(titleEl);
        rectEl.addEventListener("pointerdown", (ev) => {
          if (ev.button !== 0) return;
          ev.stopPropagation();
          selectedShape = null;
          selectedImage = null;
          selectedAnnot = null;
          openEditTextBox(blk);
        });
        svgLayer.appendChild(rectEl);
      }
    }

    if ((inEditMode || selectedImage !== null) && objectsData && Array.isArray(objectsData.imageObjects)) {
      for (const im of objectsData.imageObjects) {
        const isSel = selectedImage && selectedImage.id === im.id;
        const [ix, iy, iw, ih] = im.bboxVp;
        const imgBox = createSvgEl("rect", {
          x: ix,
          y: iy,
          width: Math.max(4, iw),
          height: Math.max(4, ih),
          fill: isSel ? "rgba(16, 185, 129, 0.12)" : "rgba(16, 185, 129, 0.03)",
          stroke: isSel ? "#10b981" : "rgba(16, 185, 129, 0.65)",
          "stroke-width": isSel ? "2" : "1.2",
          "stroke-dasharray": isSel ? "none" : "5 3",
          style: "pointer-events:auto;cursor:move;",
        });
        imgBox.addEventListener("pointerdown", (ev) => {
          if (ev.button !== 0) return;
          ev.stopPropagation();
          selectImageObject(im);
        });
        svgLayer.appendChild(imgBox);
      }
    }

    if ((inEditMode || selectedShape !== null) && objectsData && Array.isArray(objectsData.pathObjects)) {
      for (const shp of objectsData.pathObjects) {
        if (shp.isPageBackground) continue;
        const isSel = selectedShape && selectedShape.id === shp.id;
        const pathEl = createSvgEl("path", {
          d: shp.svgPath,
          fill: isSel ? "rgba(139, 92, 246, 0.14)" : "transparent",
          stroke: isSel ? "#8b5cf6" : "rgba(139, 92, 246, 0.55)",
          "stroke-width": isSel ? "2" : "1",
          "stroke-dasharray": isSel ? "none" : "4 2",
          style: "pointer-events:auto;cursor:move;",
        });
        pathEl.addEventListener("pointerdown", (ev) => {
          if (ev.button !== 0) return;
          ev.stopPropagation();
          selectShapeObject(shp);
        });
        svgLayer.appendChild(pathEl);
      }
    }

    // 2. Interactive PDF Annotations (hover/click hit targets, Sticky Note icons, Eraser highlight, Selected HUD)
    const canInteractAnnot = tool === "eraser" || tool === "draw" || tool === "shape" || isEditTool(tool) || tool === "select";
    if (Array.isArray(annotsData) && canInteractAnnot) {
      for (const a of annotsData) {
        if (!a || a.subtype === "Widget") continue;
        const [ax, ay, aw, ah] = a.bboxVp || [0, 0, 16, 16];
        const isSel = selectedAnnot && selectedAnnot.id === a.id;
        const showBox = tool === "eraser" || isEditTool(tool) || isSel;
        if (showBox || a.subtype === "Text") {
          const hitRect = createSvgEl("rect", {
            x: ax,
            y: ay,
            width: Math.max(14, aw),
            height: Math.max(14, ah),
            fill: isSel ? "rgba(47,107,255,0.12)" : tool === "eraser" ? "rgba(239,68,68,0.06)" : "transparent",
            stroke: isSel ? "#2f6bff" : tool === "eraser" ? "rgba(239,68,68,0.55)" : "transparent",
            "stroke-width": isSel ? "1.6" : "1",
            "stroke-dasharray": isSel ? "none" : "3 2",
            rx: "3",
            style: tool === "eraser" ? "pointer-events:auto;cursor:cell;" : a.subtype === "Text" || isEditTool(tool) ? "pointer-events:auto;cursor:pointer;" : "pointer-events:none;",
          });
          if (tool === "eraser") {
            hitRect.addEventListener("pointerdown", (ev) => {
              ev.stopPropagation();
              deleteAnnotationObject(a);
            });
          } else if (a.subtype === "Text" || isEditTool(tool)) {
            hitRect.addEventListener("pointerdown", (ev) => {
              if (ev.button !== 0) return;
              ev.stopPropagation();
              selectAnnotationObject(a);
            });
          }
          svgLayer.appendChild(hitRect);
        }
      }
    }

    if (selectedShape) renderSelectedShapeHud();
    if (selectedImage) renderSelectedImageHud();
    if (selectedAnnot) renderSelectedAnnotHud();
  }

  function renderSelectedShapeHud() {
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
      border: "1.5px solid #8b5cf6",
      backgroundColor: "rgba(139, 92, 246, 0.08)",
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
        renderShapeOverlays();
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
      backgroundColor: "#8b5cf6",
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
        renderShapeOverlays();
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
        selectedShape.rotationDeg = Math.round((Math.atan2(e2.clientY - cy, e2.clientX - cx) * 180) / Math.PI + 90);
        renderShapeOverlays();
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
        left: `${Math.max(8, Math.min((container.clientWidth || 600) - 340, bx * sc))}px`,
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

  function renderSelectedImageHud() {
    if (!selectedImage) return;
    const sc = curScale();
    const [bx, by, bw, bh] = selectedImage.bboxVp;
    const rot = selectedImage.rotationDeg || 0;

    const frame = createEl("div", {
      position: "absolute",
      left: `${bx * sc}px`,
      top: `${by * sc}px`,
      width: `${Math.max(16, bw * sc)}px`,
      height: `${Math.max(16, bh * sc)}px`,
      border: "2px solid #10b981",
      backgroundColor: "rgba(16, 185, 129, 0.08)",
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
      const sx = ev.clientX;
      const sy = ev.clientY;
      const orig = [...selectedImage.bboxVp];
      selectedImage.pendingAction = selectedImage.pendingAction || "move";
      const onMv = (e2) => {
        selectedImage.bboxVp = [
          orig[0] + (e2.clientX - sx) / curScale(),
          orig[1] + (e2.clientY - sy) / curScale(),
          orig[2],
          orig[3],
        ];
        renderShapeOverlays();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });

    const se = createEl("div", {
      position: "absolute",
      right: "-5px",
      bottom: "-5px",
      width: "10px",
      height: "10px",
      backgroundColor: "#10b981",
      border: "1.5px solid #fff",
      borderRadius: "2px",
      cursor: "se-resize",
    });
    se.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const sx = ev.clientX;
      const sy = ev.clientY;
      const orig = [...selectedImage.bboxVp];
      selectedImage.pendingAction = "resize";
      const onMv = (e2) => {
        selectedImage.bboxVp = [
          orig[0],
          orig[1],
          Math.max(12, orig[2] + (e2.clientX - sx) / curScale()),
          Math.max(12, orig[3] + (e2.clientY - sy) / curScale()),
        ];
        renderShapeOverlays();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });
    frame.appendChild(se);

    // Interactive Crop Rect if Crop mode toggled
    if (selectedImage.cropMode && selectedImage.cropBboxVp) {
      const [cx, cy, cw, ch] = selectedImage.cropBboxVp;
      const cropEl = createEl("div", {
        position: "absolute",
        left: `${(cx - bx) * sc}px`,
        top: `${(cy - by) * sc}px`,
        width: `${Math.max(8, cw * sc)}px`,
        height: `${Math.max(8, ch * sc)}px`,
        border: "2px dashed #f59e0b",
        backgroundColor: "rgba(245, 158, 11, 0.14)",
        boxSizing: "border-box",
        cursor: "move",
      });
      cropEl.addEventListener("pointerdown", (ev) => {
        if (ev.target !== cropEl) return;
        ev.stopPropagation();
        const sx = ev.clientX,
          sy = ev.clientY;
        const orig = [...selectedImage.cropBboxVp];
        const onMv = (e2) => {
          const dx = (e2.clientX - sx) / curScale();
          const dy = (e2.clientY - sy) / curScale();
          selectedImage.cropBboxVp = [
            Math.max(bx, Math.min(bx + bw - orig[2], orig[0] + dx)),
            Math.max(by, Math.min(by + bh - orig[3], orig[1] + dy)),
            orig[2],
            orig[3],
          ];
          renderShapeOverlays();
        };
        const onUp = () => {
          window.removeEventListener("pointermove", onMv);
          window.removeEventListener("pointerup", onUp);
        };
        window.addEventListener("pointermove", onMv);
        window.addEventListener("pointerup", onUp);
      });
      const cropSe = createEl("div", {
        position: "absolute",
        right: "-5px",
        bottom: "-5px",
        width: "10px",
        height: "10px",
        backgroundColor: "#f59e0b",
        border: "1.5px solid #fff",
        cursor: "se-resize",
      });
      cropSe.addEventListener("pointerdown", (ev) => {
        ev.stopPropagation();
        const sx = ev.clientX,
          sy = ev.clientY;
        const orig = [...selectedImage.cropBboxVp];
        const onMv = (e2) => {
          selectedImage.cropBboxVp = [
            orig[0],
            orig[1],
            Math.max(10, Math.min(bx + bw - orig[0], orig[2] + (e2.clientX - sx) / curScale())),
            Math.max(10, Math.min(by + bh - orig[1], orig[3] + (e2.clientY - sy) / curScale())),
          ];
          renderShapeOverlays();
        };
        const onUp = () => {
          window.removeEventListener("pointermove", onMv);
          window.removeEventListener("pointerup", onUp);
        };
        window.addEventListener("pointermove", onMv);
        window.addEventListener("pointerup", onUp);
      });
      cropEl.appendChild(cropSe);
      frame.appendChild(cropEl);
    }

    shapeHudLayer.appendChild(frame);

    const bar = createEl(
      "div",
      {
        position: "absolute",
        left: `${Math.max(8, Math.min((container.clientWidth || 600) - 410, bx * sc))}px`,
        top: `${Math.max(8, (by + bh) * sc + 10)}px`,
        backgroundColor: "#1e1e26",
        border: "1px solid #3f3f50",
        borderRadius: "8px",
        padding: "5px 8px",
        display: "flex",
        alignItems: "center",
        flexWrap: "wrap",
        gap: "5px",
        fontSize: "11px",
        color: "#e5e7eb",
        pointerEvents: "auto",
        boxShadow: "0 10px 24px rgba(0,0,0,0.45)",
      },
      { "data-acrobat-ui": "1" }
    );

    const mkBtn = (lbl, bg, fn) => {
      const b = createEl("button", {
        background: bg,
        color: "#fff",
        border: "none",
        borderRadius: "5px",
        padding: "3px 7px",
        cursor: "pointer",
        fontWeight: "600",
        fontSize: "11px",
      });
      b.textContent = lbl;
      b.addEventListener("click", async (ev) => {
        ev.stopPropagation();
        await fn();
      });
      return b;
    };

    bar.appendChild(document.createTextNode(`Image (${selectedImage.pixelWidth}×${selectedImage.pixelHeight}) `));
    bar.appendChild(
      mkBtn("↺ -90°", "#334155", () => {
        selectedImage.rotationDeg = ((selectedImage.rotationDeg || 0) - 90) % 360;
        selectedImage.pendingAction = "rotate";
        renderShapeOverlays();
      })
    );
    bar.appendChild(
      mkBtn("↻ +90°", "#334155", () => {
        selectedImage.rotationDeg = ((selectedImage.rotationDeg || 0) + 90) % 360;
        selectedImage.pendingAction = "rotate";
        renderShapeOverlays();
      })
    );
    bar.appendChild(
      mkBtn(selectedImage.cropMode ? "Uncrop" : "Crop", selectedImage.cropMode ? "#d97706" : "#334155", () => {
        selectedImage.cropMode = !selectedImage.cropMode;
        if (selectedImage.cropMode && !selectedImage.cropBboxVp) {
          const [x, y, w, h] = selectedImage.bboxVp;
          selectedImage.cropBboxVp = [x + w * 0.1, y + h * 0.1, w * 0.8, h * 0.8];
        }
        renderShapeOverlays();
      })
    );
    bar.appendChild(
      mkBtn("Replace…", "#0284c7", () => {
        const inp = document.createElement("input");
        inp.type = "file";
        inp.accept = "image/png,image/jpeg,image/webp,image/*";
        inp.onchange = async () => {
          const f = inp.files && inp.files[0];
          if (!f) return;
          const reader = new FileReader();
          reader.onload = async () => {
            const payload = await dataUrlToImagePayload(reader.result);
            await commitSelectedImage("replace", payload);
          };
          reader.readAsDataURL(f);
        };
        inp.click();
      })
    );
    bar.appendChild(
      mkBtn("Apply", "#10b981", () => {
        const act = selectedImage.cropMode ? "crop" : selectedImage.pendingAction || "move";
        return commitSelectedImage(act);
      })
    );
    bar.appendChild(mkBtn("Delete", "#dc2626", () => commitSelectedImage("delete")));
    bar.appendChild(
      mkBtn("✕", "#383846", () => {
        selectedImage = null;
        renderShapeOverlays();
      })
    );
    shapeHudLayer.appendChild(bar);
  }

  function renderSelectedAnnotHud() {
    if (!selectedAnnot) return;
    const sc = curScale();
    const [bx, by, bw, bh] = selectedAnnot.bboxVp || [40, 40, 80, 40];

    const frame = createEl("div", {
      position: "absolute",
      left: `${bx * sc}px`,
      top: `${by * sc}px`,
      width: `${Math.max(18, bw * sc)}px`,
      height: `${Math.max(18, bh * sc)}px`,
      border: "1.5px solid #2f6bff",
      backgroundColor: "rgba(47, 107, 255, 0.08)",
      pointerEvents: "auto",
      cursor: "move",
      boxSizing: "border-box",
    });

    frame.addEventListener("pointerdown", (ev) => {
      if (ev.target !== frame) return;
      ev.stopPropagation();
      ev.preventDefault();
      const sx = ev.clientX,
        sy = ev.clientY;
      const orig = [...selectedAnnot.bboxVp];
      const origPts = Array.isArray(selectedAnnot.pointsVp)
        ? selectedAnnot.pointsVp.map((st) => st.map((p) => [p[0], p[1]]))
        : null;
      const origRects = Array.isArray(selectedAnnot.rectsVp)
        ? selectedAnnot.rectsVp.map((r) => [...r])
        : null;
      const onMv = (e2) => {
        const dx = (e2.clientX - sx) / curScale();
        const dy = (e2.clientY - sy) / curScale();
        selectedAnnot.bboxVp = [orig[0] + dx, orig[1] + dy, orig[2], orig[3]];
        if (origPts) {
          selectedAnnot.pointsVp = origPts.map((st) => st.map((p) => [p[0] + dx, p[1] + dy]));
        }
        if (origRects) {
          selectedAnnot.rectsVp = origRects.map((r) => [r[0] + dx, r[1] + dy, r[2], r[3]]);
        }
        renderShapeOverlays();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });

    const se = createEl("div", {
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
    se.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const sx = ev.clientX,
        sy = ev.clientY;
      const orig = [...selectedAnnot.bboxVp];
      const origPts = Array.isArray(selectedAnnot.pointsVp)
        ? selectedAnnot.pointsVp.map((st) => st.map((p) => [p[0], p[1]]))
        : null;
      const onMv = (e2) => {
        const nw = Math.max(12, orig[2] + (e2.clientX - sx) / curScale());
        const nh = Math.max(12, orig[3] + (e2.clientY - sy) / curScale());
        const fx = nw / Math.max(1, orig[2]);
        const fy = nh / Math.max(1, orig[3]);
        selectedAnnot.bboxVp = [orig[0], orig[1], nw, nh];
        if (origPts) {
          selectedAnnot.pointsVp = origPts.map((st) =>
            st.map((p) => [orig[0] + (p[0] - orig[0]) * fx, orig[1] + (p[1] - orig[1]) * fy])
          );
        }
        renderShapeOverlays();
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });
    frame.appendChild(se);
    shapeHudLayer.appendChild(frame);

    const bar = createEl(
      "div",
      {
        position: "absolute",
        left: `${Math.max(8, Math.min((container.clientWidth || 600) - 360, bx * sc))}px`,
        top: `${Math.max(8, (by + bh) * sc + 10)}px`,
        backgroundColor: "#1e1e26",
        border: "1px solid #3f3f50",
        borderRadius: "8px",
        padding: "5px 8px",
        display: "flex",
        flexWrap: "wrap",
        alignItems: "center",
        gap: "5px",
        fontSize: "11px",
        color: "#e5e7eb",
        pointerEvents: "auto",
        boxShadow: "0 10px 24px rgba(0,0,0,0.45)",
      },
      { "data-acrobat-ui": "1" }
    );

    const colInp = createEl(
      "input",
      { width: "22px", height: "22px", border: "none", background: "transparent", cursor: "pointer" },
      { type: "color", value: selectedAnnot.color || "#2f6bff", title: "Annotation Color" }
    );
    colInp.addEventListener("change", () => (selectedAnnot.color = colInp.value));

    const opInp = createEl(
      "input",
      { width: "54px", cursor: "pointer" },
      {
        type: "range",
        min: "0.1",
        max: "1",
        step: "0.05",
        value: String(selectedAnnot.opacity ?? 1),
        title: "Opacity",
      }
    );
    opInp.addEventListener("input", () => (selectedAnnot.opacity = Number(opInp.value) || 1));

    let txtInp = null;
    if (["Text", "FreeText"].includes(selectedAnnot.subtype)) {
      txtInp = createEl(
        "input",
        {
          width: "140px",
          background: "#13131a",
          border: "1px solid #3c3c4a",
          borderRadius: "4px",
          color: "#fff",
          padding: "2px 5px",
          fontSize: "11px",
        },
        { type: "text", value: selectedAnnot.contents || "", placeholder: "Comment / Text..." }
      );
      txtInp.addEventListener("input", () => (selectedAnnot.contents = txtInp.value));
    }

    const mkBtn = (lbl, bg, fn) => {
      const b = createEl("button", {
        background: bg,
        color: "#fff",
        border: "none",
        borderRadius: "5px",
        padding: "3px 7px",
        cursor: "pointer",
        fontWeight: "600",
        fontSize: "11px",
      });
      b.textContent = lbl;
      b.addEventListener("click", async (ev) => {
        ev.stopPropagation();
        await fn();
      });
      return b;
    };

    bar.appendChild(document.createTextNode(`${selectedAnnot.subtype} `));
    bar.appendChild(colInp);
    bar.appendChild(opInp);
    if (txtInp) bar.appendChild(txtInp);
    bar.appendChild(mkBtn("Save", "#2f6bff", () => commitSelectedAnnot("update")));
    bar.appendChild(mkBtn("Delete", "#dc2626", () => deleteAnnotationObject(selectedAnnot)));
    bar.appendChild(
      mkBtn("✕", "#383846", () => {
        selectedAnnot = null;
        renderShapeOverlays();
      })
    );
    shapeHudLayer.appendChild(bar);
  }

  function resolveShapeObj(shpOrId) {
    if (!shpOrId) return null;
    if (typeof shpOrId === "string") {
      return (objectsData && Array.isArray(objectsData.pathObjects) && objectsData.pathObjects.find((s) => s.id === shpOrId)) || null;
    }
    return shpOrId;
  }

  function resolveImageObj(imOrId) {
    if (!imOrId) return null;
    if (typeof imOrId === "string") {
      return (objectsData && Array.isArray(objectsData.imageObjects) && objectsData.imageObjects.find((im) => im.id === imOrId)) || null;
    }
    return imOrId;
  }

  function resolveAnnotObj(aOrId) {
    if (!aOrId) return null;
    if (typeof aOrId === "string") {
      return (Array.isArray(annotsData) && annotsData.find((a) => a.id === aOrId)) || null;
    }
    return aOrId;
  }

  function selectShapeObject(shpOrId) {
    const shp = resolveShapeObj(shpOrId);
    if (!shp || !Array.isArray(shp.bboxVp)) return;
    selectedImage = null;
    selectedAnnot = null;
    selectedShape = {
      ...shp,
      origBboxVp: [...shp.bboxVp],
      origSegmentsVp: shp.segmentsVp,
      rotationDeg: 0,
    };
    renderShapeOverlays();
    if (window.__pdfforgeMotion) {
      window.__pdfforgeMotion.animateEnter(shapeHudLayer, { duration: 65 });
    }
  }

  function selectImageObject(imOrId) {
    const im = resolveImageObj(imOrId);
    if (!im || !Array.isArray(im.bboxVp)) return;
    selectedShape = null;
    selectedAnnot = null;
    selectedImage = {
      ...im,
      origBboxVp: [...im.bboxVp],
      rotationDeg: 0,
      cropMode: false,
      cropBboxVp: null,
      pendingAction: "move",
    };
    renderShapeOverlays();
    if (window.__pdfforgeMotion) {
      window.__pdfforgeMotion.animateEnter(shapeHudLayer, { duration: 65 });
    }
  }

  function selectAnnotationObject(aOrId) {
    const a = resolveAnnotObj(aOrId);
    if (!a) return;
    selectedShape = null;
    selectedImage = null;
    selectedAnnot = JSON.parse(JSON.stringify(a));
    renderShapeOverlays();
    if (window.__pdfforgeMotion) {
      window.__pdfforgeMotion.animateEnter(shapeHudLayer, { duration: 65 });
    }
  }

  function mapAnnotSubtypeToKind(a) {
    const s = (a.subtype || "").toLowerCase();
    if (s === "text") return "note";
    if (s === "strikeout") return "strikethrough";
    if (s === "line") return a.lineEndArrow ? "arrow" : "line";
    if (s === "polygon") return a.cloudy ? "cloud" : "polygon";
    return s || "ink";
  }

  async function commitSelectedAnnot(action) {
    if (!selectedAnnot) return;
    const res = await window.pdfAcrobat.commitAnnotation({
      filePath,
      page,
      action,
      id: selectedAnnot.id,
      kind: mapAnnotSubtypeToKind(selectedAnnot),
      bboxVp: selectedAnnot.bboxVp,
      color: selectedAnnot.color || "#2f6bff",
      fillColor: selectedAnnot.fillColor || null,
      opacity: selectedAnnot.opacity ?? 1,
      strokeWidth: selectedAnnot.strokeWidth ?? 2,
      isHighlighter: !!selectedAnnot.isHighlighter,
      contents: selectedAnnot.contents || "",
      author: selectedAnnot.author || "PDFForge Studio",
      pointsVp: selectedAnnot.pointsVp || null,
      rectsVp: selectedAnnot.rectsVp || null,
      fontSize: selectedAnnot.fontSize || 12,
      fontFamily: selectedAnnot.fontFamily || "Helvetica",
      align: selectedAnnot.align || "left",
      stampName: selectedAnnot.stampName || null,
      url: selectedAnnot.url || null,
      targetPage: selectedAnnot.targetPage || null,
    });
    if (res && res.ok) {
      bumpRevision(filePath, res.revision, page);
      notifyMutation(res);
      selectedAnnot = null;
      await refreshAll();
      onRefreshPdf && onRefreshPdf();
      onToast && onToast("Updated PDF annotation", "success");
    } else {
      onToast && onToast((res && res.error) || "Annotation update failed", "error");
    }
  }

  async function deleteAnnotationObject(a) {
    if (!a || !window.pdfAcrobat) return;
    const res = await window.pdfAcrobat.commitAnnotation({
      filePath,
      page,
      action: "delete",
      id: a.id,
      kind: mapAnnotSubtypeToKind(a),
      bboxVp: a.bboxVp || [0, 0, 10, 10],
    });
    if (res && res.ok) {
      if (selectedAnnot && selectedAnnot.id === a.id) selectedAnnot = null;
      bumpRevision(filePath, res.revision, page);
      notifyMutation(res);
      await refreshAll();
      onRefreshPdf && onRefreshPdf();
      onToast && onToast("Erased PDF annotation", "info");
    }
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
      notifyMutation(res);
      selectedShape = null;
      await refreshAll();
      onRefreshPdf && onRefreshPdf();
      onToast && onToast(action === "delete" ? "Removed PDF vector shape" : "Updated PDF vector shape", "success");
    } else {
      onToast && onToast((res && res.error) || "Shape update failed", "error");
    }
  }

  async function commitSelectedImage(action, extraPayload = {}) {
    if (!selectedImage || !window.pdfAcrobat) return;
    const payload = {
      filePath,
      page,
      action,
      opIndex: selectedImage.opIndex,
      resourceName: selectedImage.name,
      bboxVp: selectedImage.bboxVp,
      cropBboxVp: action === "crop" ? selectedImage.cropBboxVp : null,
      rotationDeg: selectedImage.rotationDeg || 0,
      ...extraPayload,
    };
    const res = await window.pdfAcrobat.commitImageEdit(payload);
    if (res && res.ok) {
      bumpRevision(filePath, res.revision, page);
      notifyMutation(res);
      selectedImage = null;
      await refreshAll();
      onRefreshPdf && onRefreshPdf();
      onToast && onToast(`Image ${action} applied to PDF`, "success");
    } else {
      onToast && onToast((res && res.error) || "Image update failed", "error");
    }
  }

  async function openEditTextBox(block, isNewBox = false) {
    if (activeTextEditor) activeTextEditor();
    editTextLayer.innerHTML = "";
    const sc = curScale();

    const cleanFamily = normalizeFontName(block.fontName || (getFontFamily && getFontFamily()) || "Noto Sans");
    const state = {
      text: isNewBox ? "" : block.text,
      origBboxVp: [...block.bbox],
      newBboxVp: [...block.bbox],
      originalFont: block.fontName || cleanFamily || "Noto Sans",
      fontFamily: cleanFamily,
      fontSize: Math.round((block.fontSize || (getFontSize && getFontSize()) || 12) * 10) / 10,
      color: block.color || (getActiveColor && getActiveColor()) || "#111827",
      bold: !!block.bold,
      italic: !!block.italic,
      align: block.alignment || "left",
      lineSpacing: Number(block.lineSpacing || 1.2),
      bullets: !!block.hasBullets,
      notice: isNewBox
        ? null
        : !block.hasEmbeddedFont
        ? `Font "${block.fontName}" is not embedded; offline substitute will be embedded on save.`
        : block.isSubsetFont
        ? `Font "${block.fontName}" is an embedded subset; missing characters use closest offline match.`
        : null,
    };

    const sysFonts = await getSystemFonts();
    const emitEditorSync = (active) => {
      try {
        window.dispatchEvent(
          new CustomEvent("pdfforge-edittext-change", {
            detail: active ? { active: true, ...state } : { active: false },
          })
        );
      } catch {}
    };

    const wrap = createEl(
      "div",
      {
        position: "absolute",
        left: `${state.newBboxVp[0] * sc}px`,
        top: `${state.newBboxVp[1] * sc}px`,
        width: `${Math.max(150, state.newBboxVp[2] * sc + 16)}px`,
        minHeight: `${Math.max(30, state.newBboxVp[3] * sc + 8)}px`,
        backgroundColor: "rgba(255,255,255,0.97)",
        border: "1.5px solid #2f6bff",
        boxShadow: "0 8px 28px rgba(0,0,0,0.35)",
        borderRadius: "3px",
        pointerEvents: "auto",
        boxSizing: "border-box",
      },
      { "data-acrobat-ui": "1", "data-acrobat-text-editor": "1" }
    );

    const ta = createEl(
      "textarea",
      {
        width: "100%",
        height: `${Math.max(28, state.newBboxVp[3] * sc + 4)}px`,
        border: "none",
        outline: "none",
        resize: "none",
        background: "transparent",
        padding: "2px 6px",
        margin: "0",
        overflow: "hidden",
        boxSizing: "border-box",
        color: state.color,
        fontFamily: `'${state.fontFamily}', 'Noto Sans', 'Noto Sans Bengali', sans-serif`,
        fontSize: `${state.fontSize * sc}px`,
        fontWeight: state.bold ? "700" : "400",
        fontStyle: state.italic ? "italic" : "normal",
        lineHeight: String(state.lineSpacing),
        textAlign: state.align === "justify" ? "left" : state.align,
        direction: block.isRtl ? "rtl" : "ltr",
      },
      { placeholder: isNewBox ? "Type new PDF text..." : "" }
    );
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
      maxWidth: "480px",
      overflow: "hidden",
      textOverflow: "ellipsis",
      display: state.notice ? "block" : "none",
    });
    if (state.notice) noticeBanner.textContent = state.notice;

    const toolbar = createEl("div", {
      position: "absolute",
      left: "0",
      top: "calc(100% + 8px)",
      backgroundColor: "#1f1f27",
      border: "1px solid #3f3f50",
      borderRadius: "8px",
      padding: "6px 8px",
      display: "flex",
      flexWrap: "wrap",
      alignItems: "center",
      gap: "5px",
      width: "490px",
      maxWidth: "90vw",
      color: "#f3f4f6",
      fontSize: "11px",
      boxShadow: "0 12px 30px rgba(0,0,0,0.5)",
      zIndex: "60",
    });
    toolbar.addEventListener("pointerdown", (ev) => ev.stopPropagation());

    const reflowPreview = async () => {
      state.text = ta.value;
      const bw = Math.max(48, (wrap.offsetWidth - 8) / curScale());
      state.newBboxVp[2] = bw;
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
          state.newBboxVp[3] = Math.max(state.fontSize * 1.4, res.totalHeight);
          const targetH = Math.max(26, state.newBboxVp[3] * curScale() + 8, ta.scrollHeight);
          ta.style.height = `${targetH}px`;
          if (res.notice) {
            noticeBanner.textContent = res.notice;
            noticeBanner.style.display = "block";
          } else {
            noticeBanner.style.display = "none";
          }
        }
      } catch {}
      emitEditorSync(true);
    };

    const applyBulletFormatting = (enableBullets) => {
      const lines = ta.value.split("\n");
      const next = enableBullets
        ? lines.map((ln) => {
            const tr = ln.trimStart();
            if (!tr) return ln;
            return tr.startsWith("• ") ? ln : `• ${tr}`;
          })
        : lines.map((ln) => ln.replace(/^\s*•\s+/, ""));
      ta.value = next.join("\n");
      state.text = ta.value;
    };

    const syncToolbarVisuals = () => {
      const matched = sysFonts.find((f) => f.name.toLowerCase() === String(state.fontFamily).toLowerCase());
      fontPicker._setDisplay(state.fontFamily, matched ? matched.cssFamily : `'${state.fontFamily}', sans-serif`);
      ta.style.fontFamily = (matched && matched.cssFamily) || `'${state.fontFamily}', 'Noto Sans', 'Noto Sans Bengali', sans-serif`;
      ta.style.fontSize = `${state.fontSize * curScale()}px`;
      ta.style.color = state.color;
      ta.style.fontWeight = state.bold ? "700" : "400";
      ta.style.fontStyle = state.italic ? "italic" : "normal";
      ta.style.lineHeight = String(state.lineSpacing);
      ta.style.textAlign = state.align === "justify" ? "left" : state.align;
      sizeInp.value = String(state.fontSize);
      colInp.value = /^#[0-9a-f]{6}$/i.test(state.color) ? state.color : "#111827";
      bBtn.style.background = state.bold ? "#2f6bff" : "#2c2c38";
      iBtn.style.background = state.italic ? "#2f6bff" : "#2c2c38";
      bulBtn.style.background = state.bullets ? "#2f6bff" : "#2c2c38";
      lsSel.value = String(state.lineSpacing);
      for (const [k, btn] of Object.entries(alignBtns)) {
        btn.style.background = state.align === k ? "#2f6bff" : "#2c2c38";
      }
    };

    const fontPicker = createSearchableFontPicker(state.fontFamily, sysFonts, (picked) => {
      state.fontFamily = picked.name;
      ta.style.fontFamily = picked.cssFamily || `'${picked.name}', sans-serif`;
      reflowPreview();
    });

    const sizeInp = createEl(
      "input",
      { width: "48px", background: "#13131a", color: "#fff", border: "1px solid #3b3b49", borderRadius: "4px", padding: "2px 4px", fontSize: "11px" },
      { type: "number", min: "6", max: "96", step: "0.5", value: String(state.fontSize), title: "Font Size (pt)" }
    );
    const colInp = createEl(
      "input",
      { width: "22px", height: "22px", border: "none", background: "transparent", cursor: "pointer", padding: "0" },
      { type: "color", value: /^#[0-9a-f]{6}$/i.test(state.color) ? state.color : "#111827", title: "Text Color" }
    );

    const mkTog = (lbl, active, title, onClk) => {
      const b = createEl(
        "button",
        {
          background: active ? "#2f6bff" : "#2c2c38",
          color: "#fff",
          border: "1px solid rgba(255,255,255,0.12)",
          borderRadius: "4px",
          padding: "2px 7px",
          cursor: "pointer",
          fontSize: "11px",
          fontWeight: "600",
        },
        { type: "button", title }
      );
      b.textContent = lbl;
      b.addEventListener("mousedown", (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
      });
      b.addEventListener("click", (ev) => {
        ev.stopPropagation();
        onClk(ev);
      });
      return b;
    };

    const onSizeUpdate = () => {
      const next = Math.max(6, Math.min(144, Number(sizeInp.value) || 12));
      state.fontSize = next;
      ta.style.fontSize = `${next * curScale()}px`;
      reflowPreview();
    };
    sizeInp.addEventListener("input", onSizeUpdate);
    sizeInp.addEventListener("change", onSizeUpdate);

    const onColorUpdate = () => {
      state.color = colInp.value;
      ta.style.color = state.color;
      emitEditorSync(true);
    };
    colInp.addEventListener("input", onColorUpdate);
    colInp.addEventListener("change", onColorUpdate);
    ta.addEventListener("input", reflowPreview);

    const bBtn = mkTog("B", state.bold, "Bold", () => {
      state.bold = !state.bold;
      syncToolbarVisuals();
      reflowPreview();
    });
    const iBtn = mkTog("I", state.italic, "Italic", () => {
      state.italic = !state.italic;
      syncToolbarVisuals();
      reflowPreview();
    });
    iBtn.style.fontStyle = "italic";

    const bulBtn = mkTog("•", state.bullets, "Toggle Bullets", () => {
      state.bullets = !state.bullets;
      applyBulletFormatting(state.bullets);
      syncToolbarVisuals();
      reflowPreview();
    });

    const alignBtns = {};
    for (const [k, sym, tip] of [
      ["left", "≡L", "Align Left"],
      ["center", "≡C", "Align Center"],
      ["right", "≡R", "Align Right"],
      ["justify", "≡J", "Justify"],
    ]) {
      alignBtns[k] = mkTog(sym, state.align === k, tip, () => {
        state.align = k;
        syncToolbarVisuals();
        reflowPreview();
      });
    }

    const lsSel = createEl(
      "select",
      {
        background: "#13131a",
        color: "#fff",
        border: "1px solid #3b3b49",
        borderRadius: "4px",
        padding: "2px 4px",
        fontSize: "10.5px",
        cursor: "pointer",
      },
      { title: "Line Spacing" }
    );
    for (const lv of [1.0, 1.15, 1.2, 1.35, 1.5, 2.0]) {
      const opt = document.createElement("option");
      opt.value = String(lv);
      opt.textContent = `${lv}×`;
      if (Math.abs(state.lineSpacing - lv) < 0.03) opt.selected = true;
      lsSel.appendChild(opt);
    }
    lsSel.addEventListener("change", () => {
      state.lineSpacing = Number(lsSel.value) || 1.2;
      ta.style.lineHeight = String(state.lineSpacing);
      reflowPreview();
    });

    const moveGrip = createEl("div", {
      position: "absolute",
      top: "-11px",
      right: "8px",
      background: "#2f6bff",
      color: "#fff",
      fontSize: "9px",
      padding: "0 6px",
      borderRadius: "4px",
      cursor: "move",
      userSelect: "none",
    });
    moveGrip.textContent = "Drag";
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

    const seResize = createEl(
      "div",
      {
        position: "absolute",
        right: "-5px",
        bottom: "-5px",
        width: "10px",
        height: "10px",
        backgroundColor: "#2f6bff",
        border: "1.5px solid #fff",
        borderRadius: "2px",
        cursor: "se-resize",
      },
      { title: "Drag to Resize Text Box" }
    );
    seResize.addEventListener("pointerdown", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const sx = ev.clientX;
      const sy = ev.clientY;
      const origW = state.newBboxVp[2];
      const origH = state.newBboxVp[3];
      const onMv = (e2) => {
        const nw = Math.max(60, origW + (e2.clientX - sx) / curScale());
        const nh = Math.max(24, origH + (e2.clientY - sy) / curScale());
        state.newBboxVp[2] = nw;
        state.newBboxVp[3] = nh;
        wrap.style.width = `${nw * curScale() + 16}px`;
        ta.style.height = `${nh * curScale() + 4}px`;
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
        reflowPreview();
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
    });

    const closeEditor = () => {
      editTextLayer.innerHTML = "";
      activeTextEditor = null;
      if (window.__acrobatActiveTextEditor && window.__acrobatActiveTextEditor._closeFn === closeEditor) {
        window.__acrobatActiveTextEditor = null;
      }
      emitEditorSync(false);
    };
    activeTextEditor = closeEditor;

    const doDelete = async () => {
      if (isNewBox) {
        closeEditor();
        return;
      }
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
        notifyMutation(res);
        await refreshAll();
        onRefreshPdf && onRefreshPdf();
        onToast && onToast("Removed text block from PDF", "info");
      }
    };

    const doApply = async () => {
      state.text = ta.value;
      if (isNewBox && !state.text.trim()) {
        closeEditor();
        return;
      }
      const res = await window.pdfAcrobat.commitEditText({
        filePath,
        page,
        origBboxVp: isNewBox ? [0, 0, 0, 0] : state.origBboxVp,
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
        notifyMutation(res);
        await refreshAll();
        onRefreshPdf && onRefreshPdf();
        onToast && onToast(res.notice || "Text block committed to PDF stream", "success");
      } else {
        onToast && onToast((res && res.error) || "Failed editing text block", "error");
      }
    };

    const applyBtn = mkTog("Apply", true, "Commit changes to PDF (Ctrl+Enter)", doApply);
    const delBtn = mkTog("Delete", false, "Remove text block from PDF", doDelete);
    delBtn.style.background = "#dc2626";
    const cancelBtn = mkTog("Cancel", false, "Discard edit (Esc)", closeEditor);

    ta.addEventListener("keydown", (ev) => {
      if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
        ev.preventDefault();
        doApply();
      }
    });

    window.__acrobatActiveTextEditor = {
      _closeFn: closeEditor,
      getState: () => ({ active: true, ...state }),
      updateStyle: (patch = {}) => {
        if (patch.fontFamily !== undefined) state.fontFamily = normalizeFontName(patch.fontFamily);
        if (patch.fontSize !== undefined) state.fontSize = Math.max(6, Math.min(144, Number(patch.fontSize) || 12));
        if (patch.color !== undefined) state.color = patch.color;
        if (patch.bold !== undefined) state.bold = !!patch.bold;
        if (patch.italic !== undefined) state.italic = !!patch.italic;
        if (patch.align !== undefined) state.align = patch.align;
        if (patch.lineSpacing !== undefined) state.lineSpacing = Number(patch.lineSpacing) || 1.2;
        if (patch.bullets !== undefined) {
          const nextB = !!patch.bullets;
          if (nextB !== state.bullets) {
            state.bullets = nextB;
            applyBulletFormatting(nextB);
          }
        }
        syncToolbarVisuals();
        reflowPreview();
      },
      apply: doApply,
      delete: doDelete,
      cancel: closeEditor,
    };

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
          window.__acrobatActiveTextEditor.updateStyle({ fontFamily: f.name });
        },
      }));
      const quickSizes = [9, 10, 11, 12, 14, 16, 18, 24, 32].map((sz) => ({
        label: `${sz} pt`,
        checked: Math.round(state.fontSize) === sz,
        action: () => {
          window.__acrobatActiveTextEditor.updateStyle({ fontSize: sz });
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
        { label: "Bold", icon: "B", checked: state.bold, action: () => bBtn.click() },
        { label: "Italic", icon: "I", checked: state.italic, action: () => iBtn.click() },
        { label: "Bullets", icon: "•", checked: state.bullets, action: () => bulBtn.click() },
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
    toolbar.appendChild(alignBtns.left);
    toolbar.appendChild(alignBtns.center);
    toolbar.appendChild(alignBtns.right);
    toolbar.appendChild(alignBtns.justify);
    toolbar.appendChild(lsSel);
    toolbar.appendChild(applyBtn);
    toolbar.appendChild(delBtn);
    toolbar.appendChild(cancelBtn);

    wrap.appendChild(noticeBanner);
    wrap.appendChild(moveGrip);
    wrap.appendChild(seResize);
    wrap.appendChild(ta);
    wrap.appendChild(toolbar);
    editTextLayer.appendChild(wrap);
    if (window.__pdfforgeMotion) {
      window.__pdfforgeMotion.animateEnter(wrap, { duration: 65 });
    }
    syncToolbarVisuals();
    emitEditorSync(true);
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

  async function finalizePolygonTool() {
    if (polyPoints.length < 2) {
      polyPoints = [];
      clearInkCanvas();
      return;
    }
    const shapeType = (getActiveShape && getActiveShape()) || "polygon";
    const color = (getActiveColor && getActiveColor()) || "#2f6bff";
    const lw = (getStrokeWidth && getStrokeWidth()) || 2;
    const opacity = (getOpacity && getOpacity()) ?? 1;
    const xs = polyPoints.map((p) => p[0]);
    const ys = polyPoints.map((p) => p[1]);
    const bx = Math.min(...xs),
      by = Math.min(...ys);
    const bw = Math.max(6, Math.max(...xs) - bx);
    const bh = Math.max(6, Math.max(...ys) - by);
    const kind = shapeType === "polyline" ? "polyline" : shapeType === "cloud" ? "cloud" : "polygon";
    const pts = [...polyPoints];
    polyPoints = [];
    clearInkCanvas();
    const shapeStyle = (getShapeStyle && getShapeStyle()) || "outline";
    const isHiShape = shapeStyle === "highlight";
    const isFilledShape = shapeStyle === "filled";
    const res = await window.pdfAcrobat.commitAnnotation({
      filePath,
      page,
      action: "add",
      id: `annot-${Date.now().toString(36)}`,
      kind,
      bboxVp: [bx, by, bw, bh],
      color,
      fillColor: kind === "polyline" ? null : isHiShape || isFilledShape ? color : null,
      opacity: kind === "polyline" ? opacity : isHiShape ? Math.min(opacity, 0.36) : isFilledShape ? Math.min(opacity, 0.28) : opacity,
      isHighlighter: isHiShape,
      strokeWidth: lw,
      pointsVp: [pts],
    });
    if (res && res.ok) {
      bumpRevision(filePath, res.revision, page);
      notifyMutation(res);
      await refreshAll();
      onRefreshPdf && onRefreshPdf();
      onToast && onToast(`Saved PDF ${kind} annotation`, "success");
    }
  }

  hitLayer.addEventListener("dblclick", async (ev) => {
    const tool = getTool();
    if (isShapeAnnotTool(tool) && polyPoints.length >= 2) {
      ev.preventDefault();
      ev.stopPropagation();
      await finalizePolygonTool();
      return;
    }
    if (isEditTool(tool)) {
      const [px, py] = clientToPdf(ev.clientX, ev.clientY);
      const hit = await window.pdfAcrobat.hitTest(filePath, page, px, py).catch(() => null);
      if (!hit || !hit.blockId) {
        ev.preventDefault();
        ev.stopPropagation();
        openEditTextBox(
          {
            text: "",
            bbox: [px, py, 200, 32],
            fontName: (getFontFamily && getFontFamily()) || "Noto Sans",
            fontSize: (getFontSize && getFontSize()) || 12,
            color: "#111827",
          },
          true
        );
      }
    }
  });

  hitLayer.addEventListener("pointerdown", async (ev) => {
    if (ev.button !== 0) return;
    const tool = getTool();
    const [px, py] = clientToPdf(ev.clientX, ev.clientY);

    // A) Freehand Pen / Ink Highlighter with Catmull-Rom Canvas Preview -> Real PDF /Ink + /AP /N
    if (isInkTool(tool)) {
      ev.preventDefault();
      ev.stopPropagation();
      const ctx = syncCanvasSize();
      const sc = curScale();
      const rc = container.getBoundingClientRect();
      const color = (getActiveColor && getActiveColor()) || "#2f6bff";
      const lw = (getStrokeWidth && getStrokeWidth()) || 2.5;
      const isHL = tool === "highlighter_ink" || !!(getDrawAsHighlighter && getDrawAsHighlighter());
      const opacity = isHL ? Math.min(0.45, (getOpacity && getOpacity()) ?? 0.4) : (getOpacity && getOpacity()) ?? 1;
      const hasPressure = ev.pointerType === "pen";
      const ptsVp = [[px, py]];
      const ptsScreen = [{ x: ev.clientX - rc.left, y: ev.clientY - rc.top, p: ev.pressure || 0.7 }];
      try {
        hitLayer.setPointerCapture(ev.pointerId);
      } catch {}

      const redraw = () => {
        clearInkCanvas();
        if (!ctx) return;
        ctx.save();
        ctx.strokeStyle = color;
        ctx.fillStyle = color;
        ctx.globalAlpha = opacity;
        ctx.globalCompositeOperation = isHL ? "multiply" : "source-over";
        catmullRomScreenPath(ctx, ptsScreen, (isHL ? Math.max(lw, 12) : lw) * sc, hasPressure);
        ctx.restore();
      };
      redraw();

      const onMv = (e2) => {
        const events = typeof e2.getCoalescedEvents === "function" ? e2.getCoalescedEvents() : [e2];
        for (const ce of events) {
          const sx = ce.clientX - rc.left;
          const sy = ce.clientY - rc.top;
          const last = ptsScreen[ptsScreen.length - 1];
          if (Math.hypot(sx - last.x, sy - last.y) < 1.5) continue;
          ptsScreen.push({ x: sx, y: sy, p: ce.pressure || 0.7 });
          ptsVp.push([sx / sc, sy / sc]);
        }
        redraw();
      };

      const onUp = async () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
        try {
          hitLayer.releasePointerCapture(ev.pointerId);
        } catch {}
        clearInkCanvas();
        if (ptsVp.length < 2) {
          ptsVp.push([ptsVp[0][0] + 0.5, ptsVp[0][1] + 0.5]);
        }
        const xs = ptsVp.map((p) => p[0]);
        const ys = ptsVp.map((p) => p[1]);
        const strokeW = isHL ? Math.max(lw, 12) : lw;
        const pad = strokeW * 0.75 + 2;
        const bx = Math.min(...xs) - pad;
        const by = Math.min(...ys) - pad;
        const bw = Math.max(4, Math.max(...xs) - bx + pad);
        const bh = Math.max(4, Math.max(...ys) - by + pad);

        const res = await window.pdfAcrobat.commitAnnotation({
          filePath,
          page,
          action: "add",
          id: `ink-${Date.now().toString(36)}`,
          kind: "ink",
          bboxVp: [bx, by, bw, bh],
          color,
          opacity,
          strokeWidth: strokeW,
          isHighlighter: isHL,
          contents: isHL ? "Highlighter Ink" : "Ink Stroke",
          pointsVp: [ptsVp],
        });
        if (res && res.ok) {
          bumpRevision(filePath, res.revision, page);
          notifyMutation(res);
          await refreshAll();
          onRefreshPdf && onRefreshPdf();
        } else {
          onToast && onToast((res && res.error) || "Failed saving PDF ink stroke", "error");
        }
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
      return;
    }

    // B) Eraser Tool (click or sweep over Ink / Shape annotations or vector paths)
    if (tool === "eraser") {
      ev.preventDefault();
      ev.stopPropagation();
      const eraseAt = async (vx, vy) => {
        const hitAnnot = (annotsData || [])
          .slice()
          .reverse()
          .find((a) => {
            if (!a || a.subtype === "Widget") return false;
            const [ax, ay, aw, ah] = a.bboxVp || [0, 0, 0, 0];
            return vx >= ax - 6 && vx <= ax + aw + 6 && vy >= ay - 6 && vy <= ay + ah + 6;
          });
        if (hitAnnot) {
          await deleteAnnotationObject(hitAnnot);
          return true;
        }
        return false;
      };
      await eraseAt(px, py);
      const onMv = async (e2) => {
        const [nx, ny] = clientToPdf(e2.clientX, e2.clientY);
        await eraseAt(nx, ny);
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
      return;
    }

    // C) PDF Annotation Shapes (`tool === "shape"`) + Vector Path Tools (`vec_*`)
    if (isShapeAnnotTool(tool) || isVecTool(tool)) {
      ev.preventDefault();
      ev.stopPropagation();
      const activeShp = isShapeAnnotTool(tool) ? (getActiveShape && getActiveShape()) || "rect" : tool.replace("vec_", "");
      const color = (getActiveColor && getActiveColor()) || "#2f6bff";
      const lw = (getStrokeWidth && getStrokeWidth()) || 2;
      const opacity = (getOpacity && getOpacity()) ?? 1;

      if (activeShp === "polygon" || activeShp === "polyline" || activeShp === "cloud") {
        polyPoints.push([px, py]);
        const ctx = syncCanvasSize();
        const sc = curScale();
        clearInkCanvas();
        if (ctx && polyPoints.length >= 1) {
          ctx.save();
          ctx.strokeStyle = color;
          ctx.lineWidth = lw * sc;
          ctx.globalAlpha = opacity;
          ctx.beginPath();
          ctx.moveTo(polyPoints[0][0] * sc, polyPoints[0][1] * sc);
          for (let i = 1; i < polyPoints.length; i++) {
            ctx.lineTo(polyPoints[i][0] * sc, polyPoints[i][1] * sc);
          }
          ctx.stroke();
          for (const pt of polyPoints) {
            ctx.fillStyle = "#2f6bff";
            ctx.beginPath();
            ctx.arc(pt[0] * sc, pt[1] * sc, 4, 0, Math.PI * 2);
            ctx.fill();
          }
          ctx.restore();
        }
        return;
      }

      const startPt = [px, py];
      const ctx = syncCanvasSize();
      const sc = curScale();
      const drawPreview = (nx, ny) => {
        clearInkCanvas();
        if (!ctx) return;
        ctx.save();
        ctx.strokeStyle = color;
        ctx.fillStyle = color;
        ctx.lineWidth = lw * sc;
        ctx.globalAlpha = opacity;
        const x0 = startPt[0] * sc,
          y0 = startPt[1] * sc,
          x1 = nx * sc,
          y1 = ny * sc;
        if (activeShp === "line" || activeShp === "arrow") {
          ctx.beginPath();
          ctx.moveTo(x0, y0);
          ctx.lineTo(x1, y1);
          ctx.stroke();
          if (activeShp === "arrow") {
            const ang = Math.atan2(y1 - y0, x1 - x0);
            const hl = Math.max(8, lw * sc * 3);
            ctx.beginPath();
            ctx.moveTo(x1, y1);
            ctx.lineTo(x1 - hl * Math.cos(ang - 0.45), y1 - hl * Math.sin(ang - 0.45));
            ctx.lineTo(x1 - hl * Math.cos(ang + 0.45), y1 - hl * Math.sin(ang + 0.45));
            ctx.closePath();
            ctx.fill();
          }
        } else if (activeShp === "rect" || activeShp === "rectangle" || activeShp === "square") {
          ctx.strokeRect(Math.min(x0, x1), Math.min(y0, y1), Math.abs(x1 - x0), Math.abs(y1 - y0));
        } else if (activeShp === "circle" || activeShp === "ellipse") {
          ctx.beginPath();
          ctx.ellipse((x0 + x1) / 2, (y0 + y1) / 2, Math.abs(x1 - x0) / 2, Math.abs(y1 - y0) / 2, 0, 0, Math.PI * 2);
          ctx.stroke();
        }
        ctx.restore();
      };

      const onMv = (e2) => {
        const [nx, ny] = clientToPdf(e2.clientX, e2.clientY);
        drawPreview(nx, ny);
      };
      const onUp = async (e2) => {
        window.removeEventListener("pointermove", onMv);
        window.removeEventListener("pointerup", onUp);
        clearInkCanvas();
        const [ex, ey] = clientToPdf(e2.clientX, e2.clientY);
        const bx = Math.min(startPt[0], ex),
          by = Math.min(startPt[1], ey);
        const bw = Math.max(4, Math.abs(ex - startPt[0])),
          bh = Math.max(4, Math.abs(ey - startPt[1]));
        if (bw < 5 && bh < 5) return;

        if (isVecTool(tool)) {
          const shapeKind = tool.replace("vec_", "");
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
            pointsVp: [startPt, [ex, ey]],
            rotationDeg: 0,
            fillColor: shapeKind === "rectangle" || shapeKind === "ellipse" ? color : null,
            strokeColor: color,
            fillOpacity: shapeKind === "rectangle" || shapeKind === "ellipse" ? Math.min(opacity, 0.25) : opacity,
            strokeOpacity: opacity,
            strokeWidth: lw,
          });
          if (res && res.ok) {
            bumpRevision(filePath, res.revision, page);
            notifyMutation(res);
            await refreshAll();
            onRefreshPdf && onRefreshPdf();
            onToast && onToast(`Added PDF ${shapeKind} shape`, "success");
          }
          return;
        }

        const kind =
          activeShp === "rect" || activeShp === "rectangle" || activeShp === "square"
            ? "square"
            : activeShp === "circle" || activeShp === "ellipse"
            ? "circle"
            : activeShp === "arrow"
            ? "arrow"
            : "line";
        const shapeStyle = (getShapeStyle && getShapeStyle()) || "outline";
        const isHiShape = shapeStyle === "highlight";
        const isFilledShape = shapeStyle === "filled";
        const canFill = kind === "square" || kind === "circle";
        const res = await window.pdfAcrobat.commitAnnotation({
          filePath,
          page,
          action: "add",
          id: `annot-${Date.now().toString(36)}`,
          kind,
          bboxVp: [bx, by, bw, bh],
          color,
          fillColor: canFill && (isHiShape || isFilledShape) ? color : null,
          opacity: isHiShape ? Math.min(opacity, 0.38) : isFilledShape ? Math.min(opacity, 0.28) : opacity,
          isHighlighter: isHiShape,
          strokeWidth: lw,
          pointsVp: [[startPt, [ex, ey]]],
        });
        if (res && res.ok) {
          bumpRevision(filePath, res.revision, page);
          notifyMutation(res);
          await refreshAll();
          onRefreshPdf && onRefreshPdf();
          onToast && onToast(`Saved PDF ${kind} annotation`, "success");
        }
      };
      window.addEventListener("pointermove", onMv);
      window.addEventListener("pointerup", onUp);
      return;
    }

    // D) Add New Real PDF Text Box
    if (tool === "addTextBox") {
      ev.preventDefault();
      ev.stopPropagation();
      openEditTextBox(
        {
          text: "",
          bbox: [px, py, 200, 32],
          fontName: (getFontFamily && getFontFamily()) || "Noto Sans",
          fontSize: (getFontSize && getFontSize()) || 12,
          color: (getActiveColor && getActiveColor()) || "#111827",
        },
        true
      );
      return;
    }

    // E) Unified Edit PDF Mode (Text Blocks, Images, Vector Shapes, Annotations)
    if (isEditTool(tool)) {
      ev.preventDefault();
      ev.stopPropagation();
      selectedShape = null;
      selectedImage = null;
      selectedAnnot = null;
      const hit = await window.pdfAcrobat.hitTest(filePath, page, px, py).catch(() => null);
      if (hit && hit.blockId && layoutData) {
        const blk = (layoutData.blocks || []).find((b) => b.id === hit.blockId);
        if (blk) {
          openEditTextBox(blk);
          return;
        }
      }
      if (objectsData && Array.isArray(objectsData.imageObjects)) {
        const im = objectsData.imageObjects
          .slice()
          .reverse()
          .find((i) => px >= i.bboxVp[0] && px <= i.bboxVp[0] + i.bboxVp[2] && py >= i.bboxVp[1] && py <= i.bboxVp[1] + i.bboxVp[3]);
        if (im) {
          selectImageObject(im);
          return;
        }
      }
      if (hit && hit.shapeId && objectsData) {
        const shp = (objectsData.pathObjects || []).find((s) => s.id === hit.shapeId);
        if (shp) {
          selectShapeObject(shp);
          return;
        }
      }
      renderShapeOverlays();
      return;
    }

    // F) Pure Acrobat Reading-Order Selection + Direct Highlight/Underline/Strikethrough Tool
    ev.preventDefault();
    ev.stopPropagation();
    const isMarkup = isTextMarkupTool(tool);
    const clickDetail = ev.detail || 1;
    const hit = await window.pdfAcrobat.hitTest(filePath, page, px, py).catch(() => null);
    if (!hit || hit.charIndex === null || hit.charIndex === undefined) {
      clearSelection(filePath);
      if (tool === "highlight") {
        // Area highlight fallback for scanned / non-text regions
        const startPt = [px, py];
        const ctx = syncCanvasSize();
        const sc = curScale();
        const hlCol = getSavedHighlightColor() || (getActiveColor && getActiveColor()) || "#facc15";
        const onMvArea = (e2) => {
          const [nx, ny] = clientToPdf(e2.clientX, e2.clientY);
          clearInkCanvas();
          if (!ctx) return;
          ctx.save();
          ctx.fillStyle = hlCol;
          ctx.globalAlpha = 0.35;
          ctx.fillRect(
            Math.min(startPt[0], nx) * sc,
            Math.min(startPt[1], ny) * sc,
            Math.abs(nx - startPt[0]) * sc,
            Math.abs(ny - startPt[1]) * sc
          );
          ctx.restore();
        };
        const onUpArea = async (e2) => {
          window.removeEventListener("pointermove", onMvArea);
          window.removeEventListener("pointerup", onUpArea);
          clearInkCanvas();
          const [ex, ey] = clientToPdf(e2.clientX, e2.clientY);
          const bx = Math.min(startPt[0], ex),
            by = Math.min(startPt[1], ey);
          const bw = Math.abs(ex - startPt[0]),
            bh = Math.abs(ey - startPt[1]);
          if (bw > 8 && bh > 6) {
            api.applyMarkupBatch("highlight", [{ page, rects: [{ x: bx, y: by, width: bw, height: bh }], text: "Area Highlight" }], hlCol);
          }
        };
        window.addEventListener("pointermove", onMvArea);
        window.addEventListener("pointerup", onUpArea);
      }
      return;
    }

    if (clickDetail >= 2) {
      const rng = clickDetail === 2 ? hit.wordRange : clickDetail === 3 ? hit.lineRange : hit.blockRange;
      if (rng && rng.length === 2) {
        selectionState = await computeRangeState(filePath, page, rng[0], page, rng[1], true);
        emitSelectionChange();
        updateAllSelectionLayersForFile(filePath);
        if (isMarkup && selectionState.active) {
          await applySelectionAction(tool, tool === "highlight" ? getSavedHighlightColor() : undefined);
          clearSelection(filePath);
        }
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
      if (isMarkup && selectionState.active) {
        await applySelectionAction(tool, tool === "highlight" ? getSavedHighlightColor() : undefined);
        clearSelection(filePath);
      }
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  });

  const onKeyDown = async (ev) => {
    if (destroyed) return;
    if (ev.key === "Escape") {
      if (polyPoints.length) {
        polyPoints = [];
        clearInkCanvas();
        return;
      }
      if (activeTextEditor) {
        activeTextEditor();
        return;
      }
      if (selectedShape || selectedImage || selectedAnnot) {
        selectedShape = null;
        selectedImage = null;
        selectedAnnot = null;
        renderShapeOverlays();
        return;
      }
      if (isEditTool(getTool()) && onExitEditMode) {
        onExitEditMode();
        return;
      }
    }

    const tag = ev.target && ev.target.tagName;
    if (["INPUT", "TEXTAREA", "SELECT"].includes(tag) || (ev.target && ev.target.isContentEditable)) return;

    if (ev.key === "Enter" && polyPoints.length >= 2) {
      ev.preventDefault();
      await finalizePolygonTool();
      return;
    }

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
    } else if (
      ev.altKey &&
      !ev.ctrlKey &&
      !ev.metaKey &&
      !ev.shiftKey &&
      selectionState.active &&
      selectionState.filePath === filePath &&
      selectionState.page === page
    ) {
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
    const [lay, objs, annRes] = await Promise.all([
      getCachedLayout(filePath, page),
      getCachedObjects(filePath, page),
      window.pdfAcrobat.getAnnotations(filePath, page).catch(() => null),
    ]);
    if (destroyed) return;
    layoutData = lay;
    objectsData = objs;
    annotsData = annRes && annRes.ok && Array.isArray(annRes.annotations) ? annRes.annotations : [];
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
    renderFormWidgets();
    renderShapeOverlays();
  }

  refreshAll();

  const api = {
    getPage: () => page,
    getRect: () => container.getBoundingClientRect(),
    getLayout: () => layoutData,
    getObjects: () => objectsData,
    getAnnotations: () => annotsData,
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
      renderFormWidgets();
      renderShapeOverlays();
    },
    reloadFromRust: refreshAll,
    openNewTextBoxAt: (vx = 72, vy = 72) => {
      openEditTextBox(
        {
          text: "",
          bbox: [vx, vy, 210, 32],
          fontName: (getFontFamily && getFontFamily()) || "Noto Sans",
          fontSize: (getFontSize && getFontSize()) || 12,
          color: (getActiveColor && getActiveColor()) || "#111827",
        },
        true
      );
    },
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
      return (
        objectsData.pathObjects
          .slice()
          .reverse()
          .find(
            (s) =>
              !s.isPageBackground &&
              px >= s.bboxVp[0] - 4 &&
              px <= s.bboxVp[0] + s.bboxVp[2] + 4 &&
              py >= s.bboxVp[1] - 4 &&
              py <= s.bboxVp[1] + s.bboxVp[3] + 4
          ) || null
      );
    },
    hitImageAtClient: (clientX, clientY) => {
      if (!objectsData || !objectsData.imageObjects) return null;
      const [px, py] = clientToPdf(clientX, clientY);
      return (
        objectsData.imageObjects
          .slice()
          .reverse()
          .find((im) => px >= im.bboxVp[0] && px <= im.bboxVp[0] + im.bboxVp[2] && py >= im.bboxVp[1] && py <= im.bboxVp[1] + im.bboxVp[3]) ||
        null
      );
    },
    hitAnnotationAtClient: (clientX, clientY) => {
      const [px, py] = clientToPdf(clientX, clientY);
      return (
        (annotsData || [])
          .slice()
          .reverse()
          .find((a) => {
            if (!a || a.subtype === "Widget") return false;
            const [ax, ay, aw, ah] = a.bboxVp || [0, 0, 0, 0];
            return px >= ax - 4 && px <= ax + aw + 4 && py >= ay - 4 && py <= ay + ah + 4;
          }) || null
      );
    },
    selectAndEditShape: (shp) => {
      if (shp) selectShapeObject(shp);
    },
    selectAndEditImage: (im) => {
      if (im) selectImageObject(im);
    },
    selectAndEditAnnotation: (a) => {
      if (a) selectAnnotationObject(a);
    },
    deletePdfShape: async (shpOrId) => {
      const shp = resolveShapeObj(shpOrId);
      if (!shp || !Array.isArray(shp.bboxVp) || !window.pdfAcrobat) return;
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
        notifyMutation(res);
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
