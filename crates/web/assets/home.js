// The cloud landing page (templates/landing/index.html): theme toggle, the mascot eyes, scroll
// reveal and the sample crawl. The theme is applied before paint by an inline script in <head>.
(function () {
  var $ = function (id) { return document.getElementById(id); };
  var doc = document.documentElement;
  var still = !!(window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches);
  var rnd = function (a, b) { return a + Math.random() * (b - a); };

  /* ---------- theme toggle: light → dark → system ---------- */
  var NEXT = { light: 'dark', dark: 'system', system: 'light' };
  var sysDark = window.matchMedia ? matchMedia('(prefers-color-scheme: dark)') : null;
  function applyTheme(pref) {
    var t = pref === 'dark' || (pref === 'system' && sysDark && sysDark.matches) ? 'dark' : 'light';
    doc.setAttribute('data-theme', t);
    doc.setAttribute('data-theme-pref', pref);
    var metas = document.querySelectorAll('meta[name="theme-color"]');
    for (var i = 0; i < metas.length; i++) metas[i].setAttribute('content', t === 'dark' ? '#0A1411' : '#F4F4F0');
    var b = $('theme'), label = pref === 'system' ? 'system (' + t + ')' : pref;
    b.setAttribute('aria-label', 'Theme: ' + label + '. Switch to ' + NEXT[pref]);
    b.title = 'Theme: ' + label;
  }
  applyTheme(doc.getAttribute('data-theme-pref') || 'light');
  $('theme').addEventListener('click', function () {
    var pref = NEXT[doc.getAttribute('data-theme-pref')] || 'dark';
    // Same key and values as app.js: light is the default, so it is stored as nothing.
    try { if (pref === 'light') localStorage.removeItem('codoseo-theme'); else localStorage.setItem('codoseo-theme', pref); } catch (e) {}
    applyTheme(pref);
  });
  if (sysDark) {
    var onSys = function () { if (doc.getAttribute('data-theme-pref') === 'system') applyTheme('system'); };
    if (sysDark.addEventListener) sysDark.addEventListener('change', onSys); else if (sysDark.addListener) sysDark.addListener(onSys);
  }

  /* ---------- mascot: the one template every pair of eyes comes from ---------- */
  // Geometry from the brand mark (viewBox 0 0 64 64, cropped to the eyes): rings at x 20.5 / 43.5, y 32, r 11.5.
  // Pupils are unit circles moved and scaled by CSS custom properties, so states animate with transitions.
  function mascot(state, small) {
    // Pupils are drawn first so the rings sit on top: a pupil that glances far sideways tucks under its ring.
    var pupil = function (cx) {
      return '<g transform="translate(' + cx + ' 32)"><g class="lk"><g class="pp"><g class="sw"><circle class="pu" r="1"/></g></g></g></g>';
    };
    var ring = function (cx) { return '<circle class="rg" cx="' + cx + '" cy="32" r="11.5"/>'; };
    return '<svg class="mascot' + (small ? ' sm' : '') + '" viewBox="5 17 54 30" data-state="' + state + '" aria-hidden="true" focusable="false">' +
      '<g class="bl">' + pupil(20.5) + pupil(43.5) + ring(20.5) + ring(43.5) + '</g></svg>';
  }
  var all = document.querySelectorAll('[data-mascot]');
  for (var i = 0; i < all.length; i++) {
    all[i].innerHTML = mascot(all[i].getAttribute('data-mascot'), all[i].hasAttribute('data-small'));
  }
  var svgOf = function (el) { return el.querySelector('svg.mascot') || el; };
  var setState = function (el, s) { svgOf(el).setAttribute('data-state', s); };

  /* blinking: random intervals, sometimes a double blink */
  function blink(svg) {
    if (still) return;
    svg.classList.remove('blink'); void svg.getBoundingClientRect(); svg.classList.add('blink');
    setTimeout(function () { svg.classList.remove('blink'); }, 200);
  }
  function blinker(svg, min, max) {
    if (still) return;
    (function loop() {
      setTimeout(function () {
        if (!document.hidden) {
          blink(svg); if (Math.random() < .22) setTimeout(function () { blink(svg); }, 260);
        }
        loop();
      }, rnd(min, max));
    })();
  }

  /* eyes that look at the cursor (mouse) or look around on their own (touch / idle) */
  var followers = [];
  var fl = document.querySelectorAll('[data-follow]');
  for (i = 0; i < fl.length; i++) {
    var sv = svgOf(fl[i]);
    sv.classList.add('follow');
    followers.push({ svg: sv, eyes: sv.querySelectorAll('.lk') });
    blinker(sv, 2400, 6200);
  }
  function lookAt(f, x, y) {
    var r = f.svg.getBoundingClientRect();
    if (!r.width) return;
    var k = r.width / 54; // px per viewBox unit
    for (var j = 0; j < f.eyes.length; j++) {
      var cx = r.left + ((j ? 43.5 : 20.5) - 5) * k, cy = r.top + (32 - 17) * k;
      var dx = x - cx, dy = y - cy, d = Math.sqrt(dx * dx + dy * dy) || 1;
      var m = Math.min(4.6, d / (k * 9)); // travel in viewBox units, eased by distance
      f.eyes[j].style.setProperty('--lx', (dx / d * m).toFixed(2) + 'px');
      f.eyes[j].style.setProperty('--ly', (dy / d * m).toFixed(2) + 'px');
    }
  }
  function lookDir(f, ux, uy) {
    for (var j = 0; j < f.eyes.length; j++) {
      f.eyes[j].style.setProperty('--lx', ux.toFixed(2) + 'px');
      f.eyes[j].style.setProperty('--ly', uy.toFixed(2) + 'px');
    }
  }
  if (!still && followers.length) {
    var mx = 0, my = 0, queued = false, lastMove = 0;
    // only a real mouse drives the eyes; touch and pen fall through to the look-around loop below
    window.addEventListener('pointermove', function (e) {
      if (e.pointerType !== 'mouse') return;
      mx = e.clientX; my = e.clientY; lastMove = Date.now();
      if (!queued) { queued = true; requestAnimationFrame(function () { queued = false; followers.forEach(function (f) { f.svg.classList.add('follow'); lookAt(f, mx, my); }); }); }
    }, { passive: true });
    // idle look-around loop: runs on touch screens, and on desktop when the mouse has been still a while
    var SPOTS = [[0, 0], [4, 0], [-4, 0], [3, -3], [-3, -2.5], [0, 3.5], [3.5, 2.5], [-4, 1.5], [0, -3.5]];
    (function wander() {
      setTimeout(function () {
        if (!document.hidden && Date.now() - lastMove > 4000) {
          var s = SPOTS[(Math.random() * SPOTS.length) | 0];
          followers.forEach(function (f) { f.svg.classList.remove('follow'); lookDir(f, s[0], s[1]); });
          if (Math.random() < .3) followers.forEach(function (f) { blink(f.svg); });
        } else {
          followers.forEach(function (f) { f.svg.classList.add('follow'); });
        }
        wander();
      }, rnd(1300, 2800));
    })();
  }
  // tapping or clicking the hero mascot makes it blink and look surprised for a beat
  $('peek').addEventListener('click', function () {
    var s = svgOf(this); blink(s);
    s.style.setProperty('--pr', '6.5');
    setTimeout(function () { s.style.removeProperty('--pr'); }, 700);
  });

  /* wordmark: glance on hover (CSS) plus a blink */
  var wms = document.querySelectorAll('.wm');
  for (i = 0; i < wms.length; i++) {
    wms[i].addEventListener('mouseenter', function () {
      if (still) return; var w = this; w.classList.add('blink');
      setTimeout(function () { w.classList.remove('blink'); }, 110);
    });
  }

  /* ---------- scroll reveal ---------- */
  var rev = document.querySelectorAll('.reveal');
  if ('IntersectionObserver' in window && !still) {
    var io = new IntersectionObserver(function (es) {
      es.forEach(function (e) { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); } });
    }, { rootMargin: '0px 0px -8% 0px', threshold: 0.06 });
    for (i = 0; i < rev.length; i++) io.observe(rev[i]);
  } else {
    for (i = 0; i < rev.length; i++) rev[i].classList.add('in');
  }

  /* ---------- "Start free audit" links: scroll to the box and put the cursor in it ---------- */
  var box = $('audit');
  var jumps = document.querySelectorAll('a[href="#audit"]');
  for (i = 0; i < jumps.length; i++) {
    jumps[i].addEventListener('click', function (e) {
      e.preventDefault();
      box.scrollIntoView({ behavior: still ? 'auto' : 'smooth', block: 'center' });
      box.focus({ preventScroll: true });
    });
  }

  /* ---------- sample crawl feed ---------- */
  var POOL = [
    ['/', 200, 'Indexable', 112], ['/tents', 200, 'Indexable', 98], ['/tents/ridgeline-2p', 200, 'Indexable', 141],
    ['/sale', 301, 'Redirect → /deals', 34], ['/blog/winter-camping-checklist', 200, 'Indexable', 176],
    ['/journal/old-trail-guide', 404, 'Not found', 61], ['/sleeping-bags', 200, 'Indexable', 104],
    ['/pricing', 200, 'Noindex', 88], ['/stoves/alpine-mini', 200, 'Indexable', 122],
    ['/tents?sort=price', 200, 'Canonicalised', 96], ['/api/stock-check', 503, 'Server error', 1204],
    ['/deals', 301, 'Redirect → /offers', 41], ['/about', 200, 'Indexable', 79], ['/journal/tag/hiking', 200, 'Indexable', 133],
    ['/packs/trail-35l', 200, 'Indexable', 117], ['/cart', 200, 'Blocked by robots.txt', 0],
    ['/returns', 200, 'Indexable', 72], ['/journal/gear-list-2019', 410, 'Gone', 58], ['/contact', 200, 'Indexable', 69]
  ];
  var TOTAL = 1284, tick = 0, dot = $('dot');
  var cls = function (s) { return s >= 500 ? 's5' : s >= 400 ? 's4' : s >= 300 ? 's3' : 's2'; };
  var col = function (r) { return r[2] === 'Indexable' ? 'ix-ok' : r[1] >= 400 ? 'ix-bad' : 'ix-warn'; };
  var esc = function (s) { return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;'); };

  function render(t, animate) {
    var crawled = Math.min(TOTAL, 180 + t * 13), done = crawled >= TOTAL, html = '';
    for (var i = 0; i < 9; i++) {
      var r = POOL[(((t - i) % POOL.length) + POOL.length) % POOL.length];
      html += '<div class="row' + (i === 0 && !done && animate ? ' fresh' : '') + '"><span class="p">' + esc(r[0]) +
        '</span><span class="s ' + cls(r[1]) + '">' + r[1] + '</span><span class="i ' + col(r) + '">' + esc(r[2]) +
        '</span><span class="t">' + (r[3] ? r[3] + ' ms' : '—') + '</span></div>';
    }
    $('feed').innerHTML = html;
    $('s-crawled').textContent = crawled.toLocaleString('en-US');
    $('s-queue').textContent = done ? '0' : Math.max(0, 1300 - crawled);
    $('s-rate').textContent = done ? '—' : 12 + (tick * 7) % 6;
    $('s-issues').textContent = Math.round(crawled / TOTAL * 96);
    $('prog').style.width = (crawled / TOTAL * 100).toFixed(1) + '%';
    $('progw').classList.toggle('done', done);
    setState(dot, done ? 'ok' : 'crawl');
    $('state').textContent = done ? 'Sample · complete in 1m 42s' : 'Sample crawl…';
  }
  if (still) { render(109, false); }
  else { render(0, false); setInterval(function () { if (!document.hidden) { tick++; render(tick % 110, true); } }, 420); }
})();
