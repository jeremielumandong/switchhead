// switchhead site: theme picker, live graph sample, tour, and small interactions.
// No framework and no build step; the page renders without this script, minus the sample.
(function () {
  'use strict';

  const $ = (s, el = document) => el.querySelector(s);
  const $$ = (s, el = document) => Array.from(el.querySelectorAll(s));
  const h = (tag, cls, html) => { const e = document.createElement(tag); if (cls) e.className = cls; if (html != null) e.innerHTML = html; return e; };
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const store = {
    get(k) { try { return localStorage.getItem(k); } catch (e) { return null; } },
    set(k, v) { try { localStorage.setItem(k, v); } catch (e) { /* private mode */ } },
  };

  // ---------------------------------------------------------------- themes
  // The app's palettes, from crates/app/src/theme.rs.
  const THEMES = [
    { key: 'dark', name: 'switchhead Dark', dark: true, bg: '#0e0e10', panel: '#131316', surface: '#17171a', elev: '#1d1d21', bd: '#232328', bd2: '#2f2f35', fg: '#e7e7ea', fg2: '#a1a1aa', fg3: '#6e6e77', acc: '#5aa3ec', accfg: '#0b0b0d', red: '#f05653', yellow: '#ebae42', green: '#5ec077', kw: '#bd9ff2', str: '#85cc87', num: '#efaf6f', fn: '#6ac5e8', cm: '#6e6e77' },
    { key: 'light', name: 'switchhead Light', dark: false, bg: '#f2f2f3', panel: '#f8f8f9', surface: '#ffffff', elev: '#ffffff', bd: '#e2e2e6', bd2: '#d2d2d8', fg: '#18181b', fg2: '#53535c', fg3: '#8a8a93', acc: '#2971c6', accfg: '#ffffff', red: '#d02b31', yellow: '#c57800', green: '#218a45', kw: '#6e3ead', str: '#27762f', num: '#ad5600', fn: '#00649e', cm: '#8a8a93' },
    { key: 'nord', name: 'Nord', dark: true, bg: '#242933', panel: '#2a303c', surface: '#2e3440', elev: '#3b4252', bd: '#3b4252', bd2: '#4c566a', fg: '#eceff4', fg2: '#d8dee9', fg3: '#9aa5b9', acc: '#88c0d0', accfg: '#2e3440', red: '#d57780', yellow: '#ebcb8b', green: '#a3be8c', kw: '#8fb2cf', str: '#a3be8c', num: '#c4a0bd', fn: '#88c0d0', cm: '#8a96ad' },
    { key: 'dracula', name: 'Dracula', dark: true, bg: '#1e1f29', panel: '#21222c', surface: '#282a36', elev: '#343746', bd: '#343746', bd2: '#44475a', fg: '#f8f8f2', fg2: '#d0d2dc', fg3: '#9aa1c4', acc: '#bd93f9', accfg: '#1e1f29', red: '#ff5555', yellow: '#f1fa8c', green: '#50fa7b', kw: '#ff79c6', str: '#f1fa8c', num: '#bd93f9', fn: '#50fa7b', cm: '#8a93c2' },
    { key: 'catppuccin-mocha', name: 'Catppuccin Mocha', dark: true, bg: '#11111b', panel: '#181825', surface: '#1e1e2e', elev: '#313244', bd: '#313244', bd2: '#45475a', fg: '#cdd6f4', fg2: '#bac2de', fg3: '#9399b2', acc: '#89b4fa', accfg: '#11111b', red: '#f38ba8', yellow: '#f9e2af', green: '#a6e3a1', kw: '#cba6f7', str: '#a6e3a1', num: '#fab387', fn: '#89b4fa', cm: '#9399b2' },
    { key: 'tokyo-night', name: 'Tokyo Night', dark: true, bg: '#13131a', panel: '#16161e', surface: '#1a1b26', elev: '#24283b', bd: '#24283b', bd2: '#3b4261', fg: '#c0caf5', fg2: '#a9b1d6', fg3: '#8189b5', acc: '#7aa2f7', accfg: '#16161e', red: '#f7768e', yellow: '#e0af68', green: '#9ece6a', kw: '#bb9af7', str: '#9ece6a', num: '#ff9e64', fn: '#7aa2f7', cm: '#7f88b6' },
    { key: 'gruvbox-dark', name: 'Gruvbox Dark', dark: true, bg: '#1d2021', panel: '#202324', surface: '#282828', elev: '#32302f', bd: '#3c3836', bd2: '#504945', fg: '#ebdbb2', fg2: '#d5c4a1', fg3: '#a89984', acc: '#83a598', accfg: '#1d2021', red: '#fb5f4a', yellow: '#fabd2f', green: '#b8bb26', kw: '#fb6a56', str: '#b8bb26', num: '#d3869b', fn: '#8ec07c', cm: '#928374' },
    { key: 'high-contrast', name: 'High Contrast', dark: true, bg: '#000000', panel: '#0a0a0a', surface: '#050505', elev: '#141414', bd: '#3a3a3a', bd2: '#5a5a5a', fg: '#ffffff', fg2: '#e0e0e0', fg3: '#b8b8b8', acc: '#4fc1ff', accfg: '#000000', red: '#ff6b6b', yellow: '#ffd75f', green: '#7ee787', kw: '#d7a6ff', str: '#9ef09e', num: '#ffcb6b', fn: '#7fdbff', cm: '#b8b8b8' },
    { key: 'catppuccin-latte', name: 'Catppuccin Latte', dark: false, bg: '#e6e9ef', panel: '#eceef3', surface: '#eff1f5', elev: '#f7f8fa', bd: '#ccd0da', bd2: '#bcc0cc', fg: '#3a3d55', fg2: '#53566e', fg3: '#6c6f85', acc: '#1e66f5', accfg: '#ffffff', red: '#d20f39', yellow: '#a8650a', green: '#2f7d1e', kw: '#7a2fd8', str: '#2f7d1e', num: '#a84607', fn: '#1e5fe0', cm: '#7c7f93' },
    { key: 'solarized-light', name: 'Solarized Light', dark: false, bg: '#eee8d5', panel: '#f5efdc', surface: '#fdf6e3', elev: '#fffbef', bd: '#e4dcc4', bd2: '#d6cdb2', fg: '#073642', fg2: '#47595f', fg3: '#5f7175', acc: '#1b6fae', accfg: '#ffffff', red: '#c52a27', yellow: '#8f6c00', green: '#667500', kw: '#5c6a00', str: '#17736d', num: '#b32c6f', fn: '#1b6fae', cm: '#6f8083' },
  ];
  const VARS = ['bg', 'panel', 'surface', 'elev', 'bd', 'bd2', 'fg', 'fg2', 'fg3', 'acc', 'accfg', 'red', 'yellow', 'green', 'kw', 'str', 'num', 'fn', 'cm'];
  // Graph lane colors, in the app's order (graph_paint::colors).
  const laneColors = (t) => [t.acc, t.green, t.yellow, t.kw, t.red, t.fn, t.num, t.str];
  let theme = THEMES.find((t) => t.key === document.documentElement.dataset.theme) || THEMES[0];

  function miniLanes(t) {
    const c = laneColors(t);
    return `<svg class="lanes" viewBox="0 0 44 64" fill="none" stroke-width="2.4" stroke-linecap="round">
      <path d="M10 4V60" stroke="${c[0]}"/><path d="M10 14C10 22 24 20 24 28V40C24 48 10 46 10 52" stroke="${c[1]}"/>
      <path d="M24 30C24 36 36 34 36 42V46" stroke="${c[2]}"/>
      <circle cx="10" cy="8" r="3.4" fill="${c[0]}"/><circle cx="24" cy="30" r="3.4" fill="${c[1]}"/><circle cx="36" cy="46" r="3.4" fill="${c[2]}"/><circle cx="10" cy="52" r="3.4" fill="${c[0]}"/></svg>`;
  }

  function themeCard(t) {
    const b = h('button', 'theme-card');
    b.type = 'button';
    b.dataset.key = t.key;
    b.innerHTML = `<div class="tc-prev" style="background:${t.surface}">
        <div class="bars"><i style="width:56px;background:${t.fg}"></i><i style="width:26px;background:${t.acc}"></i></div>
        <div class="bars"><i style="width:38px;background:${t.fg3}"></i></div>
        ${miniLanes(t)}
        <div class="sw">${[t.red, t.yellow, t.green, t.acc, t.kw, t.fn].map((c) => `<i style="background:${c}"></i>`).join('')}</div>
      </div><div class="tc-name">${esc(t.name)}<small>${t.dark ? 'dark' : 'light'}</small></div>`;
    b.addEventListener('click', () => applyTheme(t.key, true));
    return b;
  }

  function applyTheme(key, save) {
    theme = THEMES.find((t) => t.key === key) || THEMES[0];
    const root = document.documentElement;
    VARS.forEach((v) => root.style.setProperty('--' + v, theme[v]));
    laneColors(theme).forEach((c, i) => root.style.setProperty('--l' + i, c));
    root.dataset.theme = theme.key;
    root.dataset.mode = theme.dark ? 'dark' : 'light';
    const meta = $('meta[name="theme-color"]');
    if (meta) meta.content = theme.bg;
    $('#themePillName').textContent = theme.name;
    $('#themeNow').textContent = theme.name;
    $('#themeDots').innerHTML = [theme.acc, theme.green, theme.yellow, theme.red].map((c) => `<i style="background:${c}"></i>`).join('');
    $$('.theme-card, .theme-chips button').forEach((b) => b.classList.toggle('on', b.dataset.key === theme.key));
    if (save) store.set('switchhead-site-theme', theme.key);
  }

  function buildThemes() {
    const grid = $('#themeGrid');
    const modal = $('#themeGridModal');
    THEMES.forEach((t) => { grid.appendChild(themeCard(t)); modal.appendChild(themeCard(t)); });
    const chips = $('#themeChips');
    ['dark', 'tokyo-night', 'catppuccin-mocha', 'gruvbox-dark', 'nord', 'dracula', 'light', 'solarized-light'].forEach((k) => {
      const t = THEMES.find((x) => x.key === k);
      const b = h('button', '', `<i style="background:${t.acc}"></i>${esc(t.name.replace('switchhead ', ''))}`);
      b.type = 'button';
      b.dataset.key = k;
      b.addEventListener('click', () => applyTheme(k, true));
      chips.appendChild(b);
    });
    const all = h('button', '', 'All themes ↗');
    all.type = 'button';
    all.addEventListener('click', openThemes);
    chips.appendChild(all);
  }

  const modalEl = $('#themeModal');
  function openThemes() { modalEl.hidden = false; }
  $('#themePill').addEventListener('click', openThemes);
  modalEl.addEventListener('click', (e) => { if (e.target === modalEl || e.target.closest('[data-close]')) modalEl.hidden = true; });

  // ---------------------------------------------------------------- graph layout
  // A small version of switchhead_git::graph: lanes hold the next commit they expect;
  // a commit takes the first lane expecting it, merges join, parents get lanes.
  function layout(commits) {
    let lanes = []; // { id, color }
    let nextColor = 0;
    const rows = [];
    for (const c of commits) {
      const before = lanes.map((l) => l && { ...l });
      let col = lanes.findIndex((l) => l && l.id === c.id);
      if (col < 0) {
        col = lanes.findIndex((l) => !l);
        if (col < 0) col = lanes.length;
        lanes[col] = { id: c.id, color: nextColor++ };
      }
      const color = lanes[col].color;
      // Other lanes waiting for this commit end here.
      const joins = [];
      lanes.forEach((l, i) => { if (l && i !== col && l.id === c.id) { joins.push({ from: i, color: l.color }); lanes[i] = null; } });
      const outs = [];
      if (c.p.length === 0) {
        lanes[col] = null;
      } else {
        lanes[col] = { id: c.p[0], color };
        outs.push({ to: col, color });
        for (const p of c.p.slice(1)) {
          let j = lanes.findIndex((l) => l && l.id === p);
          if (j >= 0) { outs.push({ to: j, color: lanes[j].color }); continue; }
          j = lanes.findIndex((l, i) => !l && i > col);
          if (j < 0) j = lanes.length;
          lanes[j] = { id: p, color: nextColor++ };
          outs.push({ to: j, color: lanes[j].color });
        }
      }
      while (lanes.length && !lanes[lanes.length - 1]) lanes.pop();
      const pass = [];
      before.forEach((l, i) => { if (l && l.id !== c.id && lanes[i] && lanes[i].id === l.id) pass.push({ at: i, color: l.color }); });
      rows.push({ col, color, joins, outs, pass, top: before[col] && before[col].id === c.id });
    }
    return rows;
  }

  const LW = 14; // lane width in px
  const X = (i) => 9 + i * LW;
  function rowSvg(r, isHead, isMerge, H) {
    const m = H / 2;
    const cls = (c) => `l${c % 8}`;
    let s = '';
    for (const p of r.pass) s += `<path class="${cls(p.color)}" d="M${X(p.at)} 0V${H}"/>`;
    if (r.top) s += `<path class="${cls(r.color)}" d="M${X(r.col)} 0V${m}"/>`;
    for (const j of r.joins) s += `<path class="${cls(j.color)}" d="M${X(j.from)} 0C${X(j.from)} ${m * 0.9} ${X(r.col)} ${m * 0.6} ${X(r.col)} ${m}"/>`;
    for (const o of r.outs) {
      s += o.to === r.col
        ? `<path class="${cls(o.color)}" d="M${X(o.to)} ${m}V${H}"/>`
        : `<path class="${cls(o.color)}" d="M${X(r.col)} ${m}C${X(r.col)} ${H * 0.95} ${X(o.to)} ${m * 1.05} ${X(o.to)} ${H}"/>`;
    }
    const cx = X(r.col);
    s += isHead
      ? `<circle cx="${cx}" cy="${m}" r="4.6" class="node-head ${cls(r.color)}"/>`
      : `<circle cx="${cx}" cy="${m}" r="${isMerge ? 3.2 : 4.2}" class="node ${cls(r.color)}"/>`;
    return `<svg viewBox="0 0 120 ${H}" fill="none">${s}</svg>`;
  }

  // ---------------------------------------------------------------- live sample
  const RUST_KW = /\b(fn|let|mut|pub|use|mod|impl|struct|enum|const|static|match|if|else|for|while|loop|return|self|Self|crate|super|where|as|in|ref|move|async|await|dyn|trait|type|true|false|Some|None|Ok|Err)\b/g;
  function highlight(line, path) {
    const t = esc(line);
    if (!/\.(rs|toml|ya?ml)$/.test(path)) return t;
    const cm = path.endsWith('.rs') ? t.indexOf('//') : t.indexOf('#');
    const code = cm >= 0 ? t.slice(0, cm) : t;
    const comment = cm >= 0 ? `<span class="c">${t.slice(cm)}</span>` : '';
    const out = code
      .replace(/(&quot;.*?&quot;)/g, '\u0001$1\u0002')
      .split(/(\u0001.*?\u0002)/)
      .map((part) => part.startsWith('\u0001')
        ? `<span class="s2">${part.slice(1, -1)}</span>`
        : part.replace(RUST_KW, '<span class="k">$1</span>')
              .replace(/\b(\d[\d_]*(?:\.\d+)?)\b/g, '<span class="nm">$1</span>')
              .replace(/\b([a-z_][a-z0-9_]*)(?=\()/g, '<span class="fn">$1</span>'))
      .join('');
    return out + comment;
  }

  function buildSample() {
    const data = window.SAMPLE_HISTORY;
    if (!data) return;
    const rowsEl = $('#graphRows');
    const H = 26;
    const lay = layout(data);
    const refs = {
      [data[0].id]: '<span class="ref head">main</span><span class="ref">origin/main</span>',
      [data[1].id]: '<span class="ref">origin/claude/implement-plan-3vqnl6</span>',
    };
    let selected = 0;
    const rowEls = [];

    function renderDetails(i) {
      const c = data[i];
      const isMerge = c.p.length > 1;
      $('#detLeft').innerHTML = `<h4>${esc(c.s)}</h4>
        ${refs[c.id] ? `<div style="display:flex;gap:4px;margin-bottom:8px">${refs[c.id]}</div>` : ''}
        <dl class="det-kv"><dt>Author</dt><dd>${esc(c.a)} <span class="dim">${esc(c.d)}</span></dd>
        <dt>Commit</dt><dd class="mono">${c.id}…  <span class="link" style="font-family:var(--sans)">Copy</span></dd>
        <dt>Parent${isMerge ? 's' : ''}</dt><dd>${c.p.map((p) => `<span class="link">${p}</span>`).join('  ')}</dd></dl>
        <div class="dim" style="font-size:12px;margin-bottom:4px">${c.nf} file${c.nf === 1 ? '' : 's'} changed${isMerge ? ' (first parent)' : ''}</div>
        <div class="det-files">${c.f.map((f) => {
          const k = f[1].lastIndexOf('/');
          const on = c.df && f[1] === c.df.path ? ' on' : '';
          return `<div class="f${on}"><span class="st ${f[0]}">${f[0]}</span>${esc(f[1].slice(k + 1))} <span class="p">${esc(k > 0 ? f[1].slice(0, k) : '')}</span></div>`;
        }).join('')}</div>`;
      const d = c.df;
      if (!d) { $('#detDiff').innerHTML = ''; return; }
      const name = d.path.slice(d.path.lastIndexOf('/') + 1);
      $('#detDiff').innerHTML = `<div class="dd-head"><span class="st M" style="font:600 10px var(--mono);width:16px;height:16px;display:grid;place-items:center;border-radius:3px">M</span><span class="mono" style="font-size:12px">${esc(name)}</span><span class="add">+${d.add}</span><span class="del">-${d.del_}</span><span class="seg"><b>Inline</b><span>Side by side</span></span></div>` +
        d.l.map(([t, a, b, s]) => t === 'h'
          ? `<div class="dl hunk">${esc(s)}</div>`
          : `<div class="dl ${t === 'a' ? 'add' : t === 'd' ? 'del' : ''}"><span class="n">${a || ''}</span><span class="n">${b || ''}</span><span class="m">${t === 'a' ? '+' : t === 'd' ? '-' : ''}</span>${highlight(s, d.path) || ' '}</div>`).join('');
    }

    function select(i, scroll) {
      selected = Math.max(0, Math.min(rowEls.length - 1, i));
      rowEls.forEach((r, k) => r.classList.toggle('sel', k === selected));
      renderDetails(selected);
      if (scroll) {
        const r = rowEls[selected];
        if (r.offsetTop < rowsEl.scrollTop) rowsEl.scrollTop = r.offsetTop;
        else if (r.offsetTop + H > rowsEl.scrollTop + rowsEl.clientHeight) rowsEl.scrollTop = r.offsetTop + H - rowsEl.clientHeight;
      }
    }

    function addRow(i) {
      const c = data[i];
      const isMerge = c.p.length > 1;
      const el = h('div', 'row' + (isMerge ? ' merge' : ''));
      el.setAttribute('role', 'option');
      el.innerHTML = `<span class="g">${rowSvg(lay[i], i === 0, isMerge, H)}</span>
        <span class="d">${refs[c.id] || ''}<span class="s">${esc(c.s)}</span></span>
        <span class="hd"></span><span class="a">${esc(c.a)}</span><span class="t">${esc(c.d)}</span><span class="i">${c.id}</span>`;
      el.addEventListener('mousedown', () => { rowsEl.focus({ preventScroll: true }); select(i); });
      rowsEl.appendChild(el);
      rowEls.push(el);
    }

    // Stream rows in batches, like the log walk: a first batch, then the rest.
    const t0 = performance.now();
    const first = 14;
    for (let i = 0; i < first; i++) addRow(i);
    const ms = Math.max(1, Math.round(performance.now() - t0));
    $('#stFirst').textContent = `first rows ${ms} ms`;
    $('#stCount').textContent = `${first} commits`;
    select(0);
    let n = first;
    const more = () => {
      const end = Math.min(data.length, n + 12);
      for (; n < end; n++) addRow(n);
      $('#stCount').textContent = `${n} commits`;
      if (n < data.length) setTimeout(more, 90);
    };
    setTimeout(more, 140);

    rowsEl.tabIndex = 0;
    rowsEl.addEventListener('keydown', (e) => {
      if (e.key === 'ArrowDown') { select(selected + 1, true); e.preventDefault(); }
      if (e.key === 'ArrowUp') { select(selected - 1, true); e.preventDefault(); }
    });

    // Sidebar.
    $('#sideTree').innerHTML = [
      ['h', '▾', 'Branches', '2'], ['cur', '', 'main', '', ''], ['', '', 'claude', '', 'fold'],
      ['sub', '', 'implement-plan-3vqnl6', '', '↑2'],
      ['h', '▾', 'Remotes', '2'], ['', '', 'origin', '', 'fold'], ['sub', '', 'main'], ['sub', '', 'claude', '', 'fold'], ['sub2', '', 'implement-plan-3vqnl6'],
      ['h', '▸', 'Tags', '0'], ['h', '▸', 'Stashes', '1'], ['h', '▾', 'Submodules', '0'], ['h', '▾', 'Worktrees', '1'], ['', '', 'switchhead', '', 'main'],
    ].map(([k, car, name, n, extra]) => {
      const pad = k === 'sub2' ? 62 : k === 'sub' ? 46 : k === 'h' ? 6 : 26;
      const cls = k === 'h' ? 'r h' : k === 'cur' ? 'r cur' : 'r';
      return `<div class="${cls}" style="padding-left:${pad}px"><span class="car">${car}</span>${esc(name)}${n ? `<span class="n">${n}</span>` : ''}${extra && extra !== 'fold' ? `<span class="ab">${extra}</span>` : ''}</div>`;
    }).join('');
  }

  // ---------------------------------------------------------------- tour
  const SHOTS = [
    ['commit-graph-dark', 'Commit graph', 'Lanes painted per row, ref chips, All / Current, and how long the first rows took.'],
    ['commit-details-dark', 'Commit details', 'Header, parents to jump to, files as a list or tree, highlighted diffs.'],
    ['commit-details-light-side-by-side', 'Side-by-side diffs', 'Inline or side by side, context lines and whitespace, word-level highlights.'],
    ['local-changes', 'Local changes', 'Unstaged and staged lists with mouse and keyboard selection.'],
    ['line-staging', 'Line staging', 'Stage, unstage or discard single hunks or individual lines.'],
    ['commit-box', 'Commit box', 'Subject counter, Amend, Sign-off, Skip hooks, template and recent messages.'],
    ['merge-helper', 'Merge conflicts', 'Ours, base and theirs per conflict; take a side, both, or edit.'],
    ['rebase-editor', 'Interactive rebase', 'Drag to reorder; pick, reword, edit, squash, fixup or drop.'],
    ['image-diff-side-by-side', 'Image diffs', 'Side by side, swipe or onion skin, with pixel and file sizes.'],
    ['sidebar', 'Sidebar', 'Branch folders, remotes, tags, stashes, submodules and worktrees.'],
    ['branch-menu', 'Branch actions', 'Merge (ff, no-ff, squash), rebase, rename, push, upstream, compare.'],
    ['commit-menu', 'Commit actions', 'Cherry-pick, revert, reset, branch or tag here, copy, save as patch.'],
    ['search', 'Search', 'Commits by message, author, id or path, and content search.'],
    ['blame', 'Blame', 'Who changed each line, blame before a commit, jump to it.'],
    ['file-history', 'File history', 'Every commit that touched a file, following renames.'],
    ['reflog', 'Reflog', 'HEAD and branch reflogs; restore, branch or check out an entry.'],
    ['stash-menu', 'Stashes', 'Apply, pop, drop, branch from a stash, with its details.'],
    ['activity-panel', 'Activity panel', "Fetch, pull and push with git's full output and errors."],
    ['quick-launch', 'Quick Launch', 'Ctrl/⌘+P: every command, recent repositories, branches and tags.'],
    ['repository-manager-light', 'Repository manager', 'Open, clone or create; scan a folder for every repository.'],
  ];
  let tourIdx = 0;
  function buildTour() {
    const list = $('#tourList');
    SHOTS.forEach(([file, title, desc], i) => {
      const li = h('li');
      const b = h('button', '', `<span class="num">${String(i + 1).padStart(2, '0')}</span><span class="tt">${esc(title)}</span><span class="td">${esc(desc)}</span>`);
      b.type = 'button';
      b.addEventListener('click', () => showTour(i));
      li.appendChild(b);
      list.appendChild(li);
    });
    $('#tourImgBtn').addEventListener('click', () => openLightbox(tourIdx, true));
    showTour(0);
    // Warm the cache for the rest once the page is idle.
    const warm = () => SHOTS.forEach(([f]) => { const im = new Image(); im.src = `screenshots/${f}.png`; });
    'requestIdleCallback' in window ? requestIdleCallback(warm) : setTimeout(warm, 2000);
  }
  function showTour(i) {
    tourIdx = i;
    const [file, title, desc] = SHOTS[i];
    const img = $('#tourImg');
    img.style.opacity = '0.25';
    const next = new Image();
    next.onload = () => { img.src = next.src; img.alt = title; img.style.opacity = '1'; };
    next.src = `screenshots/${file}.png`;
    $('#tourCap').textContent = `${title}. ${desc}`;
    $$('#tourList button').forEach((b, k) => b.classList.toggle('on', k === i));
  }

  // ---------------------------------------------------------------- lightbox
  const lb = $('#lightbox');
  let lbIdx = 0, lbTour = false;
  function openLightbox(i, tour, src, cap) {
    lbTour = tour;
    lbIdx = i;
    $('#lbPrev').hidden = $('#lbNext').hidden = !tour;
    if (tour) { $('#lbImg').src = `screenshots/${SHOTS[i][0]}.png`; $('#lbCap').textContent = SHOTS[i][1]; }
    else { $('#lbImg').src = src; $('#lbCap').textContent = cap; }
    lb.hidden = false;
  }
  const lbStep = (d) => { if (lbTour) { openLightbox((lbIdx + d + SHOTS.length) % SHOTS.length, true); showTour(lbIdx); } };
  $('#lbPrev').addEventListener('click', () => lbStep(-1));
  $('#lbNext').addEventListener('click', () => lbStep(1));
  lb.addEventListener('click', (e) => { if (e.target === lb || e.target.closest('[data-close]') || e.target.tagName === 'FIGURE') lb.hidden = true; });
  $$('img[data-zoom]').forEach((img) => img.addEventListener('click', () => openLightbox(0, false, img.src, img.alt)));

  // ---------------------------------------------------------------- features
  const FEATURES = [
    ['Commit graph', ['Virtualized list, lanes painted per row', 'All refs or the current branch; Alt-click a ref to show only it', 'Picks up commits and fetches made outside the app']],
    ['Diffs', ['Inline or side by side, syntax highlighted', 'Word-level highlights, context lines, ignore whitespace', 'Images as side by side, swipe or onion skin']],
    ['Staging', ['Files, hunks or single lines', 'Discard, delete, ignore, assume-unchanged, skip-worktree', 'Destructive actions always ask first']],
    ['Commit box', ['Subject counter: amber past 50, red past 72', 'Amend, Sign-off, Skip hooks, commit.template', 'A rejected hook shows its full output']],
    ['Merge & rebase', ['Three-way conflict helper, or your merge.tool', 'Interactive rebase by drag and drop', 'Abort, Skip and Continue banner with conflicts left']],
    ['Branches', ['Folders by /, ahead / behind badges, filter', 'Merge (ff, no-ff, squash), rebase onto, compare', 'Rename, delete local and remote, set upstream']],
    ['Remotes', ['Fetch all (prune), pull (merge or rebase, autostash)', 'Push with force-with-lease, tags, set upstream', "git's progress in the status bar; cancel kills it"]],
    ['History tools', ['Cherry-pick, revert, reset soft / mixed / hard', 'Tags (lightweight or annotated) and stashes', 'Search by message, author, id, path and content']],
    ['Blame & reflog', ['Blame with jump to commit and blame-before', 'File history that follows renames', 'Reflog: restore, branch or check out an entry']],
    ['Workspace folders', ['Open a folder, get every repository under it', 'Parallel scan that skips node_modules and target', 'Submodules and worktrees open as their own tabs']],
    ['Quick Launch', ['Ctrl/⌘+P for every command', 'Recent repositories, branches and tags', 'Keyboard shortcuts modeled on Fork']],
    ['Native & themed', ['One Rust binary on GPUI, GPU-rendered', 'Ten themes plus Follow Omarchy', 'Zoom the whole UI; tabs and state remembered']],
  ];
  function buildFeatures() {
    $('#featGrid').innerHTML = FEATURES.map(([t, items], i) =>
      `<div class="feat"><h3><span class="num">${String(i + 1).padStart(2, '0')}</span>${esc(t)}</h3><ul>${items.map((x) => `<li>${esc(x)}</li>`).join('')}</ul></div>`).join('');
  }

  // ---------------------------------------------------------------- keyboard
  const KEYS = [
    ['Quick Launch', '⌘P', 'Ctrl+P'], ['Open repository', '⌘O', 'Ctrl+O'], ['Clone repository', '⌘N', 'Ctrl+N'],
    ['Repository manager', '⌘⇧M', 'Ctrl+Shift+M'], ['Next / previous tab', '⌃⇥ / ⌃⇧⇥', 'Ctrl+Tab / Ctrl+Shift+Tab'], ['Refresh', '⌘R', 'Ctrl+R'],
    ['Fetch', '⌘⇧F', 'Ctrl+Shift+F'], ['Pull', '⌘⇧L', 'Ctrl+Shift+L'], ['Push', '⌘⇧P', 'Ctrl+Shift+P'],
    ['Stash', '⌘⇧S', 'Ctrl+Shift+S'], ['New branch at commit', '⌘⇧B', 'Ctrl+Shift+B'], ['Jump to HEAD', '⌘⇧H', 'Ctrl+Shift+H'],
    ['Copy commit id', '⌘⇧C', 'Ctrl+Shift+C'], ['Toggle sidebar', '⌘B', 'Ctrl+B'], ['Stage / unstage', 'Space / ↩', 'Space / Enter'],
    ['Commit', '⌘↩', 'Ctrl+Enter'], ['Jump to message', '⌘⇧↩', 'Ctrl+Shift+Enter'], ['Zoom in / out / reset', '⌘+ / ⌘− / ⌘0', 'Ctrl++ / Ctrl+− / Ctrl+0'],
  ];
  function buildKeys(os) {
    $('#keyGrid').innerHTML = KEYS.map(([a, mac, pc]) => `<div class="key"><span>${esc(a)}</span><kbd>${esc(os === 'mac' ? mac : pc)}</kbd></div>`).join('');
    $$('#keyOs button').forEach((b) => b.classList.toggle('on', b.dataset.os === os));
  }
  $$('#keyOs button').forEach((b) => b.addEventListener('click', () => buildKeys(b.dataset.os)));

  // ---------------------------------------------------------------- install
  const REPO = 'https://github.com/jeremielumandong/switchhead';
  const INSTALL = {
    linux: { cmd: `git clone ${REPO}\ncd switchhead && cargo run --release -p switchhead-app`, note: 'Installers arrive with the first release. Until then it builds in one command: Rust 1.97+, git, and the GPUI packages listed under Build.' },
    mac: { cmd: `xcode-select --install   # once\ngit clone ${REPO}\ncd switchhead && cargo run --release -p switchhead-app`, note: 'Installers arrive with the first release. Needs Rust 1.97+ and the Xcode command line tools.' },
    win: { cmd: `git clone ${REPO}\ncd switchhead; cargo run --release -p switchhead-app`, note: 'Installers arrive with the first release. Needs Rust 1.97+ (MSVC) and Git for Windows.' },
  };
  let installOs = /Mac/.test(navigator.platform) ? 'mac' : /Win/.test(navigator.platform) ? 'win' : 'linux';
  function showInstall(os) {
    installOs = os;
    $('#installCmd').innerHTML = INSTALL[os].cmd.split('\n').map((l) => `<span class="p">$ </span>${esc(l)}`).join('\n');
    $('#installNote').textContent = INSTALL[os].note;
    $$('#install .install-tabs button').forEach((b) => b.classList.toggle('on', b.dataset.os === os));
  }
  $$('#install .install-tabs button').forEach((b) => b.addEventListener('click', () => showInstall(b.dataset.os)));
  $('#copyInstall').addEventListener('click', async (e) => {
    const text = INSTALL[installOs].cmd.replace(/\s+#.*$/gm, '');
    try { await navigator.clipboard.writeText(text); e.target.textContent = 'Copied'; } catch (err) { e.target.textContent = 'Select ↑'; }
    setTimeout(() => { e.target.textContent = 'Copy'; }, 1600);
  });

  // ---------------------------------------------------------------- assistant demo
  const AI_SCRIPT = [
    ['tool', 'switchhead.status'], ['tool', 'switchhead.diff --staged'], ['tool', 'switchhead.log -n 20'],
    ['prop', 'Watch the work tree and .git with native file events (notify)\n\nRefresh status and refs from file events instead of polling;\nthe 1 s check stays as a fallback every 10 intervals.'],
  ];
  let aiPlayed = false;
  function playAi() {
    if (aiPlayed) return;
    aiPlayed = true;
    const el = $('#aiTerm');
    el.innerHTML = '<span class="dim">$ claude -p · tools: switchhead (read-only)</span>\n';
    let i = 0;
    const step = () => {
      if (i >= AI_SCRIPT.length) return;
      const [k, v] = AI_SCRIPT[i++];
      if (k === 'tool') { el.insertAdjacentHTML('beforeend', `<span class="tool">● <b>${esc(v)}</b></span>\n`); setTimeout(step, 520); return; }
      const box = h('span', 'prop', '<span class="lbl">Proposed commit message</span><span class="txt"></span><span class="cursor"></span>');
      el.appendChild(box);
      const txt = $('.txt', box);
      let n = 0;
      const type = () => {
        n += 3;
        txt.textContent = v.slice(0, n);
        if (n < v.length) setTimeout(type, 16);
        else { $('.cursor', box).remove(); box.insertAdjacentHTML('beforeend', '<div class="btns"><span class="pri">Use message</span><span>Edit</span><span>Discard</span></div>'); }
      };
      type();
    };
    setTimeout(step, 300);
  }

  // ---------------------------------------------------------------- speed race
  function runRace() {
    const bars = $$('#race .race-bar');
    const max = 2600;
    bars.forEach((b) => { b.style.transition = 'none'; b.style.width = '0'; });
    requestAnimationFrame(() => requestAnimationFrame(() => bars.forEach((b) => {
      const ms = +b.dataset.ms;
      b.style.transition = `width ${ms}ms linear`;
      b.style.width = (ms / max) * 100 + '%';
    })));
  }
  $('#raceBtn').addEventListener('click', runRace);

  // ---------------------------------------------------------------- scroll effects
  const nav = $('#nav');
  addEventListener('scroll', () => nav.classList.toggle('scrolled', scrollY > 10), { passive: true });
  $$('main > section:not(#top):not(#demo)').forEach((s) => s.classList.add('reveal'));
  const io = new IntersectionObserver((entries) => entries.forEach((e) => {
    if (!e.isIntersecting) return;
    e.target.classList.add('in');
    if (e.target.id === 'speed') runRace();
    if (e.target.id === 'assistant') playAi();
    io.unobserve(e.target);
  }), { threshold: 0.18 });
  $$('.reveal').forEach((s) => io.observe(s));

  // ---------------------------------------------------------------- keys
  addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { modalEl.hidden = true; lb.hidden = true; }
    if (!lb.hidden && e.key === 'ArrowLeft') lbStep(-1);
    if (!lb.hidden && e.key === 'ArrowRight') lbStep(1);
    const typing = /INPUT|TEXTAREA/.test(document.activeElement.tagName);
    if (!typing && !e.metaKey && !e.ctrlKey && !e.altKey && (e.key === 't' || e.key === 'T')) { modalEl.hidden = !modalEl.hidden; }
  });

  buildThemes();
  applyTheme(theme.key, false);
  buildSample();
  buildTour();
  buildFeatures();
  buildKeys(/Mac/.test(navigator.platform) ? 'mac' : 'pc');
  showInstall(installOs);
})();
