//! The system call instruction (or import) and the native entry point.

#[cfg(target_arch = "riscv64")]
pub unsafe fn syscall(nr: usize, a: [u64; 6]) -> i64 {
    let r: i64;
    unsafe {
        core::arch::asm!("ecall",
            inlateout("a0") a[0] => r, in("a1") a[1], in("a2") a[2],
            in("a3") a[3], in("a4") a[4], in("a5") a[5], in("a7") nr);
    }
    r
}

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    ".section .text._start, \"ax\"",
    ".global _start",
    "_start:",
    "    call ros_start",
);

#[cfg(target_arch = "aarch64")]
pub unsafe fn syscall(nr: usize, a: [u64; 6]) -> i64 {
    let r: i64;
    unsafe {
        core::arch::asm!("svc #0",
            inlateout("x0") a[0] => r, in("x1") a[1], in("x2") a[2],
            in("x3") a[3], in("x4") a[4], in("x5") a[5], in("x8") nr);
    }
    r
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".section .text._start, \"ax\"",
    ".global _start",
    "_start:",
    "    bl ros_start",
);

#[cfg(target_arch = "wasm32")]
mod host {
    #[link(wasm_import_module = "sys")]
    unsafe extern "C" {
        pub fn call(nr: u32, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64;
    }
}

#[cfg(target_arch = "wasm32")]
pub unsafe fn syscall(nr: usize, a: [u64; 6]) -> i64 {
    unsafe { host::call(nr as u32, a[0], a[1], a[2], a[3], a[4], a[5]) }
}
