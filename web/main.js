// The page: a terminal on the kernel Worker, the lessons, and the system
// view (the kernel's snapshot, polled once a second while it is shown).
import { ClusterView } from './cluster.js';
import { Inspector } from './inspector.js';
import { renderLessons } from './lessons.js';

const term = document.getElementById('term');
const status = document.getElementById('status');
let worker = null;
// ?machine=NAME: another machine (its own disk; another node of the cluster).
const machine = (new URLSearchParams(location.search).get('machine') || '').replace(/[^a-z0-9-]/g, '') || 'browser';
const tasks = new Map(); // id → task thread Worker (started for the kernel Worker)

// A little of a terminal: text, newlines, the console's erase sequences
// (ESC [ n D moves back n columns, ESC [ K erases to the end) and clear
// (ESC [ 2 J).
let pending = '';
let back = 0;
let screen = ''; // the terminal's text (term shows it, with its URLs as links)
const wide = (c) => /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe4f\uff00-\uff60\uffe0-\uffe6]|[\u{20000}-\u{3fffd}]/u.test(c);
function write(text) {
  let t = pending + text;
  pending = '';
  let out = screen;
  for (let i = 0; i < t.length; ) {
    if (t[i] === '\x1b') {
      const m = t.slice(i).match(/^\x1b\[(\d*)([A-Za-z])/);
      if (!m) {
        if (t.length - i < 8) pending = t.slice(i);
        break;
      }
      if (m[2] === 'D') back += Number(m[1] || 1);
      // ESC [ 2 J clears the screen (`clear`); ESC [ H, the cursor home, is implied.
      if (m[2] === 'J') {
        out = '';
        back = 0;
      }
      if (m[2] === 'K') {
        const chars = [...out];
        while (back > 0 && chars.length && chars[chars.length - 1] !== '\n') back -= wide(chars.pop()) ? 2 : 1;
        out = chars.join('');
        back = 0;
      }
      i += m[0].length;
    } else if (t[i] === '\r') {
      i++;
    } else if (t[i] === '\b') {
      const chars = [...out];
      if (chars.length && chars[chars.length - 1] !== '\n') chars.pop();
      out = chars.join('');
      i++;
    } else {
      back = 0;
      out += t[i];
      i++;
    }
  }
  screen = out;
  show(out);
  term.scrollTop = term.scrollHeight;
}

// The text, with every http(s) URL a link that opens in a new tab (the
// sites `kube get fn` lists, for one).
const URL_RE = /https?:\/\/[^\s"'<>]+/g;
function show(text) {
  const f = document.createDocumentFragment();
  let at = 0;
  for (const m of text.matchAll(URL_RE)) {
    const url = m[0].replace(/[.,;:)\]}。，）]+$/u, '');
    f.append(text.slice(at, m.index));
    f.append(Object.assign(document.createElement('a'), { href: url, textContent: url, target: '_blank', rel: 'noopener noreferrer' }));
    at = m.index + url.length;
  }
  f.append(text.slice(at));
  term.replaceChildren(f);
}

function boot() {
  worker?.terminate();
  for (const w of tasks.values()) w.terminate();
  tasks.clear();
  screen = '';
  term.textContent = '';
  pending = '';
  const iso = self.crossOriginIsolated ? 'cross-origin isolated' : 'not isolated (no SharedArrayBuffer)';
  status.textContent = `執行中 · ${iso}`;
  worker = new Worker(new URL('kernel-worker.js', import.meta.url), { type: 'module' });
  worker.onmessage = (e) => {
    const m = e.data;
    if (m.type === 'out') {
      write(m.text);
    } else if (m.type === 'snapshot') {
      inspector.update(m.json);
    } else if (m.type === 'spawn') {
      const w = new Worker(new URL('task-worker.js', import.meta.url), { type: 'module' });
      w.onerror = (e) => worker.postMessage({ type: 'werror', id: m.id, msg: `worker error: ${e.message || 'failed to load'}` });
      w.postMessage({ port: m.port }, [m.port]);
      tasks.set(m.id, w);
    } else if (m.type === 'kill') {
      tasks.get(m.id)?.terminate();
      tasks.delete(m.id);
    } else if (m.type === 'exit') {
      for (const w of tasks.values()) w.terminate();
      tasks.clear();
      status.textContent = `核心已關機（代碼 ${m.code}）`;
    }
  };
  worker.postMessage({ type: 'boot', url: new URL('kernel.wasm', location.href).href, machine });
  kbd.focus();
}

// Keys go through a hidden textarea: `keydown` for Enter / Backspace /
// arrows / Ctrl, `input` for text (also from IMEs, phone keyboards and paste).
const kbd = document.getElementById('kbd');
const send = (bytes) => worker?.postMessage({ type: 'key', bytes });
kbd.addEventListener('keydown', (e) => {
  if (e.isComposing || e.metaKey) return;
  let bytes = null;
  if (e.key === 'Enter') bytes = [13];
  else if (e.key === 'Backspace') bytes = [127];
  // ↑ ↓: the console's line history (ESC [ A, ESC [ B).
  else if (e.key === 'ArrowUp') bytes = [27, 91, 65];
  else if (e.key === 'ArrowDown') bytes = [27, 91, 66];
  // Tab stays in the terminal (no completion yet).
  else if (e.key === 'Tab') bytes = [];
  else if (e.ctrlKey && e.key.length === 1) bytes = [e.key.toUpperCase().charCodeAt(0) & 31];
  if (bytes) {
    send(bytes);
    e.preventDefault();
  }
});
kbd.addEventListener('input', () => {
  if (kbd.value) send([...new TextEncoder().encode(kbd.value)]);
  kbd.value = '';
});
// A click types again, unless it selected text (to copy) or was on a link.
term.addEventListener('click', (e) => {
  if (e.target.closest('a') || getSelection().toString()) return;
  kbd.focus();
});

document.getElementById('reboot').onclick = boot;

// A lesson's command, typed into the terminal.
const typeCommand = (cmd) => {
  send([...new TextEncoder().encode(cmd), 13]);
  kbd.focus();
};
renderLessons(document.getElementById('lessons'), typeCommand);

const inspector = new Inspector(document.getElementById('system'));
const cluster = new ClusterView(document.getElementById('cluster'), machine);
const tabs = [['tab-lessons', 'lessons'], ['tab-system', 'system'], ['tab-cluster', 'cluster']];
for (const [tab, panel] of tabs) {
  document.getElementById(tab).onclick = () => {
    for (const [t, p] of tabs) {
      document.getElementById(t).setAttribute('aria-selected', String(t === tab));
      document.getElementById(p).hidden = p !== panel;
    }
    if (panel === 'system') worker?.postMessage({ type: 'snapshot' });
    if (panel === 'cluster') cluster.refresh();
  };
}
setInterval(() => {
  if (document.hidden) return;
  if (!document.getElementById('system').hidden) worker?.postMessage({ type: 'snapshot' });
  if (!document.getElementById('cluster').hidden) cluster.refresh();
}, 1000);

boot();
