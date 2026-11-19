// Bardwave game master panel.
//
// Talks to the server over /ws/control (see src/network/control.rs for the
// protocol): the server pushes full snapshots, the panel sends one small JSON
// command per action. Taps are applied optimistically and rolled back to the
// server's snapshot when the command fails or is never confirmed.
//
// Security notes:
//  * the game master token comes from ?g= in the page address, is moved to
//    sessionStorage and removed from the address bar at once; it is only ever
//    sent in the WebSocket address and never written to the page or the log;
//  * nothing from the server is put into the page as HTML: names are set with
//    textContent only.
(function () {
  'use strict';

  // ------------------------------------------------------------ constants ---
  const TOKEN_KEY = 'bardwave.gm.token';
  const BUSES = [
    ['music', 'Music'],
    ['ambience', 'Ambience'],
    ['sfx', 'Effects'],
  ];
  const SEND_EVERY_MS = 80; // slider throttle (the server allows 100 commands/s)
  const OVERRIDE_TTL_MS = 4000; // an optimistic value not confirmed by then is dropped
  const PING_EVERY_MS = 20000;
  const STALE_AFTER_MS = 50000; // nothing heard from the server for this long
  const BACKOFF_MIN_MS = 500;
  const BACKOFF_MAX_MS = 10000;
  const PROBE_AFTER_FAILURES = 3;
  const CLOSE_TOKEN_REVOKED = 4003;

  // ---------------------------------------------------------------- state ---
  let server = null; // last BoardState from the server
  let lastRev = -1;
  let ws = null;
  let reqSeq = 0;
  let failures = 0;
  let reconnectTimer = 0;
  let pingTimer = 0;
  let lastHeard = 0;
  let stopped = false; // no more reconnects (denied / token replaced)
  let renderQueued = false;
  let toastTimer = 0;

  const overrides = new Map(); // key -> { value, req, acked, at }
  const pending = new Map(); // req -> true
  const touching = new Set(); // slider keys under the finger
  const sliderTimers = new Map(); // key -> { timer, last }
  const busLocal = { music: 1, ambience: 1, sfx: 1 }; // fallback when the server reports none

  // ---------------------------------------------------------------- token ---
  function loadToken() {
    let token = null;
    try {
      const url = new URL(location.href);
      token = url.searchParams.get('g');
      if (token !== null) {
        url.searchParams.delete('g');
        const clean = url.pathname + (url.search || '') + url.hash;
        try {
          history.replaceState(null, '', clean);
        } catch (e) {
          /* the address stays as it is */
        }
        if (token) {
          try {
            sessionStorage.setItem(TOKEN_KEY, token);
          } catch (e) {
            /* storage may be blocked: the token then lives in memory only */
          }
        }
      }
    } catch (e) {
      /* no URL API: carry on without a token (local use) */
    }
    if (!token) {
      try {
        token = sessionStorage.getItem(TOKEN_KEY);
      } catch (e) {
        token = null;
      }
    }
    return token || '';
  }
  const token = loadToken();

  // ------------------------------------------------------------------ dom ---
  const $ = (id) => document.getElementById(id);

  function el(tag, props, children) {
    const node = document.createElement(tag);
    if (props) {
      for (const key of Object.keys(props)) {
        const value = props[key];
        if (key === 'text') node.textContent = value;
        else if (key === 'class') node.className = value;
        else node.setAttribute(key, value);
      }
    }
    if (children) for (const child of children) node.appendChild(child);
    return node;
  }

  const clamp01 = (n) => (Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : 0);
  const pctOf = (n) => Math.round(clamp01(n) * 100);

  // Optimistic value of `key`, or the server's.
  function eff(key, serverValue) {
    const o = overrides.get(key);
    return o ? o.value : serverValue;
  }

  function toast(message) {
    const node = $('toast');
    node.textContent = String(message).slice(0, 140);
    node.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => node.classList.remove('show'), 3500);
  }

  function buzz() {
    try {
      if (navigator.vibrate) navigator.vibrate(8);
    } catch (e) {
      /* not supported */
    }
  }

  // ----------------------------------------------------------- connection ---
  function setConn(state, text) {
    document.body.dataset.conn = state;
    $('connText').textContent = text;
  }

  function wsUrl() {
    const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const query = token ? '?g=' + encodeURIComponent(token) : '';
    return scheme + '//' + location.host + '/ws/control' + query;
  }

  function isOpen() {
    return ws !== null && ws.readyState === WebSocket.OPEN;
  }

  function connect() {
    if (stopped) return;
    clearTimeout(reconnectTimer);
    reconnectTimer = 0;
    if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
    setConn('connecting', failures ? 'Reconnecting…' : 'Connecting…');
    let socket;
    try {
      socket = new WebSocket(wsUrl());
    } catch (e) {
      scheduleReconnect();
      return;
    }
    ws = socket;
    socket.onopen = () => {
      if (ws !== socket) return;
      failures = 0;
      lastRev = -1; // a restarted server counts revisions from the start again
      lastHeard = Date.now();
      setConn('online', 'Connected');
      startPing();
    };
    socket.onmessage = (event) => {
      if (ws !== socket) return;
      lastHeard = Date.now();
      onMessage(event.data);
    };
    socket.onclose = (event) => {
      if (ws !== socket) return;
      ws = null;
      stopPing();
      dropPending(true);
      if (event.code === CLOSE_TOKEN_REVOKED) {
        deny('The game master link was replaced. Open the new link from the server panel.');
        return;
      }
      failures += 1;
      scheduleReconnect();
    };
    socket.onerror = () => {
      /* onclose follows; nothing useful (and nothing secret) to report */
    };
  }

  function deny(message) {
    stopped = true;
    clearTimeout(reconnectTimer);
    setConn('denied', message);
    render();
  }

  function scheduleReconnect() {
    if (stopped) return;
    if (failures === PROBE_AFTER_FAILURES) {
      probeAccess();
    }
    const base = Math.min(BACKOFF_MAX_MS, BACKOFF_MIN_MS * Math.pow(2, Math.max(0, failures - 1)));
    const delay = base / 2 + Math.random() * (base / 2);
    setConn('offline', 'Offline, retrying…');
    clearTimeout(reconnectTimer);
    reconnectTimer = setTimeout(connect, delay);
  }

  // A refused WebSocket handshake gives the browser no status code: ask once
  // over plain HTTP whether the problem is the link (401) or the network.
  async function probeAccess() {
    try {
      const headers = { 'X-Bardwave-Control': '1' };
      if (token) headers.Authorization = 'Bearer ' + token;
      const res = await fetch('/api/control/state', { headers, cache: 'no-store' });
      if (res.status === 401) {
        deny('Access denied. Open the game master link from the server panel again.');
      } else if (res.status === 429) {
        const wait = parseInt(res.headers.get('Retry-After') || '30', 10);
        toast('Too many attempts. Wait ' + (Number.isFinite(wait) ? wait : 30) + ' s.');
        failures = Math.max(failures, 6); // back off to the slowest pace
      }
    } catch (e) {
      /* offline: keep retrying */
    }
  }

  function startPing() {
    stopPing();
    pingTimer = setInterval(() => {
      if (!isOpen()) return;
      if (Date.now() - lastHeard > STALE_AFTER_MS) {
        try {
          ws.close();
        } catch (e) {
          /* already closed */
        }
        return;
      }
      try {
        ws.send(JSON.stringify({ cmd: 'ping', req: 'p' }));
      } catch (e) {
        /* the close handler takes over */
      }
    }, PING_EVERY_MS);
  }

  function stopPing() {
    clearInterval(pingTimer);
    pingTimer = 0;
  }

  // The tab came back or the network returned: do not wait for the backoff.
  function reviveNow() {
    if (stopped || isOpen()) return;
    failures = Math.min(failures, 1);
    connect();
  }
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') reviveNow();
  });
  window.addEventListener('online', reviveNow);

  // -------------------------------------------------------------- messages ---
  function onMessage(data) {
    let msg;
    try {
      msg = JSON.parse(data);
    } catch (e) {
      return;
    }
    if (!msg || typeof msg !== 'object') return;
    switch (msg.type) {
      case 'snapshot':
        onSnapshot(msg);
        break;
      case 'ack':
        onAck(msg.req);
        break;
      case 'error':
        onError(msg.req, msg.message);
        break;
      default:
        break; // pong and anything newer
    }
  }

  function onSnapshot(msg) {
    if (!msg.state || typeof msg.state !== 'object') return;
    if (typeof msg.rev === 'number') {
      if (msg.rev < lastRev) return; // older than what is shown
      lastRev = msg.rev;
    }
    server = msg.state;
    // The server has spoken: everything it confirmed is now true, and so is
    // everything it did not (rolled back). Unconfirmed taps stay for a moment.
    for (const [key, o] of overrides) {
      if (o.acked && !touching.has(key)) overrides.delete(key);
    }
    for (const [name] of BUSES) {
      const v = server.buses && server.buses[name];
      if (typeof v === 'number') busLocal[name] = clamp01(v);
    }
    queueRender();
  }

  function onAck(req) {
    if (req === undefined || req === 'p') return;
    pending.delete(req);
    for (const o of overrides.values()) {
      if (o.req === req) o.acked = true;
    }
    // The snapshot that follows an ack clears the override; if the server
    // changed nothing (so there is no snapshot) the TTL sweep does.
  }

  function onError(req, message) {
    if (req !== undefined) {
      pending.delete(req);
      for (const [key, o] of overrides) {
        if (o.req === req) overrides.delete(key); // roll back to the snapshot
      }
    }
    toast(typeof message === 'string' && message ? message : 'Command failed');
    queueRender();
  }

  function dropPending(all) {
    if (all) {
      pending.clear();
      overrides.clear();
      touching.clear();
    }
    queueRender();
  }

  setInterval(() => {
    const now = Date.now();
    let dropped = false;
    for (const [key, o] of overrides) {
      if (now - o.at > OVERRIDE_TTL_MS && !touching.has(key)) {
        overrides.delete(key);
        dropped = true;
      }
    }
    if (dropped) queueRender();
  }, 1000);

  // -------------------------------------------------------------- commands ---
  // Send `cmd`; `optimistic` maps override keys to the values shown at once.
  function send(cmd, fields, optimistic) {
    if (!isOpen()) {
      toast('Not connected');
      return false;
    }
    const req = ++reqSeq;
    const message = Object.assign({ cmd: cmd, req: req }, fields || {});
    try {
      ws.send(JSON.stringify(message));
    } catch (e) {
      toast('Not connected');
      return false;
    }
    pending.set(req, true);
    if (optimistic) {
      const at = Date.now();
      for (const key of Object.keys(optimistic)) {
        overrides.set(key, { value: optimistic[key], req: req, acked: false, at: at });
      }
      queueRender();
    }
    return true;
  }

  // Slider commands go out at most every SEND_EVERY_MS while dragging, and the
  // final value always goes out on release.
  function throttledSend(key, fire, final) {
    let slot = sliderTimers.get(key);
    if (!slot) {
      slot = { timer: 0, last: 0 };
      sliderTimers.set(key, slot);
    }
    clearTimeout(slot.timer);
    slot.timer = 0;
    const wait = final ? 0 : slot.last + SEND_EVERY_MS - Date.now();
    if (wait <= 0) {
      slot.last = Date.now();
      fire();
    } else {
      slot.timer = setTimeout(() => {
        slot.timer = 0;
        slot.last = Date.now();
        fire();
      }, wait);
    }
  }

  function bindSlider(input, label, key, onValue) {
    const apply = (final) => {
      const value = clamp01(Number(input.value) / 100);
      label.textContent = pctOf(value) + '%';
      input.setAttribute('aria-valuetext', pctOf(value) + ' percent');
      throttledSend(key, () => onValue(value), final);
    };
    input.addEventListener('pointerdown', () => touching.add(key));
    input.addEventListener('input', () => {
      touching.add(key);
      apply(false);
    });
    input.addEventListener('change', () => {
      apply(true);
      touching.delete(key);
      queueRender();
    });
    input.addEventListener('blur', () => touching.delete(key));
    input.addEventListener('pointerup', () => setTimeout(() => touching.delete(key), 0));
    input.addEventListener('pointercancel', () => touching.delete(key));
  }

  function setSlider(input, label, key, value) {
    const pct = pctOf(value);
    label.textContent = pct + '%';
    input.setAttribute('aria-valuetext', pct + ' percent');
    // Never fight the finger: a slider being dragged keeps its own value.
    if (!touching.has(key) && String(pct) !== input.value) input.value = String(pct);
  }

  // ---------------------------------------------------------------- render ---
  const ui = {
    sceneSig: '',
    scenes: new Map(),
    stingerSig: '',
    stingers: new Map(),
    soundSig: '',
    sounds: new Map(),
    plSig: '',
    playlists: new Map(),
    buses: new Map(),
  };

  function queueRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      render();
    });
  }

  function setPressed(node, on) {
    node.setAttribute('aria-pressed', on ? 'true' : 'false');
  }

  function render() {
    renderMaster();
    renderDuck();
    renderBuses();
    if (!server) return;
    renderScenes();
    renderStingers();
    renderPlaylist();
    renderSounds();
  }

  function soundList() {
    return server && Array.isArray(server.sounds) ? server.sounds : [];
  }

  function renderMaster() {
    const value = server ? eff('master', clamp01(server.master_volume)) : 0;
    setSlider($('master'), $('masterPct'), 'master', value);
  }

  function renderDuck() {
    const on = server ? Boolean(eff('duck', Boolean(server.ducked))) : false;
    setPressed($('duck'), on);
    $('duck').textContent = on ? 'Ducked' : 'Duck';
  }

  function renderBuses() {
    if (ui.buses.size === 0) {
      const box = $('buses');
      for (const [name, title] of BUSES) {
        const id = 'bus-' + name;
        const input = el('input', {
          type: 'range',
          min: '0',
          max: '100',
          step: '1',
          value: '100',
          id: id,
        });
        const pct = el('output', { class: 'pct', for: id, text: '100%' });
        const label = el('label', { for: id, text: title });
        box.appendChild(el('div', { class: 'bus' }, [label, input, pct]));
        const key = 'bus:' + name;
        bindSlider(input, pct, key, (value) => {
          busLocal[name] = value;
          send('bus', { bus: name, value: value }, { [key]: value });
        });
        ui.buses.set(name, { input, pct, key });
      }
    }
    for (const [name] of BUSES) {
      const slot = ui.buses.get(name);
      setSlider(slot.input, slot.pct, slot.key, eff(slot.key, busLocal[name]));
    }
  }

  function renderScenes() {
    const scenes = Array.isArray(server.scenes) ? server.scenes : [];
    $('secScenes').hidden = scenes.length === 0;
    const sig = JSON.stringify(scenes.map((s) => [s.id, s.name, s.color, s.sounds]));
    if (sig !== ui.sceneSig) {
      ui.sceneSig = sig;
      ui.scenes.clear();
      const box = $('scenes');
      box.replaceChildren();
      for (const scene of scenes) {
        const button = el('button', { type: 'button', class: 'btn scene-btn', 'aria-pressed': 'false' }, [
          el('span', { text: String(scene.name) }),
          el('small', { text: scene.sounds + (scene.sounds === 1 ? ' sound' : ' sounds') }),
        ]);
        if (typeof scene.color === 'string' && /^#[0-9a-fA-F]{6}$/.test(scene.color)) {
          button.style.setProperty('--scene', scene.color);
        }
        button.addEventListener('click', () => {
          buzz();
          send('scene', { id: scene.id }, { scene: scene.id });
        });
        box.appendChild(button);
        ui.scenes.set(scene.id, button);
      }
    }
    const active = eff('scene', server.active_scene);
    for (const [id, button] of ui.scenes) setPressed(button, id === active);
  }

  function renderStingers() {
    const stingers = soundList().filter((s) => s.stinger);
    $('secStingers').hidden = stingers.length === 0;
    const sig = JSON.stringify(stingers.map((s) => [s.id, s.name]));
    if (sig !== ui.stingerSig) {
      ui.stingerSig = sig;
      ui.stingers.clear();
      const box = $('stingers');
      box.replaceChildren();
      for (const sound of stingers) {
        const button = el('button', { type: 'button', class: 'btn pad', text: String(sound.name) });
        button.addEventListener('click', () => {
          buzz();
          button.classList.remove('flash');
          void button.offsetWidth; // restart the flash
          button.classList.add('flash');
          send('stinger', { id: sound.id });
        });
        box.appendChild(button);
        ui.stingers.set(sound.id, button);
      }
    }
    for (const sound of stingers) {
      ui.stingers.get(sound.id).dataset.playing = sound.playing ? 'true' : 'false';
    }
  }

  function trackLabel(pl) {
    const title = typeof pl.title === 'string' && pl.title ? pl.title : '';
    const artist = typeof pl.artist === 'string' && pl.artist ? pl.artist : '';
    if (title && artist) return artist + ' - ' + title;
    return title || (typeof pl.track === 'string' ? pl.track : '') || 'Nothing playing';
  }

  function renderPlaylist() {
    const lists = Array.isArray(server.playlists) ? server.playlists : [];
    const pl = server.playlist && typeof server.playlist === 'object' ? server.playlist : null;
    $('secPlaylist').hidden = lists.length === 0 && !pl;
    const supported = pl !== null;
    $('plNote').hidden = supported;

    const sig = JSON.stringify(lists.map((p) => [p.id, p.name, p.tracks]));
    if (sig !== ui.plSig) {
      ui.plSig = sig;
      ui.playlists.clear();
      const box = $('playlists');
      box.replaceChildren();
      for (const list of lists) {
        const button = el('button', { type: 'button', class: 'btn', 'aria-pressed': 'false' }, [
          el('span', { text: String(list.name) }),
        ]);
        button.addEventListener('click', () => {
          buzz();
          send('playlist_start', { id: list.id });
        });
        box.appendChild(button);
        ui.playlists.set(list.id, button);
      }
    }

    const status = pl && typeof pl.status === 'string' ? pl.status : 'stopped';
    const running = status === 'playing' || status === 'paused';
    for (const [id, button] of ui.playlists) {
      setPressed(button, running && pl.playlist === id);
      button.disabled = !supported;
    }
    $('nowTitle').textContent = running && pl ? trackLabel(pl) : 'Nothing playing';
    let sub = '';
    if (pl) {
      if (running) {
        const name = typeof pl.name === 'string' ? pl.name : '';
        const index = Number.isInteger(pl.track_index) ? pl.track_index + 1 : null;
        sub = [name, index && pl.track_count ? index + ' of ' + pl.track_count : '', status === 'paused' ? 'paused' : '']
          .filter(Boolean)
          .join(' · ');
      } else if (typeof pl.error === 'string' && pl.error) {
        sub = pl.error;
      }
    }
    $('nowSub').textContent = sub;

    const toggle = $('plToggle');
    toggle.textContent = status === 'playing' ? 'Pause' : 'Play';
    toggle.disabled = !supported || (!running && !pl.playlist);
    $('plPrev').disabled = !supported || !running;
    $('plNext').disabled = !supported || !running;
    $('plStop').disabled = !supported || !running;
  }

  function renderSounds() {
    const sounds = soundList().filter((s) => !s.stinger);
    $('soundsEmpty').hidden = soundList().length !== 0;
    const sig = JSON.stringify(sounds.map((s) => [s.id, s.name, s.group]));
    const list = $('sounds');
    if (sig !== ui.soundSig) {
      ui.soundSig = sig;
      ui.sounds.clear();
      list.replaceChildren();
      for (const sound of sounds) {
        const id = sound.id;
        const volumeId = 'vol-' + id;
        const name = el('div', { class: 'name' }, [el('b', { text: String(sound.name) })]);
        if (sound.group) name.appendChild(el('small', { text: String(sound.group) }));
        const play = el('button', { type: 'button', class: 'btn play', 'aria-pressed': 'false', text: 'Play' });
        const loop = el('button', { type: 'button', class: 'btn loop', 'aria-pressed': 'false', text: 'Loop' });
        const input = el('input', { type: 'range', min: '0', max: '100', step: '1', value: '0', id: volumeId });
        input.setAttribute('aria-label', String(sound.name) + ' volume');
        const pct = el('output', { class: 'pct', for: volumeId, text: '0%' });
        const volumeKey = 'vol:' + id;
        const playKey = 'play:' + id;
        const loopKey = 'loop:' + id;
        play.setAttribute('aria-label', 'Play ' + String(sound.name));
        loop.setAttribute('aria-label', 'Loop ' + String(sound.name));
        play.addEventListener('click', () => {
          const row = ui.sounds.get(id);
          const next = !row.playing;
          buzz();
          send(next ? 'play' : 'stop', { id: id }, { [playKey]: next });
        });
        loop.addEventListener('click', () => {
          const row = ui.sounds.get(id);
          const next = !row.looped;
          send('loop', { id: id, on: next }, { [loopKey]: next });
        });
        bindSlider(input, pct, volumeKey, (value) => send('volume', { id: id, value: value }, { [volumeKey]: value }));
        const row = el('li', { class: 'sound', 'data-playing': 'false' }, [
          name,
          play,
          loop,
          el('div', { class: 'slider-row' }, [input, pct]),
        ]);
        list.appendChild(row);
        ui.sounds.set(id, { row, play, loop, input, pct, playKey, loopKey, volumeKey, playing: false, looped: false });
      }
    }
    for (const sound of sounds) {
      const slot = ui.sounds.get(sound.id);
      const playing = Boolean(eff(slot.playKey, Boolean(sound.playing)));
      const looped = Boolean(eff(slot.loopKey, Boolean(sound.looped)));
      slot.playing = playing;
      slot.looped = looped;
      slot.row.dataset.playing = playing ? 'true' : 'false';
      setPressed(slot.play, playing);
      slot.play.textContent = playing ? 'Stop' : 'Play';
      setPressed(slot.loop, looped);
      setSlider(slot.input, slot.pct, slot.volumeKey, eff(slot.volumeKey, clamp01(sound.volume)));
    }
  }

  // ---------------------------------------------------------------- wiring ---
  $('duck').addEventListener('click', () => {
    const now = server ? Boolean(eff('duck', Boolean(server.ducked))) : false;
    buzz();
    send('duck', { on: !now }, { duck: !now });
  });

  $('stopAll').addEventListener('click', () => {
    buzz();
    const optimistic = {};
    for (const s of soundList()) optimistic['play:' + s.id] = false;
    send('stop_all', {}, optimistic);
  });

  $('stopScene').addEventListener('click', () => {
    send('stop_scene', {}, { scene: null });
  });

  bindSlider($('master'), $('masterPct'), 'master', (value) =>
    send('master', { value: value }, { master: value })
  );

  $('plToggle').addEventListener('click', () => {
    const pl = server && server.playlist;
    if (!pl) return;
    if (pl.status === 'playing') send('playlist_pause');
    else if (pl.status === 'paused') send('playlist_resume');
    else if (pl.playlist) send('playlist_start', { id: pl.playlist });
  });
  $('plPrev').addEventListener('click', () => send('playlist_prev'));
  $('plNext').addEventListener('click', () => send('playlist_next'));
  $('plStop').addEventListener('click', () => send('playlist_stop'));

  render();
  connect();
})();
