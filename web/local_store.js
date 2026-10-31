// This machine's own storage (doc/storage.md): keys → bytes, kept by the
// browser (IndexedDB; the Node tests use files). The kernel's remote
// channel 2 leads here; the `local` service builds files, KV namespaces,
// caches and SQLite databases on it.
//
// Requests (libsys::wire): [GET, key] → [OK, bytes] / [NOT_FOUND];
// [PUT, key, bytes] → [OK]; [DELETE, key] → [OK];
// [LIST, prefix, after] → [OK, keys after `after`, one a line] (a page of
// them: ask again after the last one until the answer is empty).
const GET = 1, PUT = 2, DELETE = 3, LIST = 4;
const OK = 0, NOT_FOUND = 1, FAILED = 2;
const PAGE = 60000;

/** Keys → bytes in IndexedDB (database `ruos-local-NAME`). */
export function idbStore(name) {
  const db = new Promise((resolve, reject) => {
    const r = indexedDB.open(`ruos-local-${name}`, 1);
    r.onupgradeneeded = () => r.result.createObjectStore('kv');
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
  const run = (mode, f) =>
    db.then((d) => new Promise((resolve, reject) => {
      const t = d.transaction('kv', mode);
      const r = f(t.objectStore('kv'));
      t.oncomplete = () => resolve(r.result);
      t.onerror = () => reject(t.error);
    }));
  return {
    get: (k) => run('readonly', (s) => s.get(k)).then((v) => v ?? null),
    put: (k, v) => run('readwrite', (s) => s.put(v, k)),
    del: (k) => run('readwrite', (s) => s.delete(k)),
    keys: (prefix) => run('readonly', (s) => s.getAllKeys(IDBKeyRange.bound(prefix, prefix + '￿'))),
  };
}

/**
 * Answers the local service's requests from `store` ({get, put, del,
 * keys}), one at a time and in order: `reply(bytes)` for each.
 */
export function localLink(store, reply) {
  const enc = new TextEncoder(), dec = new TextDecoder();
  let queue = Promise.resolve();
  const answer = (st, body = new Uint8Array()) => {
    const b = new Uint8Array(1 + body.length);
    b[0] = st;
    b.set(body, 1);
    reply(b);
  };
  // (op, fields): NUL between fields, the last one raw.
  const fields = (b, n) => {
    const f = [];
    let at = 1;
    for (let i = 1; i < n; i++) {
      const z = b.indexOf(0, at);
      if (z < 0) return null;
      f.push(b.subarray(at, z));
      at = z + 1;
    }
    f.push(b.subarray(at));
    return f;
  };
  async function handle(b) {
    const op = b[0];
    const n = { [GET]: 1, [PUT]: 2, [DELETE]: 1, [LIST]: 2 }[op];
    const f = n && fields(b, n);
    if (!f) return answer(FAILED, enc.encode('bad request'));
    const key = dec.decode(f[0]);
    switch (op) {
      case GET: {
        const v = await store.get(key);
        return v ? answer(OK, new Uint8Array(v)) : answer(NOT_FOUND);
      }
      case PUT:
        await store.put(key, f[1].slice());
        return answer(OK);
      case DELETE:
        await store.del(key);
        return answer(OK);
      case LIST: {
        const after = dec.decode(f[1]);
        const keys = (await store.keys(key)).filter((k) => k.startsWith(key) && k > after).sort();
        const page = [];
        let size = 0;
        for (const k of keys) {
          size += enc.encode(k).length + 1;
          if (size > PAGE) break;
          page.push(k);
        }
        return answer(OK, enc.encode(page.join('\n')));
      }
    }
  }
  return {
    send(b) {
      queue = queue.then(() => handle(b)).catch((e) => answer(FAILED, enc.encode(String(e?.message ?? e))));
    },
  };
}
