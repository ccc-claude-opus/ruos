//! Objects the registry lists but does not carry. A release leaves its
//! biggest images (sqlited, sqlite3) out of the crate: their refs are in
//! the registry, their objects' ids in `STORE/upstream` (one per line), and
//! the objects themselves on the release page. The first time a machine
//! asks for one, it is fetched from `UPSTREAM/ID`, checked against its id
//! (BLAKE3) and kept in `STORE/objects`.
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::id::Id;

/// Where objects come from: this version's release page.
pub const DEFAULT: &str = concat!("https://github.com/ccc-claude-opus/ruos/releases/download/v", env!("CARGO_PKG_VERSION"));
/// The biggest object fetched (the images are a few MiB).
const MAX: u64 = 64 << 20;

static BASE: OnceLock<Option<String>> = OnceLock::new();

/// `None`: never fetch.
pub fn configure(base: Option<String>) {
    let _ = BASE.set(base.map(|b| b.trim_end_matches('/').to_string()));
}

/// Object `id`'s file in `store`, fetched first if it is one of the
/// upstream's; None if there is no such object.
pub fn object(store: &Path, id: &Id) -> Option<PathBuf> {
    let p = store.join("objects").join(id.to_string());
    if p.exists() {
        return Some(p);
    }
    let base = BASE.get()?.as_ref()?;
    let listed = fs::read_to_string(store.join("upstream")).ok()?;
    if !listed.lines().any(|l| l.trim() == id.to_string()) {
        return None;
    }
    match fetch(base, id) {
        Ok(data) => {
            // Another request may be fetching it too: each writes its own
            // file, then renames it into place.
            let tmp = store.join("objects").join(format!(".up-{id}-{:?}", std::thread::current().id()));
            fs::create_dir_all(store.join("objects")).and_then(|_| fs::write(&tmp, &data)).and_then(|_| fs::rename(&tmp, &p)).ok()?;
            println!("ruos: fetched {} ({} KiB) from {base}", id, data.len() / 1024);
            Some(p)
        }
        Err(e) => {
            eprintln!("ruos: cannot fetch object {id} from {base}: {e}");
            None
        }
    }
}

fn fetch(base: &str, id: &Id) -> Result<Vec<u8>, String> {
    let r = ureq::get(&format!("{base}/{id}")).call().map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    r.into_body().into_reader().take(MAX + 1).read_to_end(&mut data).map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX {
        return Err(String::from("too big"));
    }
    if Id::of(&data) != *id {
        return Err(String::from("its bytes do not hash to its id"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// A web server that answers every request with `body`, for `n` requests.
    fn serve(body: Vec<u8>, n: usize) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming().take(n) {
                let mut s = s.unwrap();
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn fetches_listed_objects_and_checks_them() {
        let store = std::env::temp_dir().join(format!("ruos-upstream-{}", std::process::id()));
        let _ = fs::remove_dir_all(&store);
        fs::create_dir_all(&store).unwrap();
        let good = b"an object".to_vec();
        let (id, other) = (Id::of(&good), Id::of(b"not listed"));
        fs::write(store.join("upstream"), format!("{id}\n")).unwrap();
        configure(Some(serve(good.clone(), 1)));
        // Not listed: never asked for.
        assert_eq!(object(&store, &other), None);
        let p = object(&store, &id).unwrap();
        assert_eq!(fs::read(&p).unwrap(), good);
        // Kept: no second request (the server answers only one).
        assert_eq!(object(&store, &id), Some(p));
        let _ = fs::remove_dir_all(&store);
    }

    #[test]
    fn rejects_wrong_bytes() {
        let base = serve(b"something else".to_vec(), 1);
        assert!(fetch(&base, &Id::of(b"an object")).unwrap_err().contains("hash"));
    }
}
