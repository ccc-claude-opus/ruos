//! What a release carries inside the `ruos` binary (build.rs packs the
//! crate's `assets/` directory, which `./run.sh dist` fills): the web page,
//! the wasm kernel, the QEMU kernels and the image registry. A build
//! without them (from the repository) serves files from disk instead.
use std::collections::BTreeMap;
use std::sync::OnceLock;

static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/assets.bin"));

/// Path (relative to `assets/`, with `/`) → bytes.
pub fn all() -> &'static BTreeMap<&'static str, &'static [u8]> {
    static ALL: OnceLock<BTreeMap<&'static str, &'static [u8]>> = OnceLock::new();
    ALL.get_or_init(unpack)
}

fn unpack() -> BTreeMap<&'static str, &'static [u8]> {
    let mut out = BTreeMap::new();
    let mut b = PACKED;
    fn take(b: &mut &'static [u8], n: usize) -> &'static [u8] {
        let (x, rest) = b.split_at(n);
        *b = rest;
        x
    }
    while !b.is_empty() {
        let n = u32::from_le_bytes(take(&mut b, 4).try_into().unwrap()) as usize;
        let name = std::str::from_utf8(take(&mut b, n)).unwrap_or("");
        let len = u64::from_le_bytes(take(&mut b, 8).try_into().unwrap()) as usize;
        out.insert(name, take(&mut b, len));
    }
    out
}

pub fn get(path: &str) -> Option<&'static [u8]> {
    all().get(path).copied()
}

pub fn present() -> bool {
    !PACKED.is_empty()
}
