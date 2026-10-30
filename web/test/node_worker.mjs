// Node stand-in for kernel-worker.js: same messages, on worker_threads.
import { Worker, parentPort, workerData } from 'node:worker_threads';
import { bootKernel } from '../kernel_host.js';

const kernel = await bootKernel(workerData.wasm, {
  host: `node ${process.version}`,
  remoteUrl: workerData.remoteUrl,
  dropRemoteAfter: workerData.dropRemoteAfter,
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
