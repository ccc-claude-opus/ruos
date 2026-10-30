//! PSCI power management through the conduit named in the DTB.
use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::boot::BootInfo;

const SYSTEM_OFF: u64 = 0x8400_0008;
const CPU_ON: u64 = 0xC400_0003;

static SMC: AtomicBool = AtomicBool::new(false);

pub fn init(info: &BootInfo) {
    let m = info.fdt.find_compatible(&["arm,psci-1.0", "arm,psci-0.2", "arm,psci"]);
    SMC.store(m.and_then(|n| n.prop_str("method")) == Some("smc"), Ordering::Relaxed);
}

fn call(f: u64) {
    unsafe {
        if SMC.load(Ordering::Relaxed) {
            asm!("smc #0", inlateout("x0") f => _, options(nomem, nostack));
        } else {
            asm!("hvc #0", inlateout("x0") f => _, options(nomem, nostack));
        }
    }
}

/// Starts CPU `mpidr` at physical `entry` with x0 = `ctx`; 0 on success.
pub fn cpu_on(mpidr: u64, entry: u64, ctx: u64) -> i64 {
    let r: i64;
    unsafe {
        if SMC.load(Ordering::Relaxed) {
            asm!("smc #0", inlateout("x0") CPU_ON => r, in("x1") mpidr, in("x2") entry, in("x3") ctx, options(nostack));
        } else {
            asm!("hvc #0", inlateout("x0") CPU_ON => r, in("x1") mpidr, in("x2") entry, in("x3") ctx, options(nostack));
        }
    }
    r
}

/// QEMU exits; the test scripts read the result from the console.
pub fn system_off(_code: u32) -> ! {
    call(SYSTEM_OFF);
    loop {
        unsafe { asm!("wfi") }
    }
}
