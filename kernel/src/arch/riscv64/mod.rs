//! RISC-V 64 on QEMU `virt`: S-mode under OpenSBI, Sv39, PLIC, 16550 UART,
//! SBI timer and shutdown.
mod boot;
mod plic;
pub mod pte;
mod sbi;
mod thread;
mod trap;
mod uart;

pub use crate::kstack::ThreadArch;
pub use crate::mm::aspace::AddrSpace;
pub use thread::{
    Context, FpState, activate, flush_asid, fp_restore, fp_save, sync_icache, flush_tlb, init_thread, init_user_table, set_return, set_thread_cpu, switch, task_exited,
    user_addr_ok,
};

pub const ELF_MACHINE: u16 = 243;

use core::arch::asm;

use crate::boot::BootInfo;
use crate::mm;

pub const NAME: &str = "riscv64";
pub const MAX_CPUS: usize = 8;

/// Hart id of each CPU index (0 = the boot hart).
static mut HARTS: [usize; MAX_CPUS] = [0; MAX_CPUS];
static NCPUS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(1);
static ONLINE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(1);

fn set_boot_hart(h: usize) {
    unsafe { HARTS[0] = h };
}

pub fn boot_hart() -> usize {
    unsafe { HARTS[0] }
}

/// This CPU's index: `tp` in the kernel (restored on every entry from user mode).
pub fn cpu_id() -> usize {
    let c: usize;
    unsafe { asm!("mv {}, tp", out(reg) c) };
    c
}

pub fn send_ipi(cpu: usize) {
    sbi::send_ipi(unsafe { HARTS[cpu] });
}

pub fn cpus() -> usize {
    ONLINE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Starts the other harts in the DTB (SBI HSM); they idle in the scheduler.
pub fn start_cpus(info: &BootInfo) {
    crate::sched::cpu_online(0);
    let mut harts = alloc::vec::Vec::new();
    if let Some(c) = info.fdt.find("/cpus") {
        c.children(|n| {
            if n.prop_str("device_type") == Some("cpu") && n.prop_str("status").is_none_or(|s| s == "okay") {
                harts.extend(n.reg().next().map(|r| r.base as usize));
            }
        });
    }
    let entry = (boot::secondary_entry() - KERNEL_OFFSET) as usize;
    let mut n = 1;
    for h in harts {
        if h == unsafe { HARTS[0] } || n == MAX_CPUS {
            continue;
        }
        let stack = alloc::vec![0u128; 4096].leak();
        unsafe {
            boot::SECONDARY_STACKS[n] = stack.as_ptr() as u64 + 65536;
            HARTS[n] = h;
        }
        if sbi::hart_start(h, entry, n) == 0 {
            n += 1;
        }
    }
    NCPUS.store(n, core::sync::atomic::Ordering::Relaxed);
    let t0 = now_ns();
    while ONLINE.load(core::sync::atomic::Ordering::Acquire) < n && now_ns() - t0 < 1_000_000_000 {
        core::hint::spin_loop();
    }
    println!("smp: {} cpus (harts {:?})", cpus(), unsafe { &HARTS[..n] });
}

/// A secondary hart, on its own stack with paging on (boot tables).
#[unsafe(no_mangle)]
extern "C" fn riscv_secondary(cpu: usize) -> ! {
    unsafe {
        asm!("csrw satp, {}", "sfence.vma", in(reg) (8u64 << 60) | (thread::kernel_root() >> 12));
    }
    trap::init_cpu();
    unsafe { asm!("csrs sie, {}", in(reg) (1u64 << 5) | (1 << 1)) };
    timer_start(crate::HZ);
    crate::sched::cpu_online(cpu);
    ONLINE.fetch_add(1, core::sync::atomic::Ordering::Release);
    enable_interrupts();
    idle()
}
pub const HAS_BREAKPOINT: bool = true;

/// All physical memory is mapped at `PHYS_OFFSET + pa` (Sv39 upper half).
pub const PHYS_OFFSET: u64 = 0xFFFF_FFC0_0000_0000;
/// Kernel image: virtual = physical + KERNEL_OFFSET.
pub const KERNEL_OFFSET: u64 = 0xFFFF_FFFF_8020_0000 - 0x8020_0000;

pub fn phys_to_virt(pa: u64) -> u64 {
    pa + PHYS_OFFSET
}

pub fn console_write(b: &[u8]) {
    uart::write(b)
}

static mut TIMEBASE: u64 = 10_000_000;

pub fn init_memory(info: &BootInfo) {
    let mut dev = crate::device::virtio_regions(info);
    if let Some(c) = &info.console {
        dev.push(c.reg);
    }
    if let Some((_, r)) = crate::time::rtc(info) {
        dev.push(r);
    }
    if let Some(p) = plic::find(info) {
        dev.push(p);
    }
    let pt = mm::init(info, &dev);
    thread::set_kernel_root(pt.root);
    unsafe {
        asm!("csrw satp, {}", "sfence.vma", in(reg) (8u64 << 60) | (pt.root >> 12));
    }
    let bits = thread::asid_bits(pt.root);
    mm::aspace::init_asids(((1u32 << bits) - 1).min(65535));
    let (used, total) = mm::frame::stats();
    println!(
        "paging: on (Sv39, {}-bit ASIDs, root {:#x}, {} tables, {}/{} frames used)",
        bits, pt.root, pt.tables, used, total
    );
    core::mem::forget(pt);
}

/// Bytes of RAM the frame allocator manages (the root quota).
pub fn memory_total() -> u64 {
    mm::frame::stats().1 * mm::PAGE_SIZE
}

/// No host to talk to (until the network, M7).
pub fn remote_send(_b: &[u8]) -> Result<(), abi::Error> {
    Err(abi::Error::NotSupported)
}

pub fn remote_channel() -> Option<alloc::sync::Arc<crate::channel::Channel>> {
    None
}

pub fn irq_enable(n: u32) {
    plic::enable(n)
}

pub fn irq_disable(n: u32) {
    plic::disable(n)
}

/// The PLIC's interrupt specifier is one cell.
pub fn irq_number(spec: &[u32; 3]) -> u32 {
    spec[0]
}

pub fn init_traps() {
    trap::init();
}

pub fn init_irq(info: &BootInfo) {
    plic::init(info);
}

pub fn breakpoint() {
    unsafe { asm!("ebreak") }
}

pub fn timer_frequency(info: &BootInfo) -> u64 {
    let f = info.timebase.unwrap_or(10_000_000);
    unsafe { TIMEBASE = f };
    f
}

fn rdtime() -> u64 {
    let t: u64;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

pub fn now_ns() -> u64 {
    let f = unsafe { TIMEBASE };
    (rdtime() as u128 * 1_000_000_000 / f as u128) as u64
}

static mut INTERVAL: u64 = 0;

pub fn timer_start(hz: u64) {
    unsafe {
        INTERVAL = TIMEBASE / hz;
        sbi::set_timer(rdtime() + INTERVAL);
        asm!("csrs sie, {}", in(reg) (1u64 << 5) | (1 << 1));
    }
}

/// Timer interrupt: schedule the next one.
fn timer_irq() {
    sbi::set_timer(rdtime() + unsafe { INTERVAL });
    crate::time::tick(cpu_id());
}

pub fn enable_interrupts() {
    unsafe { asm!("csrsi sstatus, 2") }
}

/// Disables interrupts; returns whether they were enabled.
pub fn disable_interrupts() -> bool {
    let s: u64;
    unsafe { asm!("csrrci {}, sstatus, 2", out(reg) s) };
    s & 2 != 0
}

pub fn idle() -> ! {
    loop {
        unsafe { asm!("wfi") }
    }
}

pub fn shutdown(code: u32) -> ! {
    sbi::shutdown(code != 0)
}
