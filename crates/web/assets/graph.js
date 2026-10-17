// The site graph's in-browser views (crawl tree, folder tree, bubble map), drawn with d3 from
// `/s/{site}/graph/data.json`. Each view mounts on an element with `data-graph="<view>"`.
(() => {
  "use strict";
  const views = {};
  function mount(root) {
    for (const el of root.querySelectorAll("[data-graph]")) {
      if (el.dataset.graphMounted) continue;
      const view = views[el.dataset.graph];
      if (!view) continue;
      el.dataset.graphMounted = "1";
      view(el);
    }
  }
  document.addEventListener("DOMContentLoaded", () => mount(document));
  document.addEventListener("htmx:load", (e) => mount(e.target));
})();
