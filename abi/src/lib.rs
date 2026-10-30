//! The kernel ABI shared by the kernel and `libsys`.
//!
//! A system call takes a number and up to six word arguments and returns one
//! signed word: `>= 0` is success, `< 0` is an [`Error`].
//! riscv64: `ecall`, number in a7, arguments a0..a5, result a0.
//! aarch64: `svc #0`, number in x8, arguments x0..x5, result x0.
//! wasm32: import `sys.call(nr: i32, a0..a5: i64) -> i64`.
#![no_std]

/// System call numbers.
pub mod sys {
    /// (ptr, len) → len. Kernel log / console output.
    pub const DEBUG_WRITE: usize = 0;
    /// (code) → never returns. Ends the calling task.
    pub const TASK_EXIT: usize = 1;
    /// (code) → never returns. Ends the calling thread.
    pub const THREAD_EXIT: usize = 2;
    pub const YIELD: usize = 3;
    /// (clock) → nanoseconds: since boot (0), or since 1970 UTC (1, the
    /// wall clock: the machine's RTC at boot plus the time since).
    pub const CLOCK: usize = 4;
    /// (ns) → 0 after at least `ns` nanoseconds.
    pub const SLEEP: usize = 5;
    /// (handle) → 0.
    pub const HANDLE_CLOSE: usize = 6;
    /// (handle, rights) → new handle with `rights` (a subset).
    pub const HANDLE_DUP: usize = 7;
    /// (handle) → `type << 32 | rights`.
    pub const HANDLE_INFO: usize = 8;
    /// (entry, stack_top, arg) → thread handle.
    pub const THREAD_CREATE: usize = 9;
    /// (handle, *mut i64) → 0 once the task / thread has ended; its exit code is stored.
    pub const WAIT: usize = 10;
    /// (*const SpawnArgs) → task handle. The image is a program (ELF, or a
    /// wasm module on wasm32) in a memory object (READ). The handles move
    /// to the new task, where they are numbered 0, 1, ... The task is
    /// charged to `quota` (WRITE), or to the caller's quota.
    pub const SPAWN: usize = 11;
    /// (task handle) → 0. Needs MANAGE.
    pub const TASK_KILL: usize = 12;

    /// (*mut [u32; 2]) → 0; stores the two endpoints.
    pub const CHANNEL_CREATE: usize = 13;
    /// (h, data, len, handles, n) → 0. The handles (TRANSFER) move with the message.
    pub const CHANNEL_WRITE: usize = 14;
    /// (h, buf, cap, handles, hcap, *mut [u32; 2] actual) → 0. Never blocks:
    /// ShouldWait when empty, PeerClosed when empty and the peer is gone,
    /// BufferTooSmall (with `actual` filled in) when it does not fit.
    pub const CHANNEL_READ: usize = 15;
    /// (h, signals, timeout_ns, *mut u32 observed) → 0 once any of
    /// `signals` is set; TimedOut otherwise. `u64::MAX` waits forever.
    pub const WAIT_ONE: usize = 16;
    /// () → port handle.
    pub const PORT_CREATE: usize = 17;
    /// (port, h, signals, key) → 0. One shot: a packet {key, signals}
    /// is queued on the port when any of `signals` is set on `h`.
    pub const PORT_BIND: usize = 18;
    /// (port, timeout_ns, *mut Packet) → 0.
    pub const PORT_WAIT: usize = 19;
    /// (port, key, data) → 0. A user packet.
    pub const PORT_QUEUE: usize = 20;
    /// (size) → memory handle (zero filled, page granular).
    pub const MEMORY_CREATE: usize = 21;
    /// (h, offset, buf, len) → 0.
    pub const MEMORY_READ: usize = 22;
    /// (h, offset, buf, len) → 0.
    pub const MEMORY_WRITE: usize = 23;
    /// (h) → a copy-on-write clone.
    pub const MEMORY_CLONE: usize = 24;
    /// (h) → size in bytes.
    pub const MEMORY_SIZE: usize = 25;
    /// (h, offset, len, flags) → address. Native only (NotSupported on wasm32).
    pub const MEMORY_MAP: usize = 26;
    /// (addr, len) → 0.
    pub const MEMORY_UNMAP: usize = 27;
    /// (serial, buf, cap) → bytes read (ShouldWait when none).
    pub const SERIAL_READ: usize = 28;
    /// (serial, buf, len) → len.
    pub const SERIAL_WRITE: usize = 29;
    /// (parent quota (MANAGE), memory bytes, tasks, threads) → a quota
    /// whose usage also counts against the parent (and its parents).
    pub const QUOTA_CREATE: usize = 30;
    /// (quota, *mut QuotaInfo) → 0.
    pub const QUOTA_INFO: usize = 31;
    /// (mmio) → address of its registers mapped in the caller (native).
    pub const MMIO_MAP: usize = 32;
    /// (mmio, *mut [u64; 3]) → 0: physical address, size, device id
    /// (the virtio device id; 0 if none).
    pub const MMIO_INFO: usize = 33;
    /// (irq) → 0. The interrupt is masked from when it fires (READABLE)
    /// until the driver acknowledges it.
    pub const IRQ_ACK: usize = 34;
    /// (size) → a memory object of physically contiguous pages (for DMA).
    pub const MEMORY_CREATE_DMA: usize = 35;
    /// (memory (MANAGE), offset) → physical address of that byte.
    pub const MEMORY_PHYS: usize = 36;
    /// CHANNEL_READ that waits for a message (or the peer closing) instead
    /// of answering ShouldWait.
    pub const CHANNEL_RECV: usize = 37;
    /// (buf, len) → 0. Fills `buf` with random bytes (ChaCha20 seeded from
    /// the boot loader's entropy; at most 4096 bytes a call).
    pub const RANDOM: usize = 38;
}

/// `SPAWN`'s arguments.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SpawnArgs {
    pub image: u32,
    /// `NO_HANDLE`: the caller's quota.
    pub quota: u32,
    pub image_len: u64,
    pub name_ptr: u64,
    pub name_len: u64,
    pub handles_ptr: u64,
    pub handles_len: u64,
}

pub const NO_HANDLE: u32 = u32::MAX;

/// What `QUOTA_INFO` stores.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuotaInfo {
    pub mem_used: u64,
    pub mem_limit: u64,
    pub tasks_used: u64,
    pub tasks_limit: u64,
    pub threads_used: u64,
    pub threads_limit: u64,
}

/// Object signals (`WAIT_ONE`, `PORT_BIND`).
pub mod signal {
    pub const READABLE: u32 = 1;
    pub const WRITABLE: u32 = 2;
    pub const PEER_CLOSED: u32 = 4;
    pub const TERMINATED: u32 = 8;
}

/// `MEMORY_MAP` flags.
pub mod map {
    pub const READ: u64 = 1;
    pub const WRITE: u64 = 2;
}

/// What `PORT_WAIT` stores.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Packet {
    pub key: u64,
    /// Signals that fired (0 for user packets).
    pub signals: u32,
    pub _pad: u32,
    /// User data (`PORT_QUEUE`).
    pub data: u64,
}

/// Channel message limits.
pub const MAX_MSG_BYTES: usize = 65536;
pub const MAX_MSG_HANDLES: usize = 16;

/// Errors (returned negated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    BadHandle = 1,
    AccessDenied = 2,
    InvalidArgs = 3,
    NoMemory = 4,
    Fault = 5,
    NotFound = 6,
    NoSys = 7,
    WrongType = 8,
    Killed = 9,
    NotSupported = 10,
    ShouldWait = 11,
    PeerClosed = 12,
    TimedOut = 13,
    BufferTooSmall = 14,
    QuotaExceeded = 15,
    /// File system services (not the kernel): the name is taken.
    Exists = 16,
    /// A directory still has entries.
    NotEmpty = 17,
    NotDir = 18,
    IsDir = 19,
}

impl Error {
    pub fn code(self) -> i64 {
        -(self as i64)
    }

    pub fn from_code(c: i64) -> Option<Error> {
        use Error::*;
        Some(match -c {
            1 => BadHandle,
            2 => AccessDenied,
            3 => InvalidArgs,
            4 => NoMemory,
            5 => Fault,
            6 => NotFound,
            7 => NoSys,
            8 => WrongType,
            9 => Killed,
            10 => NotSupported,
            11 => ShouldWait,
            12 => PeerClosed,
            13 => TimedOut,
            14 => BufferTooSmall,
            15 => QuotaExceeded,
            16 => Exists,
            17 => NotEmpty,
            18 => NotDir,
            19 => IsDir,
            _ => return None,
        })
    }
}

/// What a handle allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const DUP: Rights = Rights(1);
    pub const TRANSFER: Rights = Rights(2);
    pub const WAIT: Rights = Rights(4);
    pub const READ: Rights = Rights(8);
    pub const WRITE: Rights = Rights(16);
    pub const MANAGE: Rights = Rights(32);
    pub const ALL: Rights = Rights(0xffff);

    pub fn has(self, r: Rights) -> bool {
        self.0 & r.0 == r.0
    }

    pub fn contains(self, r: Rights) -> bool {
        self.has(r)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Rights;
    fn bitor(self, o: Rights) -> Rights {
        Rights(self.0 | o.0)
    }
}

/// Kernel object types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ObjType {
    Task = 1,
    Thread = 2,
    Channel = 3,
    Memory = 4,
    Port = 5,
    Quota = 6,
    Serial = 7,
    Mmio = 8,
    Irq = 9,
}

impl ObjType {
    pub fn from_u32(v: u32) -> Option<ObjType> {
        use ObjType::*;
        Some(match v {
            1 => Task,
            2 => Thread,
            3 => Channel,
            4 => Memory,
            5 => Port,
            6 => Quota,
            7 => Serial,
            8 => Mmio,
            9 => Irq,
            _ => return None,
        })
    }
}

/// Native user address space layout.
pub mod layout {
    /// Top of the first thread's stack (below 256 GB: inside Sv39's lower half).
    pub const STACK_TOP: u64 = 0x3f_ffff_0000;
    pub const STACK_SIZE: u64 = 64 * 1024;
}
