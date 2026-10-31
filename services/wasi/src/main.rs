//! wasi: runs a WASI preview1 program (`wasm32-wasip1`: Rust, C from
//! wasi-sdk, …) on ruos. This program is the task; the WASI program (the
//! guest, `wasi:module:LEN` in the namespace) is instantiated next to it
//! in the same Worker by the host (web/task_runtime.js), and its
//! `wasi_snapshot_preview1` imports are this program's `wasi_*` exports.
//! The guest's memory is its own: the host copies to and from it
//! (`wasi_host.read` / `write`).
//!
//! What the guest gets is what this task was given (capabilities):
//! - fd 0 / 1 / 2: `console` (stdin a line at a time);
//! - fd 3 `.`: the directory in `PWD` (else `/`) of `fs`, the only files
//!   it can open (without `fs`, no files at all). wasi-libc starts its
//!   own working directory at `/` and makes every path absolute from it,
//!   so with a `/` preopen too relative paths would all start at `/`: with
//!   only `.`, relative paths are what they should be, and `/x` is `x`
//!   under that directory (like `wasmtime --dir .`): a program sees the
//!   directory it was started in, and nothing above it;
//! - arguments and environment: `exec:`; clocks, random: system calls.
//! There are no sockets in preview1; anything not here is ENOSYS.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libsys::console::Console;
use libsys::fs::Fs;
use libsys::{Channel, Error, Memory, args, ns};

libsys::entry!(main);

#[link(wasm_import_module = "wasi_host")]
unsafe extern "C" {
    /// Instantiates and runs the guest (its module is at ptr, len); its exit code.
    #[link_name = "run"]
    fn host_run(ptr: *const u8, len: usize) -> i32;
    /// Guest memory → ours, ours → guest memory.
    #[link_name = "read"]
    fn host_read(guest: u32, len: u32, ours: *mut u8) -> i32;
    #[link_name = "write"]
    fn host_write(ours: *const u8, len: u32, guest: u32) -> i32;
    /// Ends the guest (unwinds it) with `code`.
    #[link_name = "exit"]
    fn host_exit(code: i32) -> !;
    /// The last trap's message, into buf; its length.
    #[link_name = "error"]
    fn host_error(buf: *mut u8, cap: usize) -> usize;
}

// errno
const SUCCESS: i32 = 0;
const EBADF: i32 = 8;
const EEXIST: i32 = 20;
const EFAULT: i32 = 21;
const EINVAL: i32 = 28;
const EIO: i32 = 29;
const EISDIR: i32 = 31;
const ENOENT: i32 = 44;
const ENOSYS: i32 = 52;
const ENOTDIR: i32 = 54;
const ENOTEMPTY: i32 = 55;
const ESPIPE: i32 = 70;

// file types
const FT_CHAR: u8 = 2;
const FT_DIR: u8 = 3;
const FT_FILE: u8 = 4;

// oflags, fdflags, whence
const O_CREAT: u16 = 1;
const O_DIRECTORY: u16 = 2;
const O_EXCL: u16 = 4;
const O_TRUNC: u16 = 8;
const FD_APPEND: u16 = 1;

enum Kind {
    Stdin,
    Stdout,
    File { path: String, pos: u64, append: bool },
    Dir { path: String },
}

struct State {
    console: Option<Console>,
    fs: Option<Fs>,
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
    fds: Vec<Option<Kind>>,
    /// Names of the preopened directories (fd 3).
    preopens: Vec<(u32, String)>,
    /// The rest of the last stdin line; whether stdin has ended.
    stdin: Vec<u8>,
    eof: bool,
}

static mut STATE: Option<State> = None;

#[allow(static_mut_refs)]
fn st() -> &'static mut State {
    unsafe { STATE.as_mut().unwrap() }
}

fn err(e: Error) -> i32 {
    match e {
        Error::NotFound => ENOENT,
        Error::Exists => EEXIST,
        Error::NotEmpty => ENOTEMPTY,
        Error::NotDir => ENOTDIR,
        Error::IsDir => EISDIR,
        Error::InvalidArgs => EINVAL,
        _ => EIO,
    }
}

// The guest's memory.
fn gread(ptr: u32, len: u32) -> Vec<u8> {
    let mut v = alloc::vec![0u8; len as usize];
    unsafe { host_read(ptr, len, v.as_mut_ptr()) };
    v
}

fn gwrite(ptr: u32, b: &[u8]) -> bool {
    unsafe { host_write(b.as_ptr(), b.len() as u32, ptr) == 0 }
}

fn put_u32(ptr: u32, v: u32) -> bool {
    gwrite(ptr, &v.to_le_bytes())
}

fn put_u64(ptr: u32, v: u64) -> bool {
    gwrite(ptr, &v.to_le_bytes())
}

fn gstr(ptr: u32, len: u32) -> String {
    String::from_utf8_lossy(&gread(ptr, len)).into_owned()
}

/// `base` + `p`, without `.`, `..` (never above `/`).
fn join(base: &str, p: &str) -> String {
    let mut parts: Vec<&str> = if p.starts_with('/') { Vec::new() } else { base.split('/').filter(|x| !x.is_empty()).collect() };
    for x in p.split('/') {
        match x {
            "" | "." => {}
            ".." => _ = parts.pop(),
            x => parts.push(x),
        }
    }
    let mut s = String::from("/");
    s += &parts.join("/");
    s
}

fn fd_mut(fd: u32) -> Option<&'static mut Kind> {
    st().fds.get_mut(fd as usize).and_then(Option::as_mut)
}

fn dir_of(fd: u32) -> Result<String, i32> {
    match fd_mut(fd) {
        Some(Kind::Dir { path }) => Ok(path.clone()),
        Some(_) => Err(ENOTDIR),
        None => Err(EBADF),
    }
}

fn fs() -> Result<Fs, i32> {
    st().fs.ok_or(ENOENT)
}

/// (iovec base, len) pairs.
fn iovecs(iovs: u32, n: u32) -> Vec<(u32, u32)> {
    let b = gread(iovs, n * 8);
    b.chunks(8).map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]), u32::from_le_bytes([c[4], c[5], c[6], c[7]]))).collect()
}

fn strings_sizes(v: &[Vec<u8>], count: u32, size: u32) -> i32 {
    let total: usize = v.iter().map(|s| s.len() + 1).sum();
    if put_u32(count, v.len() as u32) && put_u32(size, total as u32) { SUCCESS } else { EFAULT }
}

fn strings_get(v: &[Vec<u8>], ptrs: u32, buf: u32) -> i32 {
    let mut at = buf;
    for (i, s) in v.iter().enumerate() {
        put_u32(ptrs + 4 * i as u32, at);
        let mut z = s.clone();
        z.push(0);
        gwrite(at, &z);
        at += z.len() as u32;
    }
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_args_sizes_get(count: u32, size: u32) -> i32 {
    strings_sizes(&st().args, count, size)
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_args_get(ptrs: u32, buf: u32) -> i32 {
    strings_get(&st().args, ptrs, buf)
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_environ_sizes_get(count: u32, size: u32) -> i32 {
    strings_sizes(&st().env, count, size)
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_environ_get(ptrs: u32, buf: u32) -> i32 {
    strings_get(&st().env, ptrs, buf)
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_clock_time_get(id: u32, _precision: u64, out: u32) -> i32 {
    let t = if id == 0 { libsys::realtime_ns() } else { libsys::clock_ns() };
    if put_u64(out, t) { SUCCESS } else { EFAULT }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_clock_res_get(_id: u32, out: u32) -> i32 {
    if put_u64(out, 1000) { SUCCESS } else { EFAULT }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_random_get(buf: u32, len: u32) -> i32 {
    let mut v = alloc::vec![0u8; len as usize];
    libsys::random(&mut v);
    if gwrite(buf, &v) { SUCCESS } else { EFAULT }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_proc_exit(code: i32) -> ! {
    unsafe { host_exit(code) }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_sched_yield() -> i32 {
    libsys::yield_now();
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_write(fd: u32, iovs: u32, n: u32, written: u32) -> i32 {
    let mut data = Vec::new();
    for (b, l) in iovecs(iovs, n) {
        data.extend_from_slice(&gread(b, l));
    }
    let r = match fd_mut(fd) {
        Some(Kind::Stdout) => {
            match st().console {
                Some(c) => {
                    let _ = c.write(&data);
                }
                None => libsys::print!("{}", String::from_utf8_lossy(&data)),
            }
            Ok(())
        }
        Some(Kind::File { path, pos, append }) => {
            let fs = st().fs.unwrap();
            let at = if *append { fs.stat(path).map(|(_, s)| s).unwrap_or(0) } else { *pos };
            let r = data.chunks(libsys::fs::CHUNK).enumerate().try_for_each(|(i, c)| fs.write_at(path, at + (i * libsys::fs::CHUNK) as u64, c));
            if r.is_ok() {
                *pos = at + data.len() as u64;
            }
            r.map_err(err)
        }
        Some(Kind::Stdin) => Err(EBADF),
        Some(Kind::Dir { .. }) => Err(EISDIR),
        None => Err(EBADF),
    };
    match r {
        Ok(()) => {
            put_u32(written, data.len() as u32);
            SUCCESS
        }
        Err(e) => e,
    }
}

fn read_into(fd: u32, want: usize) -> Result<Vec<u8>, i32> {
    match fd_mut(fd) {
        Some(Kind::Stdin) => {
            let s = st();
            if s.stdin.is_empty() && !s.eof {
                let Some(c) = s.console else { return Ok(Vec::new()) };
                // Lines from a served console may be long (a whole SQL request).
                let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES - 1];
                match c.read_line(&mut buf) {
                    Ok(n) => {
                        s.stdin.extend_from_slice(&buf[..n]);
                        s.stdin.push(b'\n');
                    }
                    // The end of a pipe.
                    Err(Error::PeerClosed) => s.eof = true,
                    Err(e) => return Err(err(e)),
                }
            }
            let n = want.min(s.stdin.len());
            Ok(s.stdin.drain(..n).collect())
        }
        Some(Kind::File { path, pos, .. }) => {
            let fs = st().fs.unwrap();
            let b = fs.read_at(path, *pos, want.min(libsys::fs::CHUNK)).map_err(err)?;
            *pos += b.len() as u64;
            Ok(b)
        }
        Some(Kind::Dir { .. }) => Err(EISDIR),
        Some(Kind::Stdout) | None => Err(EBADF),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_read(fd: u32, iovs: u32, n: u32, read: u32) -> i32 {
    let mut total = 0u32;
    for (b, l) in iovecs(iovs, n) {
        if l == 0 {
            continue;
        }
        match read_into(fd, l as usize) {
            Ok(d) => {
                gwrite(b, &d);
                total += d.len() as u32;
                // A short read ends it (stdin gives a line at a time).
                if (d.len() as u32) < l {
                    break;
                }
            }
            Err(e) if total == 0 => return e,
            Err(_) => break,
        }
    }
    put_u32(read, total);
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_pread(fd: u32, iovs: u32, n: u32, off: u64, read: u32) -> i32 {
    let saved = match fd_mut(fd) {
        Some(Kind::File { pos, .. }) => core::mem::replace(pos, off),
        Some(_) => return ESPIPE,
        None => return EBADF,
    };
    let r = wasi_fd_read(fd, iovs, n, read);
    if let Some(Kind::File { pos, .. }) = fd_mut(fd) {
        *pos = saved;
    }
    r
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_pwrite(fd: u32, iovs: u32, n: u32, off: u64, written: u32) -> i32 {
    let saved = match fd_mut(fd) {
        Some(Kind::File { pos, append, .. }) if !*append => core::mem::replace(pos, off),
        Some(_) => return ESPIPE,
        None => return EBADF,
    };
    let r = wasi_fd_write(fd, iovs, n, written);
    if let Some(Kind::File { pos, .. }) = fd_mut(fd) {
        *pos = saved;
    }
    r
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_seek(fd: u32, off: i64, whence: u32, out: u32) -> i32 {
    let Some(k) = fd_mut(fd) else { return EBADF };
    let Kind::File { path, pos, .. } = k else { return ESPIPE };
    let base = match whence {
        0 => 0,
        1 => *pos as i64,
        2 => st().fs.and_then(|f| f.stat(path).ok()).map(|(_, s)| s as i64).unwrap_or(0),
        _ => return EINVAL,
    };
    let n = base + off;
    if n < 0 {
        return EINVAL;
    }
    *pos = n as u64;
    put_u64(out, *pos);
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_tell(fd: u32, out: u32) -> i32 {
    match fd_mut(fd) {
        Some(Kind::File { pos, .. }) => {
            put_u64(out, *pos);
            SUCCESS
        }
        Some(_) => ESPIPE,
        None => EBADF,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_close(fd: u32) -> i32 {
    match st().fds.get_mut(fd as usize) {
        Some(slot @ Some(_)) if fd > 3 => {
            *slot = None;
            SUCCESS
        }
        Some(Some(_)) => SUCCESS,
        _ => EBADF,
    }
}

fn file_type(fd: u32) -> Option<u8> {
    Some(match fd_mut(fd)? {
        Kind::Stdin | Kind::Stdout => FT_CHAR,
        Kind::File { .. } => FT_FILE,
        Kind::Dir { .. } => FT_DIR,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_fdstat_get(fd: u32, out: u32) -> i32 {
    let Some(t) = file_type(fd) else { return EBADF };
    let mut b = [0u8; 24];
    b[0] = t;
    // All rights: ruos gave (or did not give) the capability already.
    b[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    b[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    if gwrite(out, &b) { SUCCESS } else { EFAULT }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_fdstat_set_flags(fd: u32, flags: u32) -> i32 {
    match fd_mut(fd) {
        Some(Kind::File { append, .. }) => {
            *append = flags as u16 & FD_APPEND != 0;
            SUCCESS
        }
        Some(_) => SUCCESS,
        None => EBADF,
    }
}

/// A filestat: dev, ino, filetype, nlink, size, atim, mtim, ctim.
fn filestat(out: u32, ft: u8, size: u64) -> i32 {
    let mut b = [0u8; 64];
    b[16] = ft;
    b[24..32].copy_from_slice(&1u64.to_le_bytes());
    b[32..40].copy_from_slice(&size.to_le_bytes());
    if gwrite(out, &b) { SUCCESS } else { EFAULT }
}

fn stat_path(path: &str) -> Result<(u8, u64), i32> {
    let (dir, size) = fs()?.stat(path).map_err(err)?;
    Ok((if dir { FT_DIR } else { FT_FILE }, size))
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_filestat_get(fd: u32, out: u32) -> i32 {
    match fd_mut(fd) {
        Some(Kind::File { path, .. }) | Some(Kind::Dir { path }) => {
            let p = path.clone();
            match stat_path(&p) {
                Ok((t, s)) => filestat(out, t, s),
                Err(e) => e,
            }
        }
        Some(_) => filestat(out, FT_CHAR, 0),
        None => EBADF,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_filestat_set_size(fd: u32, size: u64) -> i32 {
    match fd_mut(fd) {
        Some(Kind::File { path, .. }) => match st().fs.unwrap().truncate(path, size) {
            Ok(()) => SUCCESS,
            Err(e) => err(e),
        },
        Some(_) => EINVAL,
        None => EBADF,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_prestat_get(fd: u32, out: u32) -> i32 {
    let Some((_, name)) = st().preopens.iter().find(|(f, _)| *f == fd) else { return EBADF };
    let mut b = [0u8; 8];
    b[4..8].copy_from_slice(&(name.len() as u32).to_le_bytes());
    if gwrite(out, &b) { SUCCESS } else { EFAULT }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_prestat_dir_name(fd: u32, buf: u32, len: u32) -> i32 {
    let Some((_, name)) = st().preopens.iter().find(|(f, _)| *f == fd) else { return EBADF };
    let n = name.len().min(len as usize);
    if gwrite(buf, &name.as_bytes()[..n]) { SUCCESS } else { EFAULT }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_readdir(fd: u32, buf: u32, len: u32, cookie: u64, used: u32) -> i32 {
    let path = match dir_of(fd) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let list = match fs().and_then(|f| f.list(&path).map_err(err)) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let mut entries: Vec<(String, u8)> = alloc::vec![(String::from("."), FT_DIR), (String::from(".."), FT_DIR)];
    entries.extend(list.into_iter().map(|e| (e.name, if e.dir { FT_DIR } else { FT_FILE })));
    let mut out = Vec::new();
    for (i, (name, t)) in entries.iter().enumerate().skip(cookie as usize) {
        let mut d = [0u8; 24];
        d[0..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
        d[8..16].copy_from_slice(&(i as u64 + 1).to_le_bytes());
        d[16..20].copy_from_slice(&(name.len() as u32).to_le_bytes());
        d[20] = *t;
        out.extend_from_slice(&d);
        out.extend_from_slice(name.as_bytes());
        if out.len() >= len as usize {
            break;
        }
    }
    out.truncate(len as usize);
    gwrite(buf, &out);
    put_u32(used, out.len() as u32);
    SUCCESS
}

fn new_fd(k: Kind) -> u32 {
    let fds = &mut st().fds;
    match fds.iter().skip(4).position(Option::is_none) {
        Some(i) => {
            fds[i + 4] = Some(k);
            (i + 4) as u32
        }
        None => {
            fds.push(Some(k));
            fds.len() as u32 - 1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_open(
    dirfd: u32,
    _dirflags: u32,
    path: u32,
    path_len: u32,
    oflags: u32,
    _rights: u64,
    _inheriting: u64,
    fdflags: u32,
    out: u32,
) -> i32 {
    let r = (|| -> Result<u32, i32> {
        let base = dir_of(dirfd)?;
        let p = join(&base, &gstr(path, path_len));
        let fs = fs()?;
        let (oflags, fdflags) = (oflags as u16, fdflags as u16);
        let found = fs.stat(&p);
        match found {
            Ok(_) if oflags & O_CREAT != 0 && oflags & O_EXCL != 0 => return Err(EEXIST),
            Ok((true, _)) => return Ok(new_fd(Kind::Dir { path: p })),
            Ok((false, _)) if oflags & O_DIRECTORY != 0 => return Err(ENOTDIR),
            Ok((false, _)) => {
                if oflags & O_TRUNC != 0 {
                    fs.truncate(&p, 0).map_err(err)?;
                }
            }
            Err(Error::NotFound) if oflags & O_CREAT != 0 => fs.write(&p, &[]).map_err(err)?,
            Err(e) => return Err(err(e)),
        }
        Ok(new_fd(Kind::File { path: p, pos: 0, append: fdflags & FD_APPEND != 0 }))
    })();
    match r {
        Ok(fd) => {
            put_u32(out, fd);
            SUCCESS
        }
        Err(e) => e,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_filestat_get(dirfd: u32, _flags: u32, path: u32, len: u32, out: u32) -> i32 {
    match dir_of(dirfd).and_then(|b| stat_path(&join(&b, &gstr(path, len)))) {
        Ok((t, s)) => filestat(out, t, s),
        Err(e) => e,
    }
}

fn path_op(dirfd: u32, path: u32, len: u32, f: impl FnOnce(&Fs, &str) -> libsys::Result<()>) -> i32 {
    match dir_of(dirfd).and_then(|b| {
        let p = join(&b, &gstr(path, len));
        f(&fs()?, &p).map_err(err)
    }) {
        Ok(()) => SUCCESS,
        Err(e) => e,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_create_directory(dirfd: u32, path: u32, len: u32) -> i32 {
    path_op(dirfd, path, len, |fs, p| fs.mkdir(p))
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_remove_directory(dirfd: u32, path: u32, len: u32) -> i32 {
    path_op(dirfd, path, len, |fs, p| fs.rmdir(p))
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_unlink_file(dirfd: u32, path: u32, len: u32) -> i32 {
    path_op(dirfd, path, len, |fs, p| fs.unlink(p))
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_rename(fd: u32, old: u32, old_len: u32, fd2: u32, new: u32, new_len: u32) -> i32 {
    let r = (|| -> Result<(), i32> {
        let from = join(&dir_of(fd)?, &gstr(old, old_len));
        let to = join(&dir_of(fd2)?, &gstr(new, new_len));
        fs()?.rename(&from, &to).map_err(err)
    })();
    match r {
        Ok(()) => SUCCESS,
        Err(e) => e,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_sync(_fd: u32) -> i32 {
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_datasync(_fd: u32) -> i32 {
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_advise(_fd: u32, _off: u64, _len: u64, _advice: u32) -> i32 {
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_fd_filestat_set_times(_fd: u32, _a: u64, _m: u64, _f: u32) -> i32 {
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_filestat_set_times(_fd: u32, _fl: u32, _p: u32, _l: u32, _a: u64, _m: u64, _f: u32) -> i32 {
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_path_readlink(_fd: u32, _p: u32, _l: u32, _b: u32, _bl: u32, _used: u32) -> i32 {
    EINVAL
}

/// Clock subscriptions sleep; anything else is ready at once.
#[unsafe(no_mangle)]
pub extern "C" fn wasi_poll_oneoff(subs: u32, events: u32, n: u32, nevents: u32) -> i32 {
    let mut sleep = u64::MAX;
    let mut out = Vec::new();
    for i in 0..n {
        let s = gread(subs + i * 48, 48);
        let tag = s[8];
        if tag == 0 {
            // clock: id u32 @16, timeout u64 @24, precision @32, flags u16 @40 (1 = absolute)
            let timeout = u64::from_le_bytes(s[24..32].try_into().unwrap());
            let absolute = u16::from_le_bytes([s[40], s[41]]) & 1 != 0;
            let rel = if absolute { timeout.saturating_sub(libsys::realtime_ns()) } else { timeout };
            sleep = sleep.min(rel);
        }
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(&s[0..8]);
        e[10] = tag;
        out.push(e);
    }
    if sleep != u64::MAX && sleep > 0 {
        libsys::sleep_ns(sleep);
    }
    for (i, e) in out.iter().enumerate() {
        gwrite(events + 32 * i as u32, e);
    }
    put_u32(nevents, out.len() as u32);
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn wasi_nosys() -> i32 {
    ENOSYS
}

fn main(_arg: usize) -> i64 {
    let module = ns::names().find_map(|n| Some((ns::get(n)?, n.strip_prefix("wasi:module:")?.parse::<usize>().ok()?)));
    let Some((h, len)) = module else {
        libsys::println!("wasi: no module (run a WASI program from the shell, or with dock)");
        return 1;
    };
    let mut bytes = alloc::vec![0u8; len];
    if Memory(h).read(0, &mut bytes).is_err() {
        return 2;
    }
    let (argv, env) = args::load();
    let pwd = env.iter().find_map(|e| e.strip_prefix(b"PWD=")).map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_else(|| String::from("/"));
    let fs = ns::get("fs").map(|h| Fs(Channel(h)));
    let mut fds: Vec<Option<Kind>> = alloc::vec![Some(Kind::Stdin), Some(Kind::Stdout), Some(Kind::Stdout)];
    let mut preopens = Vec::new();
    if fs.is_some() {
        fds.push(Some(Kind::Dir { path: join("/", &pwd) }));
        preopens.push((3, String::from(".")));
    } else {
        fds.push(None);
    }
    unsafe {
        STATE = Some(State { console: ns::get("console").map(|h| Console(Channel(h))), fs, args: argv, env, fds, preopens, stdin: Vec::new(), eof: false });
    }
    let code = unsafe { host_run(bytes.as_ptr(), bytes.len()) };
    if code == i32::MIN {
        let mut b = alloc::vec![0u8; 512];
        let n = unsafe { host_error(b.as_mut_ptr(), b.len()) };
        let msg = String::from_utf8_lossy(&b[..n]).to_string();
        match st().console {
            Some(c) => libsys::cprintln!(c, "wasi: {msg}"),
            None => libsys::println!("wasi: {msg}"),
        }
        return 134;
    }
    code as i64
}
