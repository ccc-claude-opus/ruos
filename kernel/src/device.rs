//! Devices for user-space drivers (native): `Mmio` (a device's registers,
//! mapped into the driver) and `Irq` (its interrupt, a signal). The kernel
//! finds virtio-mmio devices in the DTB and hands them to init.
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::signal;

use crate::arch;
use crate::boot::{BootInfo, Device};
use crate::dtb::Reg;
use crate::signal::SignalCell;
use crate::sync::IrqLock;

pub struct Mmio {
    pub reg: Reg,
    /// virtio device id (1 net, 2 block, ...), 0 if not virtio.
    pub device_id: u32,
}

pub struct Irq {
    pub num: u32,
    pub sig: SignalCell,
}

static IRQS: IrqLock<BTreeMap<u32, Arc<Irq>>> = IrqLock::new(BTreeMap::new());

const VIRTIO_MAGIC: u32 = 0x7472_6976;

/// The virtio-mmio slots in the DTB (to map them in the kernel).
pub fn virtio_regions(info: &BootInfo) -> Vec<Reg> {
    info.fdt.nodes().filter(|n| n.is_compatible("virtio,mmio")).filter_map(|n| n.reg().next()).collect()
}

/// Virtio devices that are present, with their interrupts registered.
pub fn discover(info: &BootInfo) -> Vec<(Arc<Mmio>, Arc<Irq>)> {
    let found: Vec<Device> =
        info.fdt.nodes().filter(|n| n.is_compatible("virtio,mmio")).filter_map(|n| Device::from_node(&n)).collect();
    let mut out = Vec::new();
    for d in found {
        let regs = arch::phys_to_virt(d.reg.base) as *const u32;
        let (magic, id) = unsafe { (regs.read_volatile(), regs.add(2).read_volatile()) };
        if magic != VIRTIO_MAGIC || id == 0 {
            continue;
        }
        let num = arch::irq_number(&d.irq);
        let irq = Arc::new(Irq { num, sig: SignalCell::new(0) });
        IRQS.lock().insert(num, irq.clone());
        arch::irq_enable(num);
        out.push((Arc::new(Mmio { reg: d.reg, device_id: id }), irq));
    }
    out
}

/// From the interrupt controller: a driver's interrupt. It stays masked
/// until the driver acknowledges it.
pub fn dispatch(num: u32) -> bool {
    let Some(irq) = IRQS.lock().get(&num).cloned() else { return false };
    arch::irq_disable(num);
    irq.sig.update(0, signal::READABLE);
    true
}

impl Irq {
    pub fn ack(&self) {
        self.sig.update(signal::READABLE, 0);
        arch::irq_enable(self.num);
    }
}
