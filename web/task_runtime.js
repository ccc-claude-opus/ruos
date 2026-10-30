// One thread of a task: an instance of the task's module on its shared
// memory, inside its own Worker. System calls block in Atomics.wait on the
// thread's control block until the kernel Worker answers.
//
// Control block (SharedArrayBuffer): i32[0] = state (0 idle, 1 request,
// 2 answer); i64[1] = number, i64[2..7] = arguments, i64[8] = result.
export const IDLE = 0, REQUEST = 1, ANSWER = 2;
export const MAIN_ENTRY = 0xffffffff;

export function runThread(msg, post) {
  const { module, memory, ctl, entry, stack, arg } = msg;
  const i32 = new Int32Array(ctl);
  const i64 = new BigInt64Array(ctl);
  const sys = {
    call(nr, a0, a1, a2, a3, a4, a5) {
      i64[1] = BigInt(nr);
      i64[2] = a0; i64[3] = a1; i64[4] = a2; i64[5] = a3; i64[6] = a4; i64[7] = a5;
      Atomics.store(i32, 0, REQUEST);
      post({ type: 'sys' });
      while (Atomics.load(i32, 0) === REQUEST) Atomics.wait(i32, 0, REQUEST);
      const r = i64[8];
      Atomics.store(i32, 0, IDLE);
      return r;
    },
  };
  try {
    const inst = new WebAssembly.Instance(module, { env: { memory }, sys });
    if (entry === MAIN_ENTRY) {
      inst.exports.ros_start(arg);
    } else {
      inst.exports.__stack_pointer.value = stack;
      inst.exports.ros_thread_start(entry, arg);
    }
    post({ type: 'fault', msg: 'returned from its entry point' });
  } catch (e) {
    post({ type: 'fault', msg: String((e && e.message) || e) });
  }
}

/// The limits a module declares for its imported memory (in 64 KB pages).
export function memoryLimits(bytes) {
  let p = 8;
  const leb = () => {
    let v = 0, s = 0, b;
    do {
      b = bytes[p++];
      v += (b & 0x7f) * 2 ** s;
      s += 7;
    } while (b & 0x80);
    return v;
  };
  const name = () => { const n = leb(); p += n; };
  const limits = () => {
    const flags = bytes[p++];
    const min = leb();
    const max = flags & 1 ? leb() : undefined;
    return { min, max, shared: !!(flags & 2) };
  };
  while (p < bytes.length) {
    const id = bytes[p++];
    const size = leb();
    const end = p + size;
    if (id === 2) {
      for (let n = leb(); n > 0; n--) {
        name(); name();
        const kind = bytes[p++];
        if (kind === 0) leb();
        else if (kind === 1) { p++; limits(); }
        else if (kind === 2) return limits();
        else if (kind === 3) p += 2;
      }
      return null;
    }
    p = end;
  }
  return null;
}
