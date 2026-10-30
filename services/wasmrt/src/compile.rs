//! Pre-decoding: each function body becomes an array of `Op`s with their
//! immediates decoded and every block's else / end resolved to an index,
//! so the interpreter never decodes LEB128 or searches for a block's end.
use alloc::vec::Vec;

use crate::module::{Module, Reader, Trap};

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Unreachable,
    Nop,
    Block { params: u16, results: u16, end: u32 },
    Loop { params: u16 },
    If { params: u16, results: u16, els: u32, end: u32 },
    Else { end: u32 },
    End,
    Br(u32),
    BrIf(u32),
    /// Targets in `Func::table[at..=at + n]`, the last one the default.
    BrTable { at: u32, n: u32 },
    Return,
    Call(u32),
    CallIndirect(u32),
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    /// The load / store opcode and its offset.
    Load(u8, u32),
    Store(u8, u32),
    MemSize,
    MemGrow,
    Const(u64),
    /// A numeric instruction without immediates (its opcode).
    Num(u8),
    /// 0xFC prefix: sub-opcode and up to two immediates.
    Fc(u8, u32),
    /// 0xFE prefix (atomics): sub-opcode and offset.
    Fe(u8, u32),
    /// ref.null (pushes the null reference).
    RefNull,
    RefIsNull,
    RefFunc(u32),
    /// Exception handling: a block with catch clauses
    /// (`Func::catches[at..at + n]`).
    TryTable { params: u16, results: u16, end: u32, at: u32, n: u32 },
    Throw(u32),
    ThrowRef,
}

/// A catch clause of a try_table: kind (0 catch, 1 catch_ref, 2 catch_all,
/// 3 catch_all_ref), tag, label (relative to the try_table's context).
#[derive(Clone, Copy, Debug)]
pub struct Catch {
    pub kind: u8,
    pub tag: u32,
    pub label: u32,
}

pub struct Func {
    pub ops: Vec<Op>,
    pub table: Vec<u32>,
    pub catches: Vec<Catch>,
}

fn memarg(r: &mut Reader) -> Result<u32, Trap> {
    r.u32()?;
    r.u32()
}

pub fn compile(m: &Module, start: usize, end: usize) -> Result<Func, Trap> {
    let mut f = Func { ops: Vec::new(), table: Vec::new(), catches: Vec::new() };
    let mut r = Reader { b: &m.bytes, p: start };
    let mut open: Vec<usize> = Vec::new();
    while r.p < end {
        let op = r.byte()?;
        let here = f.ops.len();
        let o = match op {
            0x00 => Op::Unreachable,
            0x01 => Op::Nop,
            0x02 | 0x03 | 0x04 => {
                let (p, res) = m.block_type(&mut r)?;
                let (p, res) = (p as u16, res as u16);
                open.push(here);
                match op {
                    0x02 => Op::Block { params: p, results: res, end: 0 },
                    0x03 => Op::Loop { params: p },
                    _ => Op::If { params: p, results: res, els: 0, end: 0 },
                }
            }
            0x1f => {
                let (p, res) = m.block_type(&mut r)?;
                let n = r.u32()?;
                let at = f.catches.len() as u32;
                for _ in 0..n {
                    let kind = r.byte()?;
                    let tag = if kind < 2 { r.u32()? } else { 0 };
                    let label = r.u32()?;
                    f.catches.push(Catch { kind, tag, label });
                }
                open.push(here);
                Op::TryTable { params: p as u16, results: res as u16, end: 0, at, n }
            }
            0x08 => Op::Throw(r.u32()?),
            0x0a => Op::ThrowRef,
            0x05 => {
                let b = *open.last().ok_or("else without if")?;
                if let Op::If { els, .. } = &mut f.ops[b] {
                    *els = here as u32;
                }
                Op::Else { end: 0 }
            }
            0x0b => {
                if let Some(b) = open.pop() {
                    let e = here as u32;
                    match &mut f.ops[b] {
                        Op::Block { end, .. } | Op::TryTable { end, .. } => *end = e,
                        Op::If { els, end, .. } => {
                            *end = e;
                            if *els != 0 {
                                let at = *els as usize;
                                f.ops[at] = Op::Else { end: e };
                            }
                        }
                        _ => {}
                    }
                }
                Op::End
            }
            0x0c => Op::Br(r.u32()?),
            0x0d => Op::BrIf(r.u32()?),
            0x0e => {
                let n = r.u32()?;
                let at = f.table.len() as u32;
                for _ in 0..=n {
                    let t = r.u32()?;
                    f.table.push(t);
                }
                Op::BrTable { at, n }
            }
            0x0f => Op::Return,
            0x10 => Op::Call(r.u32()?),
            0x11 => {
                let t = r.u32()?;
                r.u32()?;
                Op::CallIndirect(t)
            }
            0x1a => Op::Drop,
            0x1b => Op::Select,
            0x1c => {
                for _ in 0..r.u32()? {
                    r.byte()?;
                }
                Op::Select
            }
            0x20 => Op::LocalGet(r.u32()?),
            0x21 => Op::LocalSet(r.u32()?),
            0x22 => Op::LocalTee(r.u32()?),
            0x23 => Op::GlobalGet(r.u32()?),
            0x24 => Op::GlobalSet(r.u32()?),
            0x28..=0x35 => Op::Load(op, memarg(&mut r)?),
            0x36..=0x3e => Op::Store(op, memarg(&mut r)?),
            0x3f => {
                r.byte()?;
                Op::MemSize
            }
            0x40 => {
                r.byte()?;
                Op::MemGrow
            }
            0x41 => Op::Const(r.sleb(32)? as u32 as u64),
            0x42 => Op::Const(r.sleb(64)? as u64),
            0x43 => {
                let v = u32::from_le_bytes(r.b[r.p..r.p + 4].try_into().unwrap()) as u64;
                r.p += 4;
                Op::Const(v)
            }
            0x44 => {
                let v = u64::from_le_bytes(r.b[r.p..r.p + 8].try_into().unwrap());
                r.p += 8;
                Op::Const(v)
            }
            0x45..=0xc4 => Op::Num(op),
            0xd0 => {
                r.byte()?;
                Op::RefNull
            }
            0xd1 => Op::RefIsNull,
            0xd2 => Op::RefFunc(r.u32()?),
            0xfc => {
                let sub = r.u32()?;
                let imm = match sub {
                    0..=7 => 0,
                    8 => {
                        let seg = r.u32()?;
                        r.byte()?;
                        seg
                    }
                    9 => r.u32()?,
                    10 => {
                        r.byte()?;
                        r.byte()?;
                        0
                    }
                    11 => {
                        r.byte()?;
                        0
                    }
                    _ => return Err("unsupported 0xfc instruction"),
                };
                Op::Fc(sub as u8, imm)
            }
            0xfe => {
                let sub = r.u32()?;
                let off = match sub {
                    3 => {
                        r.byte()?;
                        0
                    }
                    0..=2 | 0x10..=0x4e => memarg(&mut r)?,
                    _ => return Err("unsupported atomic instruction"),
                };
                Op::Fe(sub as u8, off)
            }
            _ => return Err("unsupported instruction"),
        };
        f.ops.push(o);
    }
    Ok(f)
}
