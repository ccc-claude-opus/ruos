//! podimg: builds ruos images on the host.
//!
//!   podimg build PODFILE --store DIR --programs ARCH=DIR [--programs ARCH=DIR ...]
//!   podimg pack --store DIR --arch ARCH --out FILE [--exclude NAME,...]
//!   podimg ls --store DIR
//!   podimg verify URL NAME[:TAG]        check an image in a registry (podsrv)
//!   podimg hash FILE
//!
//! A store directory holds `objects/<id>` and `refs/<name>/<tag>` (the id
//! of the image index). podsrv serves the same layout as a registry.
use objstore::podfile;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use objstore::{Entry, Id, Index, Kind, Manifest, Objects, Store, Tree};

pub const ARCHES: [&str; 3] = ["riscv64", "aarch64", "wasm32"];

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("podimg: {msg}");
    std::process::exit(1)
}

/// Reads a store directory.
pub fn load_store(dir: &Path) -> Store {
    let mut s = Store::default();
    if let Ok(rd) = fs::read_dir(dir.join("objects")) {
        for e in rd.flatten() {
            if let Some(id) = e.file_name().to_str().and_then(Id::parse) {
                s.objects.insert(id, fs::read(e.path()).unwrap_or_else(|e| die(e)));
            }
        }
    }
    if let Ok(rd) = fs::read_dir(dir.join("refs")) {
        for name in rd.flatten() {
            for tag in fs::read_dir(name.path()).into_iter().flatten().flatten() {
                let id = fs::read_to_string(tag.path()).ok().and_then(|x| Id::parse(x.trim()));
                if let Some(id) = id {
                    s.refs.insert(format!("{}:{}", name.file_name().to_string_lossy(), tag.file_name().to_string_lossy()), id);
                }
            }
        }
    }
    s
}

pub fn save_store(s: &Store, dir: &Path) {
    let od = dir.join("objects");
    fs::create_dir_all(&od).unwrap_or_else(|e| die(e));
    for (id, data) in &s.objects {
        let p = od.join(id.to_string());
        if !p.exists() {
            fs::write(&p, data).unwrap_or_else(|e| die(e));
        }
    }
    for (r, id) in &s.refs {
        let (name, tag) = r.split_once(':').unwrap();
        let d = dir.join("refs").join(name);
        fs::create_dir_all(&d).unwrap_or_else(|e| die(e));
        fs::write(d.join(tag), format!("{id}\n")).unwrap_or_else(|e| die(e));
    }
}

/// A directory being built: path components → file or subdirectory.
#[derive(Default)]
struct Dir {
    files: BTreeMap<String, (Kind, Vec<u8>)>,
    dirs: BTreeMap<String, Dir>,
}

impl Dir {
    fn add(&mut self, path: &str, kind: Kind, data: Vec<u8>) {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let (last, dirs) = parts.split_last().unwrap_or_else(|| die(format!("bad path {path}")));
        let mut d = self;
        for p in dirs {
            d = d.dirs.entry(p.to_string()).or_default();
        }
        d.files.insert(last.to_string(), (kind, data));
    }

    /// Stores the tree (and everything below it); returns its id.
    fn store(&self, s: &mut Store) -> Id {
        let mut t = Tree::default();
        for (name, d) in &self.dirs {
            t.entries.push(Entry { name: name.clone(), kind: Kind::Dir, id: d.store(s), size: 0 });
        }
        for (name, (kind, data)) in &self.files {
            let size = data.len() as u64;
            t.entries.push(Entry { name: name.clone(), kind: *kind, id: s.put(data.clone()), size });
        }
        s.put(t.encode())
    }
}

fn program_file(dir: &Path, prog: &str, arch: &str) -> PathBuf {
    if arch == "wasm32" { dir.join(format!("{prog}.wasm")) } else { dir.join(prog) }
}

fn build(podfile: &Path, store_dir: &Path, programs: &[(String, PathBuf)]) {
    let src = fs::read_to_string(podfile).unwrap_or_else(|e| die(format!("{}: {e}", podfile.display())));
    let pf = podfile::parse(&src).unwrap_or_else(|e| die(e));
    let base = podfile.parent().unwrap_or(Path::new("."));
    let mut s = load_store(store_dir);
    let mut index = Index { name: pf.name.clone(), manifests: vec![] };
    for (arch, dir) in programs {
        if pf.arches.as_ref().is_some_and(|a| !a.contains(arch)) {
            continue;
        }
        let mut root = Dir::default();
        let mut ok = true;
        for (path, prog) in &pf.programs {
            match fs::read(program_file(dir, prog, arch)) {
                Ok(b) => root.add(path, Kind::Exec, b),
                Err(_) => ok = false,
            }
        }
        if !ok {
            continue;
        }
        for (path, file) in &pf.files {
            let b = fs::read(base.join(file)).unwrap_or_else(|e| die(format!("{file}: {e}")));
            root.add(path, Kind::File, b);
        }
        let mut layers = vec![];
        let mut entry = pf.entry.clone();
        let mut env: Vec<(String, String)> = vec![];
        if let Some(from) = &pf.from {
            let m = s.manifest(from, arch).unwrap_or_else(|e| die(format!("from {from} ({arch}): {e:?}")));
            layers = m.layers;
            if entry.is_empty() {
                entry = m.entry;
            }
            env = m.env;
        }
        layers.push(root.store(&mut s));
        for (k, v) in &pf.env {
            env.retain(|(a, _)| a != k);
            env.push((k.clone(), v.clone()));
        }
        let m = Manifest { arch: arch.clone(), entry, layers, env };
        index.manifests.push((arch.clone(), s.put(m.encode())));
    }
    if index.manifests.is_empty() {
        die(format!("{}: no architecture has all programs", pf.name));
    }
    let id = s.put(index.encode());
    s.refs.insert(format!("{}:{}", pf.name, pf.tag), id);
    save_store(&s, store_dir);
    let arches: Vec<&str> = index.manifests.iter().map(|(a, _)| a.as_str()).collect();
    println!("{}:{} {} ({})", pf.name, pf.tag, id.short(), arches.join(", "));
}

/// The objects and refs of every image but `exclude` (machines pull those
/// from the registry when they need them).
fn pack(store_dir: &Path, arch: &str, out: &Path, exclude: &[String]) {
    let mut s = load_store(store_dir);
    s.refs.retain(|r, _| !exclude.iter().any(|x| r.split(':').next() == Some(x.as_str())));
    let mut keep = std::collections::BTreeSet::new();
    for id in s.refs.values() {
        keep.extend(s.closure(id, Some(arch)).unwrap_or_else(|e| die(format!("{e:?}"))));
    }
    let bytes = s.pack_some(|id| keep.contains(id));
    fs::write(out, &bytes).unwrap_or_else(|e| die(e));
    println!("pack {}: {} objects, {} refs, {} bytes", arch, keep.len(), s.refs.len(), bytes.len());
}

fn ls(store_dir: &Path) {
    let s = load_store(store_dir);
    for (r, id) in &s.refs {
        let arches = s
            .need(id)
            .ok()
            .and_then(|b| Index::decode(b).ok())
            .map(|x| x.manifests.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        println!("{r:<20} {} {arches}", id.short());
    }
}

/// GET (plain HTTP, `ureq`): status and body.
pub fn http_get(url: &str) -> Result<(u16, Vec<u8>), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let status = r.status().as_u16();
    let body = r.body_mut().with_config().limit(1 << 30).read_to_vec().map_err(|e| e.to_string())?;
    Ok((status, body))
}

fn verify(url: &str, name: &str) {
    let r = objstore::full_ref(name);
    let (n, t) = r.split_once(':').unwrap();
    let fetch = |id: &Id| -> Vec<u8> {
        let (st, b) = http_get(&format!("{url}/v1/blobs/{id}")).unwrap_or_else(|e| die(e));
        if st != 200 {
            die(format!("blob {id}: HTTP {st}"));
        }
        if Id::of(&b) != *id {
            die(format!("blob {id}: content does not match its id"));
        }
        b
    };
    let (st, body) = http_get(&format!("{url}/v1/refs/{n}/{t}")).unwrap_or_else(|e| die(e));
    if st != 200 {
        die(format!("{r}: HTTP {st}"));
    }
    let id = Id::parse(String::from_utf8_lossy(&body).trim()).unwrap_or_else(|| die("bad ref"));
    let mut store = Store::default();
    store.objects.insert(id, fetch(&id));
    let index = Index::decode(store.need(&id).unwrap()).unwrap_or_else(|e| die(format!("{e:?}")));
    let mut count = 1;
    for (arch, m) in &index.manifests {
        let mb = fetch(m);
        let man = Manifest::decode(&mb).unwrap_or_else(|e| die(format!("{e:?}")));
        store.objects.insert(*m, mb);
        let mut todo: Vec<Id> = man.layers.clone();
        while let Some(t) = todo.pop() {
            let tb = fetch(&t);
            for e in Tree::decode(&tb).unwrap_or_else(|e| die(format!("{e:?}"))).entries {
                match e.kind {
                    Kind::Dir => todo.push(e.id),
                    Kind::File | Kind::Exec => {
                        fetch(&e.id);
                        count += 1;
                    }
                    Kind::Whiteout => {}
                }
            }
            store.objects.insert(t, tb);
            count += 1;
        }
        store.lookup(&man.layers, &man.entry).unwrap_or_else(|e| die(format!("{arch}: entry: {e:?}")));
        count += 1;
    }
    let arches: Vec<&str> = index.manifests.iter().map(|(a, _)| a.as_str()).collect();
    println!("verified {r} {}: {count} objects, {}", id.short(), arches.join(", "));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| -> Option<String> { args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned()) };
    let need = |name: &str| flag(name).unwrap_or_else(|| die(format!("{name} is required")));
    match args.first().map(|s| s.as_str()) {
        Some("build") => {
            let pf = args.get(1).unwrap_or_else(|| die("build PODFILE"));
            let mut progs = vec![];
            for (i, a) in args.iter().enumerate() {
                if a == "--programs" {
                    let v = args.get(i + 1).unwrap_or_else(|| die("--programs ARCH=DIR"));
                    let (arch, dir) = v.split_once('=').unwrap_or_else(|| die("--programs ARCH=DIR"));
                    if !ARCHES.contains(&arch) {
                        die(format!("unknown architecture {arch}"));
                    }
                    progs.push((arch.to_string(), PathBuf::from(dir)));
                }
            }
            build(Path::new(pf), Path::new(&need("--store")), &progs);
        }
        Some("pack") => {
            let exclude: Vec<String> = flag("--exclude").map(|x| x.split(',').map(String::from).collect()).unwrap_or_default();
            pack(Path::new(&need("--store")), &need("--arch"), Path::new(&need("--out")), &exclude)
        }
        Some("ls") => ls(Path::new(&need("--store"))),
        Some("verify") => verify(
            args.get(1).unwrap_or_else(|| die("verify URL NAME")).trim_end_matches('/'),
            args.get(2).unwrap_or_else(|| die("verify URL NAME")),
        ),
        Some("hash") => {
            let f = args.get(1).unwrap_or_else(|| die("hash FILE"));
            let b = if f == "-" {
                let mut v = vec![];
                std::io::stdin().read_to_end(&mut v).unwrap_or_else(|e| die(e));
                v
            } else {
                fs::read(f).unwrap_or_else(|e| die(e))
            };
            println!("{}", Id::of(&b));
        }
        _ => {
            eprintln!("usage: podimg build PODFILE --store DIR --programs ARCH=DIR ...");
            eprintln!("       podimg pack --store DIR --arch ARCH --out FILE [--exclude NAME,...]");
            eprintln!("       podimg ls --store DIR");
            eprintln!("       podimg verify URL NAME[:TAG]");
            eprintln!("       podimg hash FILE|-");
            std::process::exit(2);
        }
    }
}
