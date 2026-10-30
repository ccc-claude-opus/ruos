// Flattened device tree builder: the wasm "machine" describes itself to the
// kernel the same way QEMU does, so the kernel's DTB code is shared.
//
// A node is { props: { name: value }, children: { name: node } }; a value is
// a string, a number (one u32 cell), an array of numbers (cells) or bytes.

const BEGIN_NODE = 1, END_NODE = 2, PROP = 3, END = 9;

export function buildDtb(root) {
  const struct = [];
  const strings = [];
  const strOff = new Map();
  const enc = new TextEncoder();

  const u32 = (v) => struct.push((v >>> 24) & 255, (v >>> 16) & 255, (v >>> 8) & 255, v & 255);
  const pad = () => { while (struct.length % 4) struct.push(0); };
  const nameOff = (name) => {
    if (!strOff.has(name)) {
      strOff.set(name, strings.length);
      strings.push(...enc.encode(name), 0);
    }
    return strOff.get(name);
  };
  const bytesOf = (v) => {
    if (typeof v === 'string') return [...enc.encode(v), 0];
    if (typeof v === 'number') v = [v];
    if (Array.isArray(v)) {
      return v.flatMap((c) => [(c >>> 24) & 255, (c >>> 16) & 255, (c >>> 8) & 255, c & 255]);
    }
    return [...v];
  };
  const node = (name, n) => {
    u32(BEGIN_NODE);
    struct.push(...enc.encode(name), 0);
    pad();
    for (const [k, v] of Object.entries(n.props || {})) {
      const b = bytesOf(v);
      u32(PROP);
      u32(b.length);
      u32(nameOff(k));
      struct.push(...b);
      pad();
    }
    for (const [k, c] of Object.entries(n.children || {})) node(k, c);
    u32(END_NODE);
  };
  node('', root);
  u32(END);

  const HEADER = 40, RSV = 16;
  const offStruct = HEADER + RSV;
  const offStrings = offStruct + struct.length;
  const total = offStrings + strings.length;
  const out = new Uint8Array(total);
  const dv = new DataView(out.buffer);
  [0xd00dfeed, total, offStruct, offStrings, HEADER, 17, 16, 0, strings.length, struct.length]
    .forEach((v, i) => dv.setUint32(i * 4, v));
  out.set(struct, offStruct);
  out.set(strings, offStrings);
  return out;
}

/// The wasm machine: linear memory, a microsecond clock, the console, the
/// host's time (an "RTC" read once at boot) and entropy (`rng-seed`, as QEMU
/// gives it).
export function wasmMachine({ memoryBytes, host }) {
  const hi = Math.floor(memoryBytes / 2 ** 32), lo = memoryBytes >>> 0;
  const now = BigInt(Date.now()) * 1000000n;
  const seed = crypto.getRandomValues(new Uint8Array(32));
  return buildDtb({
    props: {
      '#address-cells': 2,
      '#size-cells': 2,
      compatible: 'ruos,wasm',
      model: `ruos wasm32 (${host})`,
    },
    children: {
      chosen: { props: { 'stdout-path': '/console', 'rng-seed': seed } },
      rtc: {
        props: { compatible: 'ruos,wasm-rtc', 'time-ns': [Number(now >> 32n), Number(now & 0xffffffffn)] },
      },
      cpus: {
        props: { '#address-cells': 1, '#size-cells': 0, 'timebase-frequency': 1000000 },
        children: { 'cpu@0': { props: { device_type: 'cpu', reg: 0, compatible: 'wasm32' } } },
      },
      'memory@0': { props: { device_type: 'memory', reg: [0, 0, hi, lo] } },
      console: { props: { compatible: 'ruos,wasm-console', reg: [0, 0, 0, 0] } },
    },
  });
}
