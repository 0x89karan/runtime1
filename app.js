/* runtime1 site — shared interaction layer.
   No dependencies, no build step, no network: the site stays a folder of static files you can
   open from disk. Everything here degrades to the existing static page if JS is off.

   The interaction model deliberately echoes the product: agentctl watch is a keyboard-driven
   cockpit with a [?] help key, so the site is too. */
(function () {
  'use strict';

  var root = document.documentElement;
  var reduced = window.matchMedia('(prefers-reduced-motion: reduce)');

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

})();
