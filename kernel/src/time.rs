//! Timer ticks and the wall clock.
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{HZ, arch, boot::BootInfo, dtb::Reg, selftest};

static TICKS: AtomicU64 = AtomicU64::new(0);
static START_NS: AtomicU64 = AtomicU64::new(0);
/// The wall clock (ns since 1970, UTC) at `START_NS`.
static EPOCH_NS: AtomicU64 = AtomicU64::new(0);

/// The real-time clock's registers: QEMU riscv64 `virt` has a Goldfish RTC
/// (ns), aarch64 `virt` a PL031 (seconds). The wasm machine has none: its
/// `/rtc` node carries the host's time in `time-ns` instead.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn rtc(info: &BootInfo) -> Option<(&'static str, Reg)> {
    let n = info.fdt.find_compatible(&["google,goldfish-rtc", "arm,pl031"])?;
    Some((n.compatible(), n.reg().next()?))
}

fn read_rtc(info: &BootInfo) -> Option<u64> {
    if let Some(v) = info.fdt.find_compatible(&["ruos,wasm-rtc"]).and_then(|n| n.prop("time-ns")) {
        return Some(u64::from_be_bytes(v.get(..8)?.try_into().ok()?));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (compat, reg) = rtc(info)?;
        let p = arch::phys_to_virt(reg.base) as *const u32;
        // Mapped by `mm::init` (direct map, device memory).
        unsafe {
            return Some(if compat == "arm,pl031" {
                p.read_volatile() as u64 * 1_000_000_000
            } else {
                // Reading the low word latches the high one.
                let lo = p.read_volatile() as u64;
                let hi = p.add(1).read_volatile() as u64;
                hi << 32 | lo
            });
        }
    }
    #[allow(unreachable_code)]
    None
}

pub fn start(info: &BootInfo) {
    let f = arch::timer_frequency(info);
    println!("timer: {} Hz clock, {} ticks per second", f, HZ);
    START_NS.store(arch::now_ns(), Ordering::Relaxed);
    match read_rtc(info) {
        Some(t) => {
            EPOCH_NS.store(t, Ordering::Relaxed);
            println!("rtc: {} s since 1970", t / 1_000_000_000);
        }
        None => {
            // 2026-01-01 stands in for the date.
            EPOCH_NS.store(1_767_225_600 * 1_000_000_000, Ordering::Relaxed);
            println!("rtc: none");
        }
    }
    arch::timer_start(HZ);
}

/// Called by the architecture's timer interrupt on every CPU; CPU 0 keeps
/// the tick count.
pub fn tick(cpu: usize) {
    if cpu == 0 {
        let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        selftest::on_tick(n);
    }
    crate::sched::tick(uptime_ns());
}

/// Nanoseconds since `start`.
pub fn uptime_ns() -> u64 {
    arch::now_ns() - START_NS.load(Ordering::Relaxed)
}

/// Nanoseconds since 1970 (UTC).
pub fn realtime_ns() -> u64 {
    EPOCH_NS.load(Ordering::Relaxed) + uptime_ns()
}
