// The cloud landing page (templates/landing/index.html): theme toggle, the live audit demo in
// the hero, the peeking mascot, the interactive site map, the AI agent prompt, analytics events
// and the live pages-audited counter. The theme is applied before paint by an inline script.
(function () {
  // Theme: the viewer's choice wins, then the system setting.
  /* ---------- theme toggle: light -> dark -> system, applied before paint by the inline script in <head> ---------- */
  var doc = document.documentElement, NEXT = { light: 'dark', dark: 'system', system: 'light' };
  var sysDark = window.matchMedia ? matchMedia('(prefers-color-scheme: dark)') : null;
  function applyTheme(pref) {
    var t = pref === 'dark' || (pref === 'system' && sysDark && sysDark.matches) ? 'dark' : 'light';
    doc.setAttribute('data-theme', t);
    doc.setAttribute('data-theme-pref', pref);
    var metas = document.querySelectorAll('meta[name="theme-color"]');
    for (var i = 0; i < metas.length; i++) metas[i].setAttribute('content', t === 'dark' ? '#0A1411' : '#F4F4F0');
    var b = document.getElementById('theme'), label = pref === 'system' ? 'system (' + t + ')' : pref;
    if (b) { b.setAttribute('aria-label', 'Theme: ' + label + '. Switch to ' + NEXT[pref]); b.title = 'Theme: ' + label; }
  }
  applyTheme(doc.getAttribute('data-theme-pref') || 'light');
  var themeBtn = document.getElementById('theme');
  if (themeBtn) themeBtn.addEventListener('click', function () {
    var pref = NEXT[doc.getAttribute('data-theme-pref')] || 'dark';
    try { if (pref === 'light') localStorage.removeItem('codoseo-theme'); else localStorage.setItem('codoseo-theme', pref); } catch (e) {}
    applyTheme(pref);
  });
  if (sysDark && sysDark.addEventListener) sysDark.addEventListener('change', function () { if (doc.getAttribute('data-theme-pref') === 'system') applyTheme('system'); });

  var reduce = window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches;
  var $ = function (id) { return document.getElementById(id); };
  var demo = $('demo'), mascot = $('d-mascot'), status = $('d-status'), statusT = $('d-status-t');
  var typed = $('d-typed'), caret = $('d-caret');
  var rows = Array.prototype.slice.call(document.querySelectorAll('#d-rows .d-row'));
  var chks = Array.prototype.slice.call(document.querySelectorAll('#d-checks .d-chk'));
  var bots = Array.prototype.slice.call(document.querySelectorAll('#d-bots .d-bot'));
  var toast = $('d-toast');
  var URL_TEXT = typed.getAttribute('data-url') || 'https://codoseo.com';
  var C = 113; // ring circumference
  var timers = [];
  function later(fn, ms) { timers.push(setTimeout(fn, ms)); }
  function clear() { timers.forEach(clearTimeout); timers = []; }
  function mood(m) { mascot.setAttribute('class', 'mascot m-' + m); }
  function setStatus(s, t) { status.setAttribute('data-s', s); statusT.textContent = t; }
  function count(el, to, ms, bad) {
    var start = null, from = parseInt(el.textContent, 10) || 0;
    if (reduce) { el.textContent = to; if (bad && to > 0) el.classList.add('bad'); return; }
    later(function () { el.textContent = to; if (bad && to > 0) el.classList.add('bad'); }, ms + 50);
    function tick(ts) {
      if (!start) start = ts;
      var p = Math.min(1, (ts - start) / ms); p = 1 - Math.pow(1 - p, 3);
      el.textContent = Math.round(from + (to - from) * p);
      if (p < 1) requestAnimationFrame(tick); else if (bad && to > 0) el.classList.add('bad');
    }
    requestAnimationFrame(tick);
  }
  function ring(id, vid, v) { $(id).style.strokeDashoffset = C - C * v / 100; $(vid).textContent = v; }
  function reset() {
    clear();
    mood('rest'); setStatus('idle', 'Ready');
    typed.textContent = ''; caret.hidden = false;
    rows.forEach(function (r) { r.classList.remove('on', 'hit'); });
    chks.forEach(function (c) { c.classList.remove('on'); });
    bots.forEach(function (b) { b.classList.remove('on'); });
    toast.classList.remove('on');
    ['n-crawled', 'n-issues', 'n-bots'].forEach(function (i) { $(i).textContent = '0'; $(i).classList.remove('bad'); });
    ['r-seo', 'r-geo', 'r-all'].forEach(function (i) { $(i).style.strokeDashoffset = C; });
    ['v-seo', 'v-geo', 'v-all'].forEach(function (i) { $(i).textContent = '–'; });
  }
  function finalState() {
    mood('warning'); setStatus('done', 'Done · 28s');
    typed.textContent = URL_TEXT; caret.hidden = true;
    rows.forEach(function (r, i) { r.classList.add('on'); if (i === 2 || i === 4) r.classList.add('hit'); });
    chks.forEach(function (c) { c.classList.add('on'); });
    bots.forEach(function (b) { b.classList.add('on'); });
    $('n-crawled').textContent = '142'; $('n-issues').textContent = '9'; $('n-issues').classList.add('bad');
    $('n-bots').textContent = '2'; $('n-bots').classList.add('bad');
    ring('r-seo', 'v-seo', 84); ring('r-geo', 'v-geo', 71); ring('r-all', 'v-all', 79);
    toast.classList.add('on');
  }
  var issues = 0, seo = 100, geo = 100;
  function addIssues(n) { issues += n; $('n-issues').textContent = issues; if (issues > 0) $('n-issues').classList.add('bad'); }
  function scores(se, ge) {
    if (se != null) seo = se; if (ge != null) geo = ge;
    ring('r-seo', 'v-seo', seo); ring('r-geo', 'v-geo', geo); ring('r-all', 'v-all', Math.round(0.6 * seo + 0.4 * geo));
  }
  function run() {
    reset(); issues = 0; seo = 100; geo = 100;
    // type the URL, then everything runs together: pages stream in, issues climb as they are found,
    // the bot probes run alongside, and the scores settle while the crawl is still finishing.
    var i = 0;
    function type() { if (i <= URL_TEXT.length) { typed.textContent = URL_TEXT.slice(0, i++); later(type, 24); } }
    later(type, 250);
    later(function () {
      caret.hidden = true; mood('crawling'); setStatus('crawl', 'Checking pages');
      count($('n-crawled'), 142, 2600); scores(100, 100);
      // rows: /pricing (index 2) is noindex, /sale (3) is a redirect chain, old-trail-guide (4) is a 404
      var hits = { 2: 'Hidden from Google', 3: 'Slow redirects', 4: 'Broken pages' }, after = { 2: 95, 3: 93, 4: 86 };
      rows.forEach(function (r, k) { later(function () {
        r.classList.add('on');
        if (hits[k]) {
          r.classList.add('hit'); addIssues(1); scores(after[k], null);
          chks.forEach(function (c) { if (c.textContent.indexOf(hits[k]) === 0) c.classList.add('on'); });
        }
      }, k * 300); });
      // passing checks light up as the pages come in
      var passing = chks.filter(function (c) { return !c.classList.contains('warn') && !c.classList.contains('err'); });
      passing.forEach(function (c, k) { later(function () { c.classList.add('on'); }, 500 + k * 230); });
      // descriptions missing: counted once enough pages are in
      later(function () { chks.forEach(function (c) { if (c.textContent.indexOf('Missing descriptions') === 0) c.classList.add('on'); }); addIssues(6); scores(84, null); }, 2300);
    }, 1200);
    // bot probes run in parallel with the crawl
    later(function () {
      bots.forEach(function (b, k) { later(function () {
        b.classList.add('on');
        if (k === 3) { mood('error'); setStatus('probe', 'Testing AI bots'); count($('n-bots'), 2, 400, true); scores(null, 71); }
      }, k * 260); });
    }, 1900);
    // finished: the alert pops
    later(function () { setStatus('done', 'Done · 28s'); mood('warning'); }, 4600);
    later(function () { toast.classList.add('on'); }, 5600);
    later(run, 12500);
  }
  if (reduce) { finalState(); } else { run(); }
  // pause the loop while the panel is off screen
  if ('IntersectionObserver' in window && !reduce) {
    var paused = false;
    new IntersectionObserver(function (es) {
      es.forEach(function (e) {
        if (!e.isIntersecting && !paused) { paused = true; clear(); finalState(); }
        else if (e.isIntersecting && paused) { paused = false; run(); }
      });
    }, { threshold: 0.15 }).observe(demo);
  }

  // The peeking mascot looks at the pointer
  var peek = $('peek'), peekGaze = peek && peek.querySelector('.m-gaze'), peekSvg = $('peek-eyes'), pending = false, px = 0, py = 0;
  if (peek && !reduce) {
    document.addEventListener('pointermove', function (e) {
      px = e.clientX; py = e.clientY;
      if (pending) return; pending = true;
      requestAnimationFrame(function () {
        pending = false;
        var r = peek.getBoundingClientRect(), cx = r.left + r.width / 2, cy = r.top + r.height * 0.6;
        var dx = px - cx, dy = py - cy, d = Math.hypot(dx, dy) || 1, k = Math.min(1, d / 240);
        peekGaze.style.transform = 'translate(' + (dx / d * 5.5 * k).toFixed(2) + 'px, ' + (dy / d * 3.5 * k).toFixed(2) + 'px)';
      });
    });
    document.addEventListener('pointerleave', function () { peekGaze.style.transform = ''; });
    peek.addEventListener('click', function () {
      peekSvg.classList.add('is-blinking'); setTimeout(function () { peekSvg.classList.remove('is-blinking'); }, 240);
    });
  }


  // Interactive site map (canvas force layout, like the app's bubble map)
  (function () {
    var wrap = $('map-wrap'), cv = $('map'), tip = $('map-tip'), selUrl = $('sel-url'), selNote = $('sel-note'), stats = $('map-stats');
    if (!wrap || !cv) return;
    var ctx = cv.getContext('2d');
    var css = getComputedStyle(document.documentElement);
    function col(v) { return css.getPropertyValue(v).trim(); }
    // a deterministic sample site: sections with many pages, a few sub-sections
    var seedN = 7; function rnd() { seedN = (seedN * 16807) % 2147483647; return (seedN - 1) / 2147483646; }
    var sections = [
      ['journal', 46, ['guides', 'trips']], ['tents', 26, ['compare']], ['gear', 24, ['packs']],
      ['support', 12, []], ['about', 5, []], ['sale', 7, []], ['pricing', 0, []], ['contact', 0, []]
    ];
    var words = ['alpine', 'trail', 'winter', 'summer', 'ridge', 'river', 'packing', 'light', 'rain', 'stove', 'map', 'water', 'boots', 'sleep', 'camp', 'peak', 'valley', 'north', 'wind', 'family', 'solo', 'ultra', 'basics', 'care', 'repair', 'fit', 'guide', 'kit', 'review', 'faq'];
    var nodes = [], links = [];
    function add(url, st, depth, parent, label, note) {
      var n = { url: url, st: st, depth: depth, kids: 0, label: label || '', note: note, x: 0, y: 0, vx: 0, vy: 0, r: 5 };
      nodes.push(n); if (parent) { parent.kids++; links.push([parent, n]); } return n;
    }
    var root = add('/', 'ok', 0, null, '', 'Home. Every page below is reached from here.');
    var broken = 0, deep = 0, total = 1;
    sections.forEach(function (sec) {
      var name = sec[0], count = sec[1], subs = sec[2];
      var st = name === 'pricing' ? 'noindex' : name === 'sale' ? 'redirect' : 'ok';
      var note = st === 'noindex' ? 'Hidden from Google since Tuesday (noindex).' : st === 'redirect' ? 'Redirects three times before landing.' : 'Indexable. ' + count + ' pages below it.';
      var hub = add('/' + name, st, 1, root, name, note); total++;
      var subHubs = subs.map(function (sb) { total++; return add('/' + name + '/' + sb, 'ok', 2, hub, sb, 'Indexable section.'); });
      for (var i = 0; i < count; i++) {
        var parent = (subHubs.length && rnd() < 0.45) ? subHubs[Math.floor(rnd() * subHubs.length)] : hub;
        var slug = words[Math.floor(rnd() * words.length)] + '-' + words[Math.floor(rnd() * words.length)];
        var r = rnd(), pst = 'ok', pnote = 'Indexable.';
        if (r < 0.06) { pst = 'redirect'; pnote = 'Redirects. Links should point at the final page.'; }
        else if (r < 0.09) { pst = 'noindex'; pnote = 'Noindex or canonicalised elsewhere.'; }
        else if (r < 0.105 && broken < 3) { pst = 'broken'; broken++; pnote = 'Returns 404. Other pages still link here.'; }
        else if (r < 0.17) { pnote = 'Indexable. Missing meta description.'; }
        var d = parent.depth + 1; if (d >= 4) deep++;
        add(parent.url + '/' + slug, pst, d, parent, '', pnote); total++;
      }
    });
    // a few deep pages hanging off leaves
    for (var k = 0; k < 9; k++) { var leaf = nodes[20 + Math.floor(rnd() * (nodes.length - 20))]; if (leaf.depth >= 2 && leaf.depth < 4) { add(leaf.url + '/' + words[Math.floor(rnd() * words.length)], 'ok', leaf.depth + 1, leaf, '', 'Indexable. ' + (leaf.depth + 1) + ' clicks from home.'); total++; if (leaf.depth + 1 >= 4) deep++; } }
    nodes.forEach(function (n) { n.r = n.depth === 0 ? 15 : n.kids > 10 ? 12 : n.kids > 0 ? 8 + Math.min(3, n.kids * .5) : 4.6; });
    if (stats) stats.innerHTML = '<b>' + total + '</b> pages shown <b>' + broken + '</b> broken <b>' + deep + '</b> at 4+ clicks';
    var W = 0, H = 0, dpr = Math.min(2, window.devicePixelRatio || 1), hover = null, drag = null, alpha = 1, raf = null;
    function size() { var r = wrap.getBoundingClientRect(); W = r.width; H = r.height; cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr); ctx.setTransform(dpr, 0, 0, dpr, 0, 0); }
    function seed() {
      root.x = W / 2; root.y = H / 2;
      var hubs = nodes.filter(function (n) { return n.depth === 1; });
      hubs.forEach(function (h, i) { var a = (i / hubs.length) * Math.PI * 2 - Math.PI / 2; h.x = W / 2 + Math.cos(a) * Math.min(W, H) * .33; h.y = H / 2 + Math.sin(a) * Math.min(W, H) * .33; });
      nodes.forEach(function (n) { if (n.depth >= 2) { var p = links.filter(function (l) { return l[1] === n; })[0][0]; var a = rnd() * Math.PI * 2; n.x = p.x + Math.cos(a) * 30; n.y = p.y + Math.sin(a) * 30; } });
    }
    // grid-bucketed repulsion so 150 nodes stay cheap
    function step() {
      var k = 0.86, cell = 48, grid = {}, i, n;
      for (i = 0; i < nodes.length; i++) { n = nodes[i]; var key = Math.floor(n.x / cell) + ',' + Math.floor(n.y / cell); (grid[key] = grid[key] || []).push(n); }
      for (i = 0; i < nodes.length; i++) {
        var a = nodes[i], gx = Math.floor(a.x / cell), gy = Math.floor(a.y / cell);
        for (var ox = -1; ox <= 1; ox++) for (var oy = -1; oy <= 1; oy++) {
          var bucket = grid[(gx + ox) + ',' + (gy + oy)]; if (!bucket) continue;
          for (var j = 0; j < bucket.length; j++) {
            var b = bucket[j]; if (b === a) continue;
            var dx = b.x - a.x, dy = b.y - a.y, d2 = dx * dx + dy * dy + 0.01, d = Math.sqrt(d2);
            var min = a.r + b.r + 5, f = (230 / d2) * alpha; if (d < min) f += (min - d) * 0.08;
            a.vx -= dx / d * f; a.vy -= dy / d * f;
          }
        }
        a.vx += (W / 2 - a.x) * 0.001 * alpha; a.vy += (H / 2 - a.y) * 0.0012 * alpha;
      }
      links.forEach(function (l) {
        var a = l[0], b = l[1], dx = b.x - a.x, dy = b.y - a.y, d = Math.sqrt(dx * dx + dy * dy) || 1;
        var want = a.depth === 0 ? 165 : a.depth === 1 ? (b.kids ? 70 : 32) : 26, f = (d - want) * 0.045;
        a.vx += dx / d * f; a.vy += dy / d * f; b.vx -= dx / d * f; b.vy -= dy / d * f;
      });
      nodes.forEach(function (n) {
        if (n === drag) { n.vx = n.vy = 0; return; }
        n.vx *= k; n.vy *= k; n.x += n.vx; n.y += n.vy;
        n.x = Math.max(n.r + 6, Math.min(W - n.r - 6, n.x)); n.y = Math.max(n.r + 6, Math.min(H - n.r - 6, n.y));
      });
      alpha = Math.max(0.02, alpha * 0.985);
    }
    var C = {};
    function palette() { C = { ok: col('--st-ok'), redirect: col('--st-crawl'), broken: col('--st-err'), ink: col('--ink'), line: col('--line-strong'), surface: col('--surface'), bg: col('--surface-2'), yellow: col('--yellow') }; }
    function neighbour(n) { return hover && (n === hover || links.some(function (l) { return (l[0] === hover && l[1] === n) || (l[1] === hover && l[0] === n); })); }
    function draw() {
      ctx.clearRect(0, 0, W, H);
      links.forEach(function (l) {
        var lit = hover && (l[0] === hover || l[1] === hover);
        ctx.beginPath(); ctx.moveTo(l[0].x, l[0].y); ctx.lineTo(l[1].x, l[1].y);
        ctx.strokeStyle = lit ? C.ink : C.line; ctx.lineWidth = lit ? 1.5 : .8; ctx.globalAlpha = hover && !lit ? .25 : .9; ctx.stroke();
      });
      nodes.forEach(function (n) {
        if (n.depth === 0) return;
        ctx.globalAlpha = hover && !neighbour(n) ? .3 : 1;
        ctx.beginPath(); ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
        if (n.st === 'noindex') { ctx.fillStyle = C.surface; ctx.fill(); ctx.lineWidth = 1.5; ctx.strokeStyle = C.line; ctx.stroke(); }
        else {
          var base = n.st === 'ok' ? C.ok : n.st === 'redirect' ? C.redirect : C.broken;
          ctx.fillStyle = base; ctx.globalAlpha *= n.st === 'ok' ? (n.depth === 1 ? 1 : n.depth === 2 ? .8 : n.depth === 3 ? .62 : .48) : 1; ctx.fill();
          if (n === hover) { ctx.globalAlpha = 1; ctx.lineWidth = 2.5; ctx.strokeStyle = C.ink; ctx.stroke(); }
        }
        ctx.globalAlpha = 1;
        if (n.label) { ctx.fillStyle = C.ink; ctx.font = '500 11.5px ' + col('--sans'); ctx.textAlign = 'left'; ctx.textBaseline = 'middle'; ctx.fillText(n.label, n.x + n.r + 5, n.y); }
      });
      // the home page is the mascot
      ctx.globalAlpha = 1; ctx.beginPath(); ctx.arc(root.x, root.y, root.r, 0, Math.PI * 2); ctx.fillStyle = C.ink; ctx.fill();
      [-4.6, 4.6].forEach(function (ox) {
        ctx.beginPath(); ctx.arc(root.x + ox, root.y, 4.2, 0, Math.PI * 2); ctx.lineWidth = 1.8; ctx.strokeStyle = C.surface; ctx.stroke();
        ctx.beginPath(); ctx.arc(root.x + ox + .6, root.y + .4, 1.8, 0, Math.PI * 2); ctx.fillStyle = C.yellow; ctx.fill();
      });
      if (hover === root) { ctx.beginPath(); ctx.arc(root.x, root.y, root.r + 3, 0, Math.PI * 2); ctx.lineWidth = 2; ctx.strokeStyle = C.yellow; ctx.stroke(); }
    }
    function loop() { step(); draw(); if (alpha > 0.021 || drag || hover) raf = requestAnimationFrame(loop); else raf = null; }
    function kick() { if (!raf) raf = requestAnimationFrame(loop); }
    function pick(x, y) { for (var i = nodes.length - 1; i >= 0; i--) { var n = nodes[i], dx = x - n.x, dy = y - n.y, rr = Math.max(n.r, 6) + 3; if (dx * dx + dy * dy <= rr * rr) return n; } return null; }
    function pos(e) { var r = cv.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; }
    cv.addEventListener('pointermove', function (e) {
      var p = pos(e);
      if (drag) { drag.x = p[0]; drag.y = p[1]; alpha = Math.max(alpha, .3); kick(); return; }
      var n = pick(p[0], p[1]);
      if (n !== hover) { hover = n; kick(); }
      if (n) { tip.hidden = false; tip.innerHTML = '<b>' + n.url + '</b>' + n.note; tip.style.left = n.x + 'px'; tip.style.top = (n.y - n.r) + 'px'; selUrl.textContent = n.url; selNote.textContent = n.note; }
      else tip.hidden = true;
    });
    cv.addEventListener('pointerleave', function () { hover = null; tip.hidden = true; kick(); });
    cv.addEventListener('pointerdown', function (e) { var p = pos(e), n = pick(p[0], p[1]); if (n) { drag = n; cv.classList.add('grabbing'); cv.setPointerCapture(e.pointerId); kick(); } });
    function up() { drag = null; cv.classList.remove('grabbing'); alpha = Math.max(alpha, .2); kick(); }
    cv.addEventListener('pointerup', up); cv.addEventListener('pointercancel', up);
    var started = false;
    function start() {
      if (started) return; started = true;
      palette(); size(); seed();
      for (var pre = 0; pre < 150; pre++) step();
      alpha = 0.12; draw(); kick();
    }
    if ('IntersectionObserver' in window) new IntersectionObserver(function (es, io) { es.forEach(function (e) { if (e.isIntersecting) { start(); io.disconnect(); } }); }, { rootMargin: '200px' }).observe(wrap);
    else start();
    if ('ResizeObserver' in window) new ResizeObserver(function () { if (!started) return; size(); alpha = Math.max(alpha, .4); kick(); }).observe(wrap);
    function retheme() { setTimeout(function () { palette(); kick(); }, 30); }
    if (window.matchMedia) matchMedia('(prefers-color-scheme: dark)').addEventListener('change', retheme);
    var tb = document.getElementById('theme'); if (tb) tb.addEventListener('click', retheme);
  })();

  // AI agent prompts
  var agentBtn = $('agent-btn'), agentPrompts = $('agent-prompts');
  if (agentBtn) agentBtn.addEventListener('click', function () {
    var open = agentPrompts.hidden; agentPrompts.hidden = !open;
    agentBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
    agentBtn.textContent = open ? 'Hide prompts' : 'Add to your AI agent';
  });
  document.querySelectorAll('.copy').forEach(function (b) {
    b.addEventListener('click', function () {
      var el = $(b.getAttribute('data-copy')), text = el.textContent;
      function done() { b.textContent = 'Copied'; b.classList.add('done'); setTimeout(function () { b.textContent = 'Copy'; b.classList.remove('done'); }, 1600); }
      function fallback() { var r = document.createRange(); r.selectNodeContents(el); var sel = getSelection(); sel.removeAllRanges(); sel.addRange(r); }
      if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(text).then(done, fallback); else fallback();
    });
  });


  // Audit form: a demo, so just focus and shake into the panel
  /* "Start free audit" links: scroll to the box and put the cursor in it */
  var box = $('audit');
  document.querySelectorAll('a[href="#audit"]').forEach(function (a) {
    a.addEventListener('click', function (e) {
      if (!box) return;
      e.preventDefault();
      box.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'center' });
      box.focus({ preventScroll: true });
    });
  });

  /* live "pages audited" counter: the stored total, re-read every 20 s while visible */
  var counter = $('pages-crawled');
  if (counter && window.fetch) {
    setInterval(function () {
      if (document.hidden) return;
      fetch(counter.getAttribute('data-poll'), { headers: { Accept: 'application/json' } })
        .then(function (r) { return r.ok ? r.json() : null; })
        .then(function (d) { if (d && d.label) counter.textContent = d.label; })
        .catch(function () {});
    }, 20000);
  }
})();
