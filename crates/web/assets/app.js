// CodoSEO web app behaviour on top of htmx: navigation that keeps the chrome still, the ⌘K
// palette, keyboard shortcuts, toasts, a progress bar, count-up numbers, theme and sidebar.
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
  function setTheme(t) {
    if (t === "light" || t === "dark") { root.dataset.theme = t; store.set("codoseo-theme", t); }
    else { delete root.dataset.theme; store.set("codoseo-theme", null); }
    $$("[data-theme-label]").forEach((el) => (el.textContent = themeLabel()));
  }
  function themeLabel() {
    const t = root.dataset.theme;
    return t === "dark" ? "Dark" : t === "light" ? "Light" : "System";
  }
  function cycleTheme() {
    const t = root.dataset.theme;
    setTheme(t === "light" ? "dark" : t === "dark" ? "system" : "light");
    toast(`Theme: ${themeLabel()}`, "info");
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
        cur.replaceWith(next);
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
      cur.replaceWith(next);
      htmx.process(next);
      countUp(next);
    } catch {}
  }

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
        { label: "URL explorer", href: `${base}/explorer`, keys: "G E", icon: "i-explorer" },
        { label: "Site audit", href: `${base}/audit`, keys: "G A", icon: "i-audit" },
        { label: "Changes", href: `${base}/changes`, keys: "G C", icon: "i-changes" },
        { label: "Crawl history", href: `${base}/crawls`, keys: "G H", icon: "i-crawls" },
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
    el.innerHTML = "<kbd>G</kbd> then <kbd>E</kbd> explorer · <kbd>A</kbd> audit · <kbd>C</kbd> changes · <kbd>H</kbd> crawls";
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
      const map = { e: "explorer", a: "audit", c: "changes", h: "crawls" };
      const k = e.key.toLowerCase();
      if (map[k] && base) { e.preventDefault(); go(`${base}/${map[k]}`); }
      else if (k === "s") { e.preventDefault(); go("/account"); }
      return;
    }
    switch (e.key) {
      case "g": pendingG = true; clearTimeout(gTimer); gTimer = setTimeout(() => { pendingG = false; keyHint(false); }, 1200); setTimeout(() => pendingG && keyHint(true), 350); break;
      case "/": { const s = $("#main [data-search-input]"); if (s) { e.preventDefault(); s.focus(); s.select(); } else { e.preventDefault(); openPalette(); } break; }
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

  document.addEventListener("DOMContentLoaded", () => {
    countUp(document);
    $$("[data-theme-label]").forEach((el) => (el.textContent = themeLabel()));
  });
})();
