//! Boot self-test: the lines `test.sh` checks on every platform.
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use crate::{HZ, arch, time};

static BREAKS: AtomicU64 = AtomicU64::new(0);

/// The timer line at TEST_TICKS, `M1 OK` at END_TICKS. (Console input is
/// tested through the console service and the shell, M3.)
const TEST_TICKS: u64 = 10;
const END_TICKS: u64 = 30;

/// A breakpoint trap that the handler steps over.
pub fn breakpoint() {
    if !arch::HAS_BREAKPOINT {
        println!("trap: breakpoints are not available on {}", arch::NAME);
        return;
    }
    arch::breakpoint();
    println!("trap: breakpoint handled ({} trap)", BREAKS.load(Ordering::Relaxed));
}

/// Called by the trap handler for a breakpoint exception.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn breakpoint_hit(pc: u64) {
    BREAKS.fetch_add(1, Ordering::Relaxed);
    println!("trap: breakpoint at {:#x}", pc);
}

pub fn on_tick(n: u64) {
    if n == TEST_TICKS {
        let ms = time::uptime_ns() / 1_000_000;
        println!("timer: {} ticks at {} Hz ({} ms)", n, HZ, ms);
    }
    if n == END_TICKS {
        println!("M1 OK");
        M1_DONE.store(true, Ordering::Relaxed);
        maybe_shutdown();
    }
}

static M1_DONE: AtomicBool = AtomicBool::new(false);
static INIT_EXIT: AtomicI64 = AtomicI64::new(i64::MIN);

/// init has exited: power off once the boot self-test is done too.
pub fn init_exited(code: i64) {
    INIT_EXIT.store(code, Ordering::Relaxed);
    maybe_shutdown();
}

fn maybe_shutdown() {
    let code = INIT_EXIT.load(Ordering::Relaxed);
    let no_init = crate::bootimg::find("init").is_none();
    if M1_DONE.load(Ordering::Relaxed) && (code != i64::MIN || no_init) {
        arch::shutdown(if code == 0 || no_init { 0 } else { 1 });
    }
}
