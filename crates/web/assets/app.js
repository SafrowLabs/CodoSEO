// CodoSEO web app behaviour on top of htmx: navigation that keeps the chrome still, the ⌘K
// palette, keyboard shortcuts, toasts, a progress bar, count-up numbers, theme and sidebar,
// and the mascot (blinks, glances, and on the landing page, follows the cursor).
(() => {
  "use strict";
  const root = document.documentElement;
  const $ = (sel, el = document) => el.querySelector(sel);
  const $$ = (sel, el = document) => Array.from(el.querySelectorAll(sel));
  const reduced = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const store = {
    get(k) { try { return localStorage.getItem(k); } catch { return null; } },
    set(k, v) { try { v == null ? localStorage.removeItem(k) : localStorage.setItem(k, v); } catch {} },
  };
  const siteBase = () => document.body.dataset.site || "";

  // ── Toasts ───────────────────────────────────────────
  function toast(message, kind = "ok") {
    const box = $("#toasts");
    if (!box || !message) return;
    const el = document.createElement("div");
    el.className = `toast ${kind}`;
    el.setAttribute("role", kind === "error" ? "alert" : "status");
    const icon = kind === "error" ? "!" : kind === "info" ? "i" : "✓";
    el.innerHTML = `<span class="ic">${icon}</span><span class="msg"></span><button aria-label="Dismiss">✕</button>`;
    $(".msg", el).textContent = message;
    const close = () => { el.classList.add("out"); setTimeout(() => el.remove(), 220); };
    $("button", el).addEventListener("click", close);
    let timer = setTimeout(close, kind === "error" ? 6000 : 3500);
    el.addEventListener("mouseenter", () => clearTimeout(timer));
    el.addEventListener("mouseleave", () => { timer = setTimeout(close, 1800); });
    box.appendChild(el);
    while (box.children.length > 4) box.firstElementChild.remove();
  }
  window.codoseo = { toast };
  document.body.addEventListener("toast", (e) => toast(e.detail.message, e.detail.kind));

  // ── Theme and sidebar ────────────────────────────────
  // Light is the default, whatever the OS prefers. "dark" and "system" are stored and set as
  // data-theme (the head script does it before first paint); only "system" follows the OS.
  const darkOS = window.matchMedia("(prefers-color-scheme: dark)");
  const THEME_COLOR = { light: "#efefea", dark: "#0a1411" };
  function setTheme(t) {
    if (t === "dark" || t === "system") { root.dataset.theme = t; store.set("codoseo-theme", t); }
    else { delete root.dataset.theme; store.set("codoseo-theme", null); }
    syncTheme();
  }
  function themeLabel() {
    const t = root.dataset.theme;
    return t === "dark" ? "Dark" : t === "system" ? "System" : "Light";
  }
  // The theme-color metas come one per OS scheme; unless "system" is chosen, both take the
  // colour of the theme in use.
  function syncTheme() {
    const t = root.dataset.theme;
    $$('meta[name="theme-color"]').forEach((m) => {
      const own = (m.media || "").includes("dark") ? "dark" : "light";
      m.content = THEME_COLOR[t === "system" ? own : t === "dark" ? "dark" : "light"];
    });
    $$("[data-theme-label]").forEach((el) => (el.textContent = themeLabel()));
    const label = isDark() ? "Switch to light mode" : "Switch to dark mode";
    $$('[data-action="theme-toggle"]').forEach((b) => { b.setAttribute("aria-label", label); b.title = label; });
  }
  const isDark = () => root.dataset.theme === "dark" || (root.dataset.theme === "system" && darkOS.matches);
  function cycleTheme() {
    const t = root.dataset.theme;
    setTheme(t === "dark" ? "system" : t === "system" ? "light" : "dark");
    toast(`Theme: ${themeLabel()}${root.dataset.theme === "system" ? (darkOS.matches ? " (dark)" : " (light)") : ""}`, "info");
  }
  function toggleRail() {
    const on = root.classList.toggle("rail");
    store.set("codoseo-rail", on ? "1" : null);
  }
  const drawer = (open) => root.classList.toggle("drawer-open", open);

  // ── Count-up numbers: <b data-count="1284">1,284</b> ─
  function countUp(scope) {
    $$("[data-count]", scope).forEach((el) => {
      if (el.dataset.counted) return;
      el.dataset.counted = "1";
      const target = parseFloat(el.dataset.count);
      if (!isFinite(target) || reduced()) return;
      const decimals = (el.dataset.count.split(".")[1] || "").length;
      const fmt = (v) => v.toLocaleString("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
      const start = performance.now(), dur = 700;
      const step = (now) => {
        const t = Math.min(1, (now - start) / dur);
        const eased = 1 - Math.pow(1 - t, 3);
        el.textContent = fmt(target * eased);
        if (t < 1) requestAnimationFrame(step);
        else el.textContent = fmt(target);
      };
      requestAnimationFrame(step);
    });
  }

  // ── Keeping the chrome still across navigations ──────
  // A boosted link or form normally swaps <body>. When both pages have the app shell, swap
  // only #main (with a view transition) and refresh the few chrome pieces that depend on the
  // page: the nav highlight and counts, the breadcrumb, the crawler card and the site switcher.
  const CHROME = ["crumbs", "nav", "crawler", "switcher", "header-actions"];
  function syncChrome(doc) {
    CHROME.forEach((id) => {
      const cur = document.getElementById(id);
      const next = doc.getElementById(id);
      if (cur && next && cur.outerHTML !== next.outerHTML) {
        const settle = settleMascots(cur, next);
        cur.replaceWith(next);
        settle();
        if (window.htmx) htmx.process(next);
      }
    });
    if (doc.body && doc.body.dataset.site !== undefined) document.body.dataset.site = doc.body.dataset.site || "";
  }
  document.addEventListener("htmx:beforeSwap", (e) => {
    const d = e.detail;
    if (!d.boosted || !$("#main") || typeof d.serverResponse !== "string") return;
    const doc = new DOMParser().parseFromString(d.serverResponse, "text/html");
    if (!doc.getElementById("main") || !doc.querySelector(".shell")) return; // e.g. login page
    syncChrome(doc);
    d.target = $("#main");
    d.selectOverride = "#main";
    d.swapOverride = reduced() ? "outerHTML" : "outerHTML transition:true";
    drawer(false);
  });

  // A mascot that comes back in a new mood (same id) starts in its old one and moves over, the
  // way htmx settles classes on its own swaps.
  function settleMascots(cur, next) {
    const moves = $$(".mascot[id]", next).map((el) => {
      const old = cur.querySelector(`#${CSS.escape(el.id)}`);
      if (!old || old.getAttribute("class") === el.getAttribute("class")) return null;
      const to = el.getAttribute("class");
      el.setAttribute("class", old.getAttribute("class"));
      return () => el.setAttribute("class", to);
    }).filter(Boolean);
    return () => moves.length && requestAnimationFrame(() => requestAnimationFrame(() => moves.forEach((m) => m())));
  }

  // Reloads the current page's #main and chrome in place (after a crawl finishes).
  async function refresh() {
    try {
      const res = await fetch(location.href, { headers: { "HX-Request": "true", "HX-Boosted": "true" } });
      if (!res.ok) return;
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const next = doc.getElementById("main");
      const cur = $("#main");
      if (!next || !cur) return;
      syncChrome(doc);
      const before = snapshotStates();
      cur.replaceWith(next);
      htmx.process(next);
      countUp(next);
      markChanged(before);
    } catch {}
  }

  // ── AI access: state changes ─────────────────────────
  // Its bot rows and issues carry `data-state-key` and `data-state`. When the page is replaced by
  // itself (a recheck, a finished crawl, the poll below), whatever changed state gets `is-changed`
  // for one pulse, and `aia-settled` keeps the rows from fading in again (app.css).
  function snapshotStates() {
    const els = $$("[data-state-key]");
    root.classList.toggle("aia-settled", els.length > 0);
    return els.length ? new Map(els.map((el) => [el.dataset.stateKey, el.dataset.state])) : null;
  }
  function markChanged(before) {
    if (!before) return;
    $$("[data-state-key]").forEach((el) => {
      const was = before.get(el.dataset.stateKey);
      if (was !== undefined && was !== el.dataset.state) el.classList.add("is-changed");
    });
  }
  let states = null;
  document.addEventListener("htmx:beforeSwap", (e) => { if (!e.detail.isError) states = snapshotStates(); });
  document.addEventListener("htmx:afterSettle", () => { markChanged(states); states = null; });
  // A recheck queued or running: look again every 10 seconds until its result is in.
  setInterval(() => { if (!document.hidden && $("[data-recheck-pending]")) refresh(); }, 10000);

  // ── AI access: the Bots list and the robots.txt tester ──
  // The search box filters the rows as you type (the server filters too, on Enter or without
  // JavaScript); a group with no row left hides.
  function filterBots(input) {
    const q = input.value.trim().toLowerCase();
    let shown = 0;
    $$("tbody[data-bot-group]").forEach((tb) => {
      let n = 0;
      $$("tr.aia-brow", tb).forEach((tr) => {
        const hit = !q || (tr.dataset.q || "").includes(q);
        tr.hidden = !hit;
        if (hit) n++;
      });
      tb.hidden = n === 0;
      shown += n;
    });
    const none = $("[data-bot-nomatch]");
    if (none) none.hidden = shown > 0 || !$("tbody[data-bot-group]");
  }
  document.addEventListener("input", (e) => {
    if (e.target.matches && e.target.matches("[data-bot-search]")) filterBots(e.target);
  });
  // A picked row's panel came in: mark the row, and on a phone bring the panel into view.
  document.addEventListener("htmx:afterSwap", (e) => {
    const panel = e.detail.elt && e.detail.elt.id === "aia-panel" ? e.detail.elt : $("#aia-panel");
    if (!panel || !e.detail.target || e.detail.target.id !== "aia-panel") return;
    const token = panel.dataset.bot;
    $$("tr.aia-brow").forEach((tr) => {
      const on = tr.dataset.bot === token;
      tr.classList.toggle("is-sel", on);
      const a = $(".aia-bot-link", tr);
      if (a) on ? a.setAttribute("aria-current", "true") : a.removeAttribute("aria-current");
    });
    if (window.matchMedia("(max-width: 760px)").matches) panel.scrollIntoView({ block: "start", behavior: reduced() ? "auto" : "smooth" });
  });
  // The tester answers a refused bot or path with a 400 that says why: show it in place.
  document.addEventListener("htmx:beforeSwap", (e) => {
    const d = e.detail;
    const elt = d.requestConfig && d.requestConfig.elt;
    if (d.xhr && d.xhr.status === 400 && elt && elt.closest && elt.closest("[data-swap-errors]")) {
      d.shouldSwap = true;
      d.isError = false;
    }
  });
  document.addEventListener("htmx:load", () => { const i = $("[data-bot-search]"); if (i && i.value) filterBots(i); });

  // Programmatic navigation that goes through the same boosted path as a click.
  function go(href) {
    const a = document.createElement("a");
    a.href = href;
    a.style.display = "none";
    document.body.appendChild(a);
    if (window.htmx) htmx.process(a);
    a.click();
    a.remove();
  }
  window.codoseo.go = go;

  // ── htmx glue ────────────────────────────────────────
  let loadingTimer;
  let inflight = 0;
  document.addEventListener("htmx:beforeRequest", (e) => {
    // Background polls (the crawler card) don't light up the progress bar.
    if (e.detail.elt && e.detail.elt.closest("[data-quiet]")) return;
    inflight++;
    clearTimeout(loadingTimer);
    loadingTimer = setTimeout(() => { root.classList.remove("is-done"); root.classList.add("is-loading"); }, 90);
  });
  document.addEventListener("htmx:afterRequest", (e) => {
    if (e.detail.elt && e.detail.elt.closest("[data-quiet]")) return;
    inflight = Math.max(0, inflight - 1);
    if (inflight) return;
    clearTimeout(loadingTimer);
    if (root.classList.contains("is-loading")) {
      root.classList.remove("is-loading");
      root.classList.add("is-done");
      setTimeout(() => root.classList.remove("is-done"), 500);
    }
  });
  document.addEventListener("htmx:afterSettle", (e) => countUp(e.detail.elt));
  document.addEventListener("htmx:sendError", () => toast("Can't reach CodoSEO. Check your connection.", "error"));
  // An error fragment for a request that swaps nothing (hx-swap="none") becomes a toast.
  document.addEventListener("htmx:beforeSwap", (e) => {
    const d = e.detail;
    if (!d.isError) return;
    const swap = (d.requestConfig && d.requestConfig.elt && d.requestConfig.elt.getAttribute("hx-swap")) || "";
    if (swap.startsWith("none") || !d.target || d.target === document.body) {
      const doc = new DOMParser().parseFromString(String(d.serverResponse || ""), "text/html");
      const msg = doc.querySelector("[data-error-message]");
      toast(msg ? msg.textContent.trim() : "Something went wrong.", "error");
      d.shouldSwap = false;
    }
  });
  // ── Live audit: a poll replaces the list, so the rows the reader opened stay open ──
  let openRows = null;
  document.addEventListener("htmx:beforeSwap", (e) => {
    if (e.detail.target && e.detail.target.id === "live-audit") {
      openRows = new Set($$("#live-audit details[open][data-live-row]").map((d) => d.dataset.liveRow));
    }
  });
  document.addEventListener("htmx:afterSwap", () => {
    if (!openRows) return;
    $$("#live-audit details[data-live-row]").forEach((d) => { if (openRows.has(d.dataset.liveRow)) d.open = true; });
    openRows = null;
  });

  // ── Live audit: playback ─────────────────────────────
  // A poll brings the pages checked since the last one, often 5-7 at once. Instead of popping
  // in together, each new row is held back and played in a live row at the top of the list:
  // fetched (···), its checks popping in group by group, then it settles into the list and the
  // next starts. The pace follows the backlog so playback stays about one poll behind. Rows
  // there on first sight, other filters than All, a hidden tab and reduced motion: no playback.
  const LIVE = "#live-audit, #audit-main";
  const POLL = 2000;
  const BEHIND = 16; // more held rows than this: the oldest go in at once
  const lv = { el: null, shown: 0, cur: 0, row: null, timer: 0, done: false, poll: false, nums: null };
  const rowN = (r) => +r.dataset.liveRow || 0;
  const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
  const allFilter = (el) => { const on = $(".la-chip.is-on", el); return !on || on === $(".la-chip", el); };
  const held = (el) => $$("[data-live-row]", el).filter((r) => rowN(r) > lv.shown).sort((a, b) => rowN(a) - rowN(b));
  const liveRowOf = (n) => lv.el && lv.el.querySelector(`[data-live-row="${n}"]`);
  function stopPlay() {
    clearTimeout(lv.timer);
    lv.timer = lv.cur = 0;
    if (lv.row) lv.row.remove();
    lv.row = null;
  }
  function reveal(r, anim) {
    r.classList.remove("la-held");
    lv.shown = Math.max(lv.shown, rowN(r));
    if (!anim) return;
    r.classList.add("la-in");
    r.addEventListener("animationend", () => r.classList.remove("la-in"), { once: true });
  }
  function h(tag, cls, text) {
    const el = document.createElement(tag);
    if (cls) el.className = cls;
    if (text != null) el.textContent = text;
    return el;
  }
  // A row's checks, read from its own boxes (the opened view, or a <template> on the free audit).
  const MINI = { "is-ok": ["ck-ok", "✓ "], "is-critical": ["ck-crit", "✕ "], "is-warning": ["ck-warn", "✕ "] };
  function checksOf(src) {
    const t = $("template[data-live-checks]", src);
    return $$(".la-box", t ? t.content : src).map((box) => {
      const dot = $(".la-box-h .sdot", box);
      const items = $$("li", box).map((li) => {
        const [cls, mark] = MINI[(li.querySelector(".sdot") || li).classList[1]] || ["ck-note", ""];
        return { cls, label: mark + li.textContent.trim() };
      });
      $$(".la-bots .ck", box).forEach((b) => items.push({ cls: b.classList[1] || "ck-note", label: b.textContent.trim() }));
      return { name: $(".la-box-h", box).textContent.trim(), dot: dot ? dot.className : "sdot is-ok", items };
    });
  }
  function play(src, backlog) {
    const n = rowN(src);
    const budget = clamp(POLL / backlog, 250, 1600);
    const groups = checksOf(src);
    const steps = groups.reduce((k, g) => k + g.items.length, 0);
    const fetch = clamp(budget * 0.2, 60, 320);
    const gap = clamp((budget - fetch) / (steps + 1), 60, 140);
    const per = Math.ceil(steps / Math.max(1, Math.floor((budget - fetch) / gap) - 1)) || 1;

    const row = lv.row || h("div");
    row.className = `la-row la-live${src.classList.contains("q-row") ? " q-row" : ""}${lv.row ? "" : " la-in"}`;
    row.setAttribute("aria-hidden", "true");
    row.replaceChildren();
    if ($(".la-col-n", src)) row.append(h("span", "la-col-n mono", String(n)));
    const realPill = $(".la-pill", src);
    const pill = h("span", "la-pill p-run", "···");
    const ms = h("span", "la-ms", "fetching as CodoSEObot…");
    const line = h("span", "la-line");
    line.append(h("span", "mono la-path", ($(".la-path", src) || src).textContent.trim()), ms);
    const grps = h("span", "la-grps");
    const boxes = groups.map((g) => {
      const box = h("span", "la-grp");
      const dot = h("i", "sdot is-na");
      const head = h("span", "la-grp-h");
      head.append(dot, g.name);
      const list = h("span", "la-minis");
      if (g.items.length) list.append(h("span", "ck la-mini ck-wait", "waiting"));
      box.append(head, list);
      grps.append(box);
      return { g, box, dot, list, done: 0 };
    });
    const main = h("span", "la-main");
    main.append(line, grps);
    const pillWrap = h("span");
    pillWrap.append(pill);
    row.append(pillWrap, main, h("span", "la-imp imp-none", "checking…"));
    lv.row = row;
    lv.cur = n;
    src.before(row);
    if (lv.row.classList.contains("la-in")) row.addEventListener("animationend", () => row.classList.remove("la-in"), { once: true });

    let i = 0;
    const wait = () => h("span", "ck la-mini ck-wait", "checking…");
    const first = boxes.find((x) => x.g.items.length);
    if (first) { first.box.classList.add("is-on"); first.list.replaceChildren(wait()); }
    const tick = () => {
      if (i === 0) { // fetched: the real status and timing
        pill.className = realPill ? realPill.className : "la-pill";
        pill.textContent = realPill ? realPill.textContent : "";
        ms.textContent = ($(".la-ms", src) || {}).textContent || "";
      }
      $$(".is-new", row).forEach((m) => m.classList.remove("is-new"));
      for (let k = 0; k < per && i < steps; k++, i++) {
        const b = boxes.find((x) => x.done < x.g.items.length);
        const it = b.g.items[b.done++];
        if (b.done === 1) b.list.replaceChildren();
        else b.list.lastChild.remove(); // the "checking…" chip
        b.list.append(h("span", `ck la-mini ${it.cls} is-new`, it.label));
        if (b.done < b.g.items.length) b.list.append(wait());
        else { b.dot.className = b.g.dot; b.box.classList.remove("is-on"); }
      }
      const next = boxes.find((x) => x.done < x.g.items.length);
      boxes.forEach((x) => x.box.classList.toggle("is-on", x === next));
      if (next && next.done === 0) next.list.replaceChildren(wait());
      lv.timer = setTimeout(i < steps ? tick : settle, gap);
    };
    const settle = () => {
      const r = liveRowOf(n);
      if (r) reveal(r, true);
      lv.cur = 0;
      playNext();
    };
    lv.timer = setTimeout(tick, fetch);
  }
  function playNext() {
    lv.timer = 0;
    const el = lv.el;
    const q = el && el.isConnected ? held(el) : [];
    if (!q.length || lv.done) { if (lv.row) lv.row.remove(); lv.row = null; lv.cur = 0; }
    if (!q.length) return;
    if (lv.done) { reveal(q[0], true); lv.timer = setTimeout(playNext, 150); return; } // drain
    play(q[0], q.length);
  }

  // Numbers that changed on a poll tween from the old value, and the progress bar slides.
  const TWEEN = ["[data-count]", "[data-tween]", ".la-signal b", ".la-chip b", ".sc-val b", ".q-overall-v"];
  const NUM = /[−-]?\d[\d,]*(?:\.\d+)?/;
  const snapNums = (el) => ({
    text: TWEEN.map((s) => $$(s, el).map((n) => n.textContent)),
    bars: $$(".la-bar > span", el).map((b) => b.style.width),
  });
  function tween(el, from, to) {
    const a = from.match(NUM), b = to.match(NUM);
    if (!a || !b || a[0] === b[0] || el.children.length) return;
    const num = (s) => parseFloat(s.replace(/,/g, "").replace("−", "-"));
    const x = num(a[0]), y = num(b[0]);
    if (!isFinite(x) || !isFinite(y)) return;
    const dec = (b[0].split(".")[1] || "").length;
    const opts = { minimumFractionDigits: dec, maximumFractionDigits: dec, useGrouping: b[0].includes(",") || a[0].includes(",") };
    const minus = b[0][0] === "-" ? "-" : "−";
    const fmt = (v) => (v < 0 ? minus : "") + Math.abs(v).toLocaleString("en-US", opts);
    const pre = to.slice(0, b.index), post = to.slice(b.index + b[0].length);
    const start = performance.now(), dur = 600;
    const step = (now) => {
      const t = clamp((now - start) / dur, 0, 1); // a frame's time can be just before start
      el.textContent = t < 1 ? pre + fmt(x + (y - x) * (1 - Math.pow(1 - t, 3))) + post : to;
      if (t < 1 && el.isConnected) requestAnimationFrame(step);
    };
    el.textContent = pre + fmt(x) + post;
    requestAnimationFrame(step);
  }
  function tweenNums(el, snap) {
    TWEEN.forEach((s, i) => $$(s, el).forEach((n, j) => {
      if (n.dataset.count) n.dataset.counted = "1"; // no count-up from zero on every poll
      if (snap.text[i][j] != null) tween(n, snap.text[i][j], n.textContent);
    }));
    $$(".la-bar > span", el).forEach((b, j) => {
      const was = snap.bars[j], to = b.style.width;
      if (!was || was === to) return;
      b.style.width = was;
      void b.offsetWidth;
      b.style.width = to;
    });
  }

  function liveSync(poll) {
    const el = $(LIVE);
    if (el === lv.el) return;
    const prev = lv.el, nums = lv.nums;
    lv.el = el;
    lv.nums = null;
    if (!el) { stopPlay(); return; }
    lv.done = !el.hasAttribute("hx-trigger");
    if (!poll || !prev || reduced() || document.hidden || !allFilter(el)) {
      stopPlay();
      lv.shown = Math.max(0, ...$$("[data-live-row]", el).map(rowN));
      return;
    }
    if (nums) tweenNums(el, nums);
    const q = held(el);
    q.forEach((r) => r.classList.add("la-held"));
    while (q.length > BEHIND) reveal(q.shift(), false);
    // The row playing came back in the new list: carry the live row over, else drop it.
    const cur = lv.cur && liveRowOf(lv.cur);
    if (lv.done || !cur || lv.cur <= lv.shown) stopPlay();
    else {
      $$(".is-new, .la-in", lv.row).concat(lv.row).forEach((m) => m.classList.remove("is-new", "la-in"));
      cur.before(lv.row);
    }
    if (!lv.timer) playNext();
  }
  document.addEventListener("htmx:beforeSwap", (e) => {
    const t = e.detail.target, elt = e.detail.requestConfig && e.detail.requestConfig.elt;
    lv.poll = !!(t && t === lv.el && elt === t && !e.detail.isError);
    if (lv.poll && !reduced()) lv.nums = snapNums(t);
  });
  document.addEventListener("htmx:afterSwap", () => { liveSync(lv.poll); lv.poll = false; });
  document.addEventListener("htmx:historyRestore", () => liveSync(false));
  document.addEventListener("DOMContentLoaded", () => liveSync(false));

  document.body.addEventListener("crawlFinished", refresh);
  document.body.addEventListener("crawlQueued", () => {
    const card = $("#crawler");
    if (card && window.htmx) htmx.ajax("GET", `${siteBase()}/status`, { target: "#crawler", swap: "outerHTML" });
  });

  // ── Command palette ──────────────────────────────────
  const palette = $("#palette");
  let palItems = [];
  let palActive = 0;
  let searchAbort;
  let searchTimer;

  function commands() {
    const base = siteBase();
    const list = [];
    if (base) {
      list.push(
        { label: "Site audit", href: `${base}/audit`, keys: "G A", icon: "i-audit" },
        { label: "URL explorer", href: `${base}/explorer`, keys: "G E", icon: "i-explorer" },
        { label: "AI access", href: `${base}/ai-access`, keys: "G I", icon: "i-ai" },
        { label: "Changes", href: `${base}/changes`, keys: "G C", icon: "i-changes" },
        { label: "Crawl history", href: `${base}/crawls`, keys: "G H", icon: "i-crawls" },
        { label: "Site policy", href: `${base}/policy`, keys: "G P", icon: "i-policy" },
        { label: "Run crawl", run: () => $("#run-crawl") && $("#run-crawl").click(), icon: "i-play" },
        { label: "Export CSV", href: `${base}/export.csv`, download: true, icon: "i-download" },
      );
    }
    list.push(
      { label: "Sites", href: "/sites", icon: "i-globe" },
      { label: "Add a site", href: "/sites#add", icon: "i-plus" },
      { label: "Toggle theme", run: cycleTheme, icon: "i-moon" },
      { label: "Toggle sidebar", run: toggleRail, keys: "[", icon: "i-sidebar" },
      { label: "Keyboard shortcuts", run: () => openSheet(), keys: "?", icon: "i-keyboard" },
      { label: "Account settings", href: "/account", keys: "G S", icon: "i-settings" },
      { label: "AI bot registry", href: "/ai-bots", icon: "i-ai" },
    );
    return list;
  }
  const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  function highlight(label, q) {
    if (!q) return esc(label);
    const i = label.toLowerCase().indexOf(q.toLowerCase());
    return i < 0 ? esc(label) : esc(label.slice(0, i)) + "<mark>" + esc(label.slice(i, i + q.length)) + "</mark>" + esc(label.slice(i + q.length));
  }
  function renderPalette(q, pagesHtml) {
    const list = $("#palette-list");
    const cmds = commands().filter((c) => !q || c.label.toLowerCase().includes(q.toLowerCase()));
    let html = "";
    if (pagesHtml) html += `<div class="pal-section">Pages</div>${pagesHtml}`;
    if (cmds.length) {
      html += `<div class="pal-section">${q ? "Commands" : "Go to"}</div>`;
      html += cmds.map((c, i) => `<div class="pal-item" role="option" data-cmd="${i}"><svg class="icon"><use href="#${c.icon}"/></svg><span class="pal-main">${highlight(c.label, q)}</span>${c.keys ? c.keys.split(" ").map((k) => `<kbd>${k}</kbd>`).join("") : ""}</div>`).join("");
    }
    list.innerHTML = html || `<div class="pal-empty">No matches for “${esc(q)}”</div>`;
    palItems = $$(".pal-item", list);
    const cmdMap = cmds;
    palItems.forEach((el) => {
      if (el.dataset.cmd !== undefined) el._cmd = cmdMap[+el.dataset.cmd];
      el.addEventListener("mousemove", () => setActive(palItems.indexOf(el)));
      el.addEventListener("click", (ev) => { ev.preventDefault(); activate(el); });
    });
    setActive(0);
  }
  function setActive(i) {
    if (!palItems.length) return;
    palActive = (i + palItems.length) % palItems.length;
    palItems.forEach((el, j) => el.classList.toggle("is-active", j === palActive));
    palItems[palActive].scrollIntoView({ block: "nearest" });
  }
  function activate(el) {
    closePalette();
    const c = el._cmd;
    if (c) {
      if (c.run) return c.run();
      if (c.download) { location.href = c.href; return; }
      return go(c.href);
    }
    const href = el.getAttribute("href");
    if (href) go(href);
  }
  function openPalette() {
    if (!palette) return;
    if (!palette.open) palette.showModal();
    const input = $("#palette-input");
    input.value = "";
    renderPalette("", "");
    input.focus();
  }
  function closePalette() { if (palette && palette.open) palette.close(); }
  if (palette) {
    palette.addEventListener("click", (e) => { if (e.target === palette) closePalette(); });
    $("#palette-input").addEventListener("input", (e) => {
      const q = e.target.value.trim();
      renderPalette(q, "");
      clearTimeout(searchTimer);
      if (!q || !siteBase()) return;
      searchTimer = setTimeout(async () => {
        if (searchAbort) searchAbort.abort();
        searchAbort = new AbortController();
        try {
          const res = await fetch(`${siteBase()}/search?q=${encodeURIComponent(q)}`, { signal: searchAbort.signal, headers: { "HX-Request": "true" } });
          if (!res.ok) return;
          const html = (await res.text()).trim();
          if ($("#palette-input").value.trim() === q) renderPalette(q, html.includes("pal-item") ? html : "");
        } catch {}
      }, 140);
    });
    $("#palette-input").addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown") { e.preventDefault(); setActive(palActive + 1); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setActive(palActive - 1); }
      else if (e.key === "Enter") { e.preventDefault(); if (palItems[palActive]) activate(palItems[palActive]); }
    });
  }
  const sheet = $("#shortcuts");
  function openSheet() { if (sheet && !sheet.open) sheet.showModal(); }
  if (sheet) sheet.addEventListener("click", (e) => { if (e.target === sheet) sheet.close(); });

  // ── Keyboard shortcuts ───────────────────────────────
  let pendingG = false;
  let gTimer;
  function keyHint(show) {
    let el = $("#keyhint");
    if (!show) { if (el) el.remove(); return; }
    if (el) return;
    el = document.createElement("div");
    el.id = "keyhint";
    el.className = "keyhint";
    el.innerHTML = "<kbd>G</kbd> then <kbd>E</kbd> explorer · <kbd>A</kbd> audit · <kbd>C</kbd> changes · <kbd>I</kbd> AI access · <kbd>H</kbd> crawls · <kbd>P</kbd> policy";
    document.body.appendChild(el);
  }
  const typing = (el) => el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName));
  function moveRow(dir) {
    const rows = $$("#main [data-row]");
    if (!rows.length) return;
    let i = rows.findIndex((r) => r.getAttribute("aria-selected") === "true");
    i = i < 0 ? 0 : Math.max(0, Math.min(rows.length - 1, i + dir));
    rows[i].click();
    rows[i].scrollIntoView({ block: "nearest" });
  }
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      palette && palette.open ? closePalette() : openPalette();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key === "\\") { e.preventDefault(); toggleRail(); return; }
    if (e.metaKey || e.ctrlKey || e.altKey || typing(e.target) || (palette && palette.open)) {
      if (e.key === "Escape" && typing(e.target)) e.target.blur();
      return;
    }
    if (pendingG) {
      pendingG = false;
      keyHint(false);
      const base = siteBase();
      const map = { e: "explorer", a: "audit", c: "changes", i: "ai-access", h: "crawls", p: "policy" };
      const k = e.key.toLowerCase();
      if (map[k] && base) { e.preventDefault(); go(`${base}/${map[k]}`); }
      else if (k === "s") { e.preventDefault(); go("/account"); }
      return;
    }
    switch (e.key) {
      case "g": pendingG = true; clearTimeout(gTimer); gTimer = setTimeout(() => { pendingG = false; keyHint(false); }, 1200); setTimeout(() => pendingG && keyHint(true), 350); break;
      case "/": { const s = $("#main [data-search-input]") || $("[data-search-input]"); if (s) { e.preventDefault(); s.focus(); s.select(); } else { e.preventDefault(); openPalette(); } break; }
      case "?": e.preventDefault(); openSheet(); break;
      case "[": e.preventDefault(); toggleRail(); break;
      case "j": case "ArrowDown": if ($("#main [data-row]")) { e.preventDefault(); moveRow(1); } break;
      case "k": case "ArrowUp": if ($("#main [data-row]")) { e.preventDefault(); moveRow(-1); } break;
      case "Escape": drawer(false); $$("details[open]").forEach((d) => d.removeAttribute("open")); break;
    }
  });

  // ── Clicks: data-action, copy buttons, closing menus ─
  document.addEventListener("click", (e) => {
    const act = e.target.closest("[data-action]");
    if (act) {
      const a = act.dataset.action;
      if (a === "palette") { e.preventDefault(); openPalette(); }
      else if (a === "theme") { e.preventDefault(); cycleTheme(); }
      else if (a === "theme-toggle") { e.preventDefault(); setTheme(isDark() ? "light" : "dark"); }
      else if (a === "rail") { e.preventDefault(); toggleRail(); }
      else if (a === "drawer") { e.preventDefault(); drawer(!root.classList.contains("drawer-open")); }
      else if (a === "drawer-close") drawer(false);
      else if (a === "shortcuts") { e.preventDefault(); openSheet(); }
      else if (a === "close-dialog") { const d = act.closest("dialog"); if (d) d.close(); }
    }
    const copy = e.target.closest("[data-copy]");
    if (copy) {
      e.preventDefault();
      navigator.clipboard && navigator.clipboard.writeText(copy.dataset.copy).then(() => toast("Copied to clipboard", "info"));
    }
    $$("details[open].switcher, details[open].user, details[open][data-menu]").forEach((d) => { if (!d.contains(e.target)) d.removeAttribute("open"); });
  });

  // ── Site audit: the fix list's filters and page chips ─
  // [data-fx-filter] shows the rows of one severity or kind; empty groups hide with them.
  // [data-fx-toggle] shows or hides the URL chips it controls.
  document.addEventListener("click", (e) => {
    const f = e.target.closest("[data-fx-filter]");
    if (f) {
      const list = f.closest(".fx");
      const want = f.dataset.fxFilter;
      $$("[data-fx-filter]", list).forEach((b) => b.setAttribute("aria-pressed", String(b === f)));
      let shown = 0;
      $$(".fx-group", list).forEach((g) => {
        let here = 0;
        $$(".fx-row", g).forEach((r) => {
          const hit = want === "all" || r.dataset.sev === want || r.dataset.kind === want;
          r.hidden = !hit;
          if (hit) here++;
        });
        g.hidden = here === 0;
        const n = $("[data-fx-count]", g);
        if (n) n.textContent = here;
        shown += here;
      });
      const none = $(".fx-none", list);
      if (none) none.hidden = shown > 0;
      return;
    }
    const t = e.target.closest("[data-fx-toggle]");
    if (t) {
      const box = document.getElementById(t.getAttribute("aria-controls"));
      const open = t.getAttribute("aria-expanded") !== "true";
      t.setAttribute("aria-expanded", String(open));
      if (box) box.hidden = !open;
    }
  });

  // ── Popovers: placed by the button that opened them ──
  // The [popover] top layer never clips, but opens centred: put it under (or above) its
  // button, inside the viewport. A phone shows it as a sheet at the bottom (CSS). A
  // [data-hover-pop] button also opens its popover on mouse hover and keyboard focus.
  let invoker = null;
  document.addEventListener("click", (e) => {
    const b = e.target.closest && e.target.closest("[popovertarget]");
    if (b) invoker = b;
  }, true);
  function place(pop, by) {
    if (!by || window.innerWidth <= 560) { pop.style.inset = pop.style.margin = ""; return; }
    const r = by.getBoundingClientRect();
    const w = pop.offsetWidth, h = pop.offsetHeight, pad = 8;
    const left = Math.min(Math.max(pad, r.left + r.width / 2 - w / 2), window.innerWidth - w - pad);
    let top = r.bottom + 6;
    if (top + h > window.innerHeight - pad) top = Math.max(pad, r.top - h - 6);
    pop.style.margin = "0";
    pop.style.inset = `${top}px auto auto ${left}px`;
  }
  document.addEventListener("toggle", (e) => {
    const pop = e.target;
    if (!(pop instanceof HTMLElement) || !pop.matches("[popover].pop") || e.newState !== "open") return;
    const by = invoker && invoker.getAttribute("popovertarget") === pop.id
      ? invoker : document.querySelector(`[popovertarget="${pop.id}"]`);
    place(pop, by);
  }, true);
  const hoverPop = (b) => b && document.getElementById(b.getAttribute("popovertarget"));
  let hoverTimer = 0;
  function showFor(b) {
    const pop = hoverPop(b);
    if (!pop || !pop.showPopover || pop.matches(":popover-open")) return;
    clearTimeout(hoverTimer);
    invoker = b;
    try { pop.showPopover(); } catch {}
  }
  function hideLater(b) {
    const pop = hoverPop(b);
    if (!pop || !pop.hidePopover) return;
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => {
      if (!pop.matches(":hover") && !b.matches(":hover") && document.activeElement !== b) {
        try { pop.hidePopover(); } catch {}
      }
    }, 180);
  }
  document.addEventListener("pointerover", (e) => {
    if (e.pointerType !== "mouse") return;
    const b = e.target.closest && e.target.closest("[data-hover-pop]");
    if (b) showFor(b);
  });
  document.addEventListener("pointerout", (e) => {
    if (e.pointerType !== "mouse") return;
    const b = e.target.closest && e.target.closest("[data-hover-pop]");
    if (b && !b.contains(e.relatedTarget)) hideLater(b);
  });
  document.addEventListener("focusin", (e) => {
    const b = e.target.closest && e.target.closest("[data-hover-pop]");
    if (b && b.matches(":focus-visible")) showFor(b);
  });
  document.addEventListener("focusout", (e) => {
    const b = e.target.closest && e.target.closest("[data-hover-pop]");
    if (b) hideLater(b);
  });

  // ── Filter a table as you type ──────────────────────
  // <input data-filter="#table" data-filter-empty="#msg" data-filter-count="#count"> hides the
  // rows (tr[data-search], lower-cased text) that don't contain the query, and any <tbody>
  // left with none. No request: the whole table is on the page.
  function filterRows(input) {
    const table = $(input.dataset.filter);
    if (!table) return;
    const q = input.value.trim().toLowerCase();
    let shown = 0;
    $$("tbody", table).forEach((body) => {
      let here = 0;
      $$("tr[data-search]", body).forEach((tr) => {
        const hit = !q || tr.dataset.search.includes(q);
        tr.hidden = !hit;
        if (hit) here++;
      });
      body.hidden = here === 0;
      shown += here;
    });
    const empty = input.dataset.filterEmpty && $(input.dataset.filterEmpty);
    if (empty) empty.hidden = shown > 0;
    const count = input.dataset.filterCount && $(input.dataset.filterCount);
    if (count) count.textContent = q ? `${shown} matching` : count.dataset.total || "";
  }
  document.addEventListener("input", (e) => {
    const el = e.target.closest && e.target.closest("input[data-filter]");
    if (el) filterRows(el);
  });

  // ── One-shot forms: a second submit (double click, Enter again) is swallowed ─
  // For a form whose POST uses up a link and shows something once.
  document.addEventListener("submit", (e) => {
    const form = e.target.closest && e.target.closest("form[data-once]");
    if (!form) return;
    if (form.dataset.sent) { e.preventDefault(); return; }
    form.dataset.sent = "1";
    // After the submit has gone out: a button disabled earlier would drop out of the request.
    setTimeout(() => $$("button[type=submit]", form).forEach((b) => (b.disabled = true)), 0);
  });

  // ── Mascot ───────────────────────────────────────────
  // Every pair of eyes on the page blinks now and then, a little out of step.
  function blink() {
    if (!reduced() && !document.hidden) {
      $$(".mascot").forEach((el) => setTimeout(() => {
        el.classList.add("is-blinking");
        setTimeout(() => el.classList.remove("is-blinking"), 220);
      }, Math.random() * 500));
    }
    setTimeout(blink, 5000 + Math.random() * 3000);
  }

  darkOS.addEventListener("change", syncTheme);
  // A boosted page swap brings a fresh topbar, its switch labelled for light.
  document.addEventListener("htmx:afterSettle", (e) => { if (e.detail.target === document.body) syncTheme(); });

  document.addEventListener("DOMContentLoaded", () => {
    countUp(document);
    syncTheme();
    setTimeout(blink, 2500 + Math.random() * 3000);
  });
})();
