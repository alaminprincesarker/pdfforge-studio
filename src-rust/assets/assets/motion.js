// PDFForge Studio — Shared GPU-Only Motion Helpers (Interruptible, Reduced-Motion Aware)

const EASE_OUT = "cubic-bezier(0.2, 0, 0, 1)";
const EASE_IN = "cubic-bezier(0.4, 0, 1, 1)";

const mediaQuery =
  typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-reduced-motion: reduce)")
    : null;

export function isReducedMotion() {
  if (typeof document !== "undefined" && document.body && document.body.classList.contains("reduce-animations")) {
    return true;
  }
  return !!(mediaQuery && mediaQuery.matches);
}

export function setReducedMotion(enabled) {
  if (typeof document !== "undefined" && document.body) {
    document.body.classList.toggle("reduce-animations", !!enabled);
  }
}

function cancelExisting(el) {
  if (!el || typeof el.getAnimations !== "function") return;
  try {
    const anims = el.getAnimations();
    for (const a of anims) {
      a.cancel();
    }
  } catch {}
}

export function animateEnter(el, opts = {}) {
  if (!el || typeof el.animate !== "function") return null;
  cancelExisting(el);
  const reduced = isReducedMotion();
  const duration = reduced ? 50 : opts.duration ?? 110;
  const fromTransform = reduced ? "none" : opts.fromTransform || "translate3d(0, 0, 0)";
  const toTransform = opts.toTransform || "translate3d(0, 0, 0)";

  el.style.willChange = reduced ? "opacity" : "transform, opacity";
  if (opts.origin) el.style.transformOrigin = opts.origin;

  const keyframes = reduced
    ? [{ opacity: 0 }, { opacity: 1 }]
    : [
        { opacity: 0, transform: fromTransform },
        { opacity: 1, transform: toTransform },
      ];

  const anim = el.animate(keyframes, {
    duration,
    easing: EASE_OUT,
    fill: "none",
  });

  const cleanup = () => {
    if (el) el.style.willChange = "auto";
  };
  anim.onfinish = cleanup;
  anim.oncancel = cleanup;
  return anim;
}

export function animateExit(el, opts = {}) {
  const done = typeof opts.onDone === "function" ? opts.onDone : () => {};
  if (!el || typeof el.animate !== "function") {
    done();
    return null;
  }
  cancelExisting(el);
  const reduced = isReducedMotion();
  const duration = reduced ? 50 : opts.duration ?? 80;
  const toTransform = reduced ? "none" : opts.toTransform || "translate3d(0, 0, 0)";

  el.style.willChange = reduced ? "opacity" : "transform, opacity";
  if (opts.origin) el.style.transformOrigin = opts.origin;

  const keyframes = reduced
    ? [{ opacity: 1 }, { opacity: 0 }]
    : [
        { opacity: 1, transform: "translate3d(0, 0, 0)" },
        { opacity: 0, transform: toTransform },
      ];

  let finished = false;
  const finishOnce = () => {
    if (finished) return;
    finished = true;
    if (el) el.style.willChange = "auto";
    done();
  };

  const anim = el.animate(keyframes, {
    duration,
    easing: EASE_IN,
    fill: "forwards",
  });
  anim.onfinish = finishOnce;
  anim.oncancel = finishOnce;
  setTimeout(finishOnce, duration + 40);
  return anim;
}

export function captureRects(mapObj) {
  const rects = new Map();
  if (!mapObj) return rects;
  for (const [id, el] of Object.entries(mapObj)) {
    if (el && typeof el.getBoundingClientRect === "function") {
      rects.set(id, el.getBoundingClientRect());
    }
  }
  return rects;
}

export function playFlipX(mapObj, prevRects, skipId = null, duration = 95) {
  if (!mapObj || !prevRects || isReducedMotion()) return;
  for (const [id, el] of Object.entries(mapObj)) {
    if (!el || id === skipId) continue;
    const prev = prevRects.get(id);
    if (!prev) continue;
    const next = el.getBoundingClientRect();
    const dx = prev.left - next.left;
    if (Math.abs(dx) < 1) continue;
    if (typeof el.animate === "function") {
      cancelExisting(el);
      el.style.willChange = "transform";
      const anim = el.animate(
        [{ transform: `translate3d(${dx}px, 0, 0)` }, { transform: "translate3d(0, 0, 0)" }],
        { duration, easing: EASE_OUT }
      );
      const cleanup = () => {
        el.style.willChange = "auto";
      };
      anim.onfinish = cleanup;
      anim.oncancel = cleanup;
    }
  }
}

export function animateValueFlash(el) {
  if (!el) return;
  el.classList.remove("anim-flash-value");
  void el.offsetWidth;
  el.classList.add("anim-flash-value");
  setTimeout(() => {
    el && el.classList.remove("anim-flash-value");
  }, 170);
}

export function animateInvalidShake(el) {
  if (!el) return;
  el.classList.remove("anim-shake");
  el.classList.add("anim-input-error");
  void el.offsetWidth;
  el.classList.add("anim-shake");
  setTimeout(() => {
    if (el) el.classList.remove("anim-shake");
  }, 220);
  setTimeout(() => {
    if (el) el.classList.remove("anim-input-error");
  }, 900);
}

let activeScrollCancel = null;
export function smoothScrollTo(container, targetTop, durationMs = 120) {
  if (!container) return;
  if (activeScrollCancel) {
    activeScrollCancel();
    activeScrollCancel = null;
  }
  const clamped = Math.max(0, Math.min(Math.max(0, container.scrollHeight - container.clientHeight), targetTop));
  if (isReducedMotion() || durationMs <= 0) {
    container.scrollTop = clamped;
    return;
  }
  const startTop = container.scrollTop;
  const delta = clamped - startTop;
  if (Math.abs(delta) < 2) {
    container.scrollTop = clamped;
    return;
  }
  const dur = Math.min(200, Math.max(80, durationMs));
  const t0 = performance.now();
  let rafId = 0;
  let cancelled = false;

  const stop = () => {
    cancelled = true;
    if (rafId) cancelAnimationFrame(rafId);
    container.removeEventListener("wheel", stop);
    container.removeEventListener("touchstart", stop);
    container.removeEventListener("pointerdown", stop);
    if (activeScrollCancel === stop) activeScrollCancel = null;
  };

  activeScrollCancel = stop;
  container.addEventListener("wheel", stop, { passive: true, once: true });
  container.addEventListener("touchstart", stop, { passive: true, once: true });
  container.addEventListener("pointerdown", stop, { passive: true, once: true });

  const step = (now) => {
    if (cancelled) return;
    const p = Math.min(1, (now - t0) / dur);
    const eased = 1 - Math.pow(1 - p, 3);
    container.scrollTop = startTop + delta * eased;
    if (p < 1) {
      rafId = requestAnimationFrame(step);
    } else {
      stop();
    }
  };
  rafId = requestAnimationFrame(step);
}

export function saveFocus() {
  return typeof document !== "undefined" ? document.activeElement : null;
}

export function focusDialogInitial(dialogEl) {
  if (!dialogEl) return;
  requestAnimationFrame(() => {
    try {
      const target =
        dialogEl.querySelector("[autofocus]") ||
        dialogEl.querySelector("input:not([disabled]), select:not([disabled]), textarea:not([disabled]), button:not([disabled])");
      if (target && typeof target.focus === "function") {
        target.focus({ preventScroll: true });
      } else {
        if (!dialogEl.hasAttribute("tabindex")) dialogEl.setAttribute("tabindex", "-1");
        dialogEl.focus({ preventScroll: true });
      }
    } catch {}
  });
}

export function restoreFocus(prevEl) {
  if (!prevEl || typeof prevEl.focus !== "function") return;
  requestAnimationFrame(() => {
    try {
      if (document.contains(prevEl)) {
        prevEl.focus({ preventScroll: true });
      }
    } catch {}
  });
}

export function initTooltipManager() {
  if (typeof window === "undefined" || window.__pdfforgeTooltipInit) return;
  window.__pdfforgeTooltipInit = true;

  const tip = document.createElement("div");
  tip.id = "pdfforge-tooltip";
  tip.setAttribute("role", "tooltip");
  document.body.appendChild(tip);

  let timer = null;
  let activeHost = null;

  const hideNow = () => {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    tip.classList.remove("is-visible");
    activeHost = null;
  };

  const showFor = (el) => {
    const text = el.getAttribute("data-pdfforge-tip");
    if (!text) return;
    tip.textContent = text;
    tip.style.left = "-9999px";
    tip.style.top = "-9999px";
    tip.classList.add("is-visible");

    const rc = el.getBoundingClientRect();
    const tw = tip.offsetWidth || 120;
    const th = tip.offsetHeight || 26;
    const vw = window.innerWidth || 1280;
    const vh = window.innerHeight || 800;

    let left = rc.left + rc.width / 2 - tw / 2;
    left = Math.max(8, Math.min(vw - tw - 8, left));
    let top = rc.bottom + 6;
    if (top + th > vh - 8) {
      top = Math.max(8, rc.top - th - 6);
    }
    tip.style.left = `${Math.round(left)}px`;
    tip.style.top = `${Math.round(top)}px`;
  };

  const harvestTitle = (target) => {
    if (!target || typeof target.closest !== "function") return null;
    const host = target.closest("[title], [data-pdfforge-tip]");
    if (!host) return null;
    const nativeTitle = host.getAttribute("title");
    if (nativeTitle) {
      host.setAttribute("data-pdfforge-tip", nativeTitle);
      host.removeAttribute("title");
    }
    return host.getAttribute("data-pdfforge-tip") ? host : null;
  };

  document.addEventListener(
    "pointerover",
    (ev) => {
      const host = harvestTitle(ev.target);
      if (!host) {
        if (activeHost) hideNow();
        return;
      }
      if (host === activeHost) return;
      hideNow();
      activeHost = host;
      const delay = isReducedMotion() ? 180 : 250;
      timer = setTimeout(() => {
        if (activeHost === host && document.contains(host)) {
          showFor(host);
        }
      }, delay);
    },
    true
  );

  document.addEventListener(
    "pointerout",
    (ev) => {
      if (!activeHost) return;
      const rel = ev.relatedTarget;
      if (!rel || !activeHost.contains(rel)) {
        hideNow();
      }
    },
    true
  );

  window.addEventListener("pointerdown", hideNow, true);
  window.addEventListener("keydown", hideNow, true);
  window.addEventListener("scroll", hideNow, true);
  window.addEventListener("blur", hideNow);
}

let splashDismissed = false;
export function dismissSplash(immediate = false) {
  if (splashDismissed) return;
  splashDismissed = true;
  const splash = document.getElementById("pdfforge-splash");
  const root = document.getElementById("root");
  const reduced = isReducedMotion();
  if (root && !immediate && !reduced) {
    root.classList.add("anim-app-enter");
    setTimeout(() => root.classList.remove("anim-app-enter"), 180);
  }
  if (!splash) return;
  if (immediate || reduced) {
    splash.remove();
    return;
  }
  splash.classList.add("is-leaving");
  setTimeout(() => {
    try {
      splash.remove();
    } catch {}
  }, 120);
}

export async function runCloseSequence(sessionPayload) {
  const root = document.getElementById("root");
  if (sessionPayload) {
    try {
      fetch("/__rust_api__/store/saveSession", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(sessionPayload),
        keepalive: true,
      }).catch(() => {});
    } catch {}
  }

  if (root && !isReducedMotion()) {
    root.style.willChange = "transform, opacity";
    root.classList.add("pf-app-closing");
    await new Promise((res) => setTimeout(res, 80));
  }

  if (window.ipc && typeof window.ipc.postMessage === "function") {
    window.ipc.postMessage(
      JSON.stringify({
        cmd: "confirmClose",
        proceed: true,
        session: sessionPayload || null,
      })
    );
  }
}

const MotionAPI = {
  isReducedMotion,
  setReducedMotion,
  animateEnter,
  animateExit,
  captureRects,
  playFlipX,
  animateValueFlash,
  animateInvalidShake,
  smoothScrollTo,
  saveFocus,
  focusDialogInitial,
  restoreFocus,
  initTooltipManager,
  dismissSplash,
  runCloseSequence,
};

if (typeof window !== "undefined") {
  window.__pdfforgeMotion = MotionAPI;
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initTooltipManager, { once: true });
  } else {
    initTooltipManager();
  }
  setTimeout(() => dismissSplash(true), 600);
}

export default MotionAPI;
