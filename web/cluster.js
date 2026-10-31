// The cluster view: kube's records on the server (GET v1/cluster), drawn
// for students. Each machine connected to the same server is a node; the
// leader decides where each pod goes, each node runs its own. What is
// wanted (Deployments) is on one side, what runs (pods on nodes) on the other.

const el = (tag, attrs = {}, ...kids) => {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') e.className = v;
    else e.setAttribute(k, v);
  }
  for (const k of kids) e.append(k);
  return e;
};

// A record's value: `field value` lines.
const fields = (v) => {
  const f = {};
  for (const l of (v || '').split('\n')) {
    const i = l.indexOf(' ');
    if (l) f[i < 0 ? l : l.slice(0, i)] = i < 0 ? '' : l.slice(i + 1);
  }
  return f;
};

const NODE_LEASE = 8000;

const ago = (now, t) => {
  const s = Math.max(0, Math.floor((now - t) / 1000));
  return s < 120 ? `${s}s` : s < 7200 ? `${Math.floor(s / 60)}m` : `${Math.floor(s / 3600)}h`;
};

// What `kube get pods` says, and a colour for it.
function podState(pod, st) {
  if (pod.node === '-') return ['Unschedulable', 'bad'];
  if (!st || st.node !== pod.node) return ['Pending', 'dim'];
  switch (st.phase) {
    case 'Running': return st.ready === '1' ? ['Running', 'good'] : ['Running（未就緒）', 'warn'];
    case 'BackOff': return ['CrashLoopBackOff', 'bad'];
    case 'Done': return st.code === '0' ? ['Completed', 'dim'] : ['Error', 'bad'];
    default: return [st.reason ? `Pending：${st.reason}` : 'Pending', 'dim'];
  }
}

export class ClusterView {
  constructor(root, machine) {
    this.root = root;
    this.machine = machine;
    this.last = '';
  }

  async refresh() {
    let text;
    try {
      const r = await fetch('v1/cluster', { cache: 'no-store' });
      if (!r.ok) throw new Error(r.status);
      text = await r.text();
    } catch {
      this.root.replaceChildren(el('p', { class: 'hint' }, '連不到 server：這台機器自己是一個節點的叢集。'));
      this.last = '';
      return;
    }
    // Draw again only when something changed (keeps scroll and focus).
    if (text === this.last) return;
    this.last = text;
    this.draw(JSON.parse(text));
  }

  draw({ now, records, fn_port: fnPort }) {
    const get = (k) => records[k];
    const prefix = (p) => Object.entries(records).filter(([k]) => k.startsWith(p)).map(([k, r]) => [k.slice(p.length), r]);
    const leader = fields(get('leader')?.value).node;
    const nodes = prefix('nodes/').map(([name, r]) => ({ name, ...fields(r.value), up: now - r.updated < NODE_LEASE }));
    const pods = prefix('pod/').map(([name, r]) => ({ name, ...fields(r.value) }));
    const status = Object.fromEntries(prefix('status/').map(([n, r]) => [n, fields(r.value)]));
    const deploys = prefix('deploy/').map(([name, r]) => {
      const d = { name, ...fields(r.value) };
      const [head, manifest = ''] = (get(`rev/${name}/${d.rev}`)?.value || '').split('---\n');
      d.image = fields(head).image;
      const m = (k) => manifest.match(new RegExp(`^${k} = "([^"]*)"`, 'm'))?.[1];
      const list = (k) => (manifest.match(new RegExp(`^${k} = \\[([^\\]]*)\\]`, 'm'))?.[1] || '').match(/[a-z0-9:-]+/g) || [];
      d.serves = m('serves');
      d.uses = list('uses');
      d.rolling = fields(get(`ctl/${name}`)?.value).rolling === '1';
      d.pods = pods.filter((p) => p.deploy === name);
      d.ready = d.pods.filter((p) => status[p.name]?.ready === '1' && status[p.name]?.node === p.node).length;
      return d;
    });

    // Functions: instances are each node's own (fnstat/NODE: `NAME N BUSY REQUESTS COLD` lines).
    const fns = prefix('fn/').map(([name, r]) => {
      const m = (k) => r.value.match(new RegExp(`^${k} = "?([^"\\n]*)"?`, 'm'))?.[1];
      return { name, image: m('image'), max: m('max') || '4', idle: m('idle') || '30', expose: m('expose') === 'true' };
    });
    const fnstat = Object.fromEntries(prefix('fnstat/').map(([node, r]) => [node, r.value.split('\n').filter(Boolean).map((l) => {
      const [name, n, busy, requests, cold] = l.split(' ');
      return { name, n: Number(n), busy: Number(busy), requests: Number(requests), cold: Number(cold) };
    })]));
    const out = [];
    const up = nodes.filter((n) => n.up).length;
    out.push(el('p', { class: 'summary' },
      `${up} 個節點上線（共 ${nodes.length}）· leader：${leader || '—'} · 這一頁是 ${this.machine}`));

    out.push(el('h3', {}, '節點與在上面跑的 pod'));
    const grid = el('div', { class: 'nodes' });
    for (const n of nodes) {
      const card = el('div', { class: `node-card${n.up ? '' : ' down'}${n.name === this.machine ? ' mine' : ''}` });
      const badges = el('span', { class: 'badges' });
      badges.append(el('span', { class: `badge ${n.up ? 'good' : 'bad'}` }, n.up ? 'Ready' : 'NotReady'));
      if (n.name === leader && n.up) badges.append(el('span', { class: 'badge accent' }, 'leader'));
      if (n.name === this.machine) badges.append(el('span', { class: 'badge' }, '這一頁'));
      card.append(el('div', { class: 'node-head' }, el('strong', {}, n.name), el('span', { class: 'qnum' }, n.arch || '?'), badges));
      const mine = pods.filter((p) => p.node === n.name);
      const ul = el('ul', { class: 'pod-list' });
      for (const p of mine) {
        const [label, cls] = podState(p, status[p.name]);
        const st = status[p.name];
        const restarts = st && st.restarts !== '0' ? ` · 重啟 ${st.restarts}` : '';
        ul.append(el('li', {}, el('span', { class: `dot ${cls}`, 'aria-hidden': 'true' }), el('span', { class: 'mono' }, p.name),
          el('span', { class: 'qnum' }, ` ${label} · rev ${p.rev}${restarts}`)));
      }
      for (const f of (fnstat[n.name] || []).filter((f) => f.n > 0)) {
        ul.append(el('li', {}, el('span', { class: `dot ${f.busy ? 'warn' : 'good'}`, 'aria-hidden': 'true' }), el('span', { class: 'mono' }, `ƒ ${f.name}`),
          el('span', { class: 'qnum' }, ` 函式 instance ×${f.n}${f.busy ? `（忙 ${f.busy}）` : ''}`)));
      }
      if (!ul.children.length) ul.append(el('li', { class: 'qnum' }, '（沒有 pod）'));
      card.append(ul);
      grid.append(card);
    }
    const lost = pods.filter((p) => p.node === '-');
    if (lost.length) {
      const card = el('div', { class: 'node-card down' }, el('div', { class: 'node-head' }, el('strong', {}, '沒有節點可放'),
        el('span', { class: 'badges' }, el('span', { class: 'badge bad' }, 'Unschedulable'))));
      const ul = el('ul', { class: 'pod-list' });
      for (const p of lost) ul.append(el('li', {}, el('span', { class: 'dot bad', 'aria-hidden': 'true' }), el('span', { class: 'mono' }, p.name)));
      card.append(ul);
      grid.append(card);
    }
    if (!nodes.length) grid.append(el('p', { class: 'hint' }, '還沒有節點。'));
    out.push(grid);
    out.push(el('p', { class: 'hint' }, `再開一頁 ?machine=b2（換個名字），就多一台機器、一個節點；關掉它，${Math.round(NODE_LEASE / 1000)} 秒後它的 pod 會在別的節點重建。`));

    out.push(el('h3', {}, '想要的狀態（Deployment）'));
    if (deploys.length) {
      const t = el('table');
      t.append(el('tr', {}, el('th', {}, '名稱'), el('th', {}, '就緒'), el('th', {}, '版本'), el('th', {}, '映像'), el('th', {}, 'service')));
      for (const d of deploys) {
        const svc = [d.serves ? `提供 ${d.serves}` : '', d.uses.length ? `使用 ${d.uses.join(', ')}` : ''].filter(Boolean).join('；') || '—';
        const rev = el('td', {}, `${d.rev}`);
        if (d.rolling) rev.append(el('span', { class: 'badge warn' }, '更新中'));
        t.append(el('tr', {}, el('td', { class: 'mono' }, d.name), el('td', {}, `${d.ready}/${d.replicas}`), rev,
          el('td', { class: 'mono' }, d.image || '?'), el('td', {}, svc)));
      }
      out.push(t);
    } else {
      out.push(el('p', { class: 'hint' }, '還沒有 Deployment。試試第 10 課：kube apply kube/web.toml'));
    }

    if (fns.length) {
      out.push(el('h3', {}, '函式（Function）'));
      const t = el('table');
      t.append(el('tr', {}, el('th', {}, '名稱'), el('th', {}, '映像'), el('th', {}, 'instance（各節點）'), el('th', {}, '請求'), el('th', {}, '冷啟動'), el('th', {}, '網址')));
      for (const f of fns) {
        const per = Object.entries(fnstat).map(([node, l]) => [node, l.find((x) => x.name === f.name)]).filter(([, x]) => x);
        const where = per.map(([node, x]) => `${node}: ${x.n}`).join('，') || '0（縮到 0）';
        const sum = (k) => per.reduce((a, [, x]) => a + x[k], 0);
        // Exposed ones are on the function port: another origin than this page.
        const url = f.expose && fnPort ? `${location.protocol}//${location.hostname}:${fnPort}/fn/${f.name}/` : null;
        const link = url ? el('a', { href: url, target: '_blank', rel: 'noopener' }, url) : '—';
        t.append(el('tr', {}, el('td', { class: 'mono' }, f.name), el('td', { class: 'mono' }, f.image || '?'), el('td', {}, where),
          el('td', {}, String(sum('requests'))), el('td', {}, String(sum('cold'))), el('td', { class: 'mono' }, link)));
      }
      out.push(t);
      out.push(el('p', { class: 'hint' }, '函式沒有固定的 pod：請求來了才在那台機器上冷啟動，閒置一段時間就收掉。'));
    }

    const events = [];
    for (const [d, r] of prefix('events/')) {
      for (const l of r.value.split('\n').filter(Boolean)) {
        const i = l.indexOf(' ');
        events.push([Number(l.slice(0, i)), d, l.slice(i + 1)]);
      }
    }
    events.sort((a, b) => b[0] - a[0]);
    if (events.length) {
      out.push(el('h3', {}, '最近發生的事'));
      const t = el('table');
      for (const [t0, d, m] of events.slice(0, 12)) {
        t.append(el('tr', {}, el('td', { class: 'qnum' }, ago(now, t0)), el('td', { class: 'mono' }, d), el('td', {}, m)));
      }
      out.push(t);
    }
    this.root.replaceChildren(...out);
  }
}
