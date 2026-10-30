use alloc::vec::Vec;

use super::*;

extern crate std;

#[test]
fn blake3_known() {
    assert_eq!(Id::of(b"").to_string(), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
    assert_eq!(Id::of(b"abc").to_string(), "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85");
}

fn sample() -> (Store, Id) {
    let mut s = Store::default();
    let prog = s.put(b"\x7fELF program".to_vec());
    let motd = s.put(b"hello\n".to_vec());
    let bin = s.put(Tree { entries: alloc::vec![Entry { name: "hello".into(), kind: Kind::Exec, id: prog, size: 12 }] }.encode());
    let etc = s.put(Tree { entries: alloc::vec![Entry { name: "motd".into(), kind: Kind::File, id: motd, size: 6 }] }.encode());
    let root = s.put(
        Tree {
            entries: alloc::vec![
                Entry { name: "etc".into(), kind: Kind::Dir, id: etc, size: 0 },
                Entry { name: "bin".into(), kind: Kind::Dir, id: bin, size: 0 },
            ],
        }
        .encode(),
    );
    // An upper layer hides /etc/motd.
    let up_etc = s.put(Tree { entries: alloc::vec![Entry { name: "motd".into(), kind: Kind::Whiteout, id: Id([0; 32]), size: 0 }] }.encode());
    let upper = s.put(Tree { entries: alloc::vec![Entry { name: "etc".into(), kind: Kind::Dir, id: up_etc, size: 0 }] }.encode());
    let m = Manifest { arch: "riscv64".into(), entry: "/bin/hello".into(), layers: alloc::vec![root, upper], env: alloc::vec![] };
    let mid = s.put(m.encode());
    let idx = s.put(Index { name: "hello".into(), manifests: alloc::vec![("riscv64".into(), mid)] }.encode());
    s.refs.insert("hello:latest".into(), idx);
    (s, idx)
}

#[test]
fn formats_round_trip() {
    let (s, idx) = sample();
    let x = Index::decode(s.need(&idx).unwrap()).unwrap();
    let m = Manifest::decode(s.need(&x.for_arch("riscv64").unwrap()).unwrap()).unwrap();
    assert_eq!(Manifest::decode(&m.encode()).unwrap(), m);
    assert_eq!(m.entry, "/bin/hello");
    let t = Tree::decode(s.need(&m.layers[0]).unwrap()).unwrap();
    assert_eq!(t.entries.len(), 2);
    // Encoding is canonical: sorted by name.
    assert!(t.encode().starts_with(b"dir"));
    assert_eq!(Tree::decode(&t.encode()).unwrap().encode(), t.encode());
}

#[test]
fn lookup_through_layers() {
    let (s, _) = sample();
    let m = s.manifest("hello", "riscv64").unwrap();
    assert_eq!(s.lookup(&m.layers, "/bin/hello").unwrap().kind, Kind::Exec);
    assert_eq!(s.lookup(&m.layers, "/etc/motd"), Err(Error::NotFound));
    assert_eq!(s.lookup(&m.layers[..1], "/etc/motd").unwrap().size, 6);
    assert_eq!(s.manifest("hello", "aarch64").err(), Some(Error::NotFound));
}

#[test]
fn pack_round_trip() {
    let (s, idx) = sample();
    let bytes = s.pack();
    let p = Pack::parse(&bytes, true).unwrap();
    assert_eq!(p.objects.len(), s.objects.len());
    assert_eq!(p.refs.get("hello:latest"), Some(&idx));
    assert_eq!(p.program("hello", "riscv64").unwrap(), b"\x7fELF program");
    // A flipped byte is caught.
    let mut bad = bytes.clone();
    let at = bad.windows(7).position(|w| w == b"program").unwrap();
    bad[at] ^= 1;
    assert!(matches!(Pack::parse(&bad, true), Err(Error::BadHash(_))));
    assert_eq!(s.closure(&idx, Some("riscv64")).unwrap().len(), 9);
}
