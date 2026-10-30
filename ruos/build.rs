//! Packs `assets/` (if it is there: `./run.sh dist` makes it for a release)
//! into OUT_DIR/assets.bin: for every file, the path's length (u32 LE), the
//! path, the length (u64 LE) and the bytes.
use std::fs;
use std::path::Path;

fn walk(root: &Path, dir: &Path, out: &mut Vec<u8>) {
    let mut es: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
    es.sort_by_key(|e| e.file_name());
    for e in es {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
            continue;
        }
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let data = fs::read(&p).unwrap();
        out.extend((rel.len() as u32).to_le_bytes());
        out.extend(rel.as_bytes());
        out.extend((data.len() as u64).to_le_bytes());
        out.extend(data);
    }
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(&dir, &dir, &mut out);
    }
    fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("assets.bin"), out).unwrap();
}
