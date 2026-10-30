//! Random numbers for user space (`RANDOM`): ChaCha20 (`rand_chacha`)
//! seeded from the boot loader's entropy, `/chosen` `rng-seed` (QEMU puts
//! 32 bytes from the host there; the wasm machine fills it from
//! `crypto.getRandomValues`), mixed with the clock.
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};

use crate::boot::BootInfo;
use crate::sync::IrqLock;

static RNG: IrqLock<Option<ChaCha20Rng>> = IrqLock::new(None);

pub fn init(info: &BootInfo) {
    let mut seed = [0u8; 32];
    let given = info.fdt.find("/chosen").and_then(|c| c.prop("rng-seed")).unwrap_or(&[]);
    for (i, b) in given.iter().enumerate() {
        seed[i % 32] ^= b;
    }
    // Without a seed from the host this is only as good as the clock.
    for (i, b) in crate::arch::now_ns().to_le_bytes().iter().enumerate() {
        seed[24 + i] ^= b;
    }
    println!("random: {} bytes of seed from the boot loader", given.len());
    *RNG.lock() = Some(ChaCha20Rng::from_seed(seed));
}

pub fn fill(buf: &mut [u8]) {
    if let Some(r) = RNG.lock().as_mut() {
        r.fill_bytes(buf);
    }
}
