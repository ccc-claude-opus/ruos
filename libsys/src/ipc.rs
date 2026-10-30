//! Channels, ports, memory objects and the serial console.
use crate::{Handle, Packet, Result, call, check, nr};

/// Forever, for timeouts.
pub const FOREVER: u64 = u64::MAX;

/// Waits until any of `signals` is set on `h` (or `timeout_ns` passes);
/// returns the signals observed.
pub fn wait_one(h: Handle, signals: u32, timeout_ns: u64) -> Result<u32> {
    let mut seen = 0u32;
    check(call(nr::WAIT_ONE, [h.0 as u64, signals as u64, timeout_ns, &mut seen as *mut u32 as u64, 0, 0]))?;
    Ok(seen)
}

/// A channel endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel(pub Handle);

impl Channel {
    pub fn create() -> Result<(Channel, Channel)> {
        let mut h = [0u32; 2];
        check(call(nr::CHANNEL_CREATE, [h.as_mut_ptr() as u64, 0, 0, 0, 0, 0]))?;
        Ok((Channel(Handle(h[0])), Channel(Handle(h[1]))))
    }

    /// Sends `data`; `handles` move to the receiver.
    pub fn write(&self, data: &[u8], handles: &[Handle]) -> Result<()> {
        let a = [self.0.0 as u64, data.as_ptr() as u64, data.len() as u64, handles.as_ptr() as u64, handles.len() as u64, 0];
        check(call(nr::CHANNEL_WRITE, a)).map(|_| ())
    }

    /// Takes the next message if there is one: (bytes, handles).
    pub fn read(&self, buf: &mut [u8], handles: &mut [Handle]) -> Result<(usize, usize)> {
        let mut actual = [0u32; 2];
        let a = [
            self.0.0 as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            handles.as_mut_ptr() as u64,
            handles.len() as u64,
            actual.as_mut_ptr() as u64,
        ];
        check(call(nr::CHANNEL_READ, a))?;
        Ok((actual[0] as usize, actual[1] as usize))
    }

    /// Waits for a message (PeerClosed once the other end is gone).
    /// CHANNEL_RECV reads at once or waits; after a wait it is called again.
    pub fn recv(&self, buf: &mut [u8], handles: &mut [Handle]) -> Result<(usize, usize)> {
        let mut actual = [0u32; 2];
        let a = [
            self.0.0 as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            handles.as_mut_ptr() as u64,
            handles.len() as u64,
            actual.as_mut_ptr() as u64,
        ];
        loop {
            match check(call(nr::CHANNEL_RECV, a)) {
                Err(crate::Error::ShouldWait) => continue,
                r => {
                    r?;
                    return Ok((actual[0] as usize, actual[1] as usize));
                }
            }
        }
    }

    pub fn close(self) -> Result<()> {
        self.0.close()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port(pub Handle);

impl Port {
    pub fn create() -> Result<Port> {
        check(call(nr::PORT_CREATE, [0; 6])).map(|h| Port(Handle(h as u32)))
    }

    /// One shot: a packet with `key` arrives when any of `signals` is set on `h`.
    pub fn bind(&self, h: Handle, signals: u32, key: u64) -> Result<()> {
        check(call(nr::PORT_BIND, [self.0.0 as u64, h.0 as u64, signals as u64, key, 0, 0])).map(|_| ())
    }

    pub fn wait(&self, timeout_ns: u64) -> Result<Packet> {
        let mut p = Packet::default();
        check(call(nr::PORT_WAIT, [self.0.0 as u64, timeout_ns, &mut p as *mut Packet as u64, 0, 0, 0]))?;
        Ok(p)
    }

    pub fn queue(&self, key: u64, data: u64) -> Result<()> {
        check(call(nr::PORT_QUEUE, [self.0.0 as u64, key, data, 0, 0, 0])).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory(pub Handle);

impl Memory {
    pub fn create(size: u64) -> Result<Memory> {
        check(call(nr::MEMORY_CREATE, [size, 0, 0, 0, 0, 0])).map(|h| Memory(Handle(h as u32)))
    }

    pub fn read(&self, off: u64, buf: &mut [u8]) -> Result<()> {
        check(call(nr::MEMORY_READ, [self.0.0 as u64, off, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0])).map(|_| ())
    }

    pub fn write(&self, off: u64, buf: &[u8]) -> Result<()> {
        check(call(nr::MEMORY_WRITE, [self.0.0 as u64, off, buf.as_ptr() as u64, buf.len() as u64, 0, 0])).map(|_| ())
    }

    /// A copy-on-write clone.
    pub fn clone_cow(&self) -> Result<Memory> {
        check(call(nr::MEMORY_CLONE, [self.0.0 as u64, 0, 0, 0, 0, 0])).map(|h| Memory(Handle(h as u32)))
    }

    pub fn size(&self) -> Result<u64> {
        check(call(nr::MEMORY_SIZE, [self.0.0 as u64, 0, 0, 0, 0, 0]))
    }

    /// Maps `[off, off + len)` (`map::READ | map::WRITE`); native only.
    pub fn map(&self, off: u64, len: u64, flags: u64) -> Result<*mut u8> {
        check(call(nr::MEMORY_MAP, [self.0.0 as u64, off, len, flags, 0, 0])).map(|a| a as usize as *mut u8)
    }

    pub fn unmap(addr: *mut u8, len: u64) -> Result<()> {
        check(call(nr::MEMORY_UNMAP, [addr as usize as u64, len, 0, 0, 0, 0])).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Serial(pub Handle);

impl Serial {
    /// Bytes typed so far (ShouldWait when none).
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        check(call(nr::SERIAL_READ, [self.0.0 as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0])).map(|n| n as usize)
    }

    pub fn write(&self, buf: &[u8]) -> Result<()> {
        check(call(nr::SERIAL_WRITE, [self.0.0 as u64, buf.as_ptr() as u64, buf.len() as u64, 0, 0, 0])).map(|_| ())
    }
}

/// A device's registers (native).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mmio(pub Handle);

impl Mmio {
    /// Maps the registers; returns their address.
    pub fn map(&self) -> Result<*mut u8> {
        check(call(nr::MMIO_MAP, [self.0.0 as u64, 0, 0, 0, 0, 0])).map(|a| a as usize as *mut u8)
    }

    /// (physical address, size, virtio device id).
    pub fn info(&self) -> Result<(u64, u64, u32)> {
        let mut v = [0u64; 3];
        check(call(nr::MMIO_INFO, [self.0.0 as u64, v.as_mut_ptr() as u64, 0, 0, 0, 0]))?;
        Ok((v[0], v[1], v[2] as u32))
    }
}

/// A device interrupt: READABLE when it fired; masked until `ack`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Irq(pub Handle);

impl Irq {
    pub fn wait(&self, timeout_ns: u64) -> Result<()> {
        wait_one(self.0, crate::signal::READABLE, timeout_ns).map(|_| ())
    }

    pub fn ack(&self) -> Result<()> {
        check(call(nr::IRQ_ACK, [self.0.0 as u64, 0, 0, 0, 0, 0])).map(|_| ())
    }
}

impl Memory {
    /// Physically contiguous memory for DMA (native).
    pub fn create_dma(size: u64) -> Result<Memory> {
        check(call(nr::MEMORY_CREATE_DMA, [size, 0, 0, 0, 0, 0])).map(|h| Memory(Handle(h as u32)))
    }

    /// Physical address of byte `off`.
    pub fn phys(&self, off: u64) -> Result<u64> {
        check(call(nr::MEMORY_PHYS, [self.0.0 as u64, off, 0, 0, 0, 0]))
    }
}
