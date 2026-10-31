//! AArch64 on QEMU `virt` (cortex-a72): EL1 (entered from EL2 if needed),
//! 4 KB granule with 4 levels, kernel in TTBR1, GICv2, PL011, generic timer
//! and PSCI.
mod boot;
mod gic;
mod pl011;
mod psci;
pub mod pte;
mod thread;
mod trap;

pub use crate::kstack::ThreadArch;
pub use crate::mm::aspace::AddrSpace;
pub use thread::{
    Context, FpState, activate, flush_asid, fp_restore, fp_save, sync_icache, flush_tlb, init_thread, init_user_table, set_return, set_thread_cpu, switch, task_exited,
    user_addr_ok,
};

pub const ELF_MACHINE: u16 = 183;

use core::arch::asm;

use crate::boot::BootInfo;
use crate::mm;

pub const NAME: &str = "aarch64";
pub const MAX_CPUS: usize = 8;

static ONLINE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(1);
static mut KERNEL_ROOT: u64 = 0;
static mut USER_EMPTY: u64 = 0;

/// This CPU's index (TPIDR_EL1).
pub fn cpu_id() -> usize {
    let c: usize;
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) c) };
    c
}

pub fn send_ipi(cpu: usize) {
    gic::send_sgi(cpu)
}

pub fn cpus() -> usize {
    ONLINE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Starts the other CPUs in the DTB (PSCI CPU_ON).
pub fn start_cpus(info: &BootInfo) {
    crate::sched::cpu_online(0);
    let me: u64;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) me) };
    let me = me & 0xff_ffff;
    let mut ids = alloc::vec::Vec::new();
    if let Some(c) = info.fdt.find("/cpus") {
        c.children(|n| {
            if n.prop_str("device_type") == Some("cpu") {
                ids.extend(n.reg().next().map(|r| r.base));
            }
        });
    }
    let entry = boot::secondary_entry() - KERNEL_OFFSET;
    let mut n = 1;
    for id in ids {
        if id == me || n == MAX_CPUS {
            continue;
        }
        let stack = alloc::vec![0u128; 4096].leak();
        unsafe { boot::SECONDARY_STACKS[n] = stack.as_ptr() as u64 + 65536 };
        if psci::cpu_on(id, entry, n as u64) == 0 {
            n += 1;
        }
    }
    let t0 = now_ns();
    while ONLINE.load(core::sync::atomic::Ordering::Acquire) < n && now_ns() - t0 < 1_000_000_000 {
        core::hint::spin_loop();
    }
    println!("smp: {} cpus", cpus());
}

/// A secondary CPU, on its own stack with the boot tables.
#[unsafe(no_mangle)]
extern "C" fn aarch64_secondary(cpu: usize) -> ! {
    unsafe {
        asm!(
            "msr ttbr1_el1, {k}",
            "msr ttbr0_el1, {u}",
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            k = in(reg) KERNEL_ROOT,
            u = in(reg) USER_EMPTY,
        );
    }
    trap::init_cpu();
    gic::init_cpu();
    timer_start(crate::HZ);
    crate::sched::cpu_online(cpu);
    ONLINE.fetch_add(1, core::sync::atomic::Ordering::Release);
    enable_interrupts();
    idle()
}
pub const HAS_BREAKPOINT: bool = true;

/// All physical memory is mapped at `PHYS_OFFSET + pa` (TTBR1 half).
pub const PHYS_OFFSET: u64 = 0xFFFF_0000_0000_0000;
/// Kernel image: virtual = physical + KERNEL_OFFSET.
pub const KERNEL_OFFSET: u64 = 0xFFFF_FFFF_C008_0000 - 0x4008_0000;

pub fn phys_to_virt(pa: u64) -> u64 {
    pa + PHYS_OFFSET
}

pub fn console_write(b: &[u8]) {
    pl011::write(b)
}

pub fn init_memory(info: &BootInfo) {
    let mut dev = crate::device::virtio_regions(info);
    if let Some(c) = &info.console {
        dev.push(c.reg);
    }
    if let Some((_, r)) = crate::time::rtc(info) {
        dev.push(r);
    }
    if let Some((d, c)) = gic::find(info) {
        dev.push(d);
        dev.push(c);
    }
    let pt = mm::init(info, &dev);
    // User space (TTBR0) is empty until there are tasks.
    let user = mm::paging::PageTable::new();
    unsafe {
        asm!(
            "msr ttbr1_el1, {k}",
            "msr ttbr0_el1, {u}",
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            k = in(reg) pt.root,
            u = in(reg) user.root,
        );
    }
    unsafe {
        KERNEL_ROOT = pt.root;
        USER_EMPTY = user.root;
    }
    // 8-bit ASIDs (TCR_EL1.AS = 0), taken from TTBR0 (TCR_EL1.A1 = 0).
    mm::aspace::init_asids(255);
    let (used, total) = mm::frame::stats();
    println!(
        "paging: on (4 KB granule, 4 levels, 8-bit ASIDs, TTBR1 root {:#x}, {} tables, {}/{} frames used)",
        pt.root, pt.tables, used, total
    );
    core::mem::forget(pt);
    core::mem::forget(user);
}

/// Bytes of RAM the frame allocator manages (the root quota).
pub fn memory_total() -> u64 {
    mm::frame::stats().1 * mm::PAGE_SIZE
}

/// No host to talk to (until the network, M7).
pub fn remote_send(_i: u8, _b: &[u8]) -> Result<(), abi::Error> {
    Err(abi::Error::NotSupported)
}

pub fn remote_channel(_i: u8) -> Option<alloc::sync::Arc<crate::channel::Channel>> {
    None
}

pub fn irq_enable(n: u32) {
    gic::enable(n)
}

pub fn irq_disable(n: u32) {
    gic::disable(n)
}

pub fn irq_number(spec: &[u32; 3]) -> u32 {
    gic::number(spec)
}

pub fn init_traps() {
    trap::init();
}

pub fn init_irq(info: &BootInfo) {
    gic::init(info);
}

pub fn breakpoint() {
    unsafe { asm!("brk #0") }
}

pub fn timer_frequency(_info: &BootInfo) -> u64 {
    cntfrq()
}

fn cntfrq() -> u64 {
    let f: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f) };
    f
}

fn cntvct() -> u64 {
    let t: u64;
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) t) };
    t
}

pub fn now_ns() -> u64 {
    (cntvct() as u128 * 1_000_000_000 / cntfrq() as u128) as u64
}

static mut INTERVAL: u64 = 0;
/// Each CPU's next timer deadline.
static mut NEXT: [u64; MAX_CPUS] = [0; MAX_CPUS];

pub fn timer_start(hz: u64) {
    unsafe {
        INTERVAL = cntfrq() / hz;
        let next = cntvct() + INTERVAL;
        NEXT[cpu_id()] = next;
        asm!("msr cntv_cval_el0, {}", "msr cntv_ctl_el0, {}", in(reg) next, in(reg) 1u64);
    }
}

/// Virtual timer interrupt: schedule the next one.
fn timer_irq() {
    let cpu = cpu_id();
    unsafe {
        NEXT[cpu] += INTERVAL;
        asm!("msr cntv_cval_el0, {}", in(reg) NEXT[cpu]);
    }
    crate::time::tick(cpu);
}

pub fn enable_interrupts() {
    unsafe { asm!("msr daifclr, #2") }
}

/// Disables interrupts; returns whether they were enabled.
pub fn disable_interrupts() -> bool {
    let d: u64;
    unsafe { asm!("mrs {}, daif", "msr daifset, #2", out(reg) d) };
    d & (1 << 7) == 0
}

pub fn idle() -> ! {
    loop {
        unsafe { asm!("wfi") }
    }
}

pub fn shutdown(code: u32) -> ! {
    psci::system_off(code)
}
