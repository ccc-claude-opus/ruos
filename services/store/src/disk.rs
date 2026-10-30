//! The store's persistent log. Sector 0: `ROSDISK1`, `used BYTES`,
//! `boot N`. From sector 1: records appended one after another:
//! `obj ID LEN` + bytes + `\n`, and `ref NAME:TAG ID`. Replayed at boot.
//! The disk is the blk service (native) or the server's (wasm32).
use alloc::string::String;
use alloc::vec::Vec;

use libsys::blk::{Disk, MAX_SECTORS, SECTOR};
use libsys::store::{DISK_INFO, DISK_READ, DISK_WRITE, OK};
use libsys::{Channel, Error, Result};

pub enum Dev {
    Blk(Disk),
    /// The server, through the remote channel.
    Remote(Channel),
}

fn remote_call(c: Channel, msg: &[u8]) -> Result<Vec<u8>> {
    c.write(msg, &[])?;
    let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
    let (n, _) = c.recv(&mut buf, &mut [])?;
    if buf.first() != Some(&OK) {
        return Err(Error::Fault);
    }
    Ok(buf[1..n].to_vec())
}

impl Dev {
    fn sectors(&self) -> Result<u64> {
        match self {
            Dev::Blk(d) => d.sectors(),
            Dev::Remote(c) => {
                let r = remote_call(*c, &[DISK_INFO])?;
                Ok(u64::from_le_bytes(r.get(..8).ok_or(Error::Fault)?.try_into().unwrap()))
            }
        }
    }

    fn read(&self, sector: u64, count: usize) -> Result<Vec<u8>> {
        match self {
            Dev::Blk(d) => d.read(sector, count),
            Dev::Remote(c) => {
                let mut req = Vec::from(&[DISK_READ][..]);
                req.extend_from_slice(&sector.to_le_bytes());
                req.extend_from_slice(&(count as u32).to_le_bytes());
                remote_call(*c, &req)
            }
        }
    }

    fn write(&self, sector: u64, data: &[u8]) -> Result<()> {
        match self {
            Dev::Blk(d) => d.write(sector, data),
            Dev::Remote(c) => {
                let mut req = Vec::from(&[DISK_WRITE][..]);
                req.extend_from_slice(&sector.to_le_bytes());
                req.extend_from_slice(data);
                req.resize(9 + data.len().div_ceil(SECTOR) * SECTOR, 0);
                remote_call(*c, &req).map(|_| ())
            }
        }
    }
}

pub struct Log {
    dev: Dev,
    pub used: u64,
    pub boot: u64,
    pub sectors: u64,
    /// The last, partly written sector.
    tail: Vec<u8>,
}

pub enum Record {
    Obj(Vec<u8>),
    Ref(String, String),
}

impl Log {
    /// Opens (or formats) the log; returns its records.
    pub fn open(dev: Dev) -> Result<(Log, Vec<Record>)> {
        let sectors = dev.sectors()?;
        let sb = dev.read(0, 1)?;
        let text = core::str::from_utf8(&sb).unwrap_or("");
        let (mut used, mut boot) = (0u64, 0u64);
        if text.starts_with("ROSDISK1\n") {
            for l in text.lines() {
                if let Some(v) = l.strip_prefix("used ") {
                    used = v.parse().unwrap_or(0);
                }
                if let Some(v) = l.strip_prefix("boot ") {
                    boot = v.parse().unwrap_or(0);
                }
            }
        }
        let mut data = Vec::with_capacity(used as usize);
        let mut s = 1;
        while (data.len() as u64) < used {
            let left = (used - data.len() as u64).div_ceil(SECTOR as u64) as usize;
            let chunk = dev.read(s, left.min(MAX_SECTORS))?;
            s += (chunk.len() / SECTOR) as u64;
            data.extend_from_slice(&chunk);
        }
        data.truncate(used as usize);
        let records = parse(&data);
        let tail = data[data.len() - data.len() % SECTOR..].to_vec();
        let log = Log { dev, used, boot: boot + 1, sectors, tail };
        log.superblock()?;
        Ok((log, records))
    }

    fn superblock(&self) -> Result<()> {
        let sb = alloc::format!("ROSDISK1\nused {}\nboot {}\n", self.used, self.boot);
        self.dev.write(0, sb.as_bytes())
    }

    pub fn append(&mut self, rec: &[u8]) -> Result<()> {
        if (self.used + rec.len() as u64).div_ceil(SECTOR as u64) + 1 > self.sectors {
            return Err(Error::NoMemory);
        }
        let first = 1 + self.used / SECTOR as u64;
        let mut bytes = core::mem::take(&mut self.tail);
        bytes.extend_from_slice(rec);
        for (i, c) in bytes.chunks(SECTOR * MAX_SECTORS).enumerate() {
            self.dev.write(first + (i * MAX_SECTORS) as u64, c)?;
        }
        self.used += rec.len() as u64;
        self.tail = bytes[bytes.len() - bytes.len() % SECTOR..].to_vec();
        self.superblock()
    }
}

fn parse(mut d: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    while let Some(nl) = d.iter().position(|&c| c == b'\n') {
        let line = core::str::from_utf8(&d[..nl]).unwrap_or("");
        d = &d[nl + 1..];
        let w: Vec<&str> = line.split(' ').collect();
        match w.as_slice() {
            ["obj", _, len] => {
                let Ok(n) = len.parse::<usize>() else { break };
                if d.len() < n + 1 {
                    break;
                }
                out.push(Record::Obj(d[..n].to_vec()));
                d = &d[n + 1..];
            }
            ["ref", r, id] => out.push(Record::Ref(String::from(*r), String::from(*id))),
            _ => break,
        }
    }
    out
}
