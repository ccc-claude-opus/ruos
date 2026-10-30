// The system view: what the kernel's snapshot (kernel_snapshot, JSON) says,
// drawn for students. Tasks are nodes; a channel whose two ends are held by
// two tasks is an edge between them (who can talk to whom). The quota tree
// shows what each part of the system may use (a pod is a quota of its own).
// Click a task to see its handle table: its capabilities.

const svgNS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, ...kids) => {
  const e = tag.startsWith('svg:') ? document.createElementNS(svgNS, tag.slice(4)) : document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') e.setAttribute('class', v);
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v);
  }
  for (const k of kids) e.append(k);
  return e;
};

const kib = (b) => (b >= 1 << 20 ? `${(b / (1 << 20)).toFixed(1)} MiB` : `${Math.ceil(b / 1024)} KiB`);
const TYPE = { task: '任務', thread: '執行緒', channel: '通道', memory: '記憶體', port: 'port', quota: 'quota', serial: '序列埠' };

export class Inspector {
  constructor(root) {
    this.root = root;
    this.selected = null;
    this.snap = null;
    this.graph = el('svg:svg', { class: 'graph', viewBox: '0 0 400 300', role: 'img', 'aria-label': '任務與通道' });
    this.detail = el('div', { class: 'detail' });
    this.quotas = el('div', { class: 'quotas' });
    this.summary = el('div', { class: 'summary' });
    root.append(
      this.summary,
      el('h3', {}, '誰能跟誰說話（任務與通道）'),
      this.graph,
      el('p', { class: 'hint' }, '點一個任務，看它持有的 handle：它能用的就只有這些。'),
      this.detail,
      el('h3', {}, '資源額度（quota）'),
      this.quotas,
    );
  }

  update(json) {
    let snap;
    try {
      snap = JSON.parse(json);
    } catch {
      return;
    }
    this.snap = snap;
    // Only the clock moved: keep the drawing (and keyboard focus in it).
    const key = JSON.stringify({ ...snap, uptime_ms: 0 });
    if (key === this.key) return this.drawSummary();
    this.key = key;
    this.render();
  }

  drawSummary() {
    const s = this.snap, live = s.tasks.filter((t) => t.exit === null);
    this.summary.textContent = `開機 ${(s.uptime_ms / 1000).toFixed(0)} 秒 · ${live.length} 個任務 · ${live.reduce((n, t) => n + t.handles.length, 0)} 個 handle`;
  }

  render() {
    const s = this.snap;
    if (!s) return;
    const live = s.tasks.filter((t) => t.exit === null);
    if (this.selected !== null && !live.some((t) => t.id === this.selected)) this.selected = null;
    // Who holds each object (an object's id is its address in the kernel).
    const holders = new Map();
    for (const t of live) for (const h of t.handles) {
      if (!holders.has(h.id)) holders.set(h.id, []);
      holders.get(h.id).push({ task: t, h: h.h });
    }
    this.holders = holders;
    this.drawSummary();
    this.drawGraph(live, holders);
    this.drawDetail(live);
    this.drawQuotas(s, live);
  }

  drawGraph(live, holders) {
    const g = this.graph;
    g.replaceChildren();
    const W = 400, H = 300, cx = W / 2, cy = H / 2, r = Math.min(W, H) / 2 - 34;
    const nodes = live.map((t, i) => {
      const a = (i / Math.max(live.length, 1)) * 2 * Math.PI - Math.PI / 2;
      return { t, x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) };
    });
    const pos = new Map(nodes.map((n) => [n.t.id, n]));
    // Edges: channel ends held by two different tasks; or a task and the server.
    const edges = new Map();
    let server = false;
    for (const t of live) for (const h of t.handles) {
      if (h.type !== 'channel') continue;
      if (h.peer === 'host') {
        server = true;
        edges.set(`${t.id}-server`, { a: t.id, b: 'server' });
        continue;
      }
      for (const o of holders.get(h.peer) || []) {
        if (o.task.id === t.id) continue;
        const [a, b] = t.id < o.task.id ? [t.id, o.task.id] : [o.task.id, t.id];
        edges.set(`${a}-${b}`, { a, b });
      }
    }
    if (server) pos.set('server', { x: W - 30, y: 24, server: true });
    for (const e of edges.values()) {
      const p = pos.get(e.a), q = pos.get(e.b);
      const on = this.selected !== null && (e.a === this.selected || e.b === this.selected);
      g.append(el('svg:line', { x1: p.x, y1: p.y, x2: q.x, y2: q.y, class: on ? 'edge on' : 'edge' }));
    }
    if (server) {
      const p = pos.get('server');
      g.append(el('svg:rect', { x: p.x - 26, y: p.y - 11, width: 52, height: 22, rx: 4, class: 'server' }));
      g.append(el('svg:text', { x: p.x, y: p.y + 4, class: 'label' }, '伺服器'));
    }
    for (const n of nodes) {
      const sel = n.t.id === this.selected;
      const node = el('svg:g', { class: sel ? 'node on' : 'node', tabindex: 0, role: 'button', onclick: () => this.select(n.t.id),
        onkeydown: (e) => (e.key === 'Enter' || e.key === ' ') && this.select(n.t.id) });
      node.append(el('svg:circle', { cx: n.x, cy: n.y, r: 14 }));
      node.append(el('svg:text', { x: n.x, y: n.y + 4, class: 'num' }, String(n.t.id)));
      node.append(el('svg:text', { x: n.x, y: n.y + 28, class: 'label' }, n.t.name));
      node.append(el('svg:title', {}, `${n.t.name}（task ${n.t.id}）：${n.t.handles.length} 個 handle`));
      g.append(node);
    }
  }

  select(id) {
    this.selected = this.selected === id ? null : id;
    this.render();
  }

  // What a handle reaches, in words.
  describe(t, h) {
    const who = (o) => `${o.task.name}（task ${o.task.id}）的 h${o.h}`;
    switch (h.type) {
      case 'channel': {
        if (h.peer === 'host') return '另一端是伺服器（ruos serve）';
        const others = (this.holders.get(h.peer) || []);
        const q = h.queued ? `，${h.queued} 則訊息待讀` : '';
        if (!others.length) return `另一端已關閉${q}`;
        return `另一端：${others.map(who).join('、')}${q}`;
      }
      case 'task': {
        const o = this.snap.tasks.find((x) => x.id === h.task);
        return `可以等待或結束 ${o ? o.name : '?'}（task ${h.task}）`;
      }
      case 'memory': {
        const sharers = (this.holders.get(h.id) || []).filter((o) => o.task.id !== t.id);
        return `${kib(h.size)}${sharers.length ? `，與 ${sharers.map(who).join('、')} 共用` : ''}`;
      }
      case 'quota': return '可以從這份額度切出子額度';
      case 'serial': return '主控台的鍵盤與螢幕';
      case 'port': return '等待事件用';
      default: return '';
    }
  }

  drawDetail(live) {
    const t = live.find((x) => x.id === this.selected);
    if (!t) {
      this.detail.replaceChildren();
      return;
    }
    const rows = t.handles.map((h) =>
      el('tr', {}, el('td', {}, `h${h.h}`), el('td', {}, TYPE[h.type] || h.type), el('td', { class: 'mono' }, h.rights), el('td', {}, this.describe(t, h))));
    this.detail.replaceChildren(
      el('h4', {}, `${t.name}（task ${t.id}）· ${t.threads} 個執行緒`),
      el('table', {}, el('thead', {}, el('tr', {}, el('th', {}, 'handle'), el('th', {}, '物件'), el('th', {}, '權限'), el('th', {}, '連到哪裡'))), el('tbody', {}, ...rows)),
      el('p', { class: 'hint' }, '權限：d 複製、t 轉交、w 等待、r 讀、W 寫、m 管理。'),
    );
  }

  drawQuotas(s, live) {
    const kids = new Map();
    for (const q of s.quotas) {
      const p = q.parent ?? 'none';
      if (!kids.has(p)) kids.set(p, []);
      kids.get(p).push(q);
    }
    const node = (q, depth) => {
      const tasks = live.filter((t) => t.quota === q.id);
      const sub = kids.get(q.id) || [];
      const name = q.parent === null ? '全部（根）' : depth === 1 && !tasks.length ? 'pods 總額' : tasks.length ? `pod：${tasks[tasks.length - 1].name}` : 'quota';
      const pct = q.mem[1] ? Math.min(100, (q.mem[0] / q.mem[1]) * 100) : 0;
      const lim = (u, l) => (l === null ? `${u}` : `${u}/${l}`);
      return el('li', {},
        el('div', { class: 'q' },
          el('span', { class: 'qname' }, name),
          el('span', { class: 'bar' }, el('span', { style: `width:${pct}%` })),
          el('span', { class: 'qnum' }, `${kib(q.mem[0])}${q.mem[1] !== null ? ` / ${kib(q.mem[1])}` : ''} · 任務 ${lim(q.tasks[0], q.tasks[1])}`)),
        tasks.length ? el('div', { class: 'chips' }, ...tasks.map((t) => el('button', { class: t.id === this.selected ? 'chip on' : 'chip', onclick: () => this.select(t.id) }, `${t.name} ${t.id}`))) : '',
        sub.length ? el('ul', {}, ...sub.map((c) => node(c, depth + 1))) : '');
    };
    this.quotas.replaceChildren(el('ul', {}, ...(kids.get('none') || []).map((q) => node(q, 0))));
  }
}
