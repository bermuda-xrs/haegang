'use strict';
// All dynamic text goes through textContent; innerHTML is only used for the static SVG icons.

const $ = (s, r = document) => r.querySelector(s);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};
const svg = (d) => `<svg viewBox="0 0 24 24" aria-hidden="true">${d}</svg>`;
const ICON = {
  messages: svg('<path d="M21 3 3 10.5l7 3 3 7z"/><path d="m10 13.5 11-10.5"/>'),
  compose: svg('<path d="M12 4H5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h13a2 2 0 0 0 2-2v-7"/><path d="M17.5 3.5a2.1 2.1 0 0 1 3 3L11 16l-4 1 1-4z"/>'),
  info: svg('<circle cx="12" cy="12" r="9.5"/><path d="M12 11v6"/><circle cx="12" cy="7.5" r=".6" fill="currentColor"/>'),
  settings: svg('<path d="M4 7h10M18 7h2M4 17h2M10 17h10"/><circle cx="16" cy="7" r="2"/><circle cx="8" cy="17" r="2"/>'),
  heart: svg('<path d="M12 20.5s-8.5-5-8.5-11A4.7 4.7 0 0 1 12 6.8a4.7 4.7 0 0 1 8.5 2.7c0 6-8.5 11-8.5 11z"/>'),
  back: svg('<path d="m15 5-7 7 7 7"/>'),
  smile: svg('<circle cx="12" cy="12" r="9.5"/><path d="M8 14.2a4.5 4.5 0 0 0 8 0"/><circle cx="9" cy="9.7" r=".7" fill="currentColor"/><circle cx="15" cy="9.7" r=".7" fill="currentColor"/>'),
};

// space-separated so multi-codepoint emoji (e.g. ❤️) stay intact
const EMOJI = '😀 😂 🥹 😍 😘 😎 🤔 😅 😭 😡 🥺 😴 🙄 😏 🤗 😬 👍 👎 👏 🙏 💪 🔥 ✨ 💀 ❤️ 🖤 🤍 💔 🎉 👀 🙌 🤝 ☕ 🍕 🌙 ⭐ 🥀 🦇 🕯️ 🗡️'.split(' ');

const S = {
  personas: [], cur: null, chat: [], models: null, live: {}, drafts: {}, filter: '', me: { avatar: null },
  infoOpen: false, infoFor: null, infoParts: [], pollTimer: null, importToken: null, importAbort: null,
};

async function api(method, path, body) {
  const r = await fetch(path, {
    method,
    headers: body !== undefined ? { 'Content-Type': 'application/json' } : {},
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}
const pp = (n) => '/api/personas/' + encodeURIComponent(n);

/** Feed each NDJSON line of a streaming response to `onLine`. */
async function readLines(res, onLine) {
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let buf = '';
  for (;;) {
    const { value, done } = await reader.read();
    if (value) buf += dec.decode(value, { stream: true });
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (line) onLine(JSON.parse(line));
    }
    if (done) break;
  }
  if (buf.trim()) onLine(JSON.parse(buf));
}

function ago(ts) {
  const s = (Date.now() - ts) / 1000;
  if (s < 60) return 'now';
  if (s < 3600) return Math.floor(s / 60) + 'm';
  if (s < 86400) return Math.floor(s / 3600) + 'h';
  if (s < 604800) return Math.floor(s / 86400) + 'd';
  return Math.floor(s / 604800) + 'w';
}
function stamp(ts) {
  const d = new Date(ts);
  const t = d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  return d.toDateString() === new Date().toDateString()
    ? t
    : d.toLocaleDateString([], { month: 'short', day: 'numeric' }) + ', ' + t;
}
const initial = (n) => (Array.from(n.trim())[0] || '?').toUpperCase();
const isOnline = () => S.models?.online === true;

// Profile picture if one was uploaded, else the initial. `noImg` for names that are not (yet) personas.
function avatar(name, cls = '', noImg = false) {
  const d = el('div', 'avatar ' + cls);
  const p = noImg ? null : personaOf(name);
  if (p?.avatar) {
    const i = el('img');
    i.alt = '';
    i.src = `${pp(name)}/avatar?v=${p.avatar}`;
    d.append(i);
  } else d.textContent = initial(name);
  return d;
}
function withDot(av) {
  if (isOnline()) av.append(el('span', 'dot'));
  return av;
}
function meAvatar(cls = '') {
  const d = el('div', 'avatar ' + cls);
  if (S.me.avatar) {
    const i = el('img');
    i.alt = '';
    i.src = `/api/me/avatar?v=${S.me.avatar}`;
    d.append(i);
  } else d.textContent = 'Y';
  return d;
}

/** Resolves with the chosen image file, or null if the picker was cancelled. */
function pickImage() {
  return new Promise((resolve) => {
    const inp = el('input');
    inp.type = 'file';
    inp.accept = 'image/*';
    inp.onchange = () => resolve(inp.files[0] || null);
    inp.oncancel = () => resolve(null);
    inp.click();
  });
}
// Center-crop to a square and re-encode, so uploads are small and always a supported type.
async function squareJpeg(file, size = 320) {
  const bmp = await createImageBitmap(file);
  const s = Math.min(bmp.width, bmp.height);
  const c = document.createElement('canvas');
  c.width = c.height = size;
  const g = c.getContext('2d');
  g.fillStyle = '#000';
  g.fillRect(0, 0, size, size);
  g.drawImage(bmp, (bmp.width - s) / 2, (bmp.height - s) / 2, s, s, 0, 0, size, size);
  return new Promise((r) => c.toBlob(r, 'image/jpeg', 0.9));
}
/** Returns true once the picture is stored. */
async function uploadAvatar(path, file) {
  if (!file) return false;
  let blob;
  try { blob = await squareJpeg(file); } catch { alert('That file could not be read as an image.'); return false; }
  const r = await fetch(path, { method: 'POST', body: blob });
  if (!r.ok) { alert((await r.json().catch(() => ({}))).error || r.statusText); return false; }
  return true;
}
async function changePersonaPhoto(name) {
  if (await uploadAvatar(pp(name) + '/avatar', await pickImage())) {
    await refreshList();
    if (S.cur === name) buildThread();
  }
}
async function changeMyPhoto() {
  if (!(await uploadAvatar('/api/me/avatar', await pickImage()))) return false;
  await loadMe();
  paintRail();
  return true;
}
async function loadMe() {
  try { S.me = await api('GET', '/api/me'); } catch { S.me = { avatar: null }; }
}
function paintRail() {
  $('#btn-me').replaceChildren(meAvatar('sm'));
}

async function refreshList() {
  try { S.personas = await api('GET', '/api/personas'); } catch { /* keep what is shown */ }
  renderList();
  if (S.cur && S.infoOpen) refreshInfo();
  const building = S.personas.some((p) => p.embeddings === 'building');
  if (building && !S.pollTimer) S.pollTimer = setInterval(refreshList, 3000);
  if (!building && S.pollTimer) { clearInterval(S.pollTimer); S.pollTimer = null; }
}

function renderList() {
  const box = $('#list');
  box.replaceChildren();
  const q = S.filter.toLowerCase();
  const ps = S.personas
    .filter((p) => p.name.toLowerCase().includes(q))
    .sort((a, b) => (b.last?.ts || 0) - (a.last?.ts || 0) || a.name.localeCompare(b.name));

  // row of avatars at the top, like the notes/stories strip
  const strip = $('#strip');
  strip.replaceChildren();
  strip.hidden = !S.personas.length || !!q;
  for (const p of S.personas) {
    const s = el('button', 'story');
    s.append(withDot(avatar(p.name, 'md2')), el('span', null, p.name));
    s.onclick = () => openThread(p.name);
    strip.append(s);
  }

  if (!ps.length) {
    box.append(el('div', 'list-empty', S.personas.length ? 'No matches.' : 'No personas yet. Import a chat to begin.'));
    return;
  }
  for (const p of ps) {
    const b = el('button', 'row' + (p.name === S.cur ? ' active' : ''));
    b.append(withDot(avatar(p.name)));
    const m = el('div', 'meta');
    m.append(el('div', 'name', p.name));
    const prev = p.last
      ? `${p.last.role === 'user' ? 'You: ' : ''}${p.last.text.replace(/\s+/g, ' ')} · ${ago(p.last.ts)}`
      : 'Start the conversation';
    m.append(el('div', 'preview', prev));
    b.append(m);
    b.onclick = () => openThread(p.name);
    box.append(b);
  }
}

function renderEmpty() {
  const t = $('#thread');
  t.replaceChildren();
  const e = el('div', 'empty');
  const ring = el('div', 'ring');
  ring.innerHTML = ICON.messages;
  e.append(ring, el('h2', null, 'Your messages'), el('div', 'orn', '— ✠ —'),
    el('p', null, 'Import a book, a document or a chat to raise personas from it.'));
  const b = el('button', 'btn', 'Import');
  b.onclick = openImport;
  e.append(b);
  t.append(e);
}

async function openThread(name) {
  S.cur = name;
  document.body.classList.add('in-thread');
  const chat = await api('GET', pp(name) + '/chat').catch(() => []);
  if (S.cur !== name) return; // another thread was opened meanwhile
  S.chat = chat;
  buildThread();
  renderList();
  if (S.infoOpen) renderInfo();
}

function closeThread() {
  S.cur = null;
  toggleInfo(false);
  document.body.classList.remove('in-thread');
  renderList();
  renderEmpty();
}

function personaOf(name) { return S.personas.find((p) => p.name === name); }

function buildThread() {
  const t = $('#thread');
  t.replaceChildren();
  const head = el('div', 't-head');
  const back = el('button', 'icon-btn back');
  back.innerHTML = ICON.back;
  back.onclick = closeThread;
  const who = el('div', 'who');
  const status = el('div', 's', isOnline() ? 'Active now' : 'Offline');
  ensureModels().then(() => { status.textContent = isOnline() ? 'Active now' : 'Offline'; });
  who.append(el('div', 'n', S.cur), status);
  const infoBtn = el('button', 'icon-btn');
  infoBtn.title = 'Conversation information';
  infoBtn.innerHTML = ICON.info;
  infoBtn.onclick = () => toggleInfo();
  head.append(back, withDot(avatar(S.cur, 'md')), who, infoBtn);

  const msgs = el('div');
  msgs.id = 'msgs';

  const comp = el('div', 'composer');
  const pill = el('div', 'pill');
  const ta = el('textarea');
  ta.rows = 1;
  ta.placeholder = 'Message...';
  const name = S.cur;
  ta.value = S.drafts[name] || '';
  const send = el('button', 'send', 'Send');
  send.hidden = true;
  const heart = el('button', 'heart');
  heart.innerHTML = ICON.heart;
  heart.title = 'Send a heart';
  const smile = el('button', 'smile');
  smile.innerHTML = ICON.smile;
  smile.title = 'Emoji';
  const pop = el('div', 'emoji-pop');
  pop.hidden = true;
  for (const e of EMOJI) {
    const b = el('button', null, e);
    b.onclick = () => {
      const a = ta.selectionStart, z = ta.selectionEnd;
      ta.setRangeText(e, a, z, 'end');
      ta.focus();
      sync();
    };
    pop.append(b);
  }
  smile.onclick = (ev) => { ev.stopPropagation(); pop.hidden = !pop.hidden; };
  pop.onclick = (ev) => ev.stopPropagation();
  pill.append(smile, ta, send, heart);
  comp.append(pop, pill);

  function sync() {
    ta.style.height = 'auto';
    ta.style.height = Math.min(ta.scrollHeight, 140) + 'px';
    S.drafts[name] = ta.value;
    const has = ta.value.trim().length > 0;
    send.hidden = !has;
    heart.hidden = has;
  }
  const go = (text) => {
    if (name in S.live) return; // one reply at a time; the text stays in the box
    ta.value = '';
    sync();
    sendMessage(text);
  };
  ta.oninput = sync;
  ta.onkeydown = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); if (ta.value.trim()) go(ta.value); }
  };
  send.onclick = () => go(ta.value);
  heart.onclick = () => sendMessage('❤️');

  t.append(head, msgs, comp);
  sync();
  renderMessages();
  ta.focus();
}

// Replies are split on newlines into separate bubbles (DM style); storage keeps them whole.
function expand(chat) {
  const out = [];
  for (const m of chat) {
    const parts = m.role === 'persona' ? m.text.split(/\n+/).filter(Boolean) : [m.text];
    parts.forEach((text) => out.push({ role: m.role, text, ts: m.ts }));
  }
  return out;
}

function renderMessages() {
  const box = $('#msgs');
  if (!box) return;
  box.replaceChildren();
  const p = personaOf(S.cur);
  const intro = el('div', 'intro');
  const view = el('button', 'btn ghost', 'View profile');
  view.onclick = () => toggleInfo(true);
  const learned = p ? `Learned from ${p.persona_messages.toLocaleString()} ${p.general ? 'passages' : 'of their lines'}` : '';
  intro.append(avatar(S.cur, 'lg'), el('div', 'n', S.cur), el('div', 's', learned), view);
  box.append(intro);

  const items = expand(S.chat);
  let lastOut = -1;
  items.forEach((m, i) => { if (m.role === 'user') lastOut = i; });
  items.forEach((m, i) => {
    const prev = items[i - 1], next = items[i + 1];
    if (!prev || m.ts - prev.ts > 3600e3) box.append(el('div', 'sep', stamp(m.ts)));
    const same = (a, b) => a && b && a.role === b.role && Math.abs(b.ts - a.ts) < 120e3;
    box.append(messageRow(m, same(prev, m), same(m, next)));
    // under the newest message you sent: "Seen" once they replied, otherwise "Sent"
    if (i === lastOut) box.append(el('div', 'status', i < items.length - 1 ? 'Seen' : 'Sent'));
  });
  if (S.cur in S.live) showLive();
  box.scrollTop = box.scrollHeight;
}

/** The reply being written: typing dots, then its text so far. */
function showLive() {
  const box = $('#msgs');
  const text = S.live[S.cur];
  const old = $('.live', box);
  if (old && text && !old.classList.contains('typing')) {
    $('.bubble', old).textContent = text;
  } else {
    const row = text ? messageRow({ role: 'persona', text, ts: Date.now() }, false, false) : typingRow();
    row.classList.add('live');
    if (old) old.replaceWith(row); else box.append(row);
  }
  box.scrollTop = box.scrollHeight;
}

function messageRow(m, cont, more) {
  const out = m.role === 'user';
  const row = el('div', `mrow ${out ? 'out' : 'in'}${cont ? ' cont' : ' first'}${more ? ' more' : ''}`);
  if (!out) {
    const slot = el('div', 'slot');
    if (!more) slot.append(avatar(S.cur, 'sm'));
    row.append(slot);
  }
  const b = el('div', 'bubble', m.text);
  b.title = stamp(m.ts);
  row.append(b);
  return row;
}

function typingRow() {
  const row = el('div', 'mrow in first typing');
  const slot = el('div', 'slot');
  slot.append(avatar(S.cur, 'sm'));
  const b = el('div', 'bubble');
  b.append(el('i'), el('i'), el('i'));
  row.append(slot, b);
  return row;
}

async function sendMessage(text) {
  text = text.trim();
  const name = S.cur, chat = S.chat;
  if (!text || !name || name in S.live) return;
  S.live[name] = '';
  chat.push({ role: 'user', text, ts: Date.now() });
  renderMessages();
  let error = null;
  try {
    const res = await fetch(pp(name) + '/chat', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ text }),
    });
    if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error || res.statusText);
    let reply = null, failed = null;
    await readLines(res, (j) => {
      if (j.error) failed = j.error;
      else if (j.done) reply = j;
      else if (j.t) {
        S.live[name] += j.t;
        if (S.cur === name) showLive();
      }
    });
    if (failed || !reply) throw new Error(failed || 'connection closed early');
    if (S.chat === chat) chat.push({ role: 'persona', text: reply.text, ts: reply.ts });
    else if (S.cur === name) { // left and came back, or cleared
      const stored = await api('GET', pp(name) + '/chat');
      if (S.cur === name) S.chat = stored;
    }
  } catch (e) {
    error = e.message;
  }
  delete S.live[name];
  if (S.cur === name && $('#msgs')) {
    renderMessages();
    if (error) {
      const box = $('#msgs');
      box.append(el('div', 'note', 'Could not get a reply: ' + error));
      box.scrollTop = box.scrollHeight;
    }
  }
  refreshList();
}

function toggleInfo(force) {
  S.infoOpen = typeof force === 'boolean' ? force : !S.infoOpen;
  $('#info').hidden = !S.infoOpen;
  if (S.infoOpen) renderInfo(); else S.infoFor = null;
}

async function ensureModels() {
  if (!S.models?.online) { try { S.models = await api('GET', '/api/models'); } catch { S.models = { online: false, models: [], default: '' }; } }
  return S.models;
}

/** Builds the panel when it opens or the persona changes; otherwise an unsaved prompt must survive. */
async function renderInfo() {
  const box = $('#info');
  if (!S.cur) { box.hidden = true; return; }
  if (S.infoFor === S.cur) return refreshInfo();
  const name = S.infoFor = S.cur;
  await ensureModels();
  const p = personaOf(name);
  if (!p || S.infoFor !== name) return;
  S.infoParts = [
    { make: infoCard, key: (p) => [p.avatar] },
    { make: infoModel, key: (p) => [p.model, S.models] },
    { make: infoMemory, key: (p) => [p.embeddings, p.index_error] },
  ].map((part) => ({ ...part, node: part.make(p), shown: JSON.stringify(part.key(p)) }));
  box.replaceChildren(el('div', 'i-head', 'Details'), ...S.infoParts.map((part) => part.node), infoPrompt(p), infoDanger(p));
}

/** Redraws only the sections whose data changed. */
async function refreshInfo() {
  await ensureModels();
  const p = personaOf(S.infoFor);
  if (!p) return;
  for (const part of S.infoParts) {
    const now = JSON.stringify(part.key(p));
    if (now === part.shown) continue;
    const node = part.make(p);
    part.node.replaceWith(node);
    Object.assign(part, { node, shown: now });
  }
}

function infoCard(p) {
  const card = el('div', 'i-card');
  const big = avatar(p.name, 'lg editable');
  big.title = 'Change profile photo';
  big.onclick = () => changePersonaPhoto(p.name);
  const photoActs = el('div', 'acts');
  const change = el('button', 'btn ghost', p.avatar ? 'Change photo' : 'Add photo');
  change.onclick = () => changePersonaPhoto(p.name);
  photoActs.append(change);
  if (p.avatar) {
    const rm = el('button', 'btn ghost', 'Remove');
    rm.onclick = async () => { await api('DELETE', pp(p.name) + '/avatar').catch((e) => alert(e.message)); await refreshList(); buildThread(); };
    photoActs.append(rm);
  }
  const from = p.source ? ` · from ${p.source}` : '';
  card.append(big, el('div', 'n', p.name),
    el('div', 's', `${p.persona_messages.toLocaleString()} lines${from}`), photoActs);
  return card;
}

function infoModel(p) {
  const models = S.models;
  const sec = el('div', 'i-sec');
  const sel = el('select');
  sel.append(new Option(`Default (${models.default})`, ''));
  const names = new Set(models.models);
  if (p.model) names.add(p.model);
  for (const n of names) sel.append(new Option(n, n));
  sel.value = p.model || '';
  sel.onchange = async () => {
    await api('PUT', pp(p.name) + '/model', { model: sel.value || null }).catch((e) => alert(e.message));
    await refreshList();
  };
  sec.append(el('h3', null, 'Model'), sel);
  if (!models.online) sec.append(el('div', 'hint', 'Model server is offline: ' + (models.error || '')));
  return sec;
}

function infoMemory(p) {
  const sec = el('div', 'i-sec');
  const label = { ready: 'Semantic search ready.', building: 'Building semantic index…', none: 'Keyword search only.' }[p.embeddings];
  sec.append(el('h3', null, 'Memory'), el('div', 'hint', label));
  if (p.embeddings === 'none') {
    const b = el('button', 'btn ghost', 'Build semantic index');
    b.onclick = async () => { await api('POST', pp(p.name) + '/embeddings').catch((e) => alert(e.message)); refreshList(); };
    sec.append(b);
    if (p.index_error) sec.append(el('div', 'err', 'The last build failed: ' + p.index_error));
  }
  return sec;
}

function infoPrompt(p) {
  const sec = el('div', 'i-sec');
  const ta = el('textarea', 'field');
  ta.spellcheck = false;
  const save = el('button', 'btn ghost', 'Save prompt');
  // locked until loaded: a late load must not overwrite typing, nor a failed one be saved as empty
  ta.disabled = save.disabled = true;
  ta.placeholder = 'Loading…';
  api('GET', pp(p.name) + '/prompt')
    .then((j) => { ta.value = j.prompt; ta.disabled = save.disabled = false; })
    .catch((e) => { ta.placeholder = 'Could not load the prompt: ' + e.message; });
  save.onclick = async () => {
    try { await api('PUT', pp(p.name) + '/prompt', { prompt: ta.value }); save.textContent = 'Saved'; }
    catch (e) { save.textContent = 'Failed'; alert(e.message); }
    setTimeout(() => (save.textContent = 'Save prompt'), 1500);
  };
  sec.append(el('h3', null, 'Persona prompt'), ta, save);
  return sec;
}

function infoDanger(p) {
  const sd = el('div', 'i-sec');
  const acts = el('div', 'acts');
  const clear = el('button', 'btn ghost', 'Clear chat');
  clear.onclick = async () => {
    if (!confirm('Clear this conversation?')) return;
    try { await api('DELETE', pp(p.name) + '/chat'); } catch (e) { alert(e.message); return; }
    if (S.cur === p.name) { S.chat = []; renderMessages(); }
    refreshList();
  };
  const del = el('button', 'btn danger', 'Delete persona');
  del.onclick = async () => {
    if (!confirm(`Delete “${p.name}” and all its data?`)) return;
    try { await api('DELETE', pp(p.name)); } catch (e) { alert(e.message); return; }
    delete S.drafts[p.name];
    closeThread();
    refreshList();
  };
  acts.append(clear, del);
  sd.append(acts);
  return sd;
}

function openImport() {
  const body = $('#import-body');
  body.replaceChildren();
  const drop = el('div', 'drop');
  drop.append(el('div', null, 'Drop any files here'),
    el('div', 'hint', 'PDF, Word, EPUB, text, web pages, subtitles, spreadsheets, chat exports, datasets, or a zip of them.'));
  const picks = el('div', 'picks');
  const files = el('input'); files.type = 'file'; files.multiple = true; files.hidden = true;
  const dir = el('input'); dir.type = 'file'; dir.webkitdirectory = true; dir.hidden = true;
  const b1 = el('button', 'btn', 'Choose files');
  const b2 = el('button', 'btn ghost', 'Choose folder');
  b1.onclick = () => files.click();
  b2.onclick = () => dir.click();
  picks.append(b1, b2, files, dir);
  drop.append(picks);
  const status = el('div');
  body.append(drop, status);

  const start = (list) => runImport(Array.from(list), body, status);
  files.onchange = () => start(files.files);
  dir.onchange = () => start(dir.files);
  drop.ondragover = (e) => { e.preventDefault(); drop.classList.add('over'); };
  drop.ondragleave = () => drop.classList.remove('over');
  drop.ondrop = (e) => { e.preventDefault(); drop.classList.remove('over'); droppedFiles(e.dataTransfer).then(start); };
  $('#dlg-import').showModal();
}

/** Dropped files, including those inside dropped folders. */
async function droppedFiles(data) {
  const entries = Array.from(data.items).map((i) => i.webkitGetAsEntry?.()).filter(Boolean);
  if (!entries.length) return Array.from(data.files);
  const out = [];
  const walk = async (entry) => {
    if (entry.name.startsWith('.')) return;
    if (entry.isFile) { out.push(await new Promise((ok, no) => entry.file(ok, no))); return; }
    const reader = entry.createReader();
    for (;;) { // a reader hands out a folder in batches
      const batch = await new Promise((ok, no) => reader.readEntries(ok, no));
      if (!batch.length) return;
      for (const e of batch) await walk(e);
    }
  };
  for (const e of entries) await walk(e);
  return out;
}

function progressView() {
  const label = el('div', 'hint');
  const bar = el('div', 'bar');
  const fill = el('div');
  bar.append(fill);
  return { label, bar, fill };
}

async function runImport(list, body, status) {
  const picked = list.filter((f) => !f.name.startsWith('.'));
  status.replaceChildren();
  if (!picked.length) { status.append(el('div', 'err', 'No files in that selection.')); return; }
  const view = progressView();
  view.label.textContent = `Uploading ${picked.length} file(s)…`;
  body.replaceChildren(view.label, view.bar);
  let token = null;
  const ended = () => S.importToken !== token; // the dialog was closed, and maybe another import started
  try {
    ({ token } = await api('POST', '/api/import/start', {}));
    S.importToken = token;
    for (let i = 0; i < picked.length; i++) {
      if (ended()) return;
      const query = `token=${token}&name=${encodeURIComponent(picked[i].name)}`;
      const r = await fetch('/api/import/file?' + query, { method: 'POST', body: picked[i] });
      if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText);
      view.fill.style.width = ((i + 1) / picked.length) * 100 + '%';
    }
    if (!ended()) chooseCharacters(body, token, await runIdentify(body, token));
  } catch (e) {
    if (e.name === 'AbortError' || ended()) return;
    cancelImport();
    body.replaceChildren(el('div', 'err', e.message));
  }
}

function cancelImport() {
  S.importAbort?.abort(); // stops a running identification: the server notices the closed connection
  if (S.importToken) api('DELETE', '/api/import/' + S.importToken).catch(() => {});
  S.importToken = null;
}

/** Run identification, showing progress in `body`, and resolve with the result. Throws on failure. */
async function runIdentify(body, token, correction) {
  const view = progressView();
  view.label.textContent = 'Working…';
  body.replaceChildren(view.label, view.bar);
  S.importAbort = new AbortController();
  const res = await fetch('/api/import/identify', {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ token, correction }),
    signal: S.importAbort.signal,
  });
  if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error || res.statusText);
  let result = null, error = null;
  await readLines(res, (j) => {
    if (j.stage) {
      view.label.textContent = j.total ? `${j.stage} (${j.done + 1} of ${j.total})…` : `${j.stage}…`;
      if (j.total) view.fill.style.width = (j.done / j.total) * 100 + '%';
    } else if (j.result) result = j.result;
    else if (j.error) error = j.error;
  });
  if (error || !result) throw new Error(error || 'connection closed early');
  return result;
}

function characterOption(c, checked, onChange) {
  const row = el('label', 'who-opt');
  const box = el('input');
  box.type = 'checkbox';
  box.checked = checked;
  box.onchange = onChange;
  const meta = el('div', 'who-meta');
  meta.append(el('div', 'who-name', c.name));
  const facts = c.voice ? [c.description] : [
    c.certain < c.lines ? `${c.lines.toLocaleString()} lines, ${c.certain.toLocaleString()} certain` : `${c.lines.toLocaleString()} lines`,
    ...(c.aliases.length ? ['also ' + c.aliases.join(', ')] : []),
    ...(c.description ? [c.description] : []),
  ];
  meta.append(el('div', 'hint', facts.join(' · ')));
  if (c.samples[0]) meta.append(el('div', 'sample', `“${c.samples[0]}”`));
  row.append(box, avatar(c.name, 'sm', true), meta);
  return { row, box, name: c.name };
}

/** The checkbox list, plus "Identification incorrect" for sending a correction back through the model. */
function chooseCharacters(body, token, info, error) {
  body.replaceChildren();
  body.append(el('div', 'hint', 'Read ' + info.documents.map((d) => `${d.name} (${d.format})`).join(', ') + '.'));
  for (const u of info.unreadable) body.append(el('div', 'hint', `Could not read ${u.name}: ${u.reason}`));
  body.append(el('div', 'who-title', 'Who do you want to talk to?'));

  const update = () => {
    const count = options.filter((o) => o.box.checked).length;
    why.hidden = !wrong.checked;
    go.textContent = wrong.checked ? 'Identify again' : count > 1 ? `Create ${count} personas` : 'Create persona';
    go.disabled = wrong.checked ? !why.value.trim() : count === 0;
  };
  const options = info.characters.map((c) => characterOption(c, info.characters.length === 1, update));
  body.append(...options.map((o) => o.row));

  const notes = [...info.notes];
  if (info.unattributed) notes.push(`${info.unattributed.toLocaleString()} quotes could not be matched to anyone.`);
  if (info.mentioned.length) notes.push(`Mentioned, but no lines found: ${info.mentioned.join(', ')}.`);
  for (const n of notes) body.append(el('div', 'hint', n));

  const wrongRow = el('label', 'who-wrong');
  const wrong = el('input');
  wrong.type = 'checkbox';
  wrong.onchange = () => { update(); if (wrong.checked) why.focus(); };
  wrongRow.append(wrong, el('span', null, 'Identification incorrect'));
  const why = el('textarea', 'field why');
  why.placeholder = 'Tell me how it was incorrect.';
  why.oninput = update;
  const err = el('div', 'err');
  err.hidden = !error;
  err.textContent = error || '';
  const go = el('button', 'btn');
  go.onclick = () => (wrong.checked
    ? correctIdentification(body, token, info, why.value.trim())
    : createPersonas(token, options.filter((o) => o.box.checked), err, go));
  body.append(wrongRow, why, err, go);
  update();
}

async function correctIdentification(body, token, info, correction) {
  try {
    chooseCharacters(body, token, await runIdentify(body, token, correction));
  } catch (e) {
    if (e.name !== 'AbortError') chooseCharacters(body, token, info, e.message);
  }
}

async function createPersonas(token, chosen, err, go) {
  go.disabled = true;
  try {
    const r = await api('POST', '/api/import/create', { token, characters: chosen.map((o) => o.name) });
    await refreshList();
    if (r.failed.length) {
      err.textContent = r.failed.map((f) => `${f.name}: ${f.error}`).join('; ');
      err.hidden = false;
      go.disabled = false;
      // trying again must not create the others twice
      for (const o of chosen) o.box.checked = r.failed.some((f) => f.name === o.name);
      return;
    }
    S.importToken = null; // consumed by the server
    $('#dlg-import').close();
    if (r.created[0]) openThread(r.created[0]);
  } catch (e) {
    err.textContent = e.message;
    err.hidden = false;
    go.disabled = false;
  }
}

async function openSettings() {
  const body = $('#settings-body');
  body.replaceChildren(el('div', 'hint', 'Loading…'));
  $('#dlg-settings').showModal();
  S.models = null;
  let s, models;
  try { [s, models] = [await api('GET', '/api/settings'), await ensureModels()]; }
  catch (e) { body.replaceChildren(el('div', 'err', e.message)); return; }
  body.replaceChildren();

  const field = (label, node) => { const l = el('label', 'f', label); l.append(node); return l; };
  const backend = el('select');
  backend.append(new Option('Ollama', 'Ollama'), new Option('OpenAI-compatible', 'OpenAi'));
  backend.value = s.backend;
  const url = el('input'); url.type = 'text'; url.value = s.base_url;
  const model = el('select');
  const names = new Set(models.models);
  names.add(s.model);
  for (const n of names) model.append(new Option(n, n));
  model.value = s.model;
  const emb = el('input'); emb.type = 'text'; emb.value = s.embed_model || ''; emb.placeholder = 'e.g. nomic-embed-text (blank = off)';
  const temp = el('input'); temp.type = 'number'; temp.step = '0.05'; temp.min = '0'; temp.max = '2'; temp.value = +s.gen.temperature.toFixed(2); // f32 round-trips as 0.800000011920929
  const maxt = el('input'); maxt.type = 'number'; maxt.step = '16'; maxt.min = '16'; maxt.value = s.gen.max_tokens;

  const mine = el('div', 'mine');
  const paintMine = () => {
    const change = el('button', 'btn ghost', S.me.avatar ? 'Change your photo' : 'Add your photo');
    change.onclick = async () => { if (await changeMyPhoto()) paintMine(); };
    mine.replaceChildren(meAvatar('md'), change);
    if (S.me.avatar) {
      const rm = el('button', 'btn ghost', 'Remove');
      rm.onclick = async () => {
        await api('DELETE', '/api/me/avatar').catch((e) => alert(e.message));
        await loadMe();
        paintRail();
        paintMine();
      };
      mine.append(rm);
    }
  };
  paintMine();

  body.append(
    mine,
    el('div', 'hint', models.online ? `Connected · ${models.models.length} chat model(s) installed.` : 'Cannot reach the model server: ' + (models.error || '')),
    field('Backend', backend), field('Server URL', url), field('Default model', model),
    field('Embedding model', emb), field('Temperature', temp), field('Max tokens per reply', maxt));
  const err = el('div', 'err'); err.hidden = true;
  const save = el('button', 'btn', 'Save');
  save.onclick = async () => {
    const temperature = parseFloat(temp.value); // 0 is a valid choice
    const next = {
      ...s, backend: backend.value, base_url: url.value.trim(), model: model.value,
      embed_model: emb.value.trim() || null,
      gen: { ...s.gen, temperature: Number.isFinite(temperature) ? temperature : 0.8, max_tokens: parseInt(maxt.value, 10) || 256 },
    };
    try {
      await api('PUT', '/api/settings', next);
      S.models = null;
      $('#dlg-settings').close();
      await refreshList();
      if (S.cur) buildThread();
    } catch (e) { err.textContent = e.message; err.hidden = false; }
  };
  body.append(err, save);
}

// Closing the import dialog (X / Esc) discards the session and its uploads.
$('#dlg-import').addEventListener('close', cancelImport);

// one outside-click handler for the emoji picker (the composer is rebuilt per thread)
document.addEventListener('click', () => { const pop = $('.emoji-pop'); if (pop) pop.hidden = true; });
$('#btn-messages').innerHTML = ICON.messages;
$('#btn-import-rail').innerHTML = ICON.compose;
$('#btn-settings').innerHTML = ICON.settings;
$('#btn-new').innerHTML = ICON.compose;
$('#btn-import-rail').onclick = openImport;
$('#btn-new').onclick = openImport;
$('#btn-settings').onclick = openSettings;
$('#btn-me').onclick = changeMyPhoto;
$('#btn-messages').onclick = closeThread;
$('#search').oninput = (e) => { S.filter = e.target.value; renderList(); };

renderEmpty();
Promise.all([loadMe(), ensureModels()]).then(refreshList).then(() => {
  paintRail();
  if (S.personas.length === 1) openThread(S.personas[0].name);
});
