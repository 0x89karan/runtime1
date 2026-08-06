/* runtime1 site — shared interaction layer.
   No dependencies, no build step, no network: the site stays a folder of static files you can
   open from disk. Everything here degrades to the existing static page if JS is off.

   The interaction model deliberately echoes the product: agentctl watch is a keyboard-driven
   cockpit with a [?] help key, so the site is too. */
(function () {
  'use strict';

  var root = document.documentElement;
  var reduced = window.matchMedia('(prefers-reduced-motion: reduce)');

  /* FNV-1a-ish 32-bit string hash → 8 hex chars. Illustrative only, shared by the
     verification-page miniatures (real chain, and the zk.2 illustration). */
  function h(str) {
    var hash = 2166136261;
    for (var i = 0; i < str.length; i++) {
      hash ^= str.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return (hash >>> 0).toString(16).padStart(8, '0');
  }

  /* ─── theme ───────────────────────────────────────────────────────────────
     The light palette already existed in style.css but nothing could reach it
     except an OS-level preference change. Persist an explicit choice; absent
     one, the CSS media query keeps following the system. */
  var THEME_KEY = 'runtime1-theme';

  function currentTheme() {
    if (root.dataset.theme) return root.dataset.theme;
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }

  function syncToggles(t) {
    document.querySelectorAll('[data-theme-toggle]').forEach(function (btn) {
      btn.setAttribute('aria-pressed', t === 'light' ? 'true' : 'false');
      btn.setAttribute('title', t === 'light' ? 'Switch to dark (t)' : 'Switch to light (t)');
      var lbl = btn.querySelector('.tt-label');
      if (lbl) lbl.textContent = t === 'light' ? 'light' : 'dark';
    });
  }

  function applyTheme(t) {
    root.dataset.theme = t;
    try { localStorage.setItem(THEME_KEY, t); } catch (e) { /* private mode: session-only */ }
    syncToggles(t);
  }

  function toggleTheme() { applyTheme(currentTheme() === 'light' ? 'dark' : 'light'); }

  document.addEventListener('click', function (e) {
    var btn = e.target.closest('[data-theme-toggle]');
    if (btn) { e.preventDefault(); toggleTheme(); }
  });

  /* Only pin an explicit theme once the visitor picks one. Until then the page
     keeps following the OS — including a change made while it is open. */
  syncToggles(currentTheme());
  var mq = window.matchMedia('(prefers-color-scheme: light)');
  var onScheme = function () { if (!root.dataset.theme) syncToggles(currentTheme()); };
  if (mq.addEventListener) mq.addEventListener('change', onScheme);
  else if (mq.addListener) mq.addListener(onScheme);

  /* ─── scroll progress + sticky nav ─────────────────────────────────────── */
  var bar = document.querySelector('.progress span');
  var nav = document.querySelector('.topbar') || document.querySelector('.nav');
  var ticking = false;

  function onScroll() {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(function () {
      var h = document.documentElement.scrollHeight - window.innerHeight;
      if (bar) bar.style.transform = 'scaleX(' + (h > 0 ? Math.min(window.scrollY / h, 1) : 0) + ')';
      if (nav) nav.classList.toggle('stuck', window.scrollY > 8);
      ticking = false;
    });
  }
  window.addEventListener('scroll', onScroll, { passive: true });
  onScroll();

  /* ─── reveal on scroll ─────────────────────────────────────────────────────
     Purely additive: elements are visible by default in CSS unless JS marks the
     document as reveal-capable, so no-JS and reduced-motion users see everything. */
  var revealables = document.querySelectorAll('[data-reveal]');
  if (revealables.length && !reduced.matches && 'IntersectionObserver' in window) {
    root.classList.add('reveal-on');
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) {
        if (en.isIntersecting) { en.target.classList.add('in'); io.unobserve(en.target); }
      });
    }, { rootMargin: '0px 0px -8% 0px', threshold: 0.06 });
    revealables.forEach(function (el) { io.observe(el); });
  }

  /* ─── keyboard layer ───────────────────────────────────────────────────────
     Mirrors agentctl watch: single-letter verbs, [?] for help, Esc to close. */
  var KEYS = [
    { k: '?', label: 'this help', run: function () { toggleHelp(); } },
    { k: 't', label: 'toggle light / dark', run: toggleTheme },
    { k: 'g', label: 'open the GitHub repo', run: function () { go('https://github.com/0x89karan/runtime1'); } },
    { k: 'h', label: 'home', run: function () { go('index.html'); } },
    { k: '1', label: 'the agentOS thesis', run: function () { go('thesis.html'); } },
    { k: '2', label: 'verification thesis', run: function () { go('zk-verification.html'); } },
    { k: '3', label: 'runtime schematic', run: function () { go('architecture.html'); } },
    { k: '4', label: 'roadmap', run: function () { go('roadmap.html'); } },
    { k: '5', label: 'the whitepaper', run: function () { go('whitepaper.html'); } },
    { k: 'p', label: 'pause / resume the recorder', run: function () { if (rec) rec.toggle(); }, only: 'rec' },
    { k: 'r', label: 'replay the recorder', run: function () { if (rec) rec.replay(); }, only: 'rec' }
  ];

  function go(href) { window.location.href = href; }

  function typing(el) {
    if (!el) return false;
    var t = el.tagName;
    return t === 'INPUT' || t === 'TEXTAREA' || t === 'SELECT' || el.isContentEditable;
  }

  document.addEventListener('keydown', function (e) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;      // never shadow browser shortcuts
    if (typing(document.activeElement)) return;           // never eat real input
    if (e.key === 'Escape' && helpOpen()) { e.preventDefault(); closeHelp(); return; }
    var hit = KEYS.filter(function (x) { return x.k === e.key; })[0];
    if (!hit) return;
    if (hit.only === 'rec' && !rec) return;
    e.preventDefault();
    hit.run();
  });

  /* ─── help overlay (built from KEYS so it can never drift) ─────────────── */
  var overlay = null, lastFocus = null;

  function helpOpen() { return overlay && overlay.classList.contains('open'); }

  function buildHelp() {
    overlay = document.createElement('div');
    overlay.className = 'overlay';
    overlay.setAttribute('role', 'dialog');
    overlay.setAttribute('aria-modal', 'true');
    overlay.setAttribute('aria-label', 'Keyboard shortcuts');
    var rows = KEYS.filter(function (x) { return !x.only || (x.only === 'rec' && rec); })
      .map(function (x) {
        return '<div class="krow"><kbd>' + (x.k === '?' ? '?' : x.k) + '</kbd><span>' + x.label + '</span></div>';
      }).join('');
    overlay.innerHTML =
      '<div class="sheet" role="document">' +
        '<p class="k">keyboard</p>' +
        '<h3>Shortcuts</h3>' +
        '<p class="sheet-sub">This site is driven the same way the cockpit is — ' +
        '<code>agentctl watch</code> answers to single-letter verbs and <kbd>?</kbd> too.</p>' +
        '<div class="krows">' + rows + '</div>' +
        '<button type="button" class="btn ghost sheet-close">Close (Esc)</button>' +
      '</div>';
    document.body.appendChild(overlay);
    overlay.addEventListener('click', function (e) {
      if (e.target === overlay || e.target.closest('.sheet-close')) closeHelp();
    });
  }

  function toggleHelp() { helpOpen() ? closeHelp() : openHelp(); }

  function openHelp() {
    if (!overlay) buildHelp();
    lastFocus = document.activeElement;
    overlay.classList.add('open');
    var btn = overlay.querySelector('.sheet-close');
    if (btn) btn.focus();
  }

  function closeHelp() {
    if (!overlay) return;
    overlay.classList.remove('open');
    if (lastFocus && lastFocus.focus) lastFocus.focus();
  }

  document.addEventListener('click', function (e) {
    if (e.target.closest('[data-help]')) { e.preventDefault(); toggleHelp(); }
  });

  /* ─── evidence filter ──────────────────────────────────────────────────────
     The site's house rule is that every claim carries its evidence level. The
     filter makes that rule operable: dim everything that isn't the level you
     asked for, so "what is actually enforced today" is one click, not a read. */
  (function evidenceFilter() {
    var chips = document.querySelectorAll('[data-filter]');
    if (!chips.length) return;
    var rows = document.querySelectorAll('[data-evidence]');
    var count = document.querySelector('[data-filter-count]');

    function apply(level) {
      var shown = 0;
      rows.forEach(function (r) {
        var on = level === 'all' || r.dataset.evidence === level;
        r.classList.toggle('dim', !on);
        if (on) shown++;
      });
      chips.forEach(function (c) {
        var active = c.dataset.filter === level;
        c.classList.toggle('on', active);
        c.setAttribute('aria-pressed', active ? 'true' : 'false');
      });
      if (count) {
        count.textContent = level === 'all'
          ? rows.length + ' capabilities · filter to see how each one is backed'
          : shown + ' of ' + rows.length + ' ' + (level === 'abs' ? 'not built yet' :
              level === 'enf' ? 'enforced in code' : 'declared in config or prompt');
      }
    }

    chips.forEach(function (c) {
      c.addEventListener('click', function () { apply(c.dataset.filter); });
    });
    apply('all');
  })();

  /* ─── flight recorder demo ─────────────────────────────────────────────────
     A faithful miniature of the real thing: agentd emits a structured event for
     every meaningful step, and the budget is a fuse, not a suggestion. Event
     kinds below are the real ones from the flight-recorder taxonomy. Drag the
     budget down and the run gets deferred instead of finishing — which is the
     actual ux.8' semantics (park, don't brick: a reset window revives it). */
  var rec = null;
  (function recorder() {
    var host = document.querySelector('[data-recorder]');
    if (!host) return;

    var logEl     = host.querySelector('.rec-log');
    var outEl     = host.querySelector('.rec-out');
    var slider    = host.querySelector('[data-budget]');
    var budgetLbl = host.querySelector('[data-budget-label]');
    var approvalT = host.querySelector('[data-approval]');
    var semanticT = host.querySelector('[data-semantic]');
    var proofEl   = host.querySelector('[data-proof]');
    var kbEl      = host.querySelector('[data-kb]');
    var ckptEl    = host.querySelector('[data-ckpt]');
    var capToggles= [].slice.call(host.querySelectorAll('[data-cap]'));
    var meterWrap = host.querySelector('.rec-meter');
    var meterFill = host.querySelector('.meter span');
    var spentEl   = host.querySelector('[data-spent]');
    var capTotal  = host.querySelector('[data-cap-total]');
    var playBtn   = host.querySelector('[data-play]');
    var replayBtn = host.querySelector('[data-replay]');

    /* The run. `needs` is a capability grant the step requires; `gate` marks the
       world-affecting step an approval rule would intercept. Event kinds are the
       real ones from the flight-recorder taxonomy. */
    var STEPS = [
      { t:'capabilities_resolved', c:'ext',  cost:0,
        d:function(){ var g=granted(); return 'agent=cos-inbox  enforced=[' + (g.length?g.join(', '):'—') + ']'; } },
      { t:'agent_scheduled',   c:'flow', cost:0,    d:'agent=cos-inbox  in_flight=1' },
      { t:'inference_request', c:'flow', cost:1840, d:'model=claude-sonnet  retained_tokens_est=1,840' },
      { t:'tool_call',         c:'mut',  cost:0,    needs:'Mcp{google_oauth}',
        d:'gmail.search  q="in:inbox"  cap=Mcp{google_oauth}' },
      { t:'tool_result',       c:'mut',  cost:0,    d:'gmail.search  messages=37  suppressed_count=0' },
      { t:'inference_request', c:'flow', cost:4210, d:'model=claude-sonnet  retained_tokens_est=4,210' },
      { t:'agent_checkpointed',c:'ext',  cost:0,    ckpt:'turn 4 · fsync ok',
        d:'turn=4  checkpoint.json  fsync ok' },
      { t:'tool_call',         c:'mut',  cost:0,    needs:'KbWrite{ops:briefs}', kb:true,
        d:'kb_put  segment=ops:briefs  cap=KbWrite{ops:briefs}' },
      { t:'tool_result',       c:'mut',  cost:0,
        d:function(){ return semantic()
            ? 'kb_put  ok  key=2026-08-06  embedded=1  index=semantic'
            : 'kb_put  ok  key=2026-08-06  embedded=0  index=point lookups only'; } },
      { t:'tool_call',         c:'mut',  cost:0,    needs:'FsWrite{/data/output}', gate:true,
        d:'write_file  path=/data/output/brief.md  cap=FsWrite{/data/output}' },
      { t:'inference_request', c:'flow', cost:3600, d:'model=claude-sonnet  retained_tokens_est=3,600' },
      { t:'brief_written',     c:'enf',  cost:0,    d:'brief_id=2026-08-06  run_count=3' },
      { t:'agent_completed',   c:'enf',  cost:0,    d:'agent=cos-inbox  turns=6' }
    ];

    var TICK = 520, timer = null, i = 0, spent = 0;
    var running = false, done = false, parked = false, approved = false;
    var receipts = 0, denials = 0, kbState = null, ckptState = null;

    function cap()      { return parseInt(slider.value, 10); }
    function fmt(n)     { return n.toLocaleString('en-US'); }
    function semantic() { return semanticT && semanticT.classList.contains('on'); }
    function granted()  { return capToggles.filter(function(b){return b.classList.contains('on');})
                                           .map(function(b){return b.dataset.cap;}); }
    function has(c)     { return granted().indexOf(c) !== -1; }
    function needsOk(s) { return !s.needs || has(s.needs); }

    function paintStatus() {
      // Proof: every model call is receipted, and so is every denial — that is the
      // whole point of an evidence chain that can say "no".
      if (!receipts && !denials) { proofEl.textContent = '—'; proofEl.className = 'st-v'; }
      else {
        proofEl.className = 'st-v ok';
        proofEl.innerHTML = receipts + ' receipt' + (receipts === 1 ? '' : 's') +
          (denials ? ' <span class="q">+</span> ' + denials + ' denial' + (denials === 1 ? '' : 's') : '') +
          ' <span class="q">·</span> chain verified';
      }

      if (kbState === 'denied') {
        kbEl.className = 'st-v bad'; kbEl.textContent = 'no write — KbWrite not granted';
      } else if (kbState === 'semantic') {
        kbEl.className = 'st-v ok';
        kbEl.innerHTML = 'ops:briefs <span class="q">←</span> 1 entry <span class="q">·</span> semantic index';
      } else if (kbState === 'degraded') {
        kbEl.className = 'st-v warn';
        kbEl.innerHTML = 'ops:briefs <span class="q">←</span> 1 entry <span class="q">·</span> point lookups only';
      } else { kbEl.className = 'st-v'; kbEl.textContent = '—'; }

      if (ckptState) { ckptEl.className = 'st-v ok'; ckptEl.textContent = ckptState; }
      else { ckptEl.className = 'st-v'; ckptEl.textContent = '—'; }
    }

    function paintMeter() {
      if (!meterFill) return;
      var over = spent > cap();
      meterFill.style.transform = 'scaleX(' + Math.min(spent / cap(), 1) + ')';
      meterFill.parentElement.classList.toggle('over', over);
      meterWrap.classList.toggle('over', over);
      spentEl.textContent = fmt(spent);
      capTotal.textContent = fmt(cap());
    }

    function stamp(step) {
      var base = new Date(2026, 7, 6, 8, 0, 12, 0);
      base.setMilliseconds(base.getMilliseconds() + step * 640);
      var p = function (x, n) { return String(x).padStart(n || 2, '0'); };
      return p(base.getHours()) + ':' + p(base.getMinutes()) + ':' + p(base.getSeconds()) +
             '.' + p(base.getMilliseconds(), 3);
    }

    function emit(kind, cls, detail, idx) {
      var el = document.createElement('div');
      el.className = 'rl ' + cls;
      el.innerHTML = '<span class="ts">' + stamp(idx) + '</span>' +
                     '<span class="kind">' + kind + '</span>' +
                     '<span class="det">' + detail + '</span>';
      logEl.appendChild(el);
      logEl.scrollTop = logEl.scrollHeight;
    }

    function reset() {
      clearTimeout(timer);
      logEl.innerHTML = '';
      i = 0; spent = 0; done = false; parked = false; approved = false;
      receipts = 0; denials = 0; kbState = null; ckptState = null;
      outEl.textContent = ''; outEl.className = 'rec-out';
      paintMeter(); paintStatus();
    }

    function finish(kind, extra) {
      done = true; running = false; parked = false;
      setPlay();
      var msg = {
        completed: ['good', 'Run completed. Every line above is a real flight-recorder event kind — ' +
                    'the whole point is that the record exists whether or not anyone is watching.'],
        capability:['bad',  'Denied by the capability engine: <b>' + extra + '</b> was not granted, so the ' +
                    'call never reached the world. Reject, not clamp — and the denial is recorded.'],
        approval:  ['bad',  'The operator denied the request. The agent stopped at the gate rather than ' +
                    'publishing — an approval is a hard boundary, not a suggestion.'],
        budget:    ['bad',  'Admission control stopped the run at <b>' + fmt(cap()) + '</b> tokens. The agent ' +
                    'was <b>deferred</b>, not killed — with a reset window it revives at the next rollover.']
      }[kind];
      outEl.className = 'rec-out ' + msg[0];
      outEl.innerHTML = msg[1];
    }

    function park(idx) {
      parked = true; running = false; clearTimeout(timer); setPlay();
      outEl.className = 'rec-out warn';
      outEl.innerHTML = '<span class="ask">Agent parked at the approval gate — ' +
        'it will wait here indefinitely.</span>' +
        '<span class="acts"><button type="button" class="btn approve">Approve</button>' +
        '<button type="button" class="btn deny">Deny</button></span>';
      outEl.querySelector('.approve').addEventListener('click', function () {
        approved = true; parked = false;
        emit('approval_granted', 'enf', 'act_1  resolved_by=operator', idx);
        running = true; setPlay(); step();
      });
      outEl.querySelector('.deny').addEventListener('click', function () {
        denials++;
        emit('approval_denied', 'abs', 'act_1  resolved_by=operator  reason=declined', idx);
        paintStatus();
        finish('approval');
      });
    }

    function step() {
      if (i >= STEPS.length) { finish('completed'); return; }
      var s = STEPS[i];

      // 1. capability — checked before the call is ever dispatched
      if (!needsOk(s)) {
        denials++;
        if (s.kb) kbState = 'denied';
        emit('capability_denied', 'abs', 'required=' + s.needs + '  granted=no', i);
        paintStatus();
        finish('capability', s.needs);
        return;
      }
      // 2. approval — parks the agent, does not fail it
      if (s.gate && approvalT.classList.contains('on') && !approved) {
        emit('approval_requested', 'mut', 'act_1  action=write_file  risk=medium', i);
        park(i + 1);
        return;
      }
      // 3. budget — admission control, checked before the spend
      if (s.cost > 0 && spent + s.cost > cap()) {
        denials++;
        emit('agent_admission_denied', 'abs',
             'reason=agent_budget_exhausted  would_spend=' + fmt(spent + s.cost) + '  cap=' + fmt(cap()), i);
        emit('agent_deferred', 'abs', 'agent=cos-inbox  revives_at=next budget window', i + 1);
        finish('budget');
        return;
      }

      spent += s.cost;
      if (s.t === 'inference_request') receipts++;
      if (s.kb)   kbState  = semantic() ? 'semantic' : 'degraded';
      if (s.ckpt) ckptState = s.ckpt;
      emit(s.t, s.c, (typeof s.d === 'function' ? s.d() : s.d), i);
      paintMeter(); paintStatus();
      i++;
      timer = setTimeout(step, TICK);
    }

    function setPlay() {
      if (!playBtn) return;
      playBtn.textContent = running ? '❚❚ pause' : (done ? '▶ play' : '▶ resume');
      playBtn.setAttribute('aria-label', running ? 'Pause the recorder' : 'Play the recorder');
      playBtn.disabled = parked;
    }

    function play() { if (parked) return; if (done) reset(); running = true; setPlay(); step(); }
    function pause(){ running = false; clearTimeout(timer); setPlay(); }

    rec = { toggle: function(){ running ? pause() : play(); },
            replay: function(){ reset(); play(); } };

    if (playBtn)   playBtn.addEventListener('click', function(){ rec.toggle(); });
    if (replayBtn) replayBtn.addEventListener('click', function(){ rec.replay(); });

    // Every guardrail re-runs the argument when you change it.
    function bindToggle(btn) {
      btn.addEventListener('click', function () {
        var on = btn.classList.toggle('on');
        btn.setAttribute('aria-pressed', on ? 'true' : 'false');
        rec.replay();
      });
    }
    capToggles.forEach(bindToggle);
    if (approvalT) bindToggle(approvalT);
    if (semanticT) bindToggle(semanticT);

    slider.addEventListener('input',  function(){ budgetLbl.textContent = fmt(cap()); paintMeter(); });
    slider.addEventListener('change', function(){ rec.replay(); });
    budgetLbl.textContent = fmt(cap());
    paintMeter(); paintStatus();

    // Reduced motion: render the settled result at once, no timers.
    if (reduced.matches) {
      while (i < STEPS.length) {
        var s = STEPS[i];
        if (!needsOk(s)) { emit('capability_denied','abs','required='+s.needs+'  granted=no', i);
                           finish('capability', s.needs); return; }
        if (s.gate && approvalT.classList.contains('on') && !approved) {
          emit('approval_requested','mut','act_1  action=write_file  risk=medium', i); park(i+1); return; }
        if (s.cost > 0 && spent + s.cost > cap()) {
          emit('agent_admission_denied','abs','reason=agent_budget_exhausted  cap='+fmt(cap()), i);
          emit('agent_deferred','abs','agent=cos-inbox  revives_at=next budget window', i+1);
          finish('budget'); return; }
        spent += s.cost;
        if (s.t === 'inference_request') receipts++;
        if (s.kb)   kbState  = semantic() ? 'semantic' : 'degraded';
        if (s.ckpt) ckptState = s.ckpt;
        emit(s.t, s.c, (typeof s.d === 'function' ? s.d() : s.d), i);
        paintMeter(); paintStatus();
        i++;
      }
      finish('completed');
      return;
    }

    document.addEventListener('visibilitychange', function () {
      if (document.hidden && running) pause();
    });

    if ('IntersectionObserver' in window) {
      var seen = false;
      var ro = new IntersectionObserver(function (en) {
        if (en[0].isIntersecting && !seen) { seen = true; play(); ro.disconnect(); }
      }, { threshold: 0.12 });   // low, so a short laptop viewport still trips it
      ro.observe(host);
    } else { play(); }
  })();

  /* ─── verification miniature (zk-verification.html) ─────────────────────
     Three linked panes: actions → proof generation → verification. The hash
     is a toy FNV-ish string hash, not the shipped SHA-256/Ed25519 — the page
     says so — but the shape of the check is real: sign a chain head once,
     then show that removing a receipt from underneath it is exactly what a
     recomputed-root comparison catches. */
  (function verifyDemo() {
    var host = document.querySelector('[data-verify-demo]');
    if (!host) return;

    var actionsLog = host.querySelector('[data-vd-actions]');
    var proofLog   = host.querySelector('[data-vd-proof]');
    var verifyLog  = host.querySelector('[data-vd-verify]');
    var hintEl     = host.querySelector('[data-vd-hint]');
    var artifactEl = host.querySelector('[data-vd-artifact]');
    var verdictEl  = host.querySelector('[data-vd-verdict]');
    var replayBtn  = host.querySelector('[data-vd-replay]');

    var ACTIONS = [
      { t: 'agent_scheduled',   c: 'flow', d: 'agent=cos-inbox  in_flight=1' },
      { t: 'inference_request', c: 'flow', d: 'model=claude-sonnet  retained_tokens_est=1,840' },
      { t: 'tool_call',         c: 'mut',  d: 'gmail.search  q="in:inbox"  cap=Mcp{google_oauth}' },
      { t: 'tool_call',         c: 'mut',  d: 'write_file  path=/data/output/brief.md  cap=FsWrite{/data/output}' },
      { t: 'brief_written',     c: 'enf',  d: 'brief_id=2026-08-06  run_count=3' },
      { t: 'agent_completed',   c: 'enf',  d: 'agent=cos-inbox  turns=4' }
    ];
    var GENESIS = 'runtime1-genesis';
    var TICK = 480;

    var alive = [];          // {idx, t, c, d, row}
    var signedRoot = null, signedSig = null, signedCount = 0;
    var timer = null;

    function chainRoot(items) {
      var prev = h(GENESIS);
      for (var i = 0; i < items.length; i++) {
        prev = h(prev + '|' + items[i].idx + items[i].t + items[i].d);
      }
      return prev;
    }

    function sign(rootHash) { return 'ed25519:' + h('a:' + rootHash) + h('b:' + rootHash).slice(0, 4); }

    function stamp(idx) {
      var base = new Date(2026, 7, 6, 9, 0, 0, 0);
      base.setMilliseconds(base.getMilliseconds() + idx * 640);
      var p = function (x, n) { return String(x).padStart(n || 2, '0'); };
      return p(base.getHours()) + ':' + p(base.getMinutes()) + ':' + p(base.getSeconds()) +
             '.' + p(base.getMilliseconds(), 3);
    }

    function line(el, cls, text) {
      var row = document.createElement('div');
      row.className = 'rl plain ' + (cls || 'flow');
      row.innerHTML = text;
      el.appendChild(row);
      el.scrollTop = el.scrollHeight;
      return row;
    }

    function actionRow(a) {
      var row = document.createElement('div');
      row.className = 'rl ' + a.c;
      row.dataset.idx = a.idx;
      row.innerHTML =
        '<span class="ts">' + stamp(a.idx) + '</span>' +
        '<span class="kind">' + a.t + '</span>' +
        '<span class="det">' + a.d + '</span>' +
        '<button type="button" class="vd-rm" data-vd-rm aria-label="Remove this action">&times;</button>';
      actionsLog.appendChild(row);
      actionsLog.scrollTop = actionsLog.scrollHeight;
      row.querySelector('[data-vd-rm]').addEventListener('click', function () { removeAction(a, row); });
      return row;
    }

    function removeAction(a, row) {
      alive = alive.filter(function (x) { return x !== a; });
      row.classList.add('removed');
      setTimeout(function () { row.remove(); }, 260);
      if (signedRoot) reverify();
    }

    function reverify() {
      verifyLog.innerHTML = '';
      var root2 = chainRoot(alive);
      line(verifyLog, 'flow', 'fetch published chain head + public key');
      line(verifyLog, 'mut',  'recompute chain from the log (' + alive.length + ' receipt' + (alive.length === 1 ? '' : 's') + ')');
      line(verifyLog, 'mut',  'compare  recomputed=' + root2 + '  signed=' + signedRoot);
      if (root2 === signedRoot && alive.length === signedCount) {
        line(verifyLog, 'enf', 'signature valid over ' + signedRoot);
        verdictEl.className = 'rec-out vd-note good';
        verdictEl.innerHTML = '&#10003; verified — ' + alive.length + ' receipts, chain unbroken, signature valid over <b>' + signedRoot + '</b>.';
      } else {
        line(verifyLog, 'abs', 'root mismatch — signature check FAILED');
        verdictEl.className = 'rec-out vd-note bad';
        verdictEl.innerHTML = '&#10007; verification failed — the signed root committed to <b>' + signedCount +
          '</b> receipts; the log now has <b>' + alive.length + '</b>. Recomputed root <b>' + root2 +
          '</b> &ne; signed root <b>' + signedRoot + '</b>. An action was removed after the proof was' +
          ' generated — exactly what a hash chain is built to catch.';
      }
    }

    function generateProof() {
      hintEl.textContent = 'run complete — generating proof…';
      proofLog.innerHTML = '';
      var step = 0;
      function next() {
        if (step < alive.length) {
          var a = alive[step];
          line(proofLog, 'mut', 'hash receipt #' + a.idx + '  ' + a.t + '  &rarr; ' + h(a.idx + a.t + a.d).slice(0, 8));
          step++;
          timer = setTimeout(next, TICK);
          return;
        }
        if (step === alive.length) {
          signedRoot = chainRoot(alive);
          line(proofLog, 'flow', 'chain head = ' + signedRoot);
          step++;
          timer = setTimeout(next, TICK);
          return;
        }
        signedSig = sign(signedRoot);
        signedCount = alive.length;
        line(proofLog, 'enf', 'sign(chain head, sk) = ' + signedSig);
        artifactEl.className = 'rec-out vd-note good';
        artifactEl.innerHTML = 'chain head <b>' + signedRoot + '</b> &middot; signature <b>' + signedSig +
          '</b> &middot; ' + signedCount + ' receipts signed';
        hintEl.textContent = 'verifying…';
        timer = setTimeout(runVerify, TICK);
      }
      next();
    }

    function runVerify() {
      verifyLog.innerHTML = '';
      var steps = [
        ['flow', 'fetch published chain head + public key'],
        ['mut',  'recompute chain from the log (' + alive.length + ' receipts)'],
        ['mut',  'compare  recomputed=' + signedRoot + '  signed=' + signedRoot],
        ['enf',  'signature valid over ' + signedRoot]
      ];
      var step = 0;
      function next() {
        if (step >= steps.length) {
          verdictEl.className = 'rec-out vd-note good';
          verdictEl.innerHTML = '&#10003; verified — ' + alive.length + ' receipts, chain unbroken, signature valid over <b>' + signedRoot + '</b>.';
          actionsLog.classList.add('removable');
          hintEl.textContent = 'try removing an action on the left';
          return;
        }
        line(verifyLog, steps[step][0], steps[step][1]);
        step++;
        timer = setTimeout(next, TICK);
      }
      next();
    }

    function reset() {
      clearTimeout(timer);
      actionsLog.innerHTML = ''; actionsLog.classList.remove('removable');
      proofLog.innerHTML = ''; verifyLog.innerHTML = '';
      artifactEl.className = 'rec-out vd-note'; artifactEl.textContent = 'waiting for the run to finish…';
      verdictEl.className = 'rec-out vd-note'; verdictEl.textContent = 'waiting for a proof…';
      hintEl.textContent = 'recording…';
      alive = ACTIONS.map(function (a, idx) { return { idx: idx, t: a.t, c: a.c, d: a.d }; });
      signedRoot = null; signedSig = null; signedCount = 0;
    }

    function record() {
      reset();
      var i = 0;
      function next() {
        if (i >= alive.length) { generateProof(); return; }
        actionRow(alive[i]);
        i++;
        timer = setTimeout(next, TICK);
      }
      next();
    }

    if (replayBtn) replayBtn.addEventListener('click', record);

    if (reduced.matches) {
      reset();
      alive.forEach(actionRow);
      signedRoot = chainRoot(alive);
      signedSig = sign(signedRoot);
      signedCount = alive.length;
      line(proofLog, 'enf', 'sign(chain head, sk) = ' + signedSig);
      artifactEl.className = 'rec-out vd-note good';
      artifactEl.innerHTML = 'chain head <b>' + signedRoot + '</b> &middot; signature <b>' + signedSig +
        '</b> &middot; ' + signedCount + ' receipts signed';
      line(verifyLog, 'enf', 'signature valid over ' + signedRoot);
      verdictEl.className = 'rec-out vd-note good';
      verdictEl.innerHTML = '&#10003; verified — ' + alive.length + ' receipts, chain unbroken, signature valid over <b>' + signedRoot + '</b>.';
      actionsLog.classList.add('removable');
      hintEl.textContent = 'try removing an action on the left';
    } else if ('IntersectionObserver' in window) {
      var seen = false;
      var ro = new IntersectionObserver(function (en) {
        if (en[0].isIntersecting && !seen) { seen = true; record(); ro.disconnect(); }
      }, { threshold: 0.12 });
      ro.observe(host);
    } else { record(); }
  })();

  /* ─── zk.2, illustrated (zk-verification.html) ───────────────────────────
     zk.2 is unbuilt — the page says so, repeatedly, on purpose. This is not a
     simulation of running code; it's a diagram that happens to be interactive.
     The property it shows is different from the hash-chain demo above: policy
     CONSISTENCY, checked by a verifier who never receives the policy, only a
     commitment to it. Flipping a verdict after the proof exists is the zk
     analogue of removing a receipt — same class of failure, different cause. */
  (function zkDemo() {
    var host = document.querySelector('[data-zk-demo]');
    if (!host) return;

    var decLog     = host.querySelector('[data-zk-decisions]');
    var proveLog   = host.querySelector('[data-zk-prove]');
    var verifyLog  = host.querySelector('[data-zk-verify]');
    var hintEl     = host.querySelector('[data-zk-hint]');
    var artifactEl = host.querySelector('[data-zk-artifact]');
    var verdictEl  = host.querySelector('[data-zk-verdict]');
    var replayBtn  = host.querySelector('[data-zk-replay]');

    var TEMPLATE = [
      { rule: 'cap_check',      d: 'tool=gmail.search  cap=Mcp{google_oauth}' },
      { rule: 'budget_check',   d: 'spend=1,840  cap=12,000' },
      { rule: 'cap_check',      d: 'tool=write_file  cap=FsWrite{/data/output}' },
      { rule: 'approval_check', d: 'risk=medium  gate=off' },
      { rule: 'budget_check',   d: 'spend=9,650  cap=12,000' }
    ];
    var TICK = 480;
    var decisions = [], provenVerdicts = null, commitment = null, proof = null;
    var timer = null;

    function policyCommitment() { return h('policy-v3|' + TEMPLATE.length + '|secret-rules-never-shown'); }

    function line(el, cls, text) {
      var row = document.createElement('div');
      row.className = 'rl plain ' + (cls || 'flow');
      row.innerHTML = text;
      el.appendChild(row);
      el.scrollTop = el.scrollHeight;
      return row;
    }

    function decisionRow(d) {
      var row = document.createElement('div');
      row.className = 'rl mut';
      row.dataset.idx = d.idx;
      row.innerHTML =
        '<span class="kind">' + d.rule + '</span>' +
        '<span class="det">' + d.d + '</span>' +
        '<button type="button" class="zk-verdict ' + (d.verdict === 'ALLOW' ? 'allow' : 'deny') +
        '" data-zk-toggle>' + d.verdict + '</button>';
      decLog.appendChild(row);
      decLog.scrollTop = decLog.scrollHeight;
      row.querySelector('[data-zk-toggle]').addEventListener('click', function (ev) {
        d.verdict = d.verdict === 'ALLOW' ? 'DENY' : 'ALLOW';
        ev.currentTarget.textContent = d.verdict;
        ev.currentTarget.className = 'zk-verdict ' + (d.verdict === 'ALLOW' ? 'allow' : 'deny');
        if (provenVerdicts) reverify();
      });
      return row;
    }

    function reverify() {
      verifyLog.innerHTML = '';
      var current = decisions.map(function (d) { return d.verdict; });
      line(verifyLog, 'flow', 'receive proof &pi; + policy commitment (not the policy)');
      line(verifyLog, 'mut',  'check &pi; against commitment ' + commitment + ' + claimed decisions');
      var mismatchAt = -1;
      for (var i = 0; i < current.length; i++) {
        if (current[i] !== provenVerdicts[i]) { mismatchAt = i; break; }
      }
      if (mismatchAt === -1) {
        line(verifyLog, 'enf', 'policy-consistent');
        verdictEl.className = 'rec-out vd-note good';
        verdictEl.innerHTML = '&#10003; policy-consistent — ' + current.length +
          ' decisions verified against commitment <b>' + commitment + '</b>, policy never revealed.';
      } else {
        line(verifyLog, 'abs', 'decision #' + mismatchAt + ' mismatch — proof commits to ' +
          provenVerdicts[mismatchAt] + ', journal now claims ' + current[mismatchAt]);
        line(verifyLog, 'abs', 'policy consistency check FAILED');
        verdictEl.className = 'rec-out vd-note bad';
        verdictEl.innerHTML = '&#10007; inconsistent — the proof commits to decision #' + mismatchAt +
          ' resolving <b>' + provenVerdicts[mismatchAt] + '</b>; the journal now claims <b>' +
          current[mismatchAt] + '</b>. The proof can\'t tell you which side is lying — only that ' +
          'they no longer agree.';
      }
    }

    function generateProof() {
      hintEl.textContent = 'run complete — generating proof…';
      proveLog.innerHTML = '';
      commitment = policyCommitment();
      var step = 0;
      function next() {
        if (step === 0) {
          line(proveLog, 'flow', 'commit(policy) = ' + commitment + '  <span class="zk-lock">(policy never leaves the box)</span>');
          step++; timer = setTimeout(next, TICK); return;
        }
        if (step <= decisions.length) {
          var d = decisions[step - 1];
          line(proveLog, 'mut', 'decision #' + d.idx + '  ' + d.rule + '  &rarr; ' + d.verdict);
          step++; timer = setTimeout(next, TICK); return;
        }
        provenVerdicts = decisions.map(function (d) { return d.verdict; });
        proof = 'zk:' + h('pi|' + commitment + '|' + provenVerdicts.join(',')) +
                h('pi2|' + commitment).slice(0, 4);
        line(proveLog, 'enf', '&pi; = ' + proof);
        artifactEl.className = 'rec-out vd-note good';
        artifactEl.innerHTML = 'commitment <b>' + commitment + '</b> &middot; proof <b>' + proof +
          '</b> &middot; ' + provenVerdicts.length + ' decisions proven';
        hintEl.textContent = 'verifying…';
        timer = setTimeout(runVerify, TICK);
      }
      next();
    }

    function runVerify() {
      verifyLog.innerHTML = '';
      var steps = [
        ['flow', 'receive proof &pi; + policy commitment (not the policy)'],
        ['mut',  'check &pi; against commitment ' + commitment + ' + claimed decisions'],
        ['enf',  'policy-consistent']
      ];
      var step = 0;
      function next() {
        if (step >= steps.length) {
          verdictEl.className = 'rec-out vd-note good';
          verdictEl.innerHTML = '&#10003; policy-consistent — ' + decisions.length +
            ' decisions verified against commitment <b>' + commitment + '</b>, policy never revealed.';
          hintEl.textContent = 'try flipping a verdict on the left';
          return;
        }
        line(verifyLog, steps[step][0], steps[step][1]);
        step++;
        timer = setTimeout(next, TICK);
      }
      next();
    }

    function reset() {
      clearTimeout(timer);
      decLog.innerHTML = ''; proveLog.innerHTML = ''; verifyLog.innerHTML = '';
      artifactEl.className = 'rec-out vd-note'; artifactEl.textContent = 'waiting for the run to finish…';
      verdictEl.className = 'rec-out vd-note'; verdictEl.textContent = 'waiting for a proof…';
      hintEl.textContent = 'recording…';
      decisions = TEMPLATE.map(function (t, idx) { return { idx: idx, rule: t.rule, d: t.d, verdict: 'ALLOW' }; });
      provenVerdicts = null; commitment = null; proof = null;
    }

    function record() {
      reset();
      var i = 0;
      function next() {
        if (i >= decisions.length) { generateProof(); return; }
        decisionRow(decisions[i]);
        i++;
        timer = setTimeout(next, TICK);
      }
      next();
    }

    if (replayBtn) replayBtn.addEventListener('click', record);

    if (reduced.matches) {
      reset();
      decisions.forEach(decisionRow);
      commitment = policyCommitment();
      provenVerdicts = decisions.map(function (d) { return d.verdict; });
      proof = 'zk:' + h('pi|' + commitment + '|' + provenVerdicts.join(',')) + h('pi2|' + commitment).slice(0, 4);
      line(proveLog, 'flow', 'commit(policy) = ' + commitment + '  <span class="zk-lock">(policy never leaves the box)</span>');
      line(proveLog, 'enf', '&pi; = ' + proof);
      artifactEl.className = 'rec-out vd-note good';
      artifactEl.innerHTML = 'commitment <b>' + commitment + '</b> &middot; proof <b>' + proof +
        '</b> &middot; ' + provenVerdicts.length + ' decisions proven';
      line(verifyLog, 'enf', 'policy-consistent');
      verdictEl.className = 'rec-out vd-note good';
      verdictEl.innerHTML = '&#10003; policy-consistent — ' + decisions.length +
        ' decisions verified against commitment <b>' + commitment + '</b>, policy never revealed.';
      hintEl.textContent = 'try flipping a verdict on the left';
    } else if ('IntersectionObserver' in window) {
      var seen = false;
      var ro = new IntersectionObserver(function (en) {
        if (en[0].isIntersecting && !seen) { seen = true; record(); ro.disconnect(); }
      }, { threshold: 0.12 });
      ro.observe(host);
    } else { record(); }
  })();

})();
