// Runs the wasm32 kernel inside a Worker (the browser's kernel Worker, or a
// Node worker thread in the tests). The kernel is event driven: the host
// boots it, then delivers timer ticks and console bytes through `kernel_irq`.
import { wasmMachine } from './dtb.js';
import { ANSWER, memoryLimits } from './task_runtime.js';

export class KernelExit extends Error {
  constructor(code) {
    super(`kernel exit ${code}`);
    this.code = code;
  }
}

const IRQ_TIMER = 0, IRQ_CONSOLE = 1;

/**
 * @param {BufferSource | WebAssembly.Module} wasm  the kernel
 * @param {{ write(text: string): void, exit(code: number): void, host?: string,
 *           spawnWorker(): { post(m: any): void, onMessage(f: (m: any) => void): void, terminate(): void } }} io
 *   `spawnWorker` starts a task thread Worker (task-worker.js or its Node twin).
 *   `remoteUrl` (ws://…/v1/remote): the server (ruos serve), the other end of the kernel's
 *   remote channel (the store keeps the pods' writable layers there).
 *   `dropRemoteAfter` (tests): close the connection once after that many
 *   answers, to exercise reconnecting.
 */
export async function bootKernel(wasm, io) {
  let inst = null, timer = null, done = false;
  const tasks = new Map(); // host task id → { module, memory }
  const threads = new Map(); // thread id → { worker, i32, i64 }
  let nextTask = 1;
  const kmem = () => new Uint8Array(inst.exports.memory.buffer);
  const BLOCKED = -(2n ** 63n);
  let remote = null;
  const dec = new TextDecoder();
  const t0 = performance.now();

  const stop = (code) => {
    if (done) return;
    done = true;
    if (timer) clearInterval(timer);
    for (const t of threads.values()) t.worker.terminate();
    threads.clear();
    remote?.close();
    io.exit(code);
  };
  // Every entry into the kernel goes through here: `shutdown` unwinds as KernelExit.
  const enter = (f) => {
    if (done) return undefined;
    try {
      return f();
    } catch (e) {
      if (e instanceof KernelExit) stop(e.code);
      else {
        io.write(`\nkernel trap: ${e.message}\n`);
        stop(255);
      }
    }
    return undefined;
  };
  const answer = (tid, v) => {
    const t = threads.get(tid);
    if (!t) return;
    t.i64[8] = v;
    Atomics.store(t.i32, 0, ANSWER);
    Atomics.notify(t.i32, 0);
  };
  const fromThread = (tid, m) => {
    const t = threads.get(tid);
    if (!t) return;
    if (m.type === 'sys') {
      const a = t.i64;
      const r = enter(() => inst.exports.kernel_syscall(tid, Number(a[1]), a[2], a[3], a[4], a[5], a[6], a[7]));
      if (r !== undefined && r !== BLOCKED) answer(tid, r);
    } else if (m.type === 'fault') {
      const msg = new TextEncoder().encode(m.msg).slice(0, 4096);
      kmem().set(msg, inst.exports.kernel_scratch());
      enter(() => inst.exports.kernel_fault(tid, msg.length));
    }
  };
  const env = {
    console_write(p, n) {
      io.write(dec.decode(new Uint8Array(inst.exports.memory.buffer, p, n), { stream: true }));
    },
    timer_start(hz) {
      if (timer) clearInterval(timer);
      timer = setInterval(() => enter(() => inst.exports.kernel_irq(IRQ_TIMER, 0)), 1000 / hz);
    },
    now_ms: () => performance.now() - t0,
    shutdown(code) {
      throw new KernelExit(code);
    },
    task_create(ptr, len, maxPages) {
      try {
        const bytes = kmem().slice(ptr, ptr + len);
        const lim = memoryLimits(bytes);
        if (!lim || !lim.shared) return 0;
        const module = new WebAssembly.Module(bytes);
        const maximum = Math.min(lim.max ?? 65536, maxPages >>> 0);
        if (maximum < lim.min) return 0;
        const memory = new WebAssembly.Memory({ initial: lim.min, maximum, shared: true });
        const id = nextTask++;
        tasks.set(id, { module, memory });
        return id;
      } catch (e) {
        io.write(`task_create: ${e.message}\n`);
        return 0;
      }
    },
    task_destroy(id) {
      tasks.delete(id);
    },
    thread_start(task, tid, entry, stack, arg) {
      const t = tasks.get(task);
      const ctl = new SharedArrayBuffer(128);
      const worker = io.spawnWorker();
      threads.set(tid, { worker, i32: new Int32Array(ctl), i64: new BigInt64Array(ctl) });
      worker.onMessage((m) => fromThread(tid, m));
      worker.post({ module: t.module, memory: t.memory, ctl, tid, entry: entry >>> 0, stack: stack >>> 0, arg: arg >>> 0 });
    },
    thread_resume: answer,
    thread_kill(tid) {
      const t = threads.get(tid);
      if (t) {
        t.worker.terminate();
        threads.delete(tid);
      }
    },
    user_read(task, va, len, dst) {
      const m = tasks.get(task)?.memory;
      va >>>= 0;
      if (!m || va + len > m.buffer.byteLength) return -1;
      kmem().set(new Uint8Array(m.buffer, va, len), dst);
      return 0;
    },
    remote_open: () => (io.remoteUrl ? 1 : 0),
    remote_send(p, len) {
      remote.send(kmem().slice(p, p + len));
    },
    user_write(task, va, src, len) {
      const m = tasks.get(task)?.memory;
      va >>>= 0;
      if (!m || va + len > m.buffer.byteLength) return -1;
      new Uint8Array(m.buffer, va, len).set(kmem().subarray(src, src + len));
      return 0;
    },
  };
  const r = await WebAssembly.instantiate(wasm, { env });
  inst = r.instance || r;
  if (io.remoteUrl) {
    remote = remoteLink(io.remoteUrl, io, (b) => {
      kmem().set(b, inst.exports.kernel_remote_buf());
      enter(() => inst.exports.kernel_remote(b.length));
    });
  }
  const mem = inst.exports.memory;
  const dtb = wasmMachine({ memoryBytes: mem.buffer.byteLength, host: io.host || 'browser' });
  new Uint8Array(mem.buffer, inst.exports.kernel_dtb_buf(), dtb.length).set(dtb);
  enter(() => {
    if (inst.exports.kernel_boot(dtb.length) !== 0) {
      io.write('kernel_boot: bad device tree\n');
      stop(254);
    }
  });
  return {
    input(byte) {
      enter(() => inst.exports.kernel_irq(IRQ_CONSOLE, byte));
    },
    // The kernel described as JSON (tasks, handles, quotas): the page's view.
    snapshot() {
      return enter(() => {
        const p = inst.exports.kernel_snapshot();
        return dec.decode(kmem().slice(p, p + inst.exports.kernel_snapshot_len()));
      });
    },
    stop: () => stop(-1),
  };
}

/**
 * The remote channel's WebSocket. The store asks one request at a time and
 * waits for its answer. When the connection drops (a laptop sleeps, the
 * network goes), it connects again to the same session (`session=` in the
 * URL: the server keeps the writable layers under it) and sends the requests
 * not answered yet again; the store just waits a little longer.
 */
function remoteLink(url, io, deliver) {
  const session = Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, '0')).join('');
  const full = `${url}${url.includes('?') ? '&' : '?'}session=${session}`;
  let ws = null, open = false, closed = false, everOpen = false, drops = 0, delay = 250, answers = 0;
  let queue = []; // not sent yet
  const unanswered = []; // sent, waiting for their answers (in order)
  const connect = () => {
    ws = new WebSocket(full);
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      open = true;
      if (everOpen) io.write('remote: reconnected\n');
      everOpen = true;
      delay = 250;
      for (const b of unanswered) ws.send(b);
      for (const b of queue) {
        ws.send(b);
        unanswered.push(b);
      }
      queue = [];
    };
    ws.onmessage = (e) => {
      unanswered.shift();
      deliver(new Uint8Array(e.data));
      answers++;
      if (io.dropRemoteAfter && answers === io.dropRemoteAfter) ws.close();
    };
    ws.onclose = () => {
      if (closed) return;
      open = false;
      io.write(everOpen ? 'remote: connection lost, reconnecting\n' : 'remote: cannot reach the server, retrying\n');
      drops++;
      setTimeout(connect, delay);
      delay = Math.min(delay * 2, 8000);
    };
  };
  connect();
  return {
    send(b) {
      if (open) {
        ws.send(b);
        unanswered.push(b);
      } else queue.push(b);
    },
    close() {
      closed = true;
      ws?.close();
    },
  };
}
