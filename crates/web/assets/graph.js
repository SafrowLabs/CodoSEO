// The site graph's in-browser views (crawl tree, folder tree, bubble map), drawn with d3 from
// `/s/{site}/graph/data.json`. Each view mounts on an element with `data-graph="<view>"`; the
// side panel asks `page.json` for a page's inlinks and anchors when it is selected.
//
// Under hx-boost this file can run again on every visit to a graph tab, and its script tags
// load in any order, so it defines itself once and mounts once d3 is there.
(() => {
  "use strict";
  if (window.codoGraph) {
    window.codoGraph.mount(document);
    return;
  }
  const views = {};
  const reduced = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  function mount(root, tries = 0) {
    if (!window.d3) {
      if (tries < 200) setTimeout(() => mount(root, tries + 1), 50);
      return;
    }
    const els = root.querySelectorAll ? root.querySelectorAll("[data-graph]") : [];
    for (const el of els) {
      if (el.dataset.graphMounted) continue;
      const view = views[el.dataset.graph];
      if (!view) continue;
      el.dataset.graphMounted = "1";
      view(el);
    }
  }
  window.codoGraph = { mount };
  document.addEventListener("DOMContentLoaded", () => mount(document));
  document.addEventListener("htmx:load", (e) => mount(e.target));
  if (document.readyState !== "loading") mount(document);

  // ---------- shared: fetching, theme, DOM ----------

  // One request per URL for the page's life: switching tabs reuses the crawl's graph.
  const fetched = new Map();
  function getJSON(url) {
    if (!fetched.has(url)) {
      const p = fetch(url, { credentials: "same-origin", headers: { Accept: "application/json" } })
        .then((r) => {
          if (!r.ok) throw new Error(`${url}: ${r.status}`);
          return r.json();
        })
        .catch((e) => {
          fetched.delete(url);
          throw e;
        });
      fetched.set(url, p);
    }
    return fetched.get(url);
  }

  // Theme switches (the toggle sets data-theme on <html>; "system" follows the OS) redraw
  // whatever reads colours in script. Views drop out once their element leaves the page.
  const themed = new Set();
  function onTheme(el, fn) {
    themed.add({ el, fn });
  }
  function themeChanged() {
    for (const t of themed) {
      if (!t.el.isConnected) themed.delete(t);
      else t.fn();
    }
  }
  new MutationObserver(themeChanged).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", themeChanged);

  const TOKENS = ["ink", "ink-2", "muted", "faint", "line", "ghost-2", "surface", "surface-2", "panel", "accent", "accent-strong", "warn", "g-ok", "g-noindex", "g-redirect", "g-broken", "g-orphan"];
  function colours(el) {
    const s = getComputedStyle(el);
    const C = {};
    for (const k of TOKENS) C[k.replace(/-(\w)/g, (_, c) => c.toUpperCase())] = s.getPropertyValue("--" + k).trim();
    C.dark = d3.hsl(C.surface).l < 0.5;
    return C;
  }

  // replaceChildren, skipping the null and false a conditional part leaves.
  function fill(e, ...kids) {
    e.replaceChildren(...kids.flat(Infinity).filter((c) => c != null && c !== false));
  }

  function h(tag, attrs = {}, ...kids) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === "class") e.className = v;
      else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, v === true ? "" : v);
    }
    for (const c of kids.flat(Infinity)) if (c != null && c !== false) e.append(c);
    return e;
  }

  const fmt = (n) => Number(n).toLocaleString("en");
  const plural = (n, one, many = one + "s") => `${fmt(n)} ${n === 1 ? one : many}`;
  const clicks = (d) => (d === 0 ? "Start" : plural(d, "click"));
  const DEEP = 4;
  // The smallest scale a tree zooms to by itself: card text stays readable.
  const READABLE = 0.85;
  const STATUS = { ok: "Indexable", noindex: "Noindex", redirect: "Redirect", broken: "Broken" };

  // A page's short name: the last part of its path, in words.
  function nameOf(n, root) {
    if (n === root) return "Home";
    const path = n.url.split(/[?#]/)[0].replace(/\/+$/, "");
    let seg = path.slice(path.lastIndexOf("/") + 1) || n.url;
    try {
      seg = decodeURIComponent(seg);
    } catch (_) {}
    seg = seg.replace(/\.(html?|php|aspx?)$/i, "").replace(/[-_]+/g, " ").trim() || n.url;
    return seg.charAt(0).toUpperCase() + seg.slice(1);
  }

  function statusLabel(n) {
    if (n.st === "redirect") return `Redirect ${n.status || ""}`.trim();
    if (n.st === "broken") return n.status ? `Broken ${n.status}` : "No response";
    if (n.orphan && n.st === "ok") return "Indexable, not linked";
    return STATUS[n.st];
  }

  function pill(n) {
    return h("span", { class: "gv-pill", "data-x": n.orphan && n.st === "ok" ? "orphan" : n.st }, statusLabel(n));
  }

  // A mini status bar: one segment per status, as wide as its share.
  function miniBar(mix, cls = "gv-mini") {
    const bar = h("span", { class: cls, "aria-hidden": "true" });
    for (const x of ["ok", "noindex", "redirect", "broken"]) {
      if (!mix[x]) continue;
      const seg = h("span", { "data-x": x });
      seg.style.flexGrow = mix[x];
      bar.append(seg);
    }
    return bar;
  }

  // The CodoSEO eyes, for the home card.
  function eyes() {
    const s = h("span", { class: "gv-eyes", "aria-hidden": "true" });
    s.innerHTML = '<svg width="30" height="18" viewBox="4 16 56 32"><g fill="none" stroke="currentColor" stroke-width="5"><circle cx="20.5" cy="32" r="11.5"/><circle cx="43.5" cy="32" r="11.5"/></g><circle class="gv-pupil" cx="22.5" cy="31" r="5"/><circle class="gv-pupil" cx="45.5" cy="31" r="5"/></svg>';
    return s;
  }

  function badge(text, tone) {
    return h("span", { class: `gv-badge b-${tone}` }, text);
  }

  // ---------- shared: the crawl's graph, ready to draw ----------

  function prepare(raw) {
    const nodes = raw.nodes.map((n, i) => ({
      id: i, url: n.u, hash: n.h, title: n.t, status: n.s, st: n.x, depth: n.d, pid: n.p,
      score: n.k, inlinks: n.i, orphan: n.o, kids: [], extraIn: [], extraOut: [],
    }));
    const root = nodes[raw.root ?? 0];
    for (const n of nodes) {
      n.parent = n.pid == null ? null : nodes[n.pid];
      // A linked page whose parent didn't make it hangs off home.
      if (!n.parent && n !== root && !n.orphan) {
        n.parent = root;
        n.adopted = true;
      }
      if (n.parent) n.parent.kids.push(n);
    }
    for (let i = 0; i + 1 < raw.e.length; i += 2) {
      const a = nodes[raw.e[i]], b = nodes[raw.e[i + 1]];
      if (!a || !b) continue;
      a.extraOut.push(b);
      b.extraIn.push(a);
    }
    // Subtree totals, children before parents.
    const order = [root];
    for (let i = 0; i < order.length; i++) for (const c of order[i].kids) order.push(c);
    for (let i = order.length - 1; i >= 0; i--) {
      const n = order[i];
      n.size = 1;
      n.mix = { ok: 0, noindex: 0, redirect: 0, broken: 0 };
      n.mix[n.st]++;
      n.deep = n.depth != null && n.depth >= DEEP ? 1 : 0;
      n.prob = n.st !== "ok" || n.deep ? 1 : 0;
      for (const c of n.kids) {
        n.size += c.size;
        for (const x in c.mix) n.mix[x] += c.mix[x];
        n.deep += c.deep;
        n.prob += c.prob;
      }
    }
    const byHash = new Map(nodes.map((n) => [n.hash, n]));
    const byUrl = new Map(nodes.map((n) => [n.url, n]));
    const scores = nodes.map((n) => n.score).sort((a, b) => b - a);
    return {
      nodes, root, byHash, byUrl, scores,
      orphans: nodes.filter((n) => n.orphan),
      stats: raw.stats, capped: raw.capped,
      maxDepth: d3.max(nodes, (n) => n.depth || 0) || 0,
    };
  }

  // The best-linked page at least three clicks deep: where the trees open on first look.
  function importantDeep(site) {
    let best = null;
    for (const n of site.nodes) if (n.depth >= 3 && n.st === "ok" && (!best || n.score > best.score)) best = n;
    return best;
  }

  // Share of pages that score at least as well, as a whole percent.
  function topShare(site, n) {
    let rank = 0;
    while (rank < site.scores.length && site.scores[rank] > n.score) rank++;
    return Math.max(1, Math.ceil((100 * (rank + 1)) / site.scores.length));
  }

  function routeOf(n) {
    const r = [];
    for (let p = n; p && r.length < 500; p = p.parent) r.unshift(p);
    return r;
  }

  function explorerHref(base, n) {
    return `${base}/explorer?sel=${n.hash}`;
  }

  function showEmpty(el, text) {
    el.classList.add("is-empty");
    const stage = el.querySelector('[data-role="stage"]');
    for (const m of stage.querySelectorAll(".gv-msg")) m.remove();
    stage.append(h("p", { class: "gv-msg" }, text));
  }

  function load(el, draw) {
    getJSON(el.dataset.src)
      .then((raw) => {
        if (!el.isConnected) return;
        if (raw.empty || !raw.nodes.length) return showEmpty(el, "Run a new crawl to draw this map.");
        for (const m of el.querySelectorAll('[data-role="stage"] > .gv-msg')) m.remove();
        draw(prepare(raw));
      })
      .catch((e) => {
        console.error(e);
        if (el.isConnected) showEmpty(el, "The map didn't load. Reload the page to try again.");
      });
  }

  // ---------- shared: the side panel ----------

  function sitePanel(panel, site, extra) {
    const s = site.stats;
    const dist = d3.rollup(site.nodes.filter((n) => n.depth > 0), (v) => v.length, (n) => Math.min(n.depth, 6));
    const max = d3.max(dist.values()) || 1;
    const indexable = s.pages - s.broken - s.redirects - s.noindex;
    fill(panel,
      h("div", { class: "gv-ph" }, h("span", { class: "gv-eyebrow" }, "Latest crawl"), h("h2", {}, plural(s.pages, "page"))),
      h("div", { class: "gv-kv" },
        kv("Indexable", s.pages ? Math.round((100 * indexable) / s.pages) + "%" : "0%"),
        kv("Deepest page", clicks(s.depth)),
        kv("Not linked", fmt(s.orphans), s.orphans ? "warn" : null),
        kv("Broken", fmt(s.broken), s.broken ? "err" : null)),
      h("div", { class: "gv-sec" }, h("span", { class: "gv-eyebrow" }, "Pages per click depth"),
        h("div", { class: "gv-bars" }, [1, 2, 3, 4, 5, 6].map((d) => {
          const bar = h("i", { class: d >= DEEP ? "is-deep" : null });
          bar.style.width = (100 * (dist.get(d) || 0)) / max + "%";
          return [h("span", {}, d === 6 ? "6+" : clicks(d)), h("span", { class: "gv-track" }, bar), h("span", { class: "gv-n" }, fmt(dist.get(d) || 0))];
        }))),
      site.capped && site.capped.pages ? h("p", { class: "gv-note" }, h("b", {}, "A big site. "), `This view shows the ${fmt(site.nodes.length)} best-linked pages and the pages that lead to them. ${plural(site.capped.pages, "page")} more are left out.`) : null,
      extra);
  }

  function kv(label, value, tone) {
    return h("div", {}, h("span", {}, label), h("b", { class: tone ? "c-" + tone : null }, value));
  }

  // One page in the panel. `on.select(n)` moves the selection, `on.root(n)` re-roots the view
  // on the page, `routeLabel` names the route section.
  function pagePanel(panel, site, n, ctx) {
    const route = routeOf(n);
    const deepNote = !n.orphan && n.st === "ok" && n.depth >= DEEP && topShare(site, n) <= 20;
    const linked = h("div", { class: "gv-sec" }, h("span", { class: "gv-eyebrow" }, "Linked from"), h("p", { class: "gv-muted" }, "Loading the links…"));
    fill(panel,
      h("div", { class: "gv-ph" },
        h("span", { class: "gv-eyebrow" }, "Selected page"),
        h("h2", {}, n.title || nameOf(n, site.root)),
        h("span", { class: "gv-url" }, n.url)),
      h("div", {}, pill(n)),
      h("div", { class: "gv-kv" },
        kv("Clicks from home", n.depth == null ? "none" : fmt(n.depth), n.depth >= DEEP ? "warn" : null),
        kv("Link score", fmt(n.score)),
        kv("Inlinks", fmt(n.inlinks)),
        kv("Pages below", fmt(n.size ? n.size - 1 : 0))),
      n.orphan ? h("div", { class: "gv-note" }, h("b", {}, "Not linked from any page"), h("span", {}, "It is in the sitemap, but no page links to it. Visitors and crawlers that follow links never reach it.")) : null,
      deepNote ? h("div", { class: "gv-note" }, h("b", {}, `Important, but ${plural(n.depth, "click")} deep`),
        h("span", {}, `This page is in your top ${topShare(site, n)}% by link score. ` + (route[1] && !route.some((p) => p.adopted) ? `A link from ${route[1].url} would bring it up to 2 clicks.` : "A link from a page near home would bring it closer."))) : null,
      route.length > 1 ? h("div", { class: "gv-sec" }, h("span", { class: "gv-eyebrow" }, "How a visitor gets there"),
        h("ol", { class: "gv-route" }, route.map((p, i) => [
          // The crawl reached it some other way (a redirect, a link we didn't keep).
          p.adopted ? h("li", { class: "gv-gap" }, h("span", { class: "gv-step" }, "…"), h("span", {}, "No link recorded for these clicks")) : null,
          h("li", { class: p === n ? "is-here" : null },
            h("span", { class: "gv-step" + (i === 0 ? " is-home" : "") }, String(p.depth ?? i)),
            p === n ? h("b", {}, nameOf(p, site.root)) : h("button", { type: "button", class: "gv-link", title: p.url, onclick: () => ctx.select(p) }, nameOf(p, site.root)))]))) : null,
      linked,
      h("div", { class: "gv-actions" },
        ctx.root && n.kids.length ? h("button", { type: "button", class: "btn btn-primary", onclick: () => ctx.root(n) }, ctx.rootLabel || "Start tree from here") : null,
        ctx.zoom ? h("button", { type: "button", class: "btn", onclick: () => ctx.zoom(n) }, "Zoom to") : null,
        h("a", { class: "btn", href: explorerHref(ctx.base, n) }, "Open in Explorer")));

    getJSON(`${ctx.pageSrc}?h=${n.hash}`)
      .then((p) => {
        if (!linked.isConnected) return;
        const head = h("span", { class: "gv-eyebrow" }, p.linked_from ? `Linked from ${plural(p.linked_from, "page")}` : "Linked from no page");
        const list = h("div", { class: "gv-list" }, p.links.map((l) => {
          const local = site.byHash.get(l.h);
          const label = local ? local.url : l.url;
          const anchor = l.anchor ? `“${l.anchor}”` : "no anchor text";
          const row = [h("span", { class: "gv-lu", title: l.url }, label), h("span", { class: "gv-la", title: l.anchor }, anchor + (l.nofollow ? " · nofollow" : ""))];
          return local ? h("button", { type: "button", onclick: () => ctx.select(local) }, row) : h("div", {}, row);
        }));
        const more = p.linked_from > p.links.length ? h("p", { class: "gv-muted" }, `The ${p.links.length} best-linked shown. `, h("a", { href: explorerHref(ctx.base, n) + "&tab=inlinks" }, "All inlinks in Explorer")) : null;
        fill(linked, head, p.links.length ? list : null, more);
      })
      .catch(() => linked.isConnected && fill(linked, h("span", { class: "gv-eyebrow" }, "Linked from"), h("p", { class: "gv-muted" }, "The links didn't load.")));
  }

  // ---------- shared: a tree of cards you can pan, zoom and open ----------

  // `cfg`: orient ("down" or "right"), size(d) -> [w, h] of a card, gap [breadth, depth]
  // between cards, kids(n) -> the open node's children in order, card(n, d) -> the card's
  // contents, link(d) -> classes and width for the line into d, overlay(t, layout) after any
  // move. Children past `limit` fold into a "+ N more" card.
  function cardTree(stage, cfg) {
    // The cards pan and zoom inside a view that starts right of `cfg.gutter`, kept for labels.
    const view = h("div", { class: "gv-view" });
    view.style.left = (cfg.gutter || 0) + "px";
    const layer = h("div", { class: "gv-layer" });
    const svg = d3.select(layer).append("svg").attr("class", "gv-links").attr("aria-hidden", "true");
    view.append(layer);
    stage.append(view);
    const cards = new Map();
    const limits = new Map();
    let t = d3.zoomIdentity, layout = null, first = true;

    const zoom = d3.zoom().scaleExtent([0.15, 2])
      .filter((e) => (e.type === "wheel" ? e.ctrlKey : !e.button && !e.target.closest("input, select")))
      .on("zoom", (e) => {
        t = e.transform;
        layer.style.transform = `translate(${t.x}px, ${t.y}px) scale(${t.k})`;
        cfg.overlay && layout && cfg.overlay(t, layout);
      });
    const sel = d3.select(view).call(zoom).on("dblclick.zoom", null);
    // A plain wheel or two-finger swipe pans; pinch (or ctrl + wheel) zooms.
    view.addEventListener("wheel", (e) => {
      if (e.ctrlKey) return;
      e.preventDefault();
      zoom.translateBy(sel, -e.deltaX / t.k, -e.deltaY / t.k);
    }, { passive: false });

    const limitOf = (n) => limits.get(n) || cfg.limit;
    function children(n) {
      if (n.more) return null;
      const kids = cfg.kids(n);
      if (!kids || !kids.length) return null;
      const lim = limitOf(n);
      if (kids.length <= lim) return kids;
      return [...kids.slice(0, lim), { more: true, of: n, count: kids.length - lim }];
    }
    // Where a node sits on screen axes: [x, y] of its card's top left.
    const at = (d) => {
      const [w, hh] = cfg.size(d);
      return cfg.orient === "down" ? [d.x - w / 2, d.y] : [d.y, d.x - hh / 2];
    };

    function draw(root, keep) {
      const before = keep && layout ? layout.find((d) => d.data === keep) : null;
      const b0 = before ? at(before) : null;
      const hier = d3.hierarchy(root, children);
      d3.tree().nodeSize(cfg.gap).separation((a, b) => (a.parent === b.parent ? 1 : 1.12))(hier);
      hier.each((d) => (d.y = d.depth * cfg.gap[1]));
      const nodes = hier.descendants();
      layout = { root: hier, nodes, find: (f) => nodes.find(f) };

      const seen = new Set();
      for (const d of nodes) {
        const key = d.data.more ? "m" + d.data.of.id : "n" + d.data.id;
        seen.add(key);
        let card = cards.get(key);
        const fresh = !card;
        if (fresh) {
          card = h("button", { type: "button", class: "gv-card" });
          card.addEventListener("click", () => cfg.click(card.__d.data, card.__d));
          cards.set(key, card);
          layer.append(card);
        }
        card.__d = d;
        const [w, hh] = cfg.size(d);
        card.style.width = w + "px";
        card.style.height = hh + "px";
        if (d.data.more) {
          card.className = "gv-card gv-more";
          fill(card, h("b", {}, `+ ${fmt(d.data.count)} more`), h("span", {}, `Show ${Math.min(d.data.count, cfg.limit * 2)} more`));
        } else cfg.card(d.data, d, card);
        const [x, y] = at(d);
        // New cards grow out of their parent's spot.
        if (fresh && d.parent && !first) {
          const [px, py] = at(d.parent);
          card.style.transform = `translate(${px}px, ${py}px)`;
          card.getBoundingClientRect();
        }
        card.style.transform = `translate(${x}px, ${y}px)`;
      }
      for (const [key, card] of cards) if (!seen.has(key)) {
        card.remove();
        cards.delete(key);
      }

      // Lines: plain first, the highlighted path on top.
      const links = hier.links().map((l) => ({ l, s: cfg.link(l.target) }));
      links.sort((a, b) => (a.s.on ? 1 : 0) - (b.s.on ? 1 : 0));
      svg.selectAll("path").data(links).join("path")
        .attr("class", (d) => "gv-line" + (d.s.cls ? " " + d.s.cls : ""))
        .attr("stroke-width", (d) => d.s.width || 1.5)
        .attr("d", ({ l }) => {
          const [sx, sy] = at(l.source), [sw, sh] = cfg.size(l.source);
          const [tx, ty] = at(l.target), [, th] = cfg.size(l.target);
          if (cfg.orient === "down") {
            const x0 = sx + sw / 2, y0 = sy + sh, x1 = l.target.x, y1 = ty, my = y1 - (y1 - y0) / 2;
            return `M${x0},${y0}V${my}H${x1}V${y1}`;
          }
          const x0 = sx + sw, y0 = sy + sh / 2, x1 = tx, y1 = ty + th / 2, mx = (x0 + x1) / 2;
          return `M${x0},${y0}C${mx},${y0} ${mx},${y1} ${x1},${y1}`;
        });

      if (b0) {
        const after = layout.find((d) => d.data === keep);
        if (after) {
          const b1 = at(after);
          sel.call(zoom.translateBy, b0[0] - b1[0], b0[1] - b1[1]);
        }
      }
      cfg.overlay && cfg.overlay(t, layout);
      first = false;
    }

    function bounds() {
      const xs = [], ys = [];
      for (const d of layout.nodes) {
        const [x, y] = at(d), [w, hh] = cfg.size(d);
        xs.push(x, x + w);
        ys.push(y, y + hh);
      }
      return [d3.min(xs), d3.min(ys), d3.max(xs), d3.max(ys)];
    }
    // Fits the tree, zooming out no further than `min` so cards stay readable; what doesn't fit
    // then starts at the root. Only the Fit button goes below that.
    function fit(ms = 600, min = READABLE) {
      if (!layout) return;
      const [x0, y0, x1, y1] = bounds();
      const W = view.clientWidth, H = view.clientHeight, pad = cfg.pad || [32, 32, 32, 32];
      const k = Math.max(min, Math.min(1, (W - pad[1] - pad[3]) / (x1 - x0), (H - pad[0] - pad[2]) / (y1 - y0)));
      const w = (x1 - x0) * k, hh = (y1 - y0) * k;
      const [rx, ry] = at(layout.root), [rw, rh] = cfg.size(layout.root);
      let tx = (W - w) / 2 - x0 * k, ty = pad[0] - y0 * k;
      if (w > W - pad[1] - pad[3]) tx = cfg.orient === "down" ? W / 2 - (rx + rw / 2) * k : pad[3] - x0 * k;
      if (hh > H - pad[0] - pad[2] && cfg.orient !== "down") ty = H / 2 - (ry + rh / 2) * k;
      const to = d3.zoomIdentity.translate(tx, ty).scale(k);
      (ms && !reduced() ? sel.transition().duration(ms) : sel).call(zoom.transform, to);
    }
    // Brings the node into view with the way down to it from the tree's root, when that fits at a
    // readable size; else the node alone. `opening` keeps scale 1 with the root at the top and
    // only slides sideways to the path when it fits across.
    function center(n, ms = 600, opening = false) {
      const d = layout && layout.find((d) => d.data === n);
      if (!d) return;
      const W = view.clientWidth, H = view.clientHeight;
      const box = (ds) => {
        const xs = [], ys = [];
        for (const e of ds) {
          const [x, y] = at(e), [w, hh] = cfg.size(e);
          xs.push(x, x + w);
          ys.push(y, y + hh);
        }
        return [d3.min(xs), d3.min(ys), d3.max(xs), d3.max(ys)];
      };
      const fits = ([x0, y0, x1, y1]) => Math.min(1, (W - 80) / (x1 - x0), (H - 80) / (y1 - y0));
      let b = box(d.ancestors());
      let k = fits(b), path = true;
      if (opening) {
        if (b[2] - b[0] > W - 80) return;
        k = 1;
      } else if (k < READABLE) {
        b = box([d]);
        k = Math.min(1, Math.max(t.k, READABLE));
        path = false;
      }
      // A top-down path reads from the top of the stage.
      const ty = path && cfg.orient === "down" ? 40 - b[1] * k : H / 2 - ((b[1] + b[3]) / 2) * k;
      const to = d3.zoomIdentity.translate(W / 2 - ((b[0] + b[2]) / 2) * k, ty).scale(k);
      (ms && !reduced() ? sel.transition().duration(ms) : sel).call(zoom.transform, to);
    }
    // Whether the node's card is on screen.
    function visible(n) {
      const d = layout && layout.find((d) => d.data === n);
      if (!d) return false;
      const [x, y] = at(d);
      const sx = t.x + x * t.k, sy = t.y + y * t.k;
      return sx > 0 && sy > 0 && sx < view.clientWidth - 60 && sy < view.clientHeight - 40;
    }
    return { draw, fit, center, visible, limits, limitOf, get layout() { return layout; } };
  }

  // Open the path down to `n`, raising child limits so it isn't folded away.
  function reveal(n, open, tree, kidsOf) {
    for (let c = n, p = n.parent; p; c = p, p = p.parent) {
      open.add(p);
      const kids = kidsOf(p);
      const i = kids.indexOf(c);
      if (i >= tree.limitOf(p)) tree.limits.set(p, i + 1);
    }
  }

  function wireSearch(el, list, find, go) {
    const q = el.querySelector('[data-ctl="q"]');
    if (!q) return;
    const dl = q.list;
    if (dl) fill(dl, ...list.slice(0, 3000).map((v) => h("option", { value: v })));
    const run = () => {
      const v = q.value.trim().toLowerCase();
      if (!v) return;
      const n = find(v);
      if (n) go(n);
      q.classList.toggle("is-miss", !n);
    };
    q.addEventListener("change", run);
    q.addEventListener("keydown", (e) => e.key === "Enter" && run());
  }

  function findPage(site, v) {
    return site.nodes.find((n) => n.url.toLowerCase() === v) ||
      site.nodes.find((n) => n.url.toLowerCase().includes(v)) ||
      site.nodes.find((n) => (n.title || "").toLowerCase().includes(v));
  }

  // ---------- the crawl tree: home at the top, one row per click ----------

  views.tree = (el) => load(el, (site) => crawlTree(el, site));

  function crawlTree(el, site) {
    const stage = el.querySelector('[data-role="stage"]');
    const panel = el.querySelector('[data-role="panel"]');
    const strip = el.querySelector('[data-role="unlinked"]');
    const ctl = (k) => el.querySelector(`[data-ctl="${k}"]`);
    const base = el.dataset.base;
    const CW = 140, CH = 84, RH = 64, ROW = 132, LIMIT = 10, GUTTER = 104;
    const bands = h("div", { class: "gv-bands", "aria-hidden": "true" });
    const labels = h("div", { class: "gv-blabels", "aria-hidden": "true" });
    labels.style.width = GUTTER + "px";
    const crumb = h("div", { class: "gv-float gv-crumb", hidden: true });
    stage.append(bands, labels, crumb);

    let top = site.root, selected = null, problems = false;
    const open = new Set();
    const order = (n) => {
      let kids = n.kids;
      if (problems) kids = kids.filter((c) => c.prob > 0);
      return kids.slice().sort((a, b) => b.score - a.score || b.size - a.size);
    };
    const kidsOf = (n) => (open.has(n) ? order(n) : null);
    const onPath = () => new Set(selected ? routeOf(selected) : []);
    let path = onPath();

    // Pages per click depth under the tree's top.
    let perDepth = new Map();
    const count = () => {
      perDepth = new Map();
      const stack = [top];
      while (stack.length) {
        const n = stack.pop();
        if (n.depth != null) perDepth.set(n.depth, (perDepth.get(n.depth) || 0) + 1);
        stack.push(...n.kids);
      }
    };

    const tree = cardTree(stage, {
      orient: "down",
      gap: [CW + 10, ROW],
      limit: LIMIT,
      gutter: GUTTER,
      pad: [28, 24, 28, 24],
      size: (d) => (d.data === site.root ? [150, RH] : [CW, d.data.more || !d.data.kids || !d.data.kids.length ? CH - 12 : CH]),
      kids: kidsOf,
      card: (n, d, card) => {
        const isTop = n === top, isHome = isTop && n === site.root;
        card.className = "gv-card gv-ccard" + (isHome ? " is-root" : "") + (n === selected ? " is-sel" : "") +
          (path.has(n) ? " is-path" : "") + (n.kids.length ? " has-kids" : "") + (open.has(n) ? " is-open" : "");
        card.title = n.url;
        const kids = [];
        if (n === selected && !isTop) kids.push(badge("selected", "ink"));
        else if (!isTop) {
          if (n.mix.broken) kids.push(badge(n.kids.length ? `${fmt(n.mix.broken)} broken` : "broken", "err"));
          else if (n.deep && n.kids.length) kids.push(badge(`${fmt(n.deep)} deep`, "warn"));
          else if (n.st === "redirect") kids.push(badge("redirect", "blue"));
          else if (n.st === "noindex") kids.push(badge("noindex", "muted"));
        }
        if (isHome) {
          kids.push(eyes(), h("span", { class: "gv-cn" }, h("b", {}, "Home"), h("span", { class: "gv-cu" }, n.url)));
        } else {
          kids.push(h("b", { class: "gv-cname" }, nameOf(n, site.root)), h("span", { class: "gv-cu" }, n.url));
          if (n.kids.length) kids.push(miniBar(n.mix), h("span", { class: "gv-cc" }, plural(n.size, "page")));
          else kids.push(h("span", { class: "gv-cc" }, `link score ${n.score}`));
        }
        if (n.kids.length) kids.push(h("span", { class: "gv-knob", "aria-hidden": "true" }, open.has(n) ? "−" : "+"));
        fill(card, ...kids);
        if (n.kids.length) card.setAttribute("aria-expanded", String(open.has(n)));
        else card.removeAttribute("aria-expanded");
      },
      link: (d) => (path.has(d.data) && path.has(d.parent.data) ? { cls: "is-on", width: 2.4, on: true } : {}),
      click: (n) => {
        if (n.more) {
          tree.limits.set(n.of, tree.limitOf(n.of) + LIMIT * 2);
          return redraw(n.of);
        }
        if (n === selected && n.kids.length) {
          if (open.has(n)) open.delete(n);
          else open.add(n);
          return redraw(n);
        }
        if (n.kids.length) open.add(n);
        select(n, n);
      },
      overlay: (t, layout) => {
        const levels = d3.max(layout.nodes, (d) => d.depth) + 1;
        const d0 = top.depth || 0;
        for (const box of [bands, labels]) while (box.children.length > levels) box.lastChild.remove();
        for (let i = 0; i < levels; i++) {
          let b = bands.children[i], l = labels.children[i];
          if (!b) bands.append((b = h("div", { class: "gv-band" })));
          if (!l) labels.append((l = h("div", { class: "gv-blabel" }, h("b"), h("span"))));
          const depth = d0 + i, y = t.y + (i * ROW - 22) * t.k;
          b.style.top = y + "px";
          b.style.height = ROW * t.k + "px";
          l.style.top = y + 8 + "px";
          l.classList.toggle("is-deep", depth >= DEEP);
          l.children[0].textContent = i === 0 && top === site.root ? "Start" : clicks(depth);
          l.children[1].textContent = i === 0 && top === site.root ? "" : plural(perDepth.get(depth) || 0, "page");
        }
      },
    });

    function redraw(keep) {
      path = onPath();
      tree.draw(top, keep);
    }
    function select(n, keep) {
      selected = n;
      if (n && !n.orphan && !routeOf(n).includes(top)) reroot(site.root, false);
      if (n && !n.orphan) reveal(n, open, tree, order);
      redraw(keep);
      if (n && !n.orphan && !keep && !tree.visible(n)) tree.center(n);
      for (const c of strip.querySelectorAll("[data-h]")) c.classList.toggle("is-sel", n && c.dataset.h === n.hash);
      showPanel();
    }
    function reroot(n, draw = true) {
      top = n;
      open.add(n);
      count();
      crumb.hidden = n === site.root;
      fill(crumb, h("span", {}, "Tree from"), h("b", {}, n.url), h("button", { type: "button", class: "btn btn-sm", onclick: () => reroot(site.root) }, "Show whole site"));
      if (draw) {
        redraw();
        tree.fit();
      }
    }
    function showPanel() {
      if (!selected) return sitePanel(panel, site);
      pagePanel(panel, site, selected, { base, pageSrc: el.dataset.page, select: (p) => select(p), root: (p) => reroot(p) });
    }

    // The unlinked pages, under the tree.
    function drawStrip(all) {
      const list = site.orphans;
      strip.hidden = !list.length;
      if (!list.length) return;
      const shown = all ? list.slice(0, 300) : list.slice(0, 12);
      fill(strip,
        h("div", { class: "gv-strip-head" },
          h("b", {}, "Not linked from any page ", h("span", {}, `${plural(list.length, "page")}, found only in the sitemap`)),
          h("a", { href: `${base}/graph/sitemap` }, "See in Sitemap")),
        h("div", { class: "gv-chips" },
          shown.map((n) => h("button", { type: "button", class: "gv-chip" + (n === selected ? " is-sel" : ""), "data-h": n.hash, title: n.title || n.url, onclick: () => select(n) }, n.url)),
          list.length > shown.length ? h("button", { type: "button", class: "gv-chip gv-chip-more", onclick: () => drawStrip(true) }, `+ ${fmt(list.length - shown.length)} more`) : null));
    }

    // First look: home and the pages a click away, plus the way down to the most important deep
    // page (or the page the link asked for).
    open.add(site.root);
    const asked = site.byHash.get(new URLSearchParams(location.search).get("sel") || "");
    const start = asked || importantDeep(site);
    count();
    if (start && !start.orphan) reveal(start, open, tree, order);
    selected = start || null;
    drawStrip(false);
    redraw();
    // Opens at full size, home centred at the top.
    tree.fit(0, 1);
    if (selected && !selected.orphan && !tree.visible(selected)) tree.center(selected, 0, true);
    showPanel();

    wireSearch(el, site.nodes.map((n) => n.url), (v) => findPage(site, v), (n) => select(n));
    ctl("fit").addEventListener("click", () => tree.fit(600, 0.15));
    const expand = ctl("expand");
    expand.addEventListener("click", () => {
      const all = expand.dataset.on !== "1";
      expand.dataset.on = all ? "1" : "";
      expand.textContent = all ? "Collapse all" : "Expand all";
      open.clear();
      open.add(top);
      if (all) {
        const stack = [top];
        while (stack.length) {
          const n = stack.pop();
          if (n.kids.length) open.add(n);
          stack.push(...n.kids);
        }
      }
      if (selected && !selected.orphan) reveal(selected, open, tree, order);
      redraw();
      tree.fit();
    });
    const prob = ctl("problems");
    prob.addEventListener("click", () => {
      problems = !problems;
      prob.setAttribute("aria-pressed", String(problems));
      if (problems) {
        const stack = [top];
        while (stack.length) {
          const n = stack.pop();
          if (n.kids.some((c) => c.prob)) open.add(n);
          stack.push(...n.kids.filter((c) => c.prob));
        }
      }
      redraw();
      tree.fit();
    });
  }

  // ---------- the folder tree: left to right, like a file explorer ----------

  views.folders = (el) => load(el, (site) => folderTree(el, site));

  // Folders by URL path. Each holds every page under it; pages whose path ends there are its own.
  function folderModel(site) {
    let id = 0;
    const mk = (path, level, parent) => ({
      id: id++, path, level, parent, sub: new Map(), own: [], count: 0, orphans: 0, deep: 0, score: 0,
      dsum: 0, dn: 0, best: null, mix: { ok: 0, noindex: 0, redirect: 0, broken: 0 },
    });
    const root = mk("/", 0, null);
    const add = (f, n) => {
      f.count++;
      f.mix[n.st]++;
      f.score += n.score;
      if (n.orphan) f.orphans++;
      if (n.depth != null) {
        f.dsum += n.depth;
        f.dn++;
        if (n.depth >= DEEP) f.deep++;
      }
      if (!f.best || n.score > f.best.score) f.best = n;
    };
    for (const n of site.nodes) {
      if (!n.url.startsWith("/")) continue;
      const segs = n.url.split(/[?#]/)[0].split("/").filter(Boolean);
      let f = root;
      add(f, n);
      for (const s of segs) {
        let next = f.sub.get(s);
        if (!next) f.sub.set(s, (next = mk(f.path === "/" ? "/" + s : f.path + "/" + s, f.level + 1, f)));
        f = next;
        add(f, n);
      }
      f.own.push(n);
    }
    // A folder shows the folders under it: every one at the top, deeper only those holding
    // more than one page.
    const all = [];
    const walk = (f) => {
      all.push(f);
      f.kids = [...f.sub.values()].filter((c) => f.level === 0 || c.count > 1).sort((a, b) => b.count - a.count || (a.path < b.path ? -1 : 1));
      f.prob = f.mix.broken + f.mix.redirect + f.mix.noindex + f.deep + f.orphans;
      for (const c of f.kids) walk(c);
    };
    walk(root);
    return { root, all };
  }

  function folderTree(el, site) {
    const stage = el.querySelector('[data-role="stage"]');
    const panel = el.querySelector('[data-role="panel"]');
    const ctl = (k) => el.querySelector(`[data-ctl="${k}"]`);
    const base = el.dataset.base;
    const { root, all } = folderModel(site);
    const CW = 260, CH = 40, COL = 310, LIMIT = 12;
    const heads = h("div", { class: "gv-heads", "aria-hidden": "true" });
    stage.append(heads);
    let selected = null, problems = false;
    const open = new Set([root]);
    const order = (f) => (problems ? f.kids.filter((c) => c.prob) : f.kids);
    const kidsOf = (f) => (open.has(f) ? order(f) : null);
    let path = new Set();
    const label = (f) => (f === root ? "Home" : f.level <= 2 ? f.path : "/" + f.path.slice(f.path.lastIndexOf("/") + 1));
    const unlinked = (f) => f !== root && f.orphans === f.count;

    function problem(f) {
      if (f.mix.broken) return badge(`${fmt(f.mix.broken)} broken`, "err");
      if (unlinked(f)) return badge("not linked", "warn");
      if (f.deep) return badge(`${fmt(f.deep)} deep`, "warn");
      if (f.mix.redirect) return badge(`${fmt(f.mix.redirect)} redirects`, "blue");
      if (f.mix.noindex === f.count) return badge("noindex", "muted");
      return null;
    }

    const tree = cardTree(stage, {
      orient: "right",
      gap: [CH + 10, COL],
      limit: LIMIT,
      pad: [44, 24, 24, 24],
      size: (d) => (d.data === root ? [150, 56] : [d.data.more ? 200 : CW, CH]),
      kids: kidsOf,
      card: (f, d, card) => {
        card.className = "gv-card gv-fcard" + (f === root ? " is-root" : "") + (f === selected ? " is-sel" : "") +
          (path.has(f) ? " is-path" : "") + (unlinked(f) ? " is-unlinked" : "");
        card.title = f.path;
        if (f === root) {
          fill(card, eyes(), h("span", { class: "gv-cn" }, h("b", {}, "Home"), h("span", { class: "gv-cu" }, plural(f.count, "URL"))));
          return;
        }
        const has = order(f).length > 0;
        fill(card,
          h("span", { class: "gv-chev", "aria-hidden": "true" }, has ? (open.has(f) ? "▾" : "▸") : ""),
          h("span", { class: "gv-fp" }, label(f)),
          problem(f),
          h("span", { class: "gv-fc" }, fmt(f.count)),
          miniBar(f.mix, "gv-mini gv-mini-s"));
        if (has) card.setAttribute("aria-expanded", String(open.has(f)));
        else card.removeAttribute("aria-expanded");
      },
      link: (d) => {
        const f = d.data;
        if (f.more) return { width: 1.4 };
        const width = 1.4 + 4.6 * Math.sqrt(f.count / Math.max(1, root.count));
        if (unlinked(f)) return { cls: "is-unlinked", width };
        return path.has(f) ? { cls: "is-on", width, on: true } : { width };
      },
      click: (f) => {
        if (f.more) {
          tree.limits.set(f.of, tree.limitOf(f.of) + LIMIT * 2);
          return redraw(f.of);
        }
        if (f === selected && order(f).length) {
          if (open.has(f)) open.delete(f);
          else open.add(f);
          return redraw(f);
        }
        if (order(f).length) open.add(f);
        select(f, f);
      },
      overlay: (t, layout) => {
        const levels = d3.max(layout.nodes, (d) => d.depth) + 1;
        const names = ["Site", "Folder", "Subfolder", "Next level"];
        while (heads.children.length > levels) heads.lastChild.remove();
        for (let i = 0; i < levels; i++) {
          let c = heads.children[i];
          if (!c) heads.append((c = h("span")));
          c.textContent = names[i] || `Level ${i}`;
          c.style.left = t.x + i * COL * t.k + "px";
        }
      },
    });

    function redraw(keep) {
      path = new Set();
      for (let f = selected; f; f = f.parent) path.add(f);
      tree.draw(root, keep);
    }
    function select(f, keep) {
      selected = f;
      if (f) reveal(f, open, tree, order);
      redraw(keep);
      if (f && !keep && !tree.visible(f)) tree.center(f);
      showPanel();
    }
    function showPanel() {
      if (!selected || selected === root) {
        return sitePanel(panel, site, h("p", { class: "gv-muted" }, `${plural(all.length - 1, "folder")} in your URLs. Pick one to see its pages and what to fix there.`));
      }
      const f = selected;
      const subs = f.kids.length;
      const own = f.count - f.kids.reduce((s, c) => s + c.count, 0);
      const fixes = [
        [f.mix.broken, "Broken pages", "Pages here fail to load. Fix them, or the links that lead to them."],
        [f.orphans, "Not linked from the site", "Found only in the sitemap. Link to them from a page in this folder."],
        [f.deep, `Pages ${DEEP}+ clicks deep`, "Hard to reach from home. Link them from a hub page closer to the top."],
        [f.mix.redirect, "Old URLs that redirect", "Pages here still get internal links. Point the links at the final URL."],
        [f.mix.noindex, "Noindex pages", "Fine if on purpose. They still take crawl time."],
      ].filter(([n]) => n > 0);
      const q = encodeURIComponent(f.path);
      fill(panel,
        h("div", { class: "gv-ph" },
          h("span", { class: "gv-eyebrow" }, "Selected folder"),
          h("h2", { class: "gv-mono" }, f.path),
          h("span", { class: "gv-muted" }, plural(f.count, "page") + (subs ? ` in ${plural(subs, "subfolder")}` : "") + (subs && own > 0 ? `, ${fmt(own)} directly in it` : ""))),
        h("div", { class: "gv-kv" },
          kv("Indexable", fmt(f.mix.ok)),
          kv("Average clicks", f.dn ? (f.dsum / f.dn).toFixed(1) : "none", f.dn && f.dsum / f.dn >= DEEP ? "warn" : null),
          kv("Share of pages", Math.round((100 * f.count) / Math.max(1, root.count)) + "%"),
          kv("Share of link score", Math.round((100 * f.score) / Math.max(1, root.score)) + "%")),
        h("div", { class: "gv-sec" }, h("span", { class: "gv-eyebrow" }, "What to fix here"),
          fixes.length ? fixes.map(([n, title, text]) => h("a", { class: "gv-fix", href: `${base}/explorer?q=${q}` }, h("span", { class: "gv-fixn" }, fmt(n)), h("span", {}, h("b", {}, title), h("span", {}, text)), h("span", { "aria-hidden": "true" }, "›")))
            : h("p", { class: "gv-muted" }, "Nothing to fix here.")),
        h("div", { class: "gv-actions" },
          h("a", { class: "btn btn-primary", href: `${base}/explorer?q=${q}` }, `List the ${plural(f.count, "page")}`),
          f.best && !f.best.orphan ? h("a", { class: "btn", href: `${base}/graph/tree?sel=${f.best.hash}` }, "View in crawl tree") : null));
    }

    // First look: home and the top folders, plus the way down to the folder of the most important
    // deep page.
    const deep = importantDeep(site);
    let start = null;
    if (deep && deep.url.startsWith("/")) {
      const segs = deep.url.split(/[?#]/)[0].split("/").filter(Boolean);
      let f = root;
      for (const s of segs) {
        const next = f.sub.get(s);
        if (!next || !f.kids.includes(next)) break;
        f = next;
      }
      if (f !== root) start = f;
    }
    selected = start;
    if (start) reveal(start, open, tree, order);
    redraw();
    tree.fit(0);
    if (selected && !tree.visible(selected)) tree.center(selected, 0);
    showPanel();

    wireSearch(el, all.slice(1).map((f) => f.path), (v) => all.find((f) => f.path.toLowerCase() === v) || all.find((f) => f.path.toLowerCase().includes(v)), (f) => select(f));
    ctl("fit").addEventListener("click", () => tree.fit(600, 0.15));
    const expand = ctl("expand");
    expand.addEventListener("click", () => {
      const on = expand.dataset.on !== "1";
      expand.dataset.on = on ? "1" : "";
      expand.textContent = on ? "Collapse all" : "Expand all";
      open.clear();
      open.add(root);
      if (on) for (const f of all) if (f.kids.length) open.add(f);
      if (selected) reveal(selected, open, tree, order);
      redraw();
      tree.fit();
    });
    const prob = ctl("problems");
    prob.addEventListener("click", () => {
      problems = !problems;
      prob.setAttribute("aria-pressed", String(problems));
      if (problems) for (const f of all) if (f.prob && order(f).length) open.add(f);
      redraw();
      tree.fit();
    });
  }

  // ---------- the bubble map: a force-directed picture of the whole site ----------

  views.map = (el) => load(el, (site) => bubbleMap(el, site));

  const SEC_L = ["#2a7de1", "#7a5af0", "#d9761c", "#c23a6a", "#1f9a8a", "#2fa36b", "#c79a12", "#6a726d"];
  const SEC_D = ["#6ea8ff", "#a48cff", "#f0a03c", "#ff7aa2", "#3ccfba", "#3fbf7f", "#f0c42c", "#9ba8a2"];

  function bubbleMap(el, site) {
    const stage = el.querySelector('[data-role="stage"]');
    const panel = el.querySelector('[data-role="panel"]');
    const canvas = stage.querySelector("canvas"), ctx = canvas.getContext("2d");
    const tip = stage.querySelector('[data-role="tip"]');
    const crumbEl = stage.querySelector('[data-role="crumb"]');
    const role = (r) => el.querySelector(`[data-role="${r}"]`);
    const ctl = (k) => el.querySelector(`[data-ctl="${k}"]`);
    const base = el.dataset.base;
    const FONT = getComputedStyle(document.body).fontFamily;
    let C = colours(el);

    // Pages that link to a page, and pages it links to, tree links included.
    for (const n of site.nodes) {
      n.inLinks = (n.parent ? [n.parent] : []).concat(n.extraIn);
      n.outLinks = n.kids.concat(n.extraOut);
    }
    // Sections: the seven biggest first path segments, the rest together.
    const secOf = (n) => (n.url.startsWith("/") ? n.url.split(/[/?#]/)[1] || "" : "");
    const secCount = d3.rollup(site.nodes.filter((n) => secOf(n)), (v) => v.length, secOf);
    const SECTIONS = [...secCount.entries()].sort((a, b) => b[1] - a[1]).slice(0, 7).map(([s]) => s);
    for (const n of site.nodes) {
      const i = SECTIONS.indexOf(secOf(n));
      n.sec = i < 0 ? SECTIONS.length : i;
    }
    const home = site.root;
    const R = 18 * Math.sqrt(site.nodes.length) + 140;
    const ORPHAN = { x: R * 0.9, y: R * 0.7 };

    let mode = "crawl", colorBy = "status", sizeBy = "score", depthLimit = 99, showAll = false;
    let tree = null, simNodes = [], sim = null, focusRoot = null;
    let hover = null, selected = null, related = null, transform = d3.zoomIdentity;
    let W = 0, H = 0, dpr = 1, dirty = true, autoFitted = false;
    const folders = new Map();

    // The tree the map hangs on: crawl parents, or URL folders with a node for each folder
    // that has no page of its own.
    function buildTree() {
      const parentOf = new Map(), kids = new Map();
      const list = site.nodes.slice();
      if (mode === "crawl") {
        for (const n of list) parentOf.set(n, n.parent);
      } else {
        const ensure = (path) => {
          if (path === "" || path === "/") return home;
          const real = site.byUrl.get(path) || site.byUrl.get(path + "/");
          if (real && real !== home) return real;
          let f = folders.get(path);
          if (!f) {
            f = { id: "f" + path, url: path + "/", title: path.split("/").pop() + "/", sec: SECTIONS.indexOf(path.split("/")[1]), st: "folder", virtual: true, depth: null, score: 1, inlinks: 0, inLinks: [], outLinks: [], kids: [] };
            if (f.sec < 0) f.sec = SECTIONS.length;
            folders.set(path, f);
          }
          if (!parentOf.has(f)) {
            list.push(f);
            parentOf.set(f, ensure(path.slice(0, path.lastIndexOf("/"))));
          }
          return f;
        };
        for (const n of site.nodes) {
          if (n === home) {
            parentOf.set(n, null);
            continue;
          }
          if (!n.url.startsWith("/")) {
            parentOf.set(n, home);
            continue;
          }
          const p = n.url.split(/[?#]/)[0].replace(/\/$/, "");
          parentOf.set(n, ensure(p.slice(0, p.lastIndexOf("/"))));
        }
      }
      for (const n of list) kids.set(n, []);
      for (const n of list) {
        const p = parentOf.get(n);
        if (p) kids.get(p).push(n);
      }
      const tdepth = new Map([[home, 0]]);
      const stack = [home];
      while (stack.length) {
        const n = stack.pop();
        for (const c of kids.get(n)) {
          tdepth.set(c, tdepth.get(n) + 1);
          stack.push(c);
        }
      }
      return { list, parentOf, kids, tdepth };
    }

    function descendants(n) {
      const out = [n];
      for (let i = 0; i < out.length; i++) for (const c of tree.kids.get(out[i]) || []) out.push(c);
      return out;
    }

    // ---------- encodings ----------
    const ramp = () => (C.dark ? d3.interpolateRgb("#f0c42c", "#2a7de1") : d3.interpolateRgb("#1f8a55", "#e0ae14"));
    function radius(n) {
      if (n.virtual) return 3;
      if (n === home) return 14;
      if (sizeBy === "equal") return 4.5;
      const v = sizeBy === "score" ? n.score / 100 : Math.min(1, n.inlinks / 40);
      return 3 + 13 * Math.sqrt(v);
    }
    function nodeColor(n) {
      if (n.virtual) return { fill: C.surface2, stroke: C.muted };
      if (n === home) return { fill: C.ink, stroke: null };
      if (colorBy === "section") {
        const pal = C.dark ? SEC_D : SEC_L;
        return { fill: pal[Math.min(n.sec, pal.length - 1)], stroke: n.orphan ? C.gOrphan : null };
      }
      if (colorBy === "depth") {
        if (n.depth == null) return { fill: C.surface2, stroke: C.gOrphan };
        return { fill: ramp()(Math.min(1, (n.depth - 1) / 6)), stroke: null };
      }
      // Indexability, fading with depth the way a crawl diagram reads.
      if (n.st === "noindex") return { fill: C.surface2, stroke: C.gNoindex };
      const col = { ok: C.gOk, redirect: C.gRedirect, broken: C.gBroken }[n.st];
      const fade = n.depth ? Math.min(0.5, (n.depth - 1) * 0.085) : 0;
      return { fill: d3.interpolateRgb(col, C.surface2)(fade), stroke: n.orphan ? C.gOrphan : null };
    }
    function refreshStyle() {
      for (const n of tree.list) {
        n.r = radius(n);
        const c = nodeColor(n);
        n.fill = c.fill;
        n.stroke = c.stroke;
      }
      renderLegend();
      dirty = true;
    }
    function visibleBase(n) {
      if (n.virtual) return mode === "dir";
      if (n.depth == null) return depthLimit >= 99;
      return n.depth <= depthLimit;
    }

    // ---------- simulation ----------
    const pinOrphan = (n) => n.orphan && !focusRoot && mode === "crawl";
    function startSim(keep) {
      const all = focusRoot ? descendants(focusRoot) : tree.list;
      simNodes = all;
      const set = new Set(all);
      const edges = [];
      for (const n of all) {
        const p = tree.parentOf.get(n);
        if (p && set.has(p)) edges.push({ source: p, target: n });
      }
      tree.edges = edges;
      if (!keep) seedRadial(all);
      else for (const n of all) if (n.x === undefined) {
        const p = tree.parentOf.get(n);
        n.x = ((p && p.x) || 0) + (Math.random() - 0.5) * 20;
        n.y = ((p && p.y) || 0) + (Math.random() - 0.5) * 20;
      }
      const root = focusRoot || home;
      for (const n of all) n.fx = n.fy = null;
      root.fx = root.x = focusRoot ? root.x : 0;
      root.fy = root.y = focusRoot ? root.y : 0;
      const kidsN = (n) => (tree.kids.get(n) || []).length;
      if (sim) sim.stop();
      sim = d3.forceSimulation(all)
        .alphaDecay(0.028).velocityDecay(0.38)
        .force("link", d3.forceLink(edges).distance((l) => 10 + l.source.r + l.target.r + 2.2 * Math.sqrt(kidsN(l.source))).strength(0.9).iterations(2))
        .force("charge", d3.forceManyBody().strength((n) => (n.orphan ? -14 : -22 - n.r * 2.4)).distanceMax(320).theta(0.9))
        .force("collide", d3.forceCollide((n) => n.r + 1.6).strength(0.9))
        .force("x", d3.forceX((n) => (pinOrphan(n) ? ORPHAN.x : 0)).strength((n) => (pinOrphan(n) ? 0.14 : 0.012)))
        .force("y", d3.forceY((n) => (pinOrphan(n) ? ORPHAN.y : 0)).strength((n) => (pinOrphan(n) ? 0.14 : 0.012)))
        .stop();
      if (reduced() || all.length > 3000) {
        for (let i = 0, m = all.length > 3000 ? 160 : 320; i < m; i++) sim.tick();
        fit(0);
        dirty = true;
        if (!reduced()) sim.alpha(0.12).restart().on("tick", () => (dirty = true));
      } else {
        sim.alpha(keep ? 0.55 : 0.9).restart();
        sim.on("tick", () => {
          dirty = true;
          if (!autoFitted && sim.alpha() < 0.12) {
            autoFitted = true;
            fit(900);
          }
        });
      }
    }
    function seedRadial(all) {
      const root = focusRoot || home;
      const leaves = [];
      const stack = [root];
      while (stack.length) {
        const n = stack.pop();
        const k = tree.kids.get(n) || [];
        if (!k.length) leaves.push(n);
        for (let i = k.length - 1; i >= 0; i--) stack.push(k[i]);
      }
      const ang = new Map();
      leaves.forEach((n, i) => ang.set(n, (i / leaves.length) * Math.PI * 2));
      const order = descendants(root);
      for (let i = order.length - 1; i >= 0; i--) {
        const n = order[i], k = tree.kids.get(n) || [];
        if (k.length) ang.set(n, d3.mean(k, (c) => ang.get(c)));
      }
      const d0 = tree.tdepth.get(root) || 0;
      for (const n of all) {
        if (pinOrphan(n)) {
          n.x = ORPHAN.x + (Math.random() - 0.5) * 60;
          n.y = ORPHAN.y + (Math.random() - 0.5) * 60;
          continue;
        }
        const d = (tree.tdepth.get(n) ?? 0) - d0, a = ang.get(n) ?? 0;
        n.x = (root.x || 0) + Math.cos(a) * d * 64;
        n.y = (root.y || 0) + Math.sin(a) * d * 64;
        n.vx = n.vy = 0;
      }
    }

    // ---------- highlight ----------
    function computeRelated() {
      const f = hover || selected;
      if (!f) {
        related = null;
        dirty = true;
        return;
      }
      const set = new Set([f]);
      for (let p = tree.parentOf.get(f); p; p = tree.parentOf.get(p)) set.add(p);
      for (const c of tree.kids.get(f) || []) set.add(c);
      for (const s of f.inLinks) set.add(s);
      for (const t of f.outLinks) set.add(t);
      related = { f, set };
      dirty = true;
    }
    function targetVis(n) {
      if (!visibleBase(n)) return 0;
      if (!related) return 1;
      return related.set.has(n) ? 1 : 0.13;
    }

    // ---------- drawing ----------
    function resize() {
      const r = stage.getBoundingClientRect();
      dpr = Math.min(2, window.devicePixelRatio || 1);
      W = r.width;
      H = r.height;
      canvas.width = Math.round(W * dpr);
      canvas.height = Math.round(H * dpr);
      dirty = true;
    }
    const orphanZone = () => !focusRoot && depthLimit >= 99 && mode === "crawl" && site.orphans.length > 0;
    const orphanR = () => 40 + 7 * Math.sqrt(site.orphans.length);
    function draw() {
      const k = transform.k;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.fillStyle = C.surface2;
      ctx.fillRect(0, 0, W, H);
      ctx.translate(transform.x, transform.y);
      ctx.scale(k, k);

      // The unlinked pages' corner.
      if (orphanZone()) {
        ctx.save();
        ctx.setLineDash([4 / k, 5 / k]);
        ctx.strokeStyle = C.gOrphan;
        ctx.globalAlpha = related ? 0.25 : 0.7;
        ctx.lineWidth = 1.2 / k;
        const rr = orphanR();
        ctx.beginPath();
        ctx.arc(ORPHAN.x, ORPHAN.y, rr, 0, Math.PI * 2);
        ctx.stroke();
        ctx.restore();
        ctx.font = `600 ${12 / Math.max(k, 0.6)}px ${FONT}`;
        ctx.fillStyle = C.gOrphan;
        ctx.textAlign = "center";
        ctx.globalAlpha = related ? 0.3 : 1;
        ctx.fillText("Not linked from any page", ORPHAN.x, ORPHAN.y + rr + 20);
        ctx.globalAlpha = 1;
      }

      // Tree lines.
      ctx.lineCap = "round";
      ctx.strokeStyle = C.ghost2;
      ctx.lineWidth = 1 / Math.max(k, 0.5);
      if (!related) {
        ctx.beginPath();
        for (const e of tree.edges) {
          if (e.source.vis < 0.02 || e.target.vis < 0.02) continue;
          ctx.moveTo(e.source.x, e.source.y);
          ctx.lineTo(e.target.x, e.target.y);
        }
        ctx.globalAlpha = 0.9;
        ctx.stroke();
      } else {
        for (const e of tree.edges) {
          const a = Math.min(e.source.vis, e.target.vis);
          if (a < 0.02) continue;
          ctx.globalAlpha = a * 0.9;
          ctx.beginPath();
          ctx.moveTo(e.source.x, e.source.y);
          ctx.lineTo(e.target.x, e.target.y);
          ctx.stroke();
        }
      }
      // Every other link, faint, on request.
      if (showAll && mode === "crawl") {
        ctx.globalAlpha = related ? 0.05 : 0.14;
        ctx.strokeStyle = C.muted;
        ctx.lineWidth = 0.6 / Math.max(k, 0.5);
        ctx.beginPath();
        for (const n of simNodes) for (const t of n.extraOut || []) {
          if (t.vis < 0.5 || n.vis < 0.5 || t.x === undefined) continue;
          ctx.moveTo(n.x, n.y);
          ctx.quadraticCurveTo((n.x + t.x) / 2 + (t.y - n.y) * 0.15, (n.y + t.y) / 2 - (t.x - n.x) * 0.15, t.x, t.y);
        }
        ctx.stroke();
      }
      // The focus: its route from home in ink, the pages linking to it in accent.
      if (related) {
        const f = related.f;
        ctx.globalAlpha = 1;
        ctx.strokeStyle = C.accentStrong;
        ctx.lineWidth = 1.6 / Math.max(k, 0.5);
        ctx.beginPath();
        for (const s of f.inLinks) {
          if (s.x === undefined || !s.vis) continue;
          ctx.moveTo(s.x, s.y);
          ctx.quadraticCurveTo((s.x + f.x) / 2 + (f.y - s.y) * 0.18, (s.y + f.y) / 2 - (f.x - s.x) * 0.18, f.x, f.y);
        }
        ctx.stroke();
        ctx.strokeStyle = C.ink;
        ctx.lineWidth = 2.4 / Math.max(k, 0.5);
        ctx.beginPath();
        let c = f;
        for (let p = tree.parentOf.get(c); p; c = p, p = tree.parentOf.get(p)) {
          if (p.x === undefined) break;
          ctx.moveTo(c.x, c.y);
          ctx.lineTo(p.x, p.y);
        }
        ctx.stroke();
      }

      // Bubbles.
      for (const n of simNodes) {
        const a = n.vis;
        if (a < 0.02) continue;
        ctx.globalAlpha = a;
        ctx.beginPath();
        ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
        ctx.fillStyle = n.fill;
        ctx.fill();
        if (n.stroke) {
          ctx.lineWidth = (n.virtual ? 1 : 1.5) / Math.max(k, 0.7);
          ctx.strokeStyle = n.stroke;
          if (n.orphan) ctx.setLineDash([2.5 / Math.max(k, 0.7), 2 / Math.max(k, 0.7)]);
          ctx.stroke();
          ctx.setLineDash([]);
        }
      }
      // Home wears the CodoSEO eyes.
      if (home.vis > 0.02 && simNodes.includes(home)) {
        ctx.globalAlpha = home.vis;
        ctx.strokeStyle = C.surface2;
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.arc(home.x - 4.6, home.y, 3.8, 0, Math.PI * 2);
        ctx.moveTo(home.x + 8.4, home.y);
        ctx.arc(home.x + 4.6, home.y, 3.8, 0, Math.PI * 2);
        ctx.stroke();
        ctx.fillStyle = C.accent;
        ctx.beginPath();
        ctx.arc(home.x - 3.8, home.y - 0.6, 1.7, 0, Math.PI * 2);
        ctx.arc(home.x + 5.4, home.y - 0.6, 1.7, 0, Math.PI * 2);
        ctx.fill();
      }
      // Selection rings.
      for (const [n, col] of [[selected, C.ink], [hover, C.accentStrong]]) {
        if (!n || !n.vis || n.x === undefined) continue;
        ctx.globalAlpha = 1;
        ctx.strokeStyle = col;
        ctx.lineWidth = 2.4 / Math.max(k, 0.6);
        ctx.beginPath();
        ctx.arc(n.x, n.y, n.r + 3.5 / Math.max(k, 0.6), 0, Math.PI * 2);
        ctx.stroke();
      }

      // Labels, more of them the closer you zoom.
      ctx.font = `500 ${11.5 / k}px ${FONT}`;
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.lineJoin = "round";
      for (const n of simNodes) {
        if (n.vis < 0.5) continue;
        const big = n.r * k >= 9.5;
        const rel = related && related.set.has(n) && (related.set.size < 40 || n === related.f || tree.parentOf.get(related.f) === n);
        if (!big && !rel && n !== selected) continue;
        const label = n === home ? "Home" : n.virtual ? n.title : n.url.split(/[?#]/)[0].split("/").filter(Boolean).pop() || n.url;
        const x = n.x + n.r + 4 / k, y = n.y;
        ctx.globalAlpha = n.vis;
        ctx.lineWidth = 3.5 / k;
        ctx.strokeStyle = C.surface2;
        ctx.strokeText(label, x, y);
        ctx.fillStyle = n === related?.f ? C.ink : C.muted;
        ctx.fillText(label, x, y);
      }
      ctx.globalAlpha = 1;
    }

    function loop() {
      if (!canvas.isConnected) {
        if (sim) sim.stop();
        return;
      }
      let anim = false;
      for (const n of simNodes) {
        const t = targetVis(n);
        if (n.vis === undefined) n.vis = t;
        const d = t - n.vis;
        if (Math.abs(d) > 0.005) {
          n.vis += reduced() ? d : d * 0.2;
          anim = true;
        } else n.vis = t;
      }
      if (dirty || anim) {
        draw();
        dirty = false;
      }
      requestAnimationFrame(loop);
    }

    // ---------- zoom, drag, hit test ----------
    const zoom = d3.zoom().scaleExtent([0.08, 10]).on("zoom", (e) => {
      transform = e.transform;
      dirty = true;
      hideTip();
    });
    function findAt(px, py) {
      const [x, y] = transform.invert([px, py]);
      let best = null, bd = Infinity;
      for (const n of simNodes) {
        if (n.vis < 0.5) continue;
        const dx = n.x - x, dy = n.y - y, d = dx * dx + dy * dy, rr = n.r + 4 / transform.k;
        if (d < rr * rr && d < bd) {
          bd = d;
          best = n;
        }
      }
      return best;
    }
    const drag = d3.drag()
      .subject((e) => {
        const n = findAt(e.x, e.y);
        return n ? { node: n, x: e.x, y: e.y } : null;
      })
      .on("start", (e) => {
        canvas.classList.add("is-dragging");
        if (!e.active && !reduced()) sim.alphaTarget(0.25).restart();
        const n = e.subject.node;
        n.fx = n.x;
        n.fy = n.y;
      })
      .on("drag", (e) => {
        const n = e.subject.node;
        const [x, y] = transform.invert([e.x, e.y]);
        n.fx = x;
        n.fy = y;
        if (reduced()) {
          n.x = x;
          n.y = y;
        }
        dirty = true;
      })
      .on("end", (e) => {
        canvas.classList.remove("is-dragging");
        if (!e.active) sim.alphaTarget(0);
        const n = e.subject.node;
        if (n !== home && n !== focusRoot) n.fx = n.fy = null;
      });
    d3.select(canvas).call(drag).call(zoom).on("dblclick.zoom", null);

    const at = (e) => {
      const r = canvas.getBoundingClientRect();
      return [e.clientX - r.left, e.clientY - r.top];
    };
    canvas.addEventListener("pointermove", (e) => {
      const [x, y] = at(e);
      const n = findAt(x, y);
      canvas.classList.toggle("is-over", !!n);
      if (n !== hover) {
        hover = n;
        computeRelated();
      }
      if (n) showTip(n, x, y);
      else hideTip();
    });
    canvas.addEventListener("pointerleave", () => {
      if (hover) {
        hover = null;
        computeRelated();
      }
      hideTip();
    });
    canvas.addEventListener("click", (e) => select(findAt(...at(e))));
    canvas.addEventListener("dblclick", (e) => {
      const n = findAt(...at(e));
      if (n && (tree.kids.get(n) || []).length) focusOn(n);
    });
    const onKey = (e) => {
      if (!canvas.isConnected) return document.removeEventListener("keydown", onKey);
      if (e.key !== "Escape" || e.target.closest("input, select, textarea")) return;
      if (focusRoot) unfocus();
      else select(null);
    };
    document.addEventListener("keydown", onKey);

    function showTip(n, x, y) {
      fill(tip,
        h("b", {}, n.virtual ? "Folder" : n.title || nameOf(n, home)),
        h("span", { class: "gv-tu" }, n.url),
        h("span", { class: "gv-tm" }, n.virtual
          ? plural(descendants(n).length - 1, "page") + " inside"
          : `${statusLabel(n)} · ${n.depth == null ? "not linked" : clicks(n.depth)} · link score ${n.score} · ${plural(n.inlinks, "inlink")}`));
      tip.hidden = false;
      const tw = tip.offsetWidth, th = tip.offsetHeight;
      tip.style.left = Math.max(8, Math.min(W - tw - 10, x + 14)) + "px";
      tip.style.top = (y + th + 20 > H ? y - th - 12 : y + 16) + "px";
    }
    function hideTip() {
      tip.hidden = true;
    }

    // ---------- camera ----------
    function fit(ms = 750, nodes) {
      const ns = (nodes || simNodes).filter((n) => visibleBase(n) && n.x !== undefined);
      if (!ns.length || !W) return;
      let x0 = d3.min(ns, (n) => n.x - n.r), x1 = d3.max(ns, (n) => n.x + n.r);
      let y0 = d3.min(ns, (n) => n.y - n.r), y1 = d3.max(ns, (n) => n.y + n.r);
      if (orphanZone()) {
        const rr = orphanR() + 28;
        x0 = Math.min(x0, ORPHAN.x - rr);
        x1 = Math.max(x1, ORPHAN.x + rr);
        y0 = Math.min(y0, ORPHAN.y - rr);
        y1 = Math.max(y1, ORPHAN.y + rr);
      }
      const pad = 50, k = Math.min(4, (W - pad * 2) / Math.max(1, x1 - x0), (H - pad * 2) / Math.max(1, y1 - y0));
      const t = d3.zoomIdentity.translate(W / 2, H / 2).scale(k).translate(-(x0 + x1) / 2, -(y0 + y1) / 2);
      const sel = d3.select(canvas);
      if (ms && !reduced()) sel.transition().duration(ms).ease(d3.easeCubicInOut).call(zoom.transform, t);
      else sel.call(zoom.transform, t);
    }
    function zoomTo(n) {
      if (n.x === undefined) return;
      const k = Math.max(transform.k, 2.2);
      const t = d3.zoomIdentity.translate(W / 2, H / 2).scale(k).translate(-n.x, -n.y);
      d3.select(canvas).transition().duration(reduced() ? 0 : 800).ease(d3.easeCubicInOut).call(zoom.transform, t);
    }

    // ---------- focus: re-root on a branch ----------
    function focusOn(n) {
      focusRoot = n;
      selected = n;
      hover = null;
      crumbEl.hidden = false;
      role("crumb-url").textContent = n.url;
      startSim(true);
      computeRelated();
      renderPanel();
      renderStats();
      setTimeout(() => fit(800), reduced() ? 0 : 350);
    }
    function unfocus() {
      const was = focusRoot;
      focusRoot = null;
      crumbEl.hidden = true;
      startSim(true);
      computeRelated();
      renderPanel();
      renderStats();
      setTimeout(() => fit(800), reduced() ? 0 : 350);
      if (was) select(was);
    }
    ctl("unfocus").addEventListener("click", unfocus);

    // ---------- panel, legend, stats ----------
    function select(n) {
      selected = n;
      computeRelated();
      renderPanel();
    }
    function renderPanel() {
      const n = selected;
      if (!n) {
        return sitePanel(panel, site, [
          h("p", { class: "gv-muted" }, "Each bubble is a page. Lines show the first link the crawler followed to reach it, from the home page in the middle."),
          h("p", { class: "gv-muted" }, "Hover a bubble to light up its route from home and the pages that link to it. Double-click a section to explore just that branch."),
        ]);
      }
      if (n.virtual) {
        const kids = tree.kids.get(n) || [];
        fill(panel,
          h("div", { class: "gv-ph" }, h("span", { class: "gv-eyebrow" }, "Folder"), h("h2", { class: "gv-mono" }, n.url)),
          h("p", { class: "gv-muted" }, "No page of its own: nothing answers at this URL."),
          h("div", { class: "gv-kv" }, kv("Pages inside", fmt(descendants(n).length - 1)), kv("Subfolders", fmt(kids.filter((c) => c.virtual || (tree.kids.get(c) || []).length).length))),
          kids.length ? h("div", { class: "gv-actions" },
            h("button", { type: "button", class: "btn btn-primary", onclick: () => focusOn(n) }, `Explore this branch (${fmt(descendants(n).length - 1)})`),
            h("button", { type: "button", class: "btn", onclick: () => zoomTo(n) }, "Zoom to")) : null);
        return;
      }
      pagePanel(panel, site, n, {
        base, pageSrc: el.dataset.page,
        select: (p) => {
          if (focusRoot && !descendants(focusRoot).includes(p)) unfocus();
          select(p);
          zoomTo(p);
        },
        root: (tree.kids.get(n) || []).length ? (p) => focusOn(p) : null,
        rootLabel: `Explore this branch (${fmt(descendants(n).length - 1)})`,
        zoom: zoomTo,
      });
    }
    function dot(fill, stroke, dash) {
      const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      s.setAttribute("width", 14);
      s.setAttribute("height", 14);
      s.setAttribute("aria-hidden", "true");
      const c = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      for (const [k, v] of Object.entries({ cx: 7, cy: 7, r: 5, fill })) c.setAttribute(k, v);
      if (stroke) {
        c.setAttribute("stroke", stroke);
        c.setAttribute("stroke-width", 1.5);
      }
      if (dash) c.setAttribute("stroke-dasharray", "2 2");
      s.append(c);
      return s;
    }
    function renderLegend() {
      const lg = role("legend");
      const row = (s, t) => h("div", { class: "gv-lrow" }, s, t);
      fill(lg);
      if (colorBy === "status") {
        lg.append(row(dot(C.gOk), "Indexable (lighter is deeper)"), row(dot(C.surface2, C.gNoindex), "Noindex or canonicalised"), row(dot(C.gRedirect), "Redirect"), row(dot(C.gBroken), "Broken"), row(dot(C.gOk, C.gOrphan, true), "Not linked"));
      } else if (colorBy === "depth") {
        lg.append(...[1, 3, 5, 7].map((d) => row(dot(ramp()(Math.min(1, (d - 1) / 6))), d === 7 ? "7+ clicks" : clicks(d))), row(dot(C.surface2, C.gOrphan), "Not linked"));
      } else {
        const pal = C.dark ? SEC_D : SEC_L;
        lg.append(...SECTIONS.map((s, i) => row(dot(pal[i]), "/" + s)));
        if (site.nodes.some((n) => n.sec === SECTIONS.length && n !== home)) lg.append(row(dot(pal[pal.length - 1]), "Everything else"));
      }
      if (mode === "dir") lg.append(row(dot(C.surface2, C.muted), "Folder with no page of its own"));
    }
    function renderStats() {
      const ns = simNodes.filter((n) => !n.virtual && visibleBase(n));
      const st = role("stats");
      fill(st,
        h("span", {}, h("b", {}, fmt(ns.length)), " pages shown"),
        h("span", {}, h("b", {}, fmt(ns.filter((n) => n.st === "broken").length)), " broken"),
        h("span", {}, h("b", {}, fmt(ns.filter((n) => (n.depth || 0) >= DEEP).length)), ` at ${DEEP}+ clicks`),
        site.capped && site.capped.pages ? h("span", {}, `of ${fmt(site.stats.pages)} crawled`) : null);
    }

    // ---------- controls ----------
    function setMode(m) {
      if (m === mode) return;
      mode = m;
      focusRoot = null;
      crumbEl.hidden = true;
      for (const b of el.querySelectorAll('[data-ctl="mode"]')) b.setAttribute("aria-pressed", String(b.dataset.mode === m));
      tree = buildTree();
      refreshStyle();
      autoFitted = false;
      startSim(true);
      computeRelated();
      renderPanel();
      renderStats();
    }
    for (const b of el.querySelectorAll('[data-ctl="mode"]')) b.addEventListener("click", () => setMode(b.dataset.mode));
    ctl("color").addEventListener("change", (e) => {
      colorBy = e.target.value;
      refreshStyle();
    });
    ctl("size").addEventListener("change", (e) => {
      sizeBy = e.target.value;
      refreshStyle();
      sim.force("collide").radius((n) => n.r + 1.6);
      if (!reduced()) sim.alpha(0.35).restart();
    });
    const depthIn = ctl("depth"), depthOut = ctl("depth-out");
    depthIn.max = Math.max(2, site.maxDepth + 1);
    depthIn.value = depthIn.max;
    depthIn.addEventListener("input", () => {
      const v = +depthIn.value;
      depthLimit = v >= +depthIn.max ? 99 : v;
      depthOut.textContent = depthLimit >= 99 ? "All" : "≤ " + v;
      dirty = true;
      renderStats();
    });
    const linksBtn = ctl("links");
    if (!site.nodes.some((n) => n.extraOut.length)) linksBtn.hidden = true;
    linksBtn.addEventListener("click", () => {
      showAll = !showAll;
      linksBtn.setAttribute("aria-pressed", String(showAll));
      linksBtn.textContent = showAll ? "Hide extra links" : "Show all links";
      dirty = true;
    });
    ctl("fit").addEventListener("click", () => fit(750));
    wireSearch(el, site.nodes.map((n) => n.url), (v) => findPage(site, v), (n) => {
      if (focusRoot && !descendants(focusRoot).includes(n)) unfocus();
      if (!visibleBase(n)) {
        depthIn.value = depthIn.max;
        depthIn.dispatchEvent(new Event("input"));
      }
      select(n);
      zoomTo(n);
    });

    // ---------- theme and size ----------
    onTheme(el, () => {
      C = colours(el);
      if (tree) {
        refreshStyle();
        renderPanel();
      }
    });
    const ro = new ResizeObserver(() => {
      if (!canvas.isConnected) return ro.disconnect();
      const firstSize = !W;
      resize();
      if (firstSize) fit(0);
    });
    ro.observe(stage);

    // ---------- boot ----------
    resize();
    tree = buildTree();
    refreshStyle();
    startSim(false);
    renderPanel();
    renderStats();
    fit(0);
    if (!reduced()) setTimeout(() => {
      if (!autoFitted) {
        autoFitted = true;
        fit(900);
      }
    }, 2200);
    requestAnimationFrame(loop);
  }
})();
