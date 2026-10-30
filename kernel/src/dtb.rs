//! The device tree, read with the `fdt` crate, in the shape the kernel
//! uses it: nodes by path or `compatible`, properties, `reg` decoded with
//! the parent's cells.

#[derive(Clone, Copy)]
pub struct Fdt<'a>(fdt::Fdt<'a>);

#[derive(Clone, Copy)]
pub struct Node<'b, 'a>(fdt::node::FdtNode<'b, 'a>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reg {
    pub base: u64,
    pub size: u64,
}

impl Reg {
    pub fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// Big-endian cell array.
pub fn cells(v: &[u8]) -> impl Iterator<Item = u32> + '_ {
    v.chunks_exact(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
}

impl<'a> Fdt<'a> {
    /// # Safety
    /// `p` must point at a readable device tree blob.
    pub unsafe fn from_ptr(p: *const u8) -> Option<Fdt<'a>> {
        unsafe { fdt::Fdt::from_ptr(p) }.ok().map(Fdt)
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn new(data: &'a [u8]) -> Option<Fdt<'a>> {
        fdt::Fdt::new(data).ok().map(Fdt)
    }

    pub fn size(&self) -> usize {
        self.0.total_size()
    }

    pub fn model(&self) -> &'a str {
        self.0.root().model()
    }

    /// `/memreserve/` entries.
    pub fn mem_reserve(&self) -> impl Iterator<Item = Reg> + 'a {
        self.0.memory_reservations().map(|r| Reg { base: r.address() as u64, size: r.size() as u64 })
    }

    /// RAM from the memory nodes.
    pub fn memory(&self) -> impl Iterator<Item = Reg> + '_ {
        self.0
            .all_nodes()
            .filter(|n| n.property("device_type").and_then(|p| p.as_str()) == Some("memory"))
            .flat_map(|n| Node(n).reg())
    }

    pub fn nodes(&self) -> impl Iterator<Item = Node<'_, 'a>> {
        self.0.all_nodes().map(Node)
    }

    /// A node by absolute path (unit addresses may be left out).
    pub fn find(&self, path: &str) -> Option<Node<'_, 'a>> {
        self.0.find_node(path).map(Node)
    }

    pub fn find_compatible(&self, compat: &[&str]) -> Option<Node<'_, 'a>> {
        self.0.find_compatible(compat).map(Node)
    }

    /// The `/chosen` `stdout-path` node.
    pub fn stdout(&self) -> Option<Node<'_, 'a>> {
        self.0.chosen().stdout().map(Node)
    }
}

impl<'b, 'a> Node<'b, 'a> {
    pub fn prop(&self, name: &str) -> Option<&'a [u8]> {
        self.0.property(name).map(|p| p.value)
    }

    pub fn prop_u32(&self, name: &str) -> Option<u32> {
        let v = self.prop(name)?;
        (v.len() >= 4).then(|| u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
    }

    /// A 32- or 64-bit property.
    pub fn prop_u64(&self, name: &str) -> Option<u64> {
        self.0.property(name).and_then(|p| p.as_usize()).map(|v| v as u64)
    }

    pub fn prop_str(&self, name: &str) -> Option<&'a str> {
        self.0.property(name).and_then(|p| p.as_str())
    }

    pub fn is_compatible(&self, c: &str) -> bool {
        self.0.compatible().is_some_and(|x| x.all().any(|s| s == c))
    }

    /// The first `compatible` string.
    pub fn compatible(&self) -> &'a str {
        self.0.compatible().map(|c| c.first()).unwrap_or("")
    }

    pub fn reg(&self) -> impl Iterator<Item = Reg> + use<'b, 'a> {
        self.0
            .reg()
            .into_iter()
            .flatten()
            .map(|r| Reg { base: r.starting_address as u64, size: r.size.unwrap_or(0) as u64 })
    }

    pub fn children(&self, mut f: impl FnMut(&Node<'b, 'a>)) {
        for c in self.0.children() {
            f(&Node(c));
        }
    }
}
