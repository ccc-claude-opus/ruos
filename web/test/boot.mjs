// WASM boot test: starts the server (ruos serve), checks the page's headers, fetches the
// kernel over HTTP and boots it in a worker thread (like the browser's kernel
// Worker). Prints the console, exits with the kernel's shutdown code.
//   node web/test/boot.mjs [--input FILE] [--machine NAME] [--drop-remote N] [--server URL]
// FILE (default tests/m3.input) is typed once the shell is up; NAME names
// the machine's disk on the server (and its node in the cluster); N: the
// server connection is closed once after N answers (the kernel host
// reconnects); URL: a server that is already running (machines that share
// it are one cluster), else one is started here, with a small web server
// for curl that it may reach (--http-allow); `@HTTP_PORT@` in FILE is its port. RUOS (the server binary), KERNEL_WASM override the paths.
import { spawn, execFileSync } from 'node:child_process';
import http from 'node:http';
import net from 'node:net';
import { testSite } from './testsite.mjs';
import { Worker } from 'node:worker_threads';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const server = process.env.RUOS || path.join(root, 'target/release/ruos');
const kernel = process.env.KERNEL_WASM || path.join(root, 'build/kernel.wasm');
const store = process.env.STORE || path.join(root, 'build/store');
const podimg = process.env.PODIMG || path.join(root, 'target/release/podimg');
const arg = (name, dflt) => {
  const i = process.argv.indexOf(name);
  return i > 0 ? process.argv[i + 1] : dflt;
};
const input = arg('--input', path.join(root, 'tests/m3.input'));
const machine = arg('--machine', 'node');
const dropRemoteAfter = Number(arg('--drop-remote', 0));
const serverUrl = arg('--server', null);

const fail = (msg) => {
  console.error(`boot.mjs: ${msg}`);
  srv?.kill();
  redis?.kill();
  web.close();
  process.exit(1);
};

// What curl in the machine fetches.
const web = testSite();
const webPort = serverUrl ? 0 : await new Promise((resolve) => web.listen(0, '127.0.0.1', () => resolve(web.address().port)));
if (serverUrl) web.close();
// A Redis for the backend's redis:cache, if one is installed (tests/redis.expect).
const freePort = () => new Promise((resolve) => {
  const s = net.createServer().listen(0, '127.0.0.1', () => {
    const { port } = s.address();
    s.close(() => resolve(port));
  });
});
let redis = null, redisArgs = [];
if (!serverUrl) {
  const port = await freePort();
  redis = spawn('redis-server', ['--port', String(port), '--save', '', '--appendonly', 'no'], { stdio: 'ignore' });
  redis.on('error', () => (redis = null));
  await new Promise((r) => setTimeout(r, 300));
  if (redis) redisArgs = ['--redis', `cache=redis://127.0.0.1:${port}/`];
}
const srv = serverUrl
  ? null
  : spawn(server, ['serve', '--port', '0', '--web', path.join(root, 'web'), '--kernel', kernel, '--store', store,
      '--http-allow', `127.0.0.1:${webPort}`, ...redisArgs], {
      stdio: ['ignore', 'pipe', 'inherit'],
    });
let fnBase = null;
const base = serverUrl ?? await new Promise((resolve, reject) => {
  let buf = '';
  srv.stdout.on('data', (d) => {
    buf += d;
    const f = buf.match(/functions on (http:\/\/\S+)\/fn\//);
    if (f) fnBase = f[1];
    const m = buf.match(/listening on (http:\/\/\S+)/);
    if (m && f) resolve(m[1]);
  });
  srv.on('exit', (c) => reject(new Error(`the server exited (${c})`)));
}).catch((e) => fail(e.message));

const page = await fetch(base + '/');
if (page.status !== 200) fail(`GET / → ${page.status}`);
if (page.headers.get('cross-origin-opener-policy') !== 'same-origin') fail('missing COOP header');
if (page.headers.get('cross-origin-embedder-policy') !== 'require-corp') fail('missing COEP header');
const res = await fetch(base + '/kernel.wasm');
if (res.status !== 200) fail(`GET /kernel.wasm → ${res.status}`);
if (res.headers.get('content-type') !== 'application/wasm') fail('kernel.wasm content type');
const wasm = new Uint8Array(await res.arrayBuffer());
console.log(`server: ${base} (COOP/COEP ok, kernel.wasm ${wasm.length} bytes)`);

// The registry: refs, immutable content-addressed blobs, and a full check
// of an image (every object's BLAKE3) by podimg.
const ref = await fetch(base + '/v1/refs/hello/latest');
const index = (await ref.text()).trim();
if (ref.status !== 200 || !/^[0-9a-f]{64}$/.test(index)) fail('registry ref');
console.log(`registry: GET /v1/refs/hello/latest → ${index}`);
const blob = await fetch(`${base}/v1/blobs/${index}`);
const cache = blob.headers.get('cache-control');
if (blob.status !== 200 || !cache.includes('immutable')) fail('registry blob');
const idOf = (bytes) => execFileSync(podimg, ['hash', '-'], { input: bytes }).toString().trim();
if (idOf(Buffer.from(await blob.arrayBuffer())) !== index) fail('blob does not match its id');
console.log(`registry: blobs are immutable (Cache-Control: ${cache})`);
if ((await fetch(`${base}/v1/blobs/${'0'.repeat(64)}`)).status !== 404) fail('missing blob');
process.stdout.write(execFileSync(podimg, ['verify', base, 'hello']).toString());

const remoteUrl = base.replace(/^http/, 'ws') + `/v1/remote?machine=${machine}`;
// The machine's own storage (the browser's IndexedDB): build/local/MACHINE.
const localDir = path.join(root, 'build/local', machine);
const worker = new Worker(path.join(here, 'node_worker.mjs'), { workerData: { wasm, remoteUrl, dropRemoteAfter, localDir } });
let out = '';
let typed = false;
let snapshotLine = 'snapshot: none';
let ingressTested = false;
let appsTested = false;
const timeout = setTimeout(() => fail('timeout'), Number(process.env.BOOT_TIMEOUT_MS ?? 600000));
worker.on('message', (m) => {
  if (m.type === 'out') {
    process.stdout.write(m.text);
    out += m.text;
    // Type the shell script once the shell is up, like the QEMU tests.
    if (!typed && /^shell: ready/m.test(out)) {
      typed = true;
      worker.postMessage({ type: 'snapshot' });
      const script = fs.readFileSync(input, 'utf8').replaceAll('@HTTP_PORT@', String(webPort));
      worker.postMessage({ type: 'key', bytes: [...Buffer.from(script)] });
    }
    // The script says when its exposed Functions are there: requests from outside.
    if (!ingressTested && fnBase && /^ingress-test: go$/m.test(out)) {
      ingressTested = true;
      ingressTest().catch((e) => console.log(`ingress: failed: ${e.message}`));
    }
    if (!appsTested && fnBase && /^apps-test: go$/m.test(out)) {
      appsTested = true;
      appsTest().catch((e) => console.log(`apps: failed: ${e.message}`));
    }
  } else if (m.type === 'snapshot') {
    // The page's system view: the shell's channels lead to console, store and dock.
    const s = JSON.parse(m.json);
    const live = s.tasks.filter((t) => t.exit === null);
    const shell = live.find((t) => t.name === 'shell');
    const owner = (id) => live.find((t) => t.handles.some((h) => h.id === id))?.name;
    const peers = shell.handles.filter((h) => h.type === 'channel').map((h) => owner(h.peer)).sort();
    // Printed after the run (the console is busy now).
    snapshotLine = `snapshot: ${live.length} tasks, ${s.quotas.length} quotas; the shell talks to ${peers.join(', ')}`;
  } else if (m.type === 'exit') {
    clearTimeout(timeout);
    worker.terminate();
    console.log(snapshotLine);
    afterBoot().then(
      () => {
        srv?.kill();
        redis?.kill();
        web.close();
        process.exitCode = m.code;
      },
      (e) => fail(e.message),
    );
  }
});
worker.on('error', (e) => fail(e.stack));

// Requests from outside to the function port (doc/ingress.md), printed as
// `ingress: …` lines for tests/m23.expect.
async function ingressTest() {
  const say = (s) => console.log(`ingress: ${s}`);
  let r = await fetch(`${fnBase}/fn/site/api/hello?name=ruos`);
  say(`hello ${r.status} ${(await r.text()).trim()} node=${r.headers.get('x-ruos-node')}`);
  for (let i = 0; i < 2; i++) {
    r = await fetch(`${fnBase}/fn/site/api/hit`, { method: 'POST' });
    say(`hit ${r.status} ${r.headers.get('content-type')} ${(await r.text()).trim()}`);
  }
  r = await fetch(`${fnBase}/fn/site/`);
  const page = await r.text();
  say(`page ${r.status} ${r.headers.get('content-type')} ${page.includes('<h1>') ? 'has h1' : 'no h1'}`);
  r = await fetch(`${fnBase}/fn/hello/`);
  say(`not exposed ${r.status}`);
  r = await fetch(`${fnBase}/fn/nosuch/`);
  say(`no such ${r.status}`);
  r = await fetch(`${fnBase}/`);
  say(`list ${r.status} ${(await r.text()).includes('/fn/site/') ? 'lists site' : 'no site'}`);
  r = await fetch(`${fnBase}/fn/site/api/sleep?s=8`);
  say(`slow ${r.status}`);
  // Only the OS's own pages may use the server's machine endpoints.
  const raw = (pathname, headers) =>
    new Promise((resolve) => {
      const u = new URL(base);
      http.get({ host: u.hostname, port: u.port, path: pathname, headers }, (res) => {
        res.resume();
        resolve(res.statusCode);
      }).on('error', () => resolve('error'));
    });
  say(`ingress from another origin ${await raw('/v1/ingress?machine=x&session=y', { Origin: 'http://evil.example' })}`);
  say(`ingress from the function port ${await raw('/v1/ingress?machine=x&session=y', { 'Sec-Fetch-Site': 'same-site' })}`);
  say(`websocket from another origin ${await raw('/v1/remote?machine=x', {
    Origin: 'http://evil.example', Connection: 'Upgrade', Upgrade: 'websocket',
    'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==',
  })}`);
  say('done');
}

// The example web sites (apps/webapps), from outside like a browser, as
// `apps: …` lines for tests/m26.expect.
async function appsTest() {
  const say = (s) => console.log(`apps: ${s}`);
  const call = async (method, p, body) => {
    const r = await fetch(`${fnBase}/fn/${p}`, {
      method, redirect: 'manual',
      headers: { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const t = await r.text();
    let j = null;
    try { j = JSON.parse(t); } catch {}
    return { status: r.status, j, t, r };
  };
  let r = await call('GET', 'guestbook/');
  say(`guestbook page ${r.status} ${r.t.includes('<h1>留言板</h1>') ? 'has title' : 'no title'}`);
  r = await call('POST', 'guestbook/api/notes', { name: 'ann', text: 'hi <b>there</b>' });
  say(`guestbook post ${r.status} id=${r.j?.id}`);
  await call('POST', 'guestbook/api/notes', { name: 'bob', text: '你好' });
  r = await call('POST', 'guestbook/api/notes', { name: '', text: 'x' });
  say(`guestbook bad ${r.status} ${r.j?.error}`);
  r = await call('GET', 'guestbook/api/notes');
  say(`guestbook notes ${r.j.map((n) => `${n.name}:${n.text}`).join(' | ')}`);
  // The same program with its database in this machine's browser (sql:guestbook@local).
  r = await call('POST', 'lguest/api/notes', { name: 'cy', text: 'kept in the browser' });
  say(`lguest post ${r.status} id=${r.j?.id}`);
  r = await call('GET', 'lguest/api/notes');
  say(`lguest notes ${r.j.map((n) => `${n.name}:${n.text}`).join(' | ')}`);

  await call('POST', 'todo/api/todos', { title: 'read lesson 17' });
  await call('POST', 'todo/api/todos', { title: 'write a site' });
  await call('POST', 'todo/api/todos', { title: 'nothing' });
  r = await call('POST', 'todo/api/todos/1', { done: true });
  say(`todo done ${r.status} changed=${r.j?.changed}`);
  await call('DELETE', 'todo/api/todos/3');
  r = await call('GET', 'todo/api/todos');
  say(`todo list ${r.j.map((t) => `${t.title}=${t.done}`).join(', ')}`);

  await call('PUT', `wiki/api/page?name=${encodeURIComponent('首頁')}`, { body: '# hi\nsee [[ruos]]' });
  await call('PUT', `wiki/api/page?name=${encodeURIComponent('首頁')}`, { body: '# hello\nsee [[ruos]]' });
  await call('PUT', 'wiki/api/page?name=ruos', { body: 'a teaching OS' });
  r = await call('GET', `wiki/api/page?name=${encodeURIComponent('首頁')}`);
  say(`wiki page ${r.status} revisions=${r.j?.revisions} ${JSON.stringify(r.j?.body)}`);
  r = await call('GET', 'wiki/api/page?name=nope');
  say(`wiki missing ${r.status}`);
  r = await call('GET', 'wiki/api/pages');
  say(`wiki pages ${r.j.map((p) => p.name).sort().join(' ')}`);

  r = await call('POST', 'short/api/links', { url: 'https://example.com/a?b=1' });
  const code = r.j?.code;
  say(`short made ${r.status} ${/^[a-zA-Z0-9]{6}$/.test(code) ? 'a 6-letter code' : code}`);
  r = await call('POST', 'short/api/links', { url: 'https://example.com/a?b=1' });
  say(`short again ${r.j?.code === code ? 'same code' : 'another code'}`);
  r = await call('GET', `short/s/${code}`);
  say(`short go ${r.status} ${r.r.headers.get('location')}`);
  r = await call('POST', 'short/api/links', { url: 'javascript:alert(1)' });
  say(`short bad ${r.status}`);
  r = await call('GET', 'short/s/zzzzzz');
  say(`short none ${r.status}`);

  r = await call('GET', 'weather/api/cities');
  say(`weather cities ${r.j.length}`);
  for (let i = 0; i < 2; i++) {
    r = await call('GET', 'weather/api/weather?city=kinmen');
    say(`weather ${r.status} ${r.j?.city} ${r.j?.temperature} ${r.j?.wind} cached=${r.j?.cached}`);
  }
  r = await call('GET', 'weather/api/weather?city=atlantis');
  say(`weather none ${r.status}`);
  say('done');
}

// What the machine left on the server: the pods' writable layers, and the
// image `dock push` sent to the registry.
async function afterBoot() {
  const pods = (await (await fetch(base + '/v1/pods')).text()).trim().split('\n').filter(Boolean);
  console.log(`server: writable layers ${pods.join(' ')}`);
  for (const p of pods) {
    const r = await fetch(`${base}/v1/pods/${p}/files/data/count`);
    if (r.status === 200) console.log(`server: ${p}/files/data/count = ${(await r.text()).trim()}`);
  }
  process.stdout.write(execFileSync(podimg, ['verify', base, 'snap']).toString());
}
