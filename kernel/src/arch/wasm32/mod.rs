//! WebAssembly: the kernel is one wasm instance in the browser's (or Node's)
//! kernel Worker. The host passes a DTB it built, delivers "interrupts" by
//! calling `kernel_irq`, and provides console, clock and power as imports.
//! There is no paging: every task is a wasm module with its own shared
//! linear memory, and every thread an instance of it in its own Worker.
use core::sync::atomic::{AtomicU32, Ordering};

use abi::Error;

use crate::boot::BootInfo;
use crate::dtb::Fdt;
use crate::syscall::{self, Sys};
use crate::task::{self, MAIN_ENTRY, Run, Start};

pub const NAME: &str = "wasm32";
pub const HAS_BREAKPOINT: bool = false;

pub mod host {
    #[link(wasm_import_module = "env")]
    unsafe extern "C" {
        pub fn console_write(p: *const u8, n: usize);
        pub fn timer_start(hz: u32);
        pub fn now_ms() -> f64;
        pub fn shutdown(code: u32);
        /// Compiles a task's module and creates its shared memory (at most
        /// `max_pages`) → task id.
        pub fn task_create(image: *const u8, len: usize, max_pages: u32) -> u32;
        pub fn task_destroy(task: u32);
        /// Starts a Worker running `entry` (`u32::MAX` = the program's entry).
        pub fn thread_start(task: u32, tid: u32, entry: u32, stack: u32, arg: u32);
        /// Answers a blocked system call.
        pub fn thread_resume(tid: u32, value: i64);
        pub fn thread_kill(tid: u32);
        /// Copies between a task's memory and the kernel's; 0 or -1 (fault).
        pub fn user_read(task: u32, va: u32, len: u32, dst: *mut u8) -> i32;
        pub fn user_write(task: u32, va: u32, src: *const u8, len: u32) -> i32;
        /// 1 if the host has a server connection for `remote_send`.
        pub fn remote_open(i: u32) -> u32;
        pub fn remote_send(i: u32, p: *const u8, len: usize);
    }
}

/// A task's memory lives in the host: this is its id there. Its maximum
/// is what the quota allows; its initial size is charged.
pub struct AddrSpace {
    host: AtomicU32,
    quota: alloc::sync::Arc<crate::quota::Quota>,
    charge: crate::sync::IrqLock<Option<crate::quota::Charge>>,
}

/// Bytes the root quota pretends there are.
pub fn memory_total() -> u64 {
    1 << 30
}

const WASM_PAGE: u64 = 65536;

/// (min, max) pages of the module's imported memory.
fn memory_limits(b: &[u8]) -> Option<(u64, u64)> {
    let mut p = 8usize;
    let leb = |p: &mut usize| -> Option<u64> {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let c = *b.get(*p)?;
            *p += 1;
            v |= ((c & 0x7f) as u64) << s;
            s += 7;
            if c & 0x80 == 0 {
                return Some(v);
            }
        }
    };
    while p < b.len() {
        let id = b[p];
        p += 1;
        let size = leb(&mut p)? as usize;
        let end = p + size;
        if id == 2 {
            for _ in 0..leb(&mut p)? {
                for _ in 0..2 {
                    let n = leb(&mut p)? as usize;
                    p += n;
                }
                let kind = *b.get(p)?;
                p += 1;
                match kind {
                    0 => _ = leb(&mut p)?,
                    1 => {
                        p += 1;
                        let f = *b.get(p)?;
                        p += 1;
                        leb(&mut p)?;
                        if f & 1 != 0 {
                            leb(&mut p)?;
                        }
                    }
                    2 => {
                        let f = *b.get(p)?;
                        p += 1;
                        let min = leb(&mut p)?;
                        let max = if f & 1 != 0 { leb(&mut p)? } else { 65536 };
                        return Some((min, max));
                    }
                    _ => p += 2,
                }
            }
            return None;
        }
        p = end;
    }
    None
}

impl AddrSpace {
    pub fn new(quota: &alloc::sync::Arc<crate::quota::Quota>) -> Result<AddrSpace, Error> {
        Ok(AddrSpace { host: AtomicU32::new(0), quota: quota.clone(), charge: crate::sync::IrqLock::new(None) })
    }

    pub fn set_self(&self, _me: alloc::sync::Weak<AddrSpace>) {}

    /// Linear memories cannot alias: use MEMORY_READ / MEMORY_WRITE.
    pub fn map_memory(&self, _m: &alloc::sync::Arc<crate::memory::Memory>, _off: u64, _len: u64, _w: bool) -> Result<u64, Error> {
        Err(Error::NotSupported)
    }

    pub fn unmap_memory(&self, _va: u64, _len: u64) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    pub fn host(&self) -> u32 {
        self.host.load(Ordering::Relaxed)
    }

    pub fn load(&self, image: &[u8]) -> Result<Start, Error> {
        if image.get(..4) != Some(b"\0asm") {
            return Err(Error::NotSupported);
        }
        let (min, max) = memory_limits(image).ok_or(Error::InvalidArgs)?;
        let max = max.min(self.quota.mem_left() / WASM_PAGE);
        if max < min {
            return Err(Error::QuotaExceeded);
        }
        let charge = crate::quota::Charge::new(&self.quota, crate::quota::Res::Memory, min * WASM_PAGE)?;
        let id = unsafe { host::task_create(image.as_ptr(), image.len(), max as u32) };
        if id == 0 {
            return Err(Error::InvalidArgs);
        }
        *self.charge.lock() = Some(charge);
        self.host.store(id, Ordering::Relaxed);
        Ok(Start { entry: MAIN_ENTRY, stack: 0, arg: 0 })
    }

    pub fn read(&self, va: u64, buf: &mut [u8]) -> Result<(), Error> {
        if !user_addr_ok(va) {
            return Err(Error::Fault);
        }
        match unsafe { host::user_read(self.host(), va as u32, buf.len() as u32, buf.as_mut_ptr()) } {
            0 => Ok(()),
            _ => Err(Error::Fault),
        }
    }

    pub fn write(&self, va: u64, buf: &[u8]) -> Result<(), Error> {
        if !user_addr_ok(va) {
            return Err(Error::Fault);
        }
        match unsafe { host::user_write(self.host(), va as u32, buf.as_ptr(), buf.len() as u32) } {
            0 => Ok(()),
            _ => Err(Error::Fault),
        }
    }
}

pub fn task_exited(a: &AddrSpace) {
    unsafe { host::task_destroy(a.host()) }
    a.charge.lock().take();
}

/// Threads have no kernel-side state on wasm32.
pub struct ThreadArch;

impl ThreadArch {
    pub fn new(_s: &Start) -> Result<ThreadArch, Error> {
        Ok(ThreadArch)
    }
}

pub fn user_addr_ok(va: u64) -> bool {
    va <= u32::MAX as u64
}

/// `kernel_syscall` result: the thread is blocked (or gone); the answer
/// comes later through `thread_resume`.
const BLOCKED: i64 = i64::MIN;

#[unsafe(no_mangle)]
extern "C" fn kernel_syscall(tid: u32, nr: u32, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let Some(t) = task::thread_by_id(tid) else { return BLOCKED };
    crate::sched::set_current(Some(t.clone()));
    let r = syscall::dispatch(&t, nr as usize, [a0, a1, a2, a3, a4, a5]);
    crate::sched::set_current(None);
    match r {
        Sys::Ret(v) if t.run() == Run::Running => v,
        _ => BLOCKED,
    }
}

static mut SCRATCH: [u8; 4096] = [0; 4096];

/// A buffer for the host's messages (`kernel_fault`).
#[unsafe(no_mangle)]
extern "C" fn kernel_scratch() -> *mut u8 {
    (&raw mut SCRATCH) as *mut u8
}

/// A thread's instance trapped; the message is in the scratch buffer.
#[unsafe(no_mangle)]
extern "C" fn kernel_fault(tid: u32, len: usize) {
    let Some(t) = task::thread_by_id(tid) else { return };
    let b = unsafe { core::slice::from_raw_parts((&raw const SCRATCH) as *const u8, len.min(4096)) };
    let msg = core::str::from_utf8(b).unwrap_or("trap");
    task::fault(&t, format_args!("{}", msg));
}

const DTB_MAX: usize = 64 * 1024;
static mut DTB_BUF: [u8; DTB_MAX] = [0; DTB_MAX];

/// Where the host writes the device tree before `kernel_boot`.
#[unsafe(no_mangle)]
extern "C" fn kernel_dtb_buf() -> *mut u8 {
    (&raw mut DTB_BUF) as *mut u8
}

#[unsafe(no_mangle)]
extern "C" fn kernel_boot(len: usize) -> i32 {
    let buf: &'static [u8] =
        unsafe { core::slice::from_raw_parts((&raw const DTB_BUF) as *const u8, len.min(DTB_MAX)) };
    let Some(fdt) = Fdt::new(buf) else { return -1 };
    let info = BootInfo::new(fdt, buf.as_ptr() as u64);
    crate::kmain(&info);
    0
}

pub const IRQ_TIMER: u32 = 0;
pub const IRQ_CONSOLE: u32 = 1;

/// Host events: the timer, or a console byte in `data`.
#[unsafe(no_mangle)]
extern "C" fn kernel_irq(irq: u32, data: u32) {
    match irq {
        IRQ_TIMER => crate::time::tick(0),
        IRQ_CONSOLE => crate::console::input_byte(data as u8),
        _ => {}
    }
}

pub fn console_write(b: &[u8]) {
    unsafe { host::console_write(b.as_ptr(), b.len()) }
}

pub fn init_memory(_info: &BootInfo) {
    let kib = core::arch::wasm32::memory_size(0) * 64;
    println!("paging: none (wasm32 linear memory, {} KiB; one instance per task)", kib);
}

/// Threads already run in parallel, each in its own Worker.
pub fn start_cpus(_info: &BootInfo) {
    println!("smp: threads run in parallel Workers");
}

pub fn init_traps() {
    println!("trap: wasm traps end the instance; system calls will be imports");
}

pub fn init_irq(_info: &BootInfo) {
    println!("irq: host events (timer {}, console {})", IRQ_TIMER, IRQ_CONSOLE);
}

pub fn breakpoint() {}

pub fn timer_frequency(info: &BootInfo) -> u64 {
    info.timebase.unwrap_or(1000)
}

pub fn now_ns() -> u64 {
    (unsafe { host::now_ms() } * 1e6) as u64
}

pub fn timer_start(hz: u64) {
    unsafe { host::timer_start(hz as u32) }
}

pub fn enable_interrupts() {}

/// The host never preempts the kernel, so there is nothing to mask.
pub fn disable_interrupts() -> bool {
    false
}

/// The host's `shutdown` throws, unwinding out of the instance.
pub fn shutdown(code: u32) -> ! {
    unsafe { host::shutdown(code) };
    core::arch::wasm32::unreachable()
}

/// The host's channels to the server: 0 = the server connection (the
/// store asks, the server answers), 1 = ingress (the server sends requests
/// from outside, kube answers).
static REMOTE: crate::sync::IrqLock<[Option<alloc::sync::Arc<crate::channel::Channel>>; 2]> = crate::sync::IrqLock::new([None, None]);

/// Remote channel `i`, if the host has it (given to init).
pub fn remote_channel(i: u8) -> Option<alloc::sync::Arc<crate::channel::Channel>> {
    if i > 1 || unsafe { host::remote_open(i as u32) } == 0 {
        return None;
    }
    Some(REMOTE.lock()[i as usize].get_or_insert_with(|| crate::channel::Channel::remote(i)).clone())
}

pub fn remote_send(i: u8, b: &[u8]) -> Result<(), Error> {
    unsafe { host::remote_send(i as u32, b.as_ptr(), b.len()) };
    Ok(())
}

static SNAPSHOT: crate::sync::IrqLock<alloc::string::String> = crate::sync::IrqLock::new(alloc::string::String::new());

/// The kernel described as JSON (snapshot.rs): returns where it is;
/// `kernel_snapshot_len` says how long.
#[unsafe(no_mangle)]
extern "C" fn kernel_snapshot() -> *const u8 {
    let mut s = SNAPSHOT.lock();
    *s = crate::snapshot::json();
    s.as_ptr()
}

#[unsafe(no_mangle)]
extern "C" fn kernel_snapshot_len() -> usize {
    SNAPSHOT.lock().len()
}

static mut REMOTE_BUF: [u8; 65536] = [0; 65536];

#[unsafe(no_mangle)]
extern "C" fn kernel_remote_buf() -> *mut u8 {
    (&raw mut REMOTE_BUF) as *mut u8
}

/// A message from the server for remote channel `i` is in the remote buffer.
#[unsafe(no_mangle)]
extern "C" fn kernel_remote(i: usize, len: usize) {
    let r = REMOTE.lock().get(i).cloned().flatten();
    if let Some(r) = r {
        let b = unsafe { core::slice::from_raw_parts((&raw const REMOTE_BUF) as *const u8, len.min(65536)) };
        r.deliver(b.to_vec());
    }
}
