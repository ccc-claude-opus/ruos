// Node stand-in for kernel-worker.js: same messages, on worker_threads.
import { Worker, parentPort, workerData } from 'node:worker_threads';
import fs from 'node:fs';
import path from 'node:path';
import { bootKernel } from '../kernel_host.js';

// The machine's own storage (the browser's IndexedDB): a file per key, its
// name the key in hex, in workerData.localDir.
function fileStore(dir) {
  fs.mkdirSync(dir, { recursive: true });
  const file = (k) => path.join(dir, Buffer.from(k).toString('hex'));
  return {
    get: async (k) => (fs.existsSync(file(k)) ? new Uint8Array(fs.readFileSync(file(k))) : null),
    put: async (k, v) => fs.writeFileSync(file(k), v),
    del: async (k) => fs.rmSync(file(k), { force: true }),
    keys: async (prefix) => fs.readdirSync(dir).map((n) => Buffer.from(n, 'hex').toString()).filter((k) => k.startsWith(prefix)),
  };
}

const kernel = await bootKernel(workerData.wasm, {
  host: `node ${process.version}`,
  remoteUrl: workerData.remoteUrl,
  dropRemoteAfter: workerData.dropRemoteAfter,
  localStore: workerData.localDir ? fileStore(workerData.localDir) : undefined,
  spawnWorker() {
    const w = new Worker(new URL('./node_task_worker.mjs', import.meta.url));
    return {
      post: (m) => w.postMessage(m),
      onMessage: (f) => w.on('message', f),
      terminate: () => w.terminate(),
    };
  },
  write: (text) => parentPort.postMessage({ type: 'out', text }),
  exit: (code) => {
    parentPort.postMessage({ type: 'exit', code });
    parentPort.close();
  },
});
parentPort.on('message', (m) => {
  if (m.type === 'key') for (const b of m.bytes) kernel.input(b);
  if (m.type === 'snapshot') parentPort.postMessage({ type: 'snapshot', json: kernel.snapshot() });
});
