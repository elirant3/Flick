const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const cv = document.getElementById('c');
const ctx = cv.getContext('2d');
const bar = document.getElementById('bar');
const toastEl = document.getElementById('toast');
const txt = document.getElementById('txt');

const COLORS = ['#ff3b30', '#ff9500', '#ffd60a', '#34c759', '#0a84ff', '#ffffff', '#000000'];
const SIZES = [2, 4, 8];
const TOOLS = [
  ['pen', '✎', 'Pen (P)'], ['line', '╱', 'Line (L)'], ['arrow', '➚', 'Arrow (A)'],
  ['rect', '▭', 'Rectangle (R)'], ['ellipse', '◯', 'Circle (E)'],
  ['marker', '▬', 'Marker (M)'], ['text', 'T', 'Text (T)'],
];
const KEYS = { p: 'pen', l: 'line', a: 'arrow', r: 'rect', e: 'ellipse', m: 'marker', t: 'text' };

let img = null;                 // frozen screenshot (ImageBitmap)
let sel = null;                 // {x,y,w,h} in CSS px
let anns = [];                  // committed annotations
let cur = null;                 // annotation being drawn
let tool = 'pen', color = COLORS[0], size = SIZES[1];
let drag = null;                // {mode:'new'|'handle'|'draw', ...}
let copyTimer = null;

// ---------- setup ----------
function fit() {
  const dpr = devicePixelRatio || 1;
  cv.width = innerWidth * dpr; cv.height = innerHeight * dpr;
  cv.style.width = innerWidth + 'px'; cv.style.height = innerHeight + 'px';
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
}
addEventListener('resize', () => { fit(); render(); });

function buildBar() {
  bar.innerHTML = '';
  const add = (el) => bar.appendChild(el);
  const btn = (label, title, fn, cls = '') => {
    const b = document.createElement('button');
    b.textContent = label; b.title = title; b.className = cls;
    b.onclick = (e) => { e.stopPropagation(); fn(); };
    add(b); return b;
  };
  const sep = () => { const s = document.createElement('div'); s.className = 'sep'; add(s); };
  TOOLS.forEach(([id, ico, title]) => btn(ico, title, () => setTool(id), 'tool ' + (id === tool ? 'on' : '')).dataset.tool = id);
  sep();
  COLORS.forEach((c) => {
    const s = document.createElement('div');
    s.className = 'sw' + (c === color ? ' on' : ''); s.style.background = c; s.dataset.color = c;
    s.onclick = (e) => { e.stopPropagation(); color = c; syncBar(); };
    add(s);
  });
  sep();
  SIZES.forEach((z, i) => {
    const b = btn(['•', '●', '⬤'][i], 'Size ' + z, () => { size = z; syncBar(); }, 'sz');
    b.dataset.size = z;
  });
  sep();
  btn('↶', 'Undo (⌘Z)', undo);
  btn('⤓', 'Save to Desktop (⌘S)', saveFile);
  btn('✕', 'Cancel (Esc)', close);
  btn('✓', 'Done (Enter)', done);
  syncBar();
}
function syncBar() {
  bar.querySelectorAll('.tool').forEach((b) => b.classList.toggle('on', b.dataset.tool === tool));
  bar.querySelectorAll('.sw').forEach((s) => s.classList.toggle('on', s.dataset.color === color));
  bar.querySelectorAll('.sz').forEach((b) => b.classList.toggle('on', +b.dataset.size === size));
}
function setTool(t) { commitText(); tool = t; syncBar(); }

function placeBar() {
  if (!sel) { bar.style.display = 'none'; return; }
  bar.style.display = 'flex';
  const bw = bar.offsetWidth, bh = bar.offsetHeight;
  let x = Math.min(Math.max(8, sel.x + sel.w - bw), innerWidth - bw - 8);
  let y = sel.y + sel.h + 10;
  if (y + bh > innerHeight - 8) y = sel.y - bh - 10;
  if (y < 8) y = Math.max(8, sel.y + sel.h - bh - 10);
  bar.style.left = x + 'px'; bar.style.top = y + 'px';
}

// ---------- drawing ----------
function drawAnn(c, a) {
  c.save();
  c.strokeStyle = c.fillStyle = a.color;
  c.lineCap = c.lineJoin = 'round';
  c.lineWidth = a.size;
  if (a.tool === 'pen') {
    c.beginPath(); a.pts.forEach(([x, y], i) => (i ? c.lineTo(x, y) : c.moveTo(x, y)));
    if (a.pts.length === 1) c.lineTo(a.pts[0][0] + .01, a.pts[0][1]);
    c.stroke();
  } else if (a.tool === 'marker') {
    c.globalAlpha = 0.4; c.lineWidth = a.size * 4; c.lineCap = 'butt';
    c.beginPath(); a.pts.forEach(([x, y], i) => (i ? c.lineTo(x, y) : c.moveTo(x, y))); c.stroke();
  } else if (a.tool === 'line') {
    c.beginPath(); c.moveTo(a.x1, a.y1); c.lineTo(a.x2, a.y2); c.stroke();
  } else if (a.tool === 'arrow') {
    const ang = Math.atan2(a.y2 - a.y1, a.x2 - a.x1), h = 8 + a.size * 3;
    c.beginPath(); c.moveTo(a.x1, a.y1); c.lineTo(a.x2, a.y2); c.stroke();
    c.beginPath();
    c.moveTo(a.x2, a.y2);
    c.lineTo(a.x2 - h * Math.cos(ang - 0.45), a.y2 - h * Math.sin(ang - 0.45));
    c.lineTo(a.x2 - h * Math.cos(ang + 0.45), a.y2 - h * Math.sin(ang + 0.45));
    c.closePath(); c.fill();
  } else if (a.tool === 'rect') {
    c.strokeRect(a.x1, a.y1, a.x2 - a.x1, a.y2 - a.y1);
  } else if (a.tool === 'ellipse') {
    c.beginPath();
    c.ellipse((a.x1 + a.x2) / 2, (a.y1 + a.y2) / 2, Math.abs(a.x2 - a.x1) / 2, Math.abs(a.y2 - a.y1) / 2, 0, 0, Math.PI * 2);
    c.stroke();
  } else if (a.tool === 'text') {
    c.font = `700 ${a.fs}px -apple-system, system-ui, sans-serif`; c.textBaseline = 'top';
    a.text.split('\n').forEach((ln, i) => c.fillText(ln, a.x1, a.y1 + i * a.fs * 1.2));
  }
  c.restore();
}

function render() {
  ctx.clearRect(0, 0, innerWidth, innerHeight);
  if (!img) return;
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(img, 0, 0, innerWidth, innerHeight);
  ctx.fillStyle = 'rgba(0,0,0,.45)';
  if (!sel) { ctx.fillRect(0, 0, innerWidth, innerHeight); return; }
  ctx.beginPath();
  ctx.rect(0, 0, innerWidth, innerHeight); ctx.rect(sel.x, sel.y, sel.w, sel.h);
  ctx.fill('evenodd');
  ctx.save();
  ctx.beginPath(); ctx.rect(sel.x, sel.y, sel.w, sel.h); ctx.clip();
  anns.forEach((a) => drawAnn(ctx, a)); if (cur) drawAnn(ctx, cur);
  ctx.restore();
  ctx.strokeStyle = '#ff5a3c'; ctx.lineWidth = 1.5; ctx.strokeRect(sel.x, sel.y, sel.w, sel.h);
  ctx.fillStyle = '#ff5a3c';
  handles().forEach(([hx, hy]) => ctx.fillRect(hx - 3.5, hy - 3.5, 7, 7));
  const label = `${Math.round(sel.w * scaleX())} × ${Math.round(sel.h * scaleY())}`;
  ctx.font = '600 11px -apple-system, system-ui'; const tw = ctx.measureText(label).width + 12;
  const ly = sel.y > 26 ? sel.y - 24 : sel.y + 6;
  ctx.fillStyle = '#1d1d1f'; ctx.fillRect(sel.x, ly, tw, 18);
  ctx.fillStyle = '#fff'; ctx.textBaseline = 'middle'; ctx.fillText(label, sel.x + 6, ly + 9);
}
const scaleX = () => img.width / innerWidth;
const scaleY = () => img.height / innerHeight;

// ---------- selection handles ----------
function handles() {
  const { x, y, w, h } = sel;
  return [[x, y], [x + w / 2, y], [x + w, y], [x + w, y + h / 2], [x + w, y + h], [x + w / 2, y + h], [x, y + h], [x, y + h / 2]];
}
const HANDLE_DIR = [[-1, -1], [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0]];
function hitHandle(px, py) {
  if (!sel) return -1;
  return handles().findIndex(([x, y]) => Math.abs(px - x) < 8 && Math.abs(py - y) < 8);
}
const inSel = (x, y) => sel && x >= sel.x && x <= sel.x + sel.w && y >= sel.y && y <= sel.y + sel.h;
const clampX = (x) => Math.min(Math.max(0, x), innerWidth);
const clampY = (y) => Math.min(Math.max(0, y), innerHeight);
const norm = (r) => ({ x: Math.min(r.x1, r.x2), y: Math.min(r.y1, r.y2), w: Math.abs(r.x2 - r.x1), h: Math.abs(r.y2 - r.y1) });

// ---------- pointer ----------
cv.addEventListener('pointerdown', (e) => {
  if (e.button !== 0) return;
  commitText();
  cv.setPointerCapture(e.pointerId);
  const x = e.clientX, y = e.clientY;
  const hi = hitHandle(x, y);
  if (hi >= 0) {
    const [dx, dy] = HANDLE_DIR[hi];
    drag = { mode: 'handle', dx, dy, x1: sel.x, y1: sel.y, x2: sel.x + sel.w, y2: sel.y + sel.h };
  } else if (inSel(x, y)) {
    if (tool === 'text') { startText(x, y); return; }
    cur = { tool, color, size, x1: x, y1: y, x2: x, y2: y, pts: [[x, y]] };
    drag = { mode: 'draw' };
  } else {
    // Start a fresh selection (discards annotations).
    anns = []; sel = null;
    drag = { mode: 'new', x1: x, y1: y };
    bar.style.display = 'none';
  }
});
cv.addEventListener('pointermove', (e) => {
  const x = clampX(e.clientX), y = clampY(e.clientY);
  if (!drag) {
    const hi = hitHandle(e.clientX, e.clientY);
    cv.style.cursor = hi >= 0 ? ['nwse-resize', 'ns-resize', 'nesw-resize', 'ew-resize', 'nwse-resize', 'ns-resize', 'nesw-resize', 'ew-resize'][hi] : 'crosshair';
    return;
  }
  if (drag.mode === 'new') {
    sel = norm({ x1: drag.x1, y1: drag.y1, x2: x, y2: y });
  } else if (drag.mode === 'handle') {
    if (drag.dx === -1) drag.x1 = x; if (drag.dx === 1) drag.x2 = x;
    if (drag.dy === -1) drag.y1 = y; if (drag.dy === 1) drag.y2 = y;
    sel = norm(drag);
  } else if (drag.mode === 'draw' && cur) {
    cur.x2 = x; cur.y2 = y;
    if (cur.tool === 'pen' || cur.tool === 'marker') cur.pts.push([x, y]);
    if (e.shiftKey && (cur.tool === 'line' || cur.tool === 'arrow')) {
      const ang = Math.round(Math.atan2(y - cur.y1, x - cur.x1) / (Math.PI / 4)) * (Math.PI / 4);
      const len = Math.hypot(x - cur.x1, y - cur.y1);
      cur.x2 = cur.x1 + len * Math.cos(ang); cur.y2 = cur.y1 + len * Math.sin(ang);
    }
  }
  render();
});
cv.addEventListener('pointerup', () => {
  if (!drag) return;
  const mode = drag.mode; drag = null;
  if (mode === 'new') {
    if (!sel || sel.w < 4 || sel.h < 4) { sel = null; render(); return; }
    buildBar(); placeBar(); scheduleCopy(true);
  } else if (mode === 'handle') {
    placeBar(); scheduleCopy();
  } else if (mode === 'draw' && cur) {
    const moved = cur.pts.length > 1 || Math.hypot(cur.x2 - cur.x1, cur.y2 - cur.y1) > 2;
    if (moved || cur.tool === 'pen') anns.push(cur);
    cur = null; scheduleCopy();
  }
  render();
});

// ---------- text ----------
let textPos = null;
function startText(x, y) {
  textPos = { x, y };
  const fs = 12 + size * 3;
  Object.assign(txt.style, { display: 'block', left: x + 'px', top: y + 'px', color, fontSize: fs + 'px', lineHeight: '1.2' });
  txt.value = ''; setTimeout(() => txt.focus(), 0);
}
function commitText() {
  if (txt.style.display !== 'block') return;
  const v = txt.value.trim();
  if (v && textPos) {
    anns.push({ tool: 'text', color: txt.style.color, fs: parseInt(txt.style.fontSize, 10), text: v, x1: textPos.x + 2, y1: textPos.y, size: 1 });
    scheduleCopy();
  }
  txt.style.display = 'none'; txt.value = ''; textPos = null; render();
}
txt.addEventListener('keydown', (e) => {
  e.stopPropagation();
  if (e.key === 'Enter') { e.preventDefault(); commitText(); }
  if (e.key === 'Escape') { txt.value = ''; commitText(); }
});
txt.addEventListener('blur', commitText);

// ---------- output ----------
// Crop on whole screenshot pixels, copied 1:1 so it is never resampled; annotations are then drawn at full resolution.
function exportCanvas() {
  const sx = scaleX(), sy = scaleY();
  const px = Math.round(sel.x * sx), py = Math.round(sel.y * sy);
  const pw = Math.max(1, Math.min(img.width - px, Math.round(sel.w * sx)));
  const ph = Math.max(1, Math.min(img.height - py, Math.round(sel.h * sy)));
  const out = document.createElement('canvas');
  out.width = pw; out.height = ph;
  const c = out.getContext('2d');
  c.imageSmoothingEnabled = false;
  c.drawImage(img, px, py, pw, ph, 0, 0, pw, ph);
  c.imageSmoothingEnabled = true;
  c.setTransform(sx, 0, 0, sy, -px, -py);
  anns.forEach((a) => drawAnn(c, a));
  return out;
}
const release = (canvas) => { canvas.width = canvas.height = 0; };  // free the backing store right away

let ipcQueue = Promise.resolve();  // serialises clipboard writes so the newest edit always lands last
function copyToClipboard(quiet) {
  if (!sel || !img) return Promise.resolve(false);
  const out = exportCanvas();  // snapshot now, send later
  const { width, height } = out;
  const px = out.getContext('2d').getImageData(0, 0, width, height).data;
  release(out);
  const body = new Uint8Array(px.buffer, px.byteOffset, px.byteLength);
  const job = ipcQueue
    .then(() => invoke('copy_image', body, { headers: { 'x-width': String(width), 'x-height': String(height) } }))
    .then(() => { if (!quiet) toast('Copied to clipboard ✓'); return true; },
          (err) => { toast('Copy failed: ' + err); return false; });
  ipcQueue = job;
  return job;
}
function scheduleCopy(first) {
  clearTimeout(copyTimer);
  copyTimer = setTimeout(() => copyToClipboard(false), first ? 0 : 250);
}
let toastT;
function toast(msg) {
  toastEl.textContent = msg; toastEl.classList.add('show');
  clearTimeout(toastT); toastT = setTimeout(() => toastEl.classList.remove('show'), 1400);
}
async function done() {
  commitText(); clearTimeout(copyTimer);
  if (await copyToClipboard(true)) close();  // stay open if the copy failed so nothing is lost
}
async function saveFile() {
  commitText(); clearTimeout(copyTimer);
  if (!sel || !img) return;
  const out = exportCanvas();
  const blob = await new Promise((resolve) => out.toBlob(resolve, 'image/png'));
  release(out);
  if (!blob) { toast('Save failed'); return; }
  await copyToClipboard(true);
  try { await invoke('save_image', new Uint8Array(await blob.arrayBuffer())); close(); }
  catch (err) { toast('Save failed: ' + err); }
}
function undo() { commitText(); anns.pop(); render(); scheduleCopy(); }
function close() {
  clearTimeout(copyTimer);
  drag = null; cur = null; sel = null; anns = []; armed = false;
  if (img) { img.close(); img = null; }  // release the full-screen bitmap
  bar.style.display = 'none'; txt.style.display = 'none';
  ctx.clearRect(0, 0, innerWidth, innerHeight);
  invoke('hide_overlay').catch(console.error);
}

addEventListener('keydown', (e) => {
  if (e.key === 'Escape') { close(); return; }
  if (!sel) return;
  if (e.key === 'Enter') { e.preventDefault(); done(); return; }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'c') { e.preventDefault(); done(); return; }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'z') { e.preventDefault(); undo(); return; }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') { e.preventDefault(); saveFile(); return; }
  if (!e.metaKey && !e.ctrlKey && !e.altKey && KEYS[e.key.toLowerCase()]) setTool(KEYS[e.key.toLowerCase()]);
});
addEventListener('contextmenu', (e) => { e.preventDefault(); close(); });
cv.addEventListener('pointercancel', () => { drag = null; cur = null; render(); });

// Losing focus (Cmd+Tab, Mission Control...) must never leave a screen-covering overlay stuck on top.
let armed = false;
addEventListener('focus', () => { armed = true; });
addEventListener('blur', () => { if (armed && img) close(); });

// ---------- lifecycle ----------
listen('shot', async () => {
  try {
    const png = await invoke('take_shot');  // raw binary IPC response, no base64
    const bitmap = await createImageBitmap(new Blob([png], { type: 'image/png' }));
    if (img) img.close();
    fit(); img = bitmap; sel = null; anns = []; cur = null; render();
    armed = document.hasFocus();
  } catch (err) {
    console.error(err);
    invoke('hide_overlay').catch(console.error);
  }
});
fit();
