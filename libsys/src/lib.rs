//! ruos user library: system calls, handles, console output and the
//! program entry. A program is
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! libsys::entry!(main);
//! fn main(arg: usize) -> i64 { libsys::println!("hi"); 0 }
//! ```
#![no_std]

extern crate alloc;

pub use abi::{
    Error, MAX_MSG_BYTES, MAX_MSG_HANDLES, ObjType, Packet, QuotaInfo, Rights, SpawnArgs, layout, map, signal, sys as nr,
};

mod raw;
pub mod args;
pub mod blk;
pub mod console;
pub mod dock;
pub mod fmt;
pub mod fs;
pub mod func;
pub mod http;
pub mod image;
mod heap;
pub mod ipc;
pub mod kube;
pub mod kv;
pub mod net;
pub mod ns;
pub mod store;
pub mod svc;
pub mod wire;

pub use ipc::{Channel, Irq, Memory, Mmio, Port, Serial, wait_one};

pub type Result<T> = core::result::Result<T, Error>;

/// A handle: an index into the task's handle table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(pub u32);

/// The architecture this program was built for.
pub const ARCH: &str = if cfg!(target_arch = "riscv64") {
    "riscv64"
} else if cfg!(target_arch = "aarch64") {
    "aarch64"
} else {
    "wasm32"
};

pub(crate) fn check(r: i64) -> Result<u64> {
    if r < 0 { Err(Error::from_code(r).unwrap_or(Error::NoSys)) } else { Ok(r as u64) }
}

pub(crate) fn call(nr: usize, a: [u64; 6]) -> i64 {
    unsafe { raw::syscall(nr, a) }
}

/// A system call as is (for runtimes that forward calls).
pub fn syscall_raw(nr: usize, a: [u64; 6]) -> i64 {
    call(nr, a)
}

pub fn debug_write(b: &[u8]) {
    call(nr::DEBUG_WRITE, [b.as_ptr() as u64, b.len() as u64, 0, 0, 0, 0]);
}

pub fn exit(code: i64) -> ! {
    call(nr::TASK_EXIT, [code as u64, 0, 0, 0, 0, 0]);
    unreachable!()
}

pub fn thread_exit(code: i64) -> ! {
    call(nr::THREAD_EXIT, [code as u64, 0, 0, 0, 0, 0]);
    unreachable!()
}

pub fn yield_now() {
    call(nr::YIELD, [0; 6]);
}

/// Nanoseconds since boot.
pub fn clock_ns() -> u64 {
    call(nr::CLOCK, [0; 6]) as u64
}

/// Nanoseconds since 1970 (UTC), from the machine's real-time clock.
pub fn realtime_ns() -> u64 {
    call(nr::CLOCK, [1, 0, 0, 0, 0, 0]) as u64
}

/// Fills `buf` with random bytes from the kernel (ChaCha20, seeded from the
/// host's entropy: fit for keys).
pub fn random(buf: &mut [u8]) {
    for c in buf.chunks_mut(4096) {
        call(nr::RANDOM, [c.as_mut_ptr() as u64, c.len() as u64, 0, 0, 0, 0]);
    }
}

pub fn sleep_ns(ns: u64) {
    call(nr::SLEEP, [ns, 0, 0, 0, 0, 0]);
}

pub fn sleep_ms(ms: u64) {
    sleep_ns(ms * 1_000_000)
}

impl Handle {
    pub fn close(self) -> Result<()> {
        check(call(nr::HANDLE_CLOSE, [self.0 as u64, 0, 0, 0, 0, 0])).map(|_| ())
    }

    pub fn dup(self, rights: Rights) -> Result<Handle> {
        check(call(nr::HANDLE_DUP, [self.0 as u64, rights.0 as u64, 0, 0, 0, 0])).map(|h| Handle(h as u32))
    }

    pub fn info(self) -> Result<(ObjType, Rights)> {
        let v = check(call(nr::HANDLE_INFO, [self.0 as u64, 0, 0, 0, 0, 0]))?;
        Ok((ObjType::from_u32((v >> 32) as u32).ok_or(Error::WrongType)?, Rights(v as u32)))
    }

    /// Waits for a task or thread to end; returns its exit code.
    pub fn wait(self) -> Result<i64> {
        let mut code = 0i64;
        check(call(nr::WAIT, [self.0 as u64, &mut code as *mut i64 as u64, 0, 0, 0, 0]))?;
        Ok(code)
    }

    pub fn kill(self) -> Result<()> {
        check(call(nr::TASK_KILL, [self.0 as u64, 0, 0, 0, 0, 0])).map(|_| ())
    }
}

/// Starts the program in `image` (its first `len` bytes) as task `name`,
/// moving `handles` to it.
pub fn spawn(image: Memory, len: u64, name: &str, handles: &[Handle]) -> Result<Handle> {
    spawn_with(image, len, name, handles, None)
}

/// `spawn`, charging the task to `quota` (None: the caller's quota).
pub fn spawn_with(image: Memory, len: u64, name: &str, handles: &[Handle], quota: Option<Handle>) -> Result<Handle> {
    let a = abi::SpawnArgs {
        image: image.0.0,
        quota: quota.map(|q| q.0).unwrap_or(abi::NO_HANDLE),
        image_len: len,
        name_ptr: name.as_ptr() as u64,
        name_len: name.len() as u64,
        handles_ptr: handles.as_ptr() as u64,
        handles_len: handles.len() as u64,
    };
    check(call(nr::SPAWN, [&a as *const abi::SpawnArgs as u64, 0, 0, 0, 0, 0])).map(|h| Handle(h as u32))
}

/// A quota: limits on memory (bytes), tasks and threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota(pub Handle);

impl Quota {
    /// A child quota; what it uses also counts against this one.
    pub fn child(&self, mem: u64, tasks: u64, threads: u64) -> Result<Quota> {
        check(call(nr::QUOTA_CREATE, [self.0.0 as u64, mem, tasks, threads, 0, 0])).map(|h| Quota(Handle(h as u32)))
    }

    pub fn info(&self) -> Result<QuotaInfo> {
        let mut i = QuotaInfo::default();
        check(call(nr::QUOTA_INFO, [self.0.0 as u64, &mut i as *mut QuotaInfo as u64, 0, 0, 0, 0]))?;
        Ok(i)
    }
}

/// Starts a thread running `f(arg)` on `stack`.
pub fn thread_create(f: extern "C" fn(usize) -> !, stack: &'static mut [u8], arg: usize) -> Result<Handle> {
    let top = (stack.as_mut_ptr() as usize + stack.len()) & !15;
    let r = call(nr::THREAD_CREATE, [f as usize as u64, top as u64, arg as u64, 0, 0, 0]);
    check(r).map(|h| Handle(h as u32))
}

unsafe extern "Rust" {
    fn ros_main(arg: usize) -> i64;
}

/// Called by `_start` (native) or by the host (wasm32).
#[unsafe(no_mangle)]
pub extern "C" fn ros_start(arg: usize) -> ! {
    ns::load();
    exit(unsafe { ros_main(arg) })
}

/// wasm32 thread entry: the host calls it in the thread's own instance.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn ros_thread_start(f: usize, arg: usize) -> ! {
    let f: extern "C" fn(usize) -> ! = unsafe { core::mem::transmute(f) };
    f(arg)
}

/// Declares the program's `main(arg) -> exit code`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub fn ros_main(arg: usize) -> i64 {
            $main(arg)
        }
    };
}

#[panic_handler]
fn panic(pi: &core::panic::PanicInfo) -> ! {
    println!("panic: {}", pi);
    exit(Error::Fault.code())
}
