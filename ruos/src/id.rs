//! Object ids, as the OS's object store makes them: the BLAKE3 hash of an
//! object's bytes, written as 64 lowercase hex digits (`b3:` may lead).
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Id(pub [u8; 32]);

impl Id {
    pub fn of(data: &[u8]) -> Id {
        Id(*blake3::hash(data).as_bytes())
    }

    pub fn parse(s: &str) -> Option<Id> {
        let s = s.strip_prefix("b3:").unwrap_or(s);
        if s.len() != 64 {
            return None;
        }
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Id(b))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
