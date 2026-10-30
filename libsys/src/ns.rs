//! Namespaces: what a program can reach, by name. A parent starts a child
//! with one handle, a bootstrap channel whose first message is
//! `ruos ns 1` + one name per line, carrying the matching handles.
//! `ros_start` reads it before `main`; `get("console")` finds a handle.
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

use crate::{Channel, Handle, Memory, ObjType, Result};

const MAGIC: &str = "ruos ns 1\n";
const MAX: usize = abi::MAX_MSG_HANDLES;

struct Table(UnsafeCell<Vec<(String, Handle)>>);

unsafe impl Sync for Table {}

static NAMES: Table = Table(UnsafeCell::new(Vec::new()));

/// Called once by `ros_start`, before `main` (and before other threads).
pub(crate) fn load() {
    let b = Handle(0);
    if !matches!(b.info(), Ok((ObjType::Channel, _))) {
        return;
    }
    let mut buf = [0u8; 1024];
    let mut hs = [Handle(0); MAX];
    let Ok((n, nh)) = Channel(b).read(&mut buf, &mut hs) else { return };
    let Some(text) = core::str::from_utf8(&buf[..n]).ok().and_then(|t| t.strip_prefix(MAGIC)) else { return };
    let names = unsafe { &mut *NAMES.0.get() };
    for (name, h) in text.lines().zip(&hs[..nh]) {
        names.push((String::from(name), *h));
    }
    let _ = b.close();
}

/// The handle this program was given as `name`.
pub fn get(name: &str) -> Option<Handle> {
    unsafe { &*NAMES.0.get() }.iter().find(|(n, _)| n == name).map(|(_, h)| *h)
}

/// Every name in the namespace.
pub fn names() -> impl Iterator<Item = &'static str> {
    unsafe { &*NAMES.0.get() }.iter().map(|(n, _)| n.as_str())
}

/// A namespace being put together for a child.
#[derive(Default, Clone)]
pub struct Namespace {
    entries: Vec<(String, Handle)>,
}

impl Namespace {
    pub fn new() -> Namespace {
        Namespace::default()
    }

    /// Adds `h` (which moves to the child) as `name`.
    pub fn add(mut self, name: &str, h: Handle) -> Namespace {
        self.entries.push((String::from(name), h));
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bootstrap channel's other end, the message already in it.
    fn bootstrap(&self) -> Result<Channel> {
        let (mine, theirs) = Channel::create()?;
        let mut text = String::from(MAGIC);
        for (n, _) in &self.entries {
            text += n;
            text.push('\n');
        }
        let hs: Vec<Handle> = self.entries.iter().map(|(_, h)| *h).collect();
        let r = mine.write(text.as_bytes(), &hs);
        let _ = mine.close();
        r.map(|_| theirs)
    }

    /// Starts `image` as task `name` in this namespace, charged to `quota`
    /// (None: the caller's).
    pub fn spawn(&self, image: Memory, len: u64, name: &str, quota: Option<Handle>) -> Result<Handle> {
        let b = self.bootstrap()?;
        let r = crate::spawn_with(image, len, name, &[b.0], quota);
        if r.is_err() {
            let _ = b.close();
        }
        r
    }
}
