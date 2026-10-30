// The browser's kernel Worker.
// In:  {type:'boot', url, machine}, {type:'key', bytes}, {type:'werror', id, msg}, {type:'snapshot'}.
// Out: {type:'out', text}, {type:'exit', code}, {type:'spawn', id, port}, {type:'kill', id},
//      {type:'snapshot', json}.
import { bootKernel } from './kernel_host.js';

let kernel = null;
let nextWorker = 1;
const workerErrors = new Map();

self.onmessage = async (e) => {
  const m = e.data;
  if (m.type === 'boot') {
    try {
      const bytes = await (await fetch(m.url)).arrayBuffer();
      // The page is one machine: its disk on the server outlives reloads.
      const remote = new URL(`v1/remote?machine=${m.machine || 'browser'}`, m.url);
      remote.protocol = remote.protocol === 'https:' ? 'wss:' : 'ws:';
      kernel = await bootKernel(bytes, {
        host: 'browser',
        remoteUrl: remote.href,
        // Task Workers are created by the page (nested Workers are not
        // available everywhere); the kernel talks to each over a MessagePort.
        spawnWorker() {
          const id = nextWorker++;
          const ch = new MessageChannel();
          self.postMessage({ type: 'spawn', id, port: ch.port2 }, [ch.port2]);
          return {
            post: (m) => ch.port1.postMessage(m),
            onMessage: (f) => {
              ch.port1.onmessage = (e) => f(e.data);
              workerErrors.set(id, (msg) => f({ type: 'fault', msg }));
            },
            terminate: () => {
              workerErrors.delete(id);
              ch.port1.close();
              self.postMessage({ type: 'kill', id });
            },
          };
        },
        write: (text) => self.postMessage({ type: 'out', text }),
        exit: (code) => self.postMessage({ type: 'exit', code }),
      });
    } catch (err) {
      self.postMessage({ type: 'out', text: `boot failed: ${err.message}\n` });
      self.postMessage({ type: 'exit', code: 253 });
    }
  } else if (m.type === 'werror') {
    workerErrors.get(m.id)?.(m.msg);
  } else if (m.type === 'key' && kernel) {
    for (const b of m.bytes) kernel.input(b);
  } else if (m.type === 'snapshot' && kernel) {
    const json = kernel.snapshot();
    if (json) self.postMessage({ type: 'snapshot', json });
  }
};
