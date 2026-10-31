//! System call dispatch, shared by all architectures. A call either returns
//! a value or blocks the thread; whoever unblocks it supplies the result
//! (`sched::wake`).
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::{Error, MAX_MSG_BYTES, MAX_MSG_HANDLES, Packet, Rights, sys};

use crate::channel::{Channel, Msg};
use crate::memory::Memory;
use crate::object::{Handle, Object};
use crate::port::Port;
use crate::sched;
use crate::signal::Observer;
use crate::task::{self, Start, Thread, Waiter};

pub enum Sys {
    Ret(i64),
    Block,
}

fn ok(v: Result<i64, Error>) -> Sys {
    Sys::Ret(match v {
        Ok(v) => v,
        Err(e) => e.code(),
    })
}

pub fn dispatch(t: &Arc<Thread>, nr: usize, a: [u64; 6]) -> Sys {
    match nr {
        sys::DEBUG_WRITE => ok(debug_write(t, a[0], a[1])),
        sys::TASK_EXIT => {
            task::exit_task(&t.task, a[0] as i64);
            Sys::Block
        }
        sys::THREAD_EXIT => {
            task::exit_thread(t, a[0] as i64);
            Sys::Block
        }
        sys::YIELD => {
            sched::yield_now();
            Sys::Ret(0)
        }
        sys::CLOCK if a[0] == 1 => Sys::Ret(crate::time::realtime_ns() as i64),
        sys::CLOCK => Sys::Ret(crate::time::uptime_ns() as i64),
        sys::RANDOM => ok(random(t, a[0], a[1])),
        sys::SLEEP => {
            sched::sleep(t, a[0]);
            Sys::Block
        }
        sys::HANDLE_CLOSE => {
            // The object may go away: not under the handle table's lock.
            let h = t.task.handles.lock().remove(a[0]);
            ok(h.map(|_| 0))
        }
        sys::HANDLE_DUP => ok(dup(t, a[0], a[1] as u32)),
        sys::HANDLE_INFO => ok(t
            .task
            .handles
            .lock()
            .get(a[0])
            .map(|h| ((h.obj.kind() as i64) << 32) | h.rights.0 as i64)),
        sys::THREAD_CREATE => ok(thread_create(t, a[0], a[1], a[2])),
        sys::WAIT => wait(t, a[0], a[1]),
        sys::SPAWN => ok(spawn(t, a[0])),
        sys::QUOTA_CREATE => ok(quota(t, a[0], Rights::MANAGE)
            .and_then(|q| t.task.add_handle(Object::Quota(q.child(a[1], a[2], a[3])), Rights::ALL))
            .map(|h| h as i64)),
        sys::QUOTA_INFO => ok(quota(t, a[0], Rights::NONE).and_then(|q| {
            let i = q.info();
            let w = [i.mem_used, i.mem_limit, i.tasks_used, i.tasks_limit, i.threads_used, i.threads_limit];
            let b: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
            t.task.aspace.write(a[1], &b).map(|_| 0)
        })),
        sys::TASK_KILL => ok(kill(t, a[0])),
        sys::CHANNEL_CREATE => ok(channel_create(t, a[0])),
        sys::CHANNEL_WRITE => ok(channel_write(t, a[0], a[1], a[2], a[3], a[4])),
        sys::CHANNEL_READ => ok(channel_read(t, a[0], a[1], a[2], a[3], a[4], a[5])),
        sys::CHANNEL_RECV => channel_recv(t, a),
        sys::WAIT_ONE => wait_one(t, a[0], a[1] as u32, a[2], a[3]),
        sys::PORT_CREATE => ok(t.task.add_handle(Object::Port(Port::new()), Rights::ALL).map(|h| h as i64)),
        sys::PORT_BIND => ok(port_bind(t, a[0], a[1], a[2] as u32, a[3])),
        sys::PORT_WAIT => port_wait(t, a[0], a[1], a[2]),
        sys::PORT_QUEUE => ok(port(t, a[0], Rights::WRITE).map(|p| {
            p.queue(Packet { key: a[1], signals: 0, _pad: 0, data: a[2] });
            0
        })),
        sys::MEMORY_CREATE => ok(Memory::create(a[0], Some(&t.task.quota))
            .and_then(|m| t.task.add_handle(Object::Memory(m), Rights::ALL))
            .map(|h| h as i64)),
        sys::MEMORY_READ => ok(memory_read(t, a[0], a[1], a[2], a[3])),
        sys::MEMORY_WRITE => ok(memory_write(t, a[0], a[1], a[2], a[3])),
        sys::MEMORY_CLONE => ok(memory(t, a[0], Rights::READ)
            .and_then(|m| m.clone_cow(&t.task.quota))
            .and_then(|c| t.task.add_handle(Object::Memory(c), Rights::ALL))
            .map(|h| h as i64)),
        sys::MEMORY_SIZE => ok(memory(t, a[0], Rights::NONE).map(|m| m.size() as i64)),
        sys::MEMORY_MAP => ok(memory_map(t, a[0], a[1], a[2], a[3])),
        sys::MEMORY_UNMAP => ok(t.task.aspace.unmap_memory(a[0], a[1]).map(|_| 0)),
        sys::SERIAL_READ => ok(serial_read(t, a[0], a[1], a[2])),
        sys::SERIAL_WRITE => ok(serial_write(t, a[0], a[1], a[2])),
        _ => Sys::Ret(Error::NoSys.code()),
    }
}

fn object(t: &Arc<Thread>, h: u64, rights: Rights) -> Result<Object, Error> {
    Ok(t.task.handles.lock().get_with(h, rights)?.obj.clone())
}

fn channel(t: &Arc<Thread>, h: u64, rights: Rights) -> Result<Arc<Channel>, Error> {
    match object(t, h, rights)? {
        Object::Channel(c) => Ok(c),
        _ => Err(Error::WrongType),
    }
}

fn port(t: &Arc<Thread>, h: u64, rights: Rights) -> Result<Arc<Port>, Error> {
    match object(t, h, rights)? {
        Object::Port(p) => Ok(p),
        _ => Err(Error::WrongType),
    }
}

fn memory(t: &Arc<Thread>, h: u64, rights: Rights) -> Result<Arc<Memory>, Error> {
    match object(t, h, rights)? {
        Object::Memory(m) => Ok(m),
        _ => Err(Error::WrongType),
    }
}

fn read_user(t: &Arc<Thread>, va: u64, len: usize) -> Result<Vec<u8>, Error> {
    let mut v = alloc::vec![0u8; len];
    t.task.aspace.read(va, &mut v)?;
    Ok(v)
}

fn read_handles(t: &Arc<Thread>, va: u64, n: usize) -> Result<Vec<u64>, Error> {
    let raw = read_user(t, va, 4 * n)?;
    Ok(raw.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as u64).collect())
}

fn write_u32s(t: &Arc<Thread>, va: u64, v: &[u32]) -> Result<(), Error> {
    let b: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    t.task.aspace.write(va, &b)
}

fn channel_create(t: &Arc<Thread>, out: u64) -> Result<i64, Error> {
    let (a, b) = Channel::pair();
    let ha = t.task.add_handle(Object::Channel(a), Rights::ALL)?;
    let hb = match t.task.add_handle(Object::Channel(b), Rights::ALL) {
        Ok(h) => h,
        Err(e) => {
            let _ = t.task.handles.lock().remove(ha as u64);
            return Err(e);
        }
    };
    if let Err(e) = write_u32s(t, out, &[ha, hb]) {
        let mut tab = t.task.handles.lock();
        let _ = tab.remove(ha as u64);
        let _ = tab.remove(hb as u64);
        return Err(e);
    }
    Ok(0)
}

fn channel_write(t: &Arc<Thread>, h: u64, data: u64, len: u64, hs: u64, n: u64) -> Result<i64, Error> {
    if len as usize > MAX_MSG_BYTES || n as usize > MAX_MSG_HANDLES {
        return Err(Error::InvalidArgs);
    }
    let ch = channel(t, h, Rights::WRITE)?;
    let data = read_user(t, data, len as usize)?;
    let ids = read_handles(t, hs, n as usize)?;
    let me = Object::Channel(ch.clone());
    let moved: Vec<Handle> = {
        let mut tab = t.task.handles.lock();
        for (i, &x) in ids.iter().enumerate() {
            let hd = tab.get_with(x, Rights::TRANSFER)?;
            // An endpoint cannot travel through itself.
            if ids[..i].contains(&x) || hd.obj.same(&me) {
                return Err(Error::InvalidArgs);
            }
        }
        if ch.peer().is_none() && !ch.is_remote() {
            return Err(Error::PeerClosed);
        }
        ids.iter().map(|&x| tab.remove(x).unwrap()).collect()
    };
    match ch.write(Msg { data, handles: moved }) {
        Ok(()) => Ok(0),
        Err((e, m)) => {
            let mut tab = t.task.handles.lock();
            for hd in m.handles {
                let _ = tab.add(hd);
            }
            Err(e)
        }
    }
}

fn channel_read(t: &Arc<Thread>, h: u64, buf: u64, cap: u64, hs: u64, hcap: u64, actual: u64) -> Result<i64, Error> {
    let ch = channel(t, h, Rights::READ)?;
    let (dl, hl) = ch.peek()?;
    write_u32s(t, actual, &[dl as u32, hl as u32])?;
    if dl as u64 > cap || hl as u64 > hcap {
        return Err(Error::BufferTooSmall);
    }
    let m = ch.pop().ok_or(Error::ShouldWait)?;
    if let Err(e) = t.task.aspace.write(buf, &m.data) {
        ch.unpop(m);
        return Err(e);
    }
    let mut ids = Vec::new();
    {
        let mut tab = t.task.handles.lock();
        for hd in m.handles {
            ids.push(tab.add(hd)?);
        }
    }
    write_u32s(t, hs, &ids)?;
    Ok(0)
}

/// CHANNEL_READ, or wait until there is something to read (then the
/// caller reads again: ShouldWait).
fn channel_recv(t: &Arc<Thread>, a: [u64; 6]) -> Sys {
    match channel_read(t, a[0], a[1], a[2], a[3], a[4], a[5]) {
        Err(Error::ShouldWait) => {}
        r => return ok(r),
    }
    let ch = match channel(t, a[0], Rights::READ) {
        Ok(c) => c,
        Err(e) => return Sys::Ret(e.code()),
    };
    let wgen = sched::block_current(t);
    ch.sig.observe(Observer::Recv { t: t.clone(), wgen });
    Sys::Block
}

fn wait_one(t: &Arc<Thread>, h: u64, mask: u32, timeout: u64, out: u64) -> Sys {
    let obj = match object(t, h, Rights::WAIT) {
        Ok(o) => o,
        Err(e) => return Sys::Ret(e.code()),
    };
    let Some(cell) = obj.signals() else { return Sys::Ret(Error::WrongType.code()) };
    let bits = cell.get();
    if bits & mask != 0 {
        let r = t.task.aspace.write(out, &(bits & mask).to_le_bytes());
        return Sys::Ret(r.map(|_| 0).unwrap_or_else(|e| e.code()));
    }
    if timeout == 0 {
        return Sys::Ret(Error::TimedOut.code());
    }
    let wgen = sched::block_current(t);
    cell.observe(Observer::Thread { t: t.clone(), wgen, mask, out });
    sched::timeout(t, wgen, timeout, Error::TimedOut.code());
    Sys::Block
}

fn port_bind(t: &Arc<Thread>, p: u64, h: u64, mask: u32, key: u64) -> Result<i64, Error> {
    let p = port(t, p, Rights::WRITE)?;
    let obj = object(t, h, Rights::WAIT)?;
    let cell = obj.signals().ok_or(Error::WrongType)?;
    cell.observe(Observer::Port { port: Arc::downgrade(&p), key, mask });
    Ok(0)
}

fn port_wait(t: &Arc<Thread>, p: u64, timeout: u64, out: u64) -> Sys {
    match port(t, p, Rights::READ) {
        Ok(p) => match p.wait(t, out, timeout) {
            Some(v) => Sys::Ret(v),
            None => Sys::Block,
        },
        Err(e) => Sys::Ret(e.code()),
    }
}

/// Memory ↔ user copies go through a page-sized kernel buffer.
fn memory_read(t: &Arc<Thread>, h: u64, off: u64, buf: u64, len: u64) -> Result<i64, Error> {
    let m = memory(t, h, Rights::READ)?;
    let mut tmp = [0u8; 4096];
    let mut done = 0;
    while done < len {
        let n = (len - done).min(4096) as usize;
        m.read(off + done, &mut tmp[..n])?;
        t.task.aspace.write(buf + done, &tmp[..n])?;
        done += n as u64;
    }
    Ok(0)
}

fn memory_write(t: &Arc<Thread>, h: u64, off: u64, buf: u64, len: u64) -> Result<i64, Error> {
    let m = memory(t, h, Rights::WRITE)?;
    let mut tmp = [0u8; 4096];
    let mut done = 0;
    while done < len {
        let n = (len - done).min(4096) as usize;
        t.task.aspace.read(buf + done, &mut tmp[..n])?;
        m.write(off + done, &tmp[..n])?;
        done += n as u64;
    }
    Ok(0)
}

fn memory_map(t: &Arc<Thread>, h: u64, off: u64, len: u64, flags: u64) -> Result<i64, Error> {
    let w = flags & abi::map::WRITE != 0;
    let m = memory(t, h, if w { Rights::READ | Rights::WRITE } else { Rights::READ })?;
    t.task.aspace.map_memory(&m, off, len, w).map(|va| va as i64)
}

fn serial(t: &Arc<Thread>, h: u64, rights: Rights) -> Result<Arc<crate::serial::Serial>, Error> {
    match object(t, h, rights)? {
        Object::Serial(s) => Ok(s),
        _ => Err(Error::WrongType),
    }
}

fn serial_read(t: &Arc<Thread>, h: u64, buf: u64, cap: u64) -> Result<i64, Error> {
    let s = serial(t, h, Rights::READ)?;
    let mut tmp = [0u8; 4096];
    let n = s.read(&mut tmp[..cap.min(4096) as usize]);
    if n == 0 {
        return Err(Error::ShouldWait);
    }
    t.task.aspace.write(buf, &tmp[..n])?;
    Ok(n as i64)
}

fn serial_write(t: &Arc<Thread>, h: u64, buf: u64, len: u64) -> Result<i64, Error> {
    serial(t, h, Rights::WRITE)?;
    debug_write(t, buf, len)
}

#[inline(never)]
fn debug_write(t: &Arc<Thread>, ptr: u64, len: u64) -> Result<i64, Error> {
    let len = len.min(4096) as usize;
    let mut buf = [0u8; 4096];
    t.task.aspace.read(ptr, &mut buf[..len])?;
    crate::console::write_bytes(&buf[..len]);
    Ok(len as i64)
}

/// RANDOM: in small pieces (a thread's kernel stack is 16 KiB).
#[inline(never)]
fn random(t: &Arc<Thread>, ptr: u64, len: u64) -> Result<i64, Error> {
    let mut b = [0u8; 64];
    let len = len.min(4096);
    let mut done = 0;
    while done < len {
        let n = (len - done).min(64) as usize;
        crate::random::fill(&mut b[..n]);
        t.task.aspace.write(ptr + done, &b[..n])?;
        done += n as u64;
    }
    Ok(0)
}

fn dup(t: &Arc<Thread>, h: u64, rights: u32) -> Result<i64, Error> {
    let mut tab = t.task.handles.lock();
    let x = tab.get_with(h, Rights::DUP)?.clone();
    let r = Rights(rights);
    if r.0 & !x.rights.0 != 0 {
        return Err(Error::AccessDenied);
    }
    tab.add(Handle { obj: x.obj, rights: r }).map(|v| v as i64)
}

fn thread_create(t: &Arc<Thread>, entry: u64, stack: u64, arg: u64) -> Result<i64, Error> {
    if !crate::arch::user_addr_ok(entry) || !crate::arch::user_addr_ok(stack) {
        return Err(Error::InvalidArgs);
    }
    let n = task::new_thread(&t.task, Start { entry, stack, arg })?;
    t.task.add_handle(Object::Thread(n), Rights::ALL).map(|v| v as i64)
}

fn wait(t: &Arc<Thread>, h: u64, ptr: u64) -> Sys {
    let obj = match t.task.handles.lock().get_with(h, Rights::WAIT) {
        Ok(x) => x.obj.clone(),
        Err(e) => return Sys::Ret(e.code()),
    };
    if !matches!(obj, Object::Task(_) | Object::Thread(_)) {
        return Sys::Ret(Error::WrongType.code());
    }
    let _ = sched::block_current(t);
    let w = Waiter { thread: t.clone(), ptr };
    let done = match &obj {
        Object::Task(x) => x.wait(w),
        Object::Thread(x) => x.wait(w),
        _ => unreachable!(),
    };
    match done {
        None => Sys::Block,
        Some(code) => {
            t.set_run(task::Run::Running);
            let r = t.task.aspace.write(ptr, &code.to_le_bytes());
            Sys::Ret(r.map(|_| 0).unwrap_or_else(|e| e.code()))
        }
    }
}

/// Largest program image.
const MAX_IMAGE: u64 = 64 << 20;

fn quota(t: &Arc<Thread>, h: u64, rights: Rights) -> Result<Arc<crate::quota::Quota>, Error> {
    match object(t, h, rights)? {
        Object::Quota(q) => Ok(q),
        _ => Err(Error::WrongType),
    }
}

fn spawn(t: &Arc<Thread>, args: u64) -> Result<i64, Error> {
    let mut raw = [0u8; core::mem::size_of::<abi::SpawnArgs>()];
    t.task.aspace.read(args, &mut raw)?;
    let a: abi::SpawnArgs = unsafe { core::ptr::read_unaligned(raw.as_ptr() as *const abi::SpawnArgs) };
    let (image, image_len, name, name_len, hs, n) =
        (a.image as u64, a.image_len, a.name_ptr, a.name_len, a.handles_ptr, a.handles_len);
    if name_len > 64 || n > 64 {
        return Err(Error::InvalidArgs);
    }
    let q = if a.quota == abi::NO_HANDLE { t.task.quota.clone() } else { quota(t, a.quota as u64, Rights::WRITE)? };
    let mem = memory(t, image, Rights::READ)?;
    if image_len > mem.size() || image_len > MAX_IMAGE {
        return Err(Error::InvalidArgs);
    }
    let mut nb = [0u8; 64];
    t.task.aspace.read(name, &mut nb[..name_len as usize])?;
    let name = core::str::from_utf8(&nb[..name_len as usize]).map_err(|_| Error::InvalidArgs)?;
    let mut raw = [0u8; 4 * 64];
    t.task.aspace.read(hs, &mut raw[..4 * n as usize])?;
    let ids: Vec<u64> =
        raw[..4 * n as usize].chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as u64).collect();
    // Check everything first, so a failed spawn leaves the handles in place.
    {
        let tab = t.task.handles.lock();
        for (i, &h) in ids.iter().enumerate() {
            tab.get_with(h, Rights::TRANSFER)?;
            if ids[..i].contains(&h) {
                return Err(Error::InvalidArgs);
            }
        }
    }
    let mut bytes = alloc::vec![0u8; image_len as usize];
    mem.read(0, &mut bytes)?;
    let moved: Vec<Handle> = {
        let mut tab = t.task.handles.lock();
        ids.iter().map(|&h| tab.remove(h).unwrap()).collect()
    };
    let child = match task::spawn(name, &bytes, moved.clone(), &q) {
        Ok(c) => c,
        Err(e) => {
            // Give the handles back.
            let mut tab = t.task.handles.lock();
            for h in moved {
                let _ = tab.add(h);
            }
            return Err(e);
        }
    };
    t.task.add_handle(Object::Task(child), Rights::ALL).map(|v| v as i64)
}

fn kill(t: &Arc<Thread>, h: u64) -> Result<i64, Error> {
    let obj = t.task.handles.lock().get_with(h, Rights::MANAGE)?.obj.clone();
    match obj {
        Object::Task(x) => {
            task::exit_task(&x, Error::Killed.code());
            Ok(0)
        }
        _ => Err(Error::WrongType),
    }
}
