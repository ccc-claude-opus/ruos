//! What the kernel learns from the device tree, in a form shared by all
//! architectures.
use crate::dtb::{Fdt, Node, Reg};

pub const MAX_REGIONS: usize = 16;

/// A small fixed list of physical ranges.
#[derive(Clone, Copy)]
pub struct Regions {
    r: [Reg; MAX_REGIONS],
    n: usize,
}

impl Regions {
    pub const fn new() -> Self {
        Regions { r: [Reg { base: 0, size: 0 }; MAX_REGIONS], n: 0 }
    }

    pub fn push(&mut self, r: Reg) {
        if r.size != 0 && self.n < MAX_REGIONS {
            self.r[self.n] = r;
            self.n += 1;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Reg> {
        self.r[..self.n].iter()
    }

    /// Removes `[x.base, x.end())` from every range.
    pub fn subtract(&mut self, x: Reg) {
        let old = *self;
        self.n = 0;
        for r in old.iter() {
            if x.end() <= r.base || x.base >= r.end() {
                self.push(*r);
                continue;
            }
            if x.base > r.base {
                self.push(Reg { base: r.base, size: x.base - r.base });
            }
            if x.end() < r.end() {
                self.push(Reg { base: x.end(), size: r.end() - x.end() });
            }
        }
    }
}

/// A device: its first `compatible`, first `reg` and raw `interrupts` cells.
#[derive(Clone, Copy)]
pub struct Device {
    pub compatible: &'static str,
    pub reg: Reg,
    pub irq: [u32; 3],
}

impl Device {
    pub fn from_node(n: &Node<'_, 'static>) -> Option<Device> {
        let reg = n.reg().next()?;
        let mut irq = [0; 3];
        if let Some(v) = n.prop("interrupts") {
            for (i, c) in crate::dtb::cells(v).take(3).enumerate() {
                irq[i] = c;
            }
        }
        Some(Device { compatible: n.compatible(), reg, irq })
    }
}

pub struct BootInfo {
    pub fdt: Fdt<'static>,
    pub dtb_pa: u64,
    pub dtb_size: usize,
    pub model: &'static str,
    memory: Regions,
    reserved: Regions,
    pub console: Option<Device>,
    /// `/cpus` `timebase-frequency`, if the tree has one.
    pub timebase: Option<u64>,
}

impl BootInfo {
    pub fn new(fdt: Fdt<'static>, dtb_pa: u64) -> BootInfo {
        let model = fdt.model();
        let mut memory = Regions::new();
        let mut reserved = Regions::new();
        fdt.memory().for_each(|r| memory.push(r));
        for r in fdt.mem_reserve() {
            reserved.push(r);
        }
        if let Some(rm) = fdt.find("/reserved-memory") {
            rm.children(|c| c.reg().for_each(|r| reserved.push(r)));
        }
        let console = fdt.stdout().as_ref().and_then(Device::from_node);
        let timebase = fdt.find("/cpus").and_then(|c| c.prop_u64("timebase-frequency"));
        BootInfo {
            fdt,
            dtb_pa,
            dtb_size: fdt.size(),
            model,
            memory,
            reserved,
            console,
            timebase,
        }
    }

    pub fn memory(&self) -> impl Iterator<Item = &Reg> {
        self.memory.iter()
    }

    pub fn reserved(&self) -> impl Iterator<Item = &Reg> {
        self.reserved.iter()
    }

    /// RAM minus reserved ranges, the DTB and `kernel`, page aligned.
    pub fn usable(&self, kernel: Reg) -> Regions {
        let mut u = Regions::new();
        for r in self.memory() {
            let b = (r.base + 0xfff) & !0xfff;
            let e = r.end() & !0xfff;
            if e > b {
                u.push(Reg { base: b, size: e - b });
            }
        }
        for r in self.reserved() {
            u.subtract(page_out(*r));
        }
        u.subtract(page_out(Reg { base: self.dtb_pa, size: self.dtb_size as u64 }));
        u.subtract(page_out(kernel));
        u
    }
}

/// Grows a range to page boundaries.
fn page_out(r: Reg) -> Reg {
    let b = r.base & !0xfff;
    let e = (r.end() + 0xfff) & !0xfff;
    Reg { base: b, size: e - b }
}
