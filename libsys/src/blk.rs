//! The block device protocol (served by `blk`; on wasm32 the store uses the
//! server's disk instead). Sectors are 512 bytes; at most `MAX_SECTORS`
//! per request. Requests: [INFO] → [OK, sectors u64]; [READ, sector u64,
//! count u32] → [OK, data]; [WRITE, sector u64, data] → [OK].
use alloc::vec::Vec;

use crate::wire::OK;
use crate::{Channel, Error, Result};

pub const INFO: u8 = 1;
pub const READ: u8 = 2;
pub const WRITE: u8 = 3;
pub const SECTOR: usize = 512;
pub const MAX_SECTORS: usize = 64;

#[derive(Clone, Copy)]
pub struct Disk(pub Channel);

impl Disk {
    fn call(&self, req: &[u8]) -> Result<Vec<u8>> {
        self.0.write(req, &[])?;
        let mut buf = alloc::vec![0u8; 1 + SECTOR * MAX_SECTORS];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        buf.truncate(n);
        if buf.first() != Some(&OK) {
            return Err(Error::Fault);
        }
        buf.remove(0);
        Ok(buf)
    }

    /// Size in sectors.
    pub fn sectors(&self) -> Result<u64> {
        let r = self.call(&[INFO])?;
        Ok(u64::from_le_bytes(r.get(..8).ok_or(Error::Fault)?.try_into().unwrap()))
    }

    pub fn read(&self, sector: u64, count: usize) -> Result<Vec<u8>> {
        let mut req = Vec::from(&[READ][..]);
        req.extend_from_slice(&sector.to_le_bytes());
        req.extend_from_slice(&(count as u32).to_le_bytes());
        self.call(&req)
    }

    /// Writes whole sectors (`data` is padded with zeros).
    pub fn write(&self, sector: u64, data: &[u8]) -> Result<()> {
        let mut req = Vec::from(&[WRITE][..]);
        req.extend_from_slice(&sector.to_le_bytes());
        req.extend_from_slice(data);
        req.resize(9 + data.len().div_ceil(SECTOR) * SECTOR, 0);
        self.call(&req).map(|_| ())
    }
}
