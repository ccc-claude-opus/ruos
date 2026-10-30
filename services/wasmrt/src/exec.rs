//! The interpreter over pre-decoded functions (compile.rs). Values are
//! 64-bit words (i32 zero-extended, floats as bits); frames and labels live
//! on explicit stacks, not the native one.
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};

use crate::compile::Op;
use crate::module::{Data, Module, Trap};

/// The linear memory, shared by every instance (thread) of a module. A
/// shared memory is allocated at its maximum and never moves; a module's
/// own (WASI, one thread) memory grows by reallocation.
pub struct Mem {
    pub base: AtomicPtr<u8>,
    pub size: AtomicUsize,
    pub max: usize,
    pub growable: bool,
    /// Passive data segments already dropped.
    pub dropped: AtomicU64,
}

unsafe impl Send for Mem {}
unsafe impl Sync for Mem {}

const ALIGN: usize = 16;

impl Mem {
    /// `size` zeroed bytes now, up to `max` later; None if out of memory.
    pub fn new(size: usize, max: usize, growable: bool) -> Option<Mem> {
        let n = if growable { size } else { max };
        let base = if n == 0 {
            ALIGN as *mut u8
        } else {
            let p = unsafe { alloc::alloc::alloc_zeroed(Layout::from_size_align(n, ALIGN).ok()?) };
            if p.is_null() {
                return None;
            }
            p
        };
        Some(Mem { base: AtomicPtr::new(base), size: AtomicUsize::new(size), max, growable, dropped: AtomicU64::new(0) })
    }

    /// Native address of `[addr, addr + len)`.
    pub fn at(&self, addr: u64, len: u64) -> Result<*mut u8, Trap> {
        if addr.checked_add(len).is_none_or(|e| e > self.size.load(SeqCst) as u64) {
            return Err("memory access out of bounds");
        }
        Ok(unsafe { self.base.load(SeqCst).add(addr as usize) })
    }

    /// Adds `n` bytes; the old size, or None.
    pub fn grow(&self, n: usize) -> Option<usize> {
        let old = self.size.load(SeqCst);
        if old + n > self.max {
            return None;
        }
        if n == 0 {
            return Some(old);
        }
        if !self.growable {
            return self.size.compare_exchange(old, old + n, SeqCst, SeqCst).ok();
        }
        let p = self.base.load(SeqCst);
        let q = unsafe {
            if old == 0 {
                alloc::alloc::alloc_zeroed(Layout::from_size_align(n, ALIGN).ok()?)
            } else {
                let q = alloc::alloc::realloc(p, Layout::from_size_align(old, ALIGN).ok()?, old + n);
                if !q.is_null() {
                    core::ptr::write_bytes(q.add(old), 0, n);
                }
                q
            }
        };
        if q.is_null() {
            return None;
        }
        self.base.store(q, SeqCst);
        self.size.store(old + n, SeqCst);
        Some(old)
    }
}

/// What imported functions do.
pub trait Host {
    fn call(&self, inst: &mut Instance, import: usize, args: &[u64]) -> Result<Option<u64>, Trap>;

    /// Called now and then (every TICK branches and calls), between
    /// instructions: signals for a program that makes no calls.
    fn poll(&self, _inst: &mut Instance) -> Result<(), Trap> {
        Ok(())
    }
}

/// How often `Host::poll` runs (branches and calls).
const TICK: u32 = 1 << 16;

pub struct Instance {
    pub m: Arc<Module>,
    pub mem: Arc<Mem>,
    pub globals: Vec<u64>,
    /// Exceptions thrown (an exnref is an index here; they are not freed).
    exns: Vec<(u32, Vec<u64>)>,
}

struct Frame {
    func: usize,
    pc: usize,
    locals: usize,
    labels: usize,
    height: usize,
    arity: usize,
}

struct Label {
    cont: usize,
    height: usize,
    arity: usize,
    /// A loop's label stays when branched to (its continuation is its start).
    is_loop: bool,
    /// A try_table's catch clauses: (first, count) in `Func::catches`.
    catches: (u32, u32),
}

fn f32v(x: u64) -> f32 {
    f32::from_bits(x as u32)
}

fn f64v(x: u64) -> f64 {
    f64::from_bits(x)
}

fn bf32(x: f32) -> u64 {
    x.to_bits() as u64
}

fn bf64(x: f64) -> u64 {
    x.to_bits()
}

fn fmin64(a: f64, b: f64, min: bool) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        let (x, y) = (a.to_bits(), b.to_bits());
        return f64::from_bits(if min { x | y } else { x & y });
    }
    if (a < b) == min { a } else { b }
}

fn fmin32(a: f32, b: f32, min: bool) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a == b {
        let (x, y) = (a.to_bits(), b.to_bits());
        return f32::from_bits(if min { x | y } else { x & y });
    }
    if (a < b) == min { a } else { b }
}

/// Float → integer that traps (wasm `trunc`): the value must fit.
fn trunc(v: f64, lo: f64, hi: f64) -> Result<f64, Trap> {
    if v.is_nan() {
        return Err("invalid conversion to integer");
    }
    let t = core::f64::math::trunc(v);
    if t < lo || t >= hi {
        return Err("integer overflow");
    }
    Ok(t)
}

impl Instance {
    pub fn new(m: Arc<Module>, mem: Arc<Mem>) -> Instance {
        let globals = m.globals.iter().map(|(v, _)| *v).collect();
        Instance { m, mem, globals, exns: Vec::new() }
    }

    /// Calls function `f` with `args`; its results.
    pub fn invoke(&mut self, host: &dyn Host, f: u32, args: &[u64]) -> Result<Vec<u64>, Trap> {
        let mut stack: Vec<u64> = Vec::with_capacity(1024);
        stack.extend_from_slice(args);
        self.run(host, f as usize, &mut stack)?;
        Ok(stack)
    }

    fn run(&mut self, host: &dyn Host, entry: usize, stack: &mut Vec<u64>) -> Result<(), Trap> {
        let m = self.m.clone();
        let nimp = m.imports.len();
        let mut frames: Vec<Frame> = Vec::new();
        let mut labels: Vec<Label> = Vec::new();
        let mut locals: Vec<u64> = Vec::new();

        macro_rules! pop {
            () => {
                stack.pop().ok_or("stack underflow")?
            };
        }
        macro_rules! push {
            ($v:expr) => {
                stack.push($v)
            };
        }
        let mut ticks = TICK;
        macro_rules! tick {
            () => {{
                ticks -= 1;
                if ticks == 0 {
                    ticks = TICK;
                    host.poll(self)?;
                }
            }};
        }

        // Enters function `f` (its arguments are on the stack), or calls an import.
        let enter = |f: usize, stack: &mut Vec<u64>, frames: &mut Vec<Frame>, locals: &mut Vec<u64>, labels: &Vec<Label>, inst: &mut Instance| -> Result<bool, Trap> {
            let ty = &m.types[*m.funcs.get(f).ok_or("bad function")? as usize];
            if f < nimp {
                let at = stack.len().checked_sub(ty.params).ok_or("stack underflow")?;
                let args: Vec<u64> = stack.drain(at..).collect();
                if let Some(v) = host.call(inst, f, &args)? {
                    stack.push(v);
                }
                return Ok(false);
            }
            if frames.len() > 10_000 {
                return Err("call stack exhausted");
            }
            let c = &m.code[f - nimp];
            let at = stack.len().checked_sub(ty.params).ok_or("stack underflow")?;
            let base = locals.len();
            locals.extend(stack.drain(at..));
            locals.resize(base + ty.params + c.locals, 0);
            frames.push(Frame { func: f, pc: 0, locals: base, labels: labels.len(), height: stack.len(), arity: ty.results });
            Ok(true)
        };

        if !enter(entry, stack, &mut frames, &mut locals, &labels, self)? {
            return Ok(());
        }
        let mut code = m.func(entry - nimp)?;
        let mut pc = 0usize;
        let mut lbase = 0usize;

        // Leaves the current function with its results.
        macro_rules! ret {
            () => {{
                let fr = frames.pop().unwrap();
                let at = stack.len() - fr.arity;
                let res: Vec<u64> = stack.drain(at..).collect();
                stack.truncate(fr.height);
                stack.extend(res);
                locals.truncate(fr.locals);
                labels.truncate(fr.labels);
                match frames.last() {
                    Some(f) => {
                        code = m.func(f.func - nimp)?;
                        pc = f.pc;
                        lbase = f.locals;
                    }
                    None => return Ok(()),
                }
            }};
        }
        // Branch to label `n` (counting outwards).
        macro_rules! br {
            ($n:expr) => {{
                let n = $n as usize;
                let fl = frames.last().unwrap().labels;
                if n >= labels.len() - fl {
                    ret!();
                } else {
                    let i = labels.len() - 1 - n;
                    let (cont, height, arity, is_loop) = (labels[i].cont, labels[i].height, labels[i].arity, labels[i].is_loop);
                    if stack.len() - arity != height {
                        let at = stack.len() - arity;
                        stack.copy_within(at.., height);
                        stack.truncate(height + arity);
                    }
                    labels.truncate(if is_loop { i + 1 } else { i });
                    pc = cont;
                }
            }};
        }
        macro_rules! load {
            ($off:expr, $t:ty, $conv:expr) => {{
                let a = pop!() as u32 as u64 + $off as u64;
                let p = self.mem.at(a, core::mem::size_of::<$t>() as u64)?;
                let v = unsafe { (p as *const $t).read_unaligned() };
                push!($conv(v))
            }};
        }
        macro_rules! store {
            ($off:expr, $t:ty) => {{
                let v = pop!();
                let a = pop!() as u32 as u64 + $off as u64;
                let p = self.mem.at(a, core::mem::size_of::<$t>() as u64)?;
                unsafe { (p as *mut $t).write_unaligned(v as $t) };
            }};
        }
        macro_rules! bin32 {
            ($f:expr) => {{
                let y = pop!() as u32;
                let x = pop!() as u32;
                push!($f(x, y) as u32 as u64)
            }};
        }
        macro_rules! bin64 {
            ($f:expr) => {{
                let y = pop!();
                let x = pop!();
                push!($f(x, y) as u64)
            }};
        }
        macro_rules! cmp32 {
            ($f:expr) => {{
                let y = pop!() as u32;
                let x = pop!() as u32;
                push!($f(x, y) as u64)
            }};
        }
        macro_rules! cmp64 {
            ($f:expr) => {{
                let y = pop!();
                let x = pop!();
                push!($f(x, y) as u64)
            }};
        }
        macro_rules! f32bin {
            ($f:expr) => {{
                let y = f32v(pop!());
                let x = f32v(pop!());
                push!(bf32($f(x, y)))
            }};
        }
        macro_rules! f64bin {
            ($f:expr) => {{
                let y = f64v(pop!());
                let x = f64v(pop!());
                push!(bf64($f(x, y)))
            }};
        }
        macro_rules! f32cmp {
            ($f:expr) => {{
                let y = f32v(pop!());
                let x = f32v(pop!());
                push!($f(x, y) as u64)
            }};
        }
        macro_rules! f64cmp {
            ($f:expr) => {{
                let y = f64v(pop!());
                let x = f64v(pop!());
                push!($f(x, y) as u64)
            }};
        }

        loop {
            let op = code.ops[pc];
            pc += 1;
            match op {
                Op::Unreachable => return Err("unreachable"),
                Op::Nop => {}
                Op::Block { params, results, end } => {
                    let height = stack.len() - params as usize;
                    labels.push(Label { cont: end as usize + 1, height, arity: results as usize, is_loop: false, catches: (0, 0) });
                }
                Op::TryTable { params, results, end, at, n } => {
                    let height = stack.len() - params as usize;
                    labels.push(Label { cont: end as usize + 1, height, arity: results as usize, is_loop: false, catches: (at, n) });
                }
                Op::Throw(_) | Op::ThrowRef => {
                    let exn = match op {
                        Op::Throw(tag) => {
                            let n = *m.tags.get(tag as usize).ok_or("bad tag")?;
                            let at = stack.len().checked_sub(n).ok_or("stack underflow")?;
                            let vals: Vec<u64> = stack.drain(at..).collect();
                            self.exns.push((tag, vals));
                            self.exns.len() - 1
                        }
                        _ => {
                            let r = pop!();
                            if r == u64::MAX {
                                return Err("null exception reference");
                            }
                            r as usize
                        }
                    };
                    let (tag, vals) = self.exns.get(exn).cloned().ok_or("bad exception reference")?;
                    // The innermost try_table (in this function or a caller) that catches it.
                    let mut found = None;
                    let mut i = labels.len();
                    while i > 0 && found.is_none() {
                        i -= 1;
                        let (at, n) = labels[i].catches;
                        if n == 0 {
                            continue;
                        }
                        // The function the label belongs to: its catch table.
                        let fi = frames.iter().rposition(|f| f.labels <= i).ok_or("bad label")?;
                        let fc = m.func(frames[fi].func - nimp)?;
                        for c in &fc.catches[at as usize..(at + n) as usize] {
                            if c.kind >= 2 || c.tag == tag {
                                found = Some((i, fi, *c));
                                break;
                            }
                        }
                    }
                    let Some((i, fi, c)) = found else { return Err("uncaught exception") };
                    // Leave the frames above, then the try_table: its height,
                    // the payload, a branch to the catch's label.
                    if let Some(next) = frames.get(fi + 1) {
                        locals.truncate(next.locals);
                    }
                    frames.truncate(fi + 1);
                    let fr = frames.last().unwrap();
                    code = m.func(fr.func - nimp)?;
                    lbase = fr.locals;
                    stack.truncate(labels[i].height);
                    labels.truncate(i);
                    if c.kind < 2 {
                        stack.extend_from_slice(&vals);
                    }
                    if c.kind == 1 || c.kind == 3 {
                        push!(exn as u64);
                    }
                    br!(c.label);
                }
                Op::Loop { params } => {
                    let height = stack.len() - params as usize;
                    labels.push(Label { cont: pc, height, arity: params as usize, is_loop: true, catches: (0, 0) });
                }
                Op::If { params, results, els, end } => {
                    let c = pop!() as u32;
                    let height = stack.len() - params as usize;
                    let l = Label { cont: end as usize + 1, height, arity: results as usize, is_loop: false, catches: (0, 0) };
                    if c != 0 {
                        labels.push(l);
                    } else if els != 0 {
                        labels.push(l);
                        pc = els as usize + 1;
                    } else {
                        pc = end as usize + 1;
                    }
                }
                // End of an if's then-branch: on past the else-branch.
                Op::Else { end } => {
                    labels.pop();
                    pc = end as usize + 1;
                }
                Op::End => {
                    if labels.len() > frames.last().unwrap().labels {
                        labels.pop();
                    } else {
                        ret!();
                    }
                }
                Op::Br(n) => {
                    tick!();
                    br!(n)
                }
                Op::BrIf(n) => {
                    if pop!() as u32 != 0 {
                        tick!();
                        br!(n);
                    }
                }
                Op::BrTable { at, n } => {
                    let i = (pop!() as u32).min(n);
                    let t = code.table[(at + i) as usize];
                    br!(t);
                }
                Op::Return => ret!(),
                Op::Call(_) | Op::CallIndirect(_) => {
                    tick!();
                    let f = match op {
                        Op::Call(f) => f as usize,
                        Op::CallIndirect(ty) => {
                            let i = pop!() as u32 as usize;
                            let f = *m.table.get(i).ok_or("undefined table element")?;
                            if f == u32::MAX {
                                return Err("uninitialized table element");
                            }
                            if m.types[m.funcs[f as usize] as usize] != m.types[ty as usize] {
                                return Err("indirect call type mismatch");
                            }
                            f as usize
                        }
                        _ => unreachable!(),
                    };
                    frames.last_mut().unwrap().pc = pc;
                    if enter(f, stack, &mut frames, &mut locals, &labels, self)? {
                        let fr = frames.last().unwrap();
                        code = m.func(fr.func - nimp)?;
                        pc = 0;
                        lbase = fr.locals;
                    }
                }
                Op::Drop => {
                    pop!();
                }
                Op::Select => {
                    let c = pop!() as u32;
                    let y = pop!();
                    let x = pop!();
                    push!(if c != 0 { x } else { y });
                }
                Op::LocalGet(i) => push!(locals[lbase + i as usize]),
                Op::LocalSet(i) => {
                    let v = pop!();
                    locals[lbase + i as usize] = v;
                }
                Op::LocalTee(i) => {
                    let v = *stack.last().ok_or("stack underflow")?;
                    locals[lbase + i as usize] = v;
                }
                Op::GlobalGet(i) => push!(self.globals[i as usize]),
                Op::GlobalSet(i) => {
                    self.globals[i as usize] = pop!();
                }
                Op::Load(op, off) => match op {
                    0x28 => load!(off, u32, |v: u32| v as u64),
                    0x29 => load!(off, u64, |v: u64| v),
                    0x2a => load!(off, u32, |v: u32| v as u64),
                    0x2b => load!(off, u64, |v: u64| v),
                    0x2c => load!(off, i8, |v: i8| v as i32 as u32 as u64),
                    0x2d => load!(off, u8, |v: u8| v as u64),
                    0x2e => load!(off, i16, |v: i16| v as i32 as u32 as u64),
                    0x2f => load!(off, u16, |v: u16| v as u64),
                    0x30 => load!(off, i8, |v: i8| v as i64 as u64),
                    0x31 => load!(off, u8, |v: u8| v as u64),
                    0x32 => load!(off, i16, |v: i16| v as i64 as u64),
                    0x33 => load!(off, u16, |v: u16| v as u64),
                    0x34 => load!(off, i32, |v: i32| v as i64 as u64),
                    _ => load!(off, u32, |v: u32| v as u64),
                },
                Op::Store(op, off) => match op {
                    0x36 | 0x38 | 0x3e => store!(off, u32),
                    0x37 | 0x39 => store!(off, u64),
                    0x3a | 0x3c => store!(off, u8),
                    _ => store!(off, u16),
                },
                Op::MemSize => push!((self.mem.size.load(SeqCst) / 65536) as u64),
                Op::MemGrow => {
                    let n = pop!() as u32 as usize * 65536;
                    push!(match self.mem.grow(n) {
                        Some(old) => (old / 65536) as u64,
                        None => u32::MAX as u64,
                    });
                }
                Op::Const(v) => push!(v),
                Op::RefNull => push!(u64::MAX),
                Op::RefIsNull => {
                    let x = pop!();
                    push!((x == u64::MAX) as u64)
                }
                Op::RefFunc(f) => push!(f as u64),
                Op::Fc(sub, imm) => self.op_fc(sub, imm, stack)?,
                Op::Fe(sub, off) => self.op_fe(sub, off, stack)?,
                Op::Num(op) => match op {
                    0x45 => {
                        let x = pop!() as u32;
                        push!((x == 0) as u64)
                    }
                    0x46 => cmp32!(|x, y| x == y),
                    0x47 => cmp32!(|x, y| x != y),
                    0x48 => cmp32!(|x: u32, y: u32| (x as i32) < (y as i32)),
                    0x49 => cmp32!(|x, y| x < y),
                    0x4a => cmp32!(|x: u32, y: u32| (x as i32) > (y as i32)),
                    0x4b => cmp32!(|x, y| x > y),
                    0x4c => cmp32!(|x: u32, y: u32| (x as i32) <= (y as i32)),
                    0x4d => cmp32!(|x, y| x <= y),
                    0x4e => cmp32!(|x: u32, y: u32| (x as i32) >= (y as i32)),
                    0x4f => cmp32!(|x, y| x >= y),
                    0x50 => {
                        let x = pop!();
                        push!((x == 0) as u64)
                    }
                    0x51 => cmp64!(|x, y| x == y),
                    0x52 => cmp64!(|x, y| x != y),
                    0x53 => cmp64!(|x: u64, y: u64| (x as i64) < (y as i64)),
                    0x54 => cmp64!(|x, y| x < y),
                    0x55 => cmp64!(|x: u64, y: u64| (x as i64) > (y as i64)),
                    0x56 => cmp64!(|x, y| x > y),
                    0x57 => cmp64!(|x: u64, y: u64| (x as i64) <= (y as i64)),
                    0x58 => cmp64!(|x, y| x <= y),
                    0x59 => cmp64!(|x: u64, y: u64| (x as i64) >= (y as i64)),
                    0x5a => cmp64!(|x, y| x >= y),
                    0x5b => f32cmp!(|x, y| x == y),
                    0x5c => f32cmp!(|x, y| x != y),
                    0x5d => f32cmp!(|x, y| x < y),
                    0x5e => f32cmp!(|x, y| x > y),
                    0x5f => f32cmp!(|x, y| x <= y),
                    0x60 => f32cmp!(|x, y| x >= y),
                    0x61 => f64cmp!(|x, y| x == y),
                    0x62 => f64cmp!(|x, y| x != y),
                    0x63 => f64cmp!(|x, y| x < y),
                    0x64 => f64cmp!(|x, y| x > y),
                    0x65 => f64cmp!(|x, y| x <= y),
                    0x66 => f64cmp!(|x, y| x >= y),
                    0x67 => {
                        let x = pop!() as u32;
                        push!(x.leading_zeros() as u64)
                    }
                    0x68 => {
                        let x = pop!() as u32;
                        push!(x.trailing_zeros() as u64)
                    }
                    0x69 => {
                        let x = pop!() as u32;
                        push!(x.count_ones() as u64)
                    }
                    0x6a => bin32!(|x: u32, y| x.wrapping_add(y)),
                    0x6b => bin32!(|x: u32, y| x.wrapping_sub(y)),
                    0x6c => bin32!(|x: u32, y| x.wrapping_mul(y)),
                    0x6d | 0x6f => {
                        let y = pop!() as u32 as i32;
                        let x = pop!() as u32 as i32;
                        if y == 0 {
                            return Err("integer divide by zero");
                        }
                        if op == 0x6d && x == i32::MIN && y == -1 {
                            return Err("integer overflow");
                        }
                        push!((if op == 0x6d { x.wrapping_div(y) } else { x.wrapping_rem(y) }) as u32 as u64)
                    }
                    0x6e | 0x70 => {
                        let y = pop!() as u32;
                        let x = pop!() as u32;
                        if y == 0 {
                            return Err("integer divide by zero");
                        }
                        push!((if op == 0x6e { x / y } else { x % y }) as u64)
                    }
                    0x71 => bin32!(|x: u32, y| x & y),
                    0x72 => bin32!(|x: u32, y| x | y),
                    0x73 => bin32!(|x: u32, y| x ^ y),
                    0x74 => bin32!(|x: u32, y| x.wrapping_shl(y)),
                    0x75 => bin32!(|x: u32, y| (x as i32).wrapping_shr(y) as u32),
                    0x76 => bin32!(|x: u32, y| x.wrapping_shr(y)),
                    0x77 => bin32!(|x: u32, y| x.rotate_left(y % 32)),
                    0x78 => bin32!(|x: u32, y| x.rotate_right(y % 32)),
                    0x79 => {
                        let x = pop!();
                        push!(x.leading_zeros() as u64)
                    }
                    0x7a => {
                        let x = pop!();
                        push!(x.trailing_zeros() as u64)
                    }
                    0x7b => {
                        let x = pop!();
                        push!(x.count_ones() as u64)
                    }
                    0x7c => bin64!(|x: u64, y| x.wrapping_add(y)),
                    0x7d => bin64!(|x: u64, y| x.wrapping_sub(y)),
                    0x7e => bin64!(|x: u64, y| x.wrapping_mul(y)),
                    0x7f | 0x81 => {
                        let y = pop!() as i64;
                        let x = pop!() as i64;
                        if y == 0 {
                            return Err("integer divide by zero");
                        }
                        if op == 0x7f && x == i64::MIN && y == -1 {
                            return Err("integer overflow");
                        }
                        push!((if op == 0x7f { x.wrapping_div(y) } else { x.wrapping_rem(y) }) as u64)
                    }
                    0x80 | 0x82 => {
                        let y = pop!();
                        let x = pop!();
                        if y == 0 {
                            return Err("integer divide by zero");
                        }
                        push!(if op == 0x80 { x / y } else { x % y })
                    }
                    0x83 => bin64!(|x: u64, y| x & y),
                    0x84 => bin64!(|x: u64, y| x | y),
                    0x85 => bin64!(|x: u64, y| x ^ y),
                    0x86 => bin64!(|x: u64, y: u64| x.wrapping_shl(y as u32)),
                    0x87 => bin64!(|x: u64, y: u64| (x as i64).wrapping_shr(y as u32) as u64),
                    0x88 => bin64!(|x: u64, y: u64| x.wrapping_shr(y as u32)),
                    0x89 => bin64!(|x: u64, y: u64| x.rotate_left((y % 64) as u32)),
                    0x8a => bin64!(|x: u64, y: u64| x.rotate_right((y % 64) as u32)),
                    0x8b => {
                        let x = pop!();
                        push!(x & 0x7fff_ffff)
                    }
                    0x8c => {
                        let x = pop!();
                        push!((x ^ 0x8000_0000) & 0xffff_ffff)
                    }
                    0x8d => {
                        let x = f32v(pop!());
                        push!(bf32(core::f32::math::ceil(x)))
                    }
                    0x8e => {
                        let x = f32v(pop!());
                        push!(bf32(core::f32::math::floor(x)))
                    }
                    0x8f => {
                        let x = f32v(pop!());
                        push!(bf32(core::f32::math::trunc(x)))
                    }
                    0x90 => {
                        let x = f32v(pop!());
                        push!(bf32(core::f32::math::round_ties_even(x)))
                    }
                    0x91 => {
                        let x = f32v(pop!());
                        push!(bf32(core::f32::math::sqrt(x)))
                    }
                    0x92 => f32bin!(|x: f32, y: f32| x + y),
                    0x93 => f32bin!(|x: f32, y: f32| x - y),
                    0x94 => f32bin!(|x: f32, y: f32| x * y),
                    0x95 => f32bin!(|x: f32, y: f32| x / y),
                    0x96 => f32bin!(|x, y| fmin32(x, y, true)),
                    0x97 => f32bin!(|x, y| fmin32(x, y, false)),
                    0x98 => {
                        let y = pop!();
                        let x = pop!();
                        push!((x & 0x7fff_ffff) | (y & 0x8000_0000))
                    }
                    0x99 => {
                        let x = pop!();
                        push!(x & 0x7fff_ffff_ffff_ffff)
                    }
                    0x9a => {
                        let x = pop!();
                        push!(x ^ 0x8000_0000_0000_0000)
                    }
                    0x9b => {
                        let x = f64v(pop!());
                        push!(bf64(core::f64::math::ceil(x)))
                    }
                    0x9c => {
                        let x = f64v(pop!());
                        push!(bf64(core::f64::math::floor(x)))
                    }
                    0x9d => {
                        let x = f64v(pop!());
                        push!(bf64(core::f64::math::trunc(x)))
                    }
                    0x9e => {
                        let x = f64v(pop!());
                        push!(bf64(core::f64::math::round_ties_even(x)))
                    }
                    0x9f => {
                        let x = f64v(pop!());
                        push!(bf64(core::f64::math::sqrt(x)))
                    }
                    0xa0 => f64bin!(|x: f64, y: f64| x + y),
                    0xa1 => f64bin!(|x: f64, y: f64| x - y),
                    0xa2 => f64bin!(|x: f64, y: f64| x * y),
                    0xa3 => f64bin!(|x: f64, y: f64| x / y),
                    0xa4 => f64bin!(|x, y| fmin64(x, y, true)),
                    0xa5 => f64bin!(|x, y| fmin64(x, y, false)),
                    0xa6 => {
                        let y = pop!();
                        let x = pop!();
                        push!((x & 0x7fff_ffff_ffff_ffff) | (y & 0x8000_0000_0000_0000))
                    }
                    0xa7 => {
                        let x = pop!();
                        push!(x & 0xffff_ffff)
                    }
                    0xa8..=0xab | 0xae..=0xb1 => {
                        let x = pop!();
                        let v = if op == 0xa8 || op == 0xa9 || op == 0xae || op == 0xaf { f32v(x) as f64 } else { f64v(x) };
                        push!(match op {
                            0xa8 | 0xaa => trunc(v, -2147483648.0, 2147483648.0)? as i32 as u32 as u64,
                            0xa9 | 0xab => trunc(v, -0.999999, 4294967296.0)? as u32 as u64,
                            0xae | 0xb0 => trunc(v, -9223372036854775808.0, 9223372036854775808.0)? as i64 as u64,
                            _ => trunc(v, -0.999999, 18446744073709551616.0)? as u64,
                        })
                    }
                    0xac => {
                        let x = pop!();
                        push!(x as u32 as i32 as i64 as u64)
                    }
                    0xad => {
                        let x = pop!();
                        push!(x & 0xffff_ffff)
                    }
                    0xb2 => {
                        let x = pop!();
                        push!(bf32(x as u32 as i32 as f32))
                    }
                    0xb3 => {
                        let x = pop!();
                        push!(bf32(x as u32 as f32))
                    }
                    0xb4 => {
                        let x = pop!();
                        push!(bf32(x as i64 as f32))
                    }
                    0xb5 => {
                        let x = pop!();
                        push!(bf32(x as f32))
                    }
                    0xb6 => {
                        let x = f64v(pop!());
                        push!(bf32(x as f32))
                    }
                    0xb7 => {
                        let x = pop!();
                        push!(bf64(x as u32 as i32 as f64))
                    }
                    0xb8 => {
                        let x = pop!();
                        push!(bf64(x as u32 as f64))
                    }
                    0xb9 => {
                        let x = pop!();
                        push!(bf64(x as i64 as f64))
                    }
                    0xba => {
                        let x = pop!();
                        push!(bf64(x as f64))
                    }
                    0xbb => {
                        let x = f32v(pop!());
                        push!(bf64(x as f64))
                    }
                    0xbc..=0xbf => {}
                    0xc0 => {
                        let x = pop!();
                        push!(x as u8 as i8 as i32 as u32 as u64)
                    }
                    0xc1 => {
                        let x = pop!();
                        push!(x as u16 as i16 as i32 as u32 as u64)
                    }
                    0xc2 => {
                        let x = pop!();
                        push!(x as u8 as i8 as i64 as u64)
                    }
                    0xc3 => {
                        let x = pop!();
                        push!(x as u16 as i16 as i64 as u64)
                    }
                    0xc4 => {
                        let x = pop!();
                        push!(x as u32 as i32 as i64 as u64)
                    }

                    _ => return Err("unsupported instruction"),
                },
            }
        }
    }

    fn op_fc(&mut self, sub: u8, imm: u32, stack: &mut Vec<u64>) -> Result<(), Trap> {
        let pop = |s: &mut Vec<u64>| s.pop().ok_or("stack underflow");
        match sub as u32 {
            // Saturating conversions: Rust's `as` saturates the same way.
            k @ 0..=7 => {
                let x = pop(stack)?;
                let v = if k == 0 || k == 1 || k == 4 || k == 5 { f32v(x) as f64 } else { f64v(x) };
                stack.push(match k {
                    0 | 2 => v as i32 as u32 as u64,
                    1 | 3 => v as u32 as u64,
                    4 | 6 => v as i64 as u64,
                    _ => v as u64,
                });
            }
            8 => {
                let seg = imm as usize;
                let n = pop(stack)? as u32 as usize;
                let src = pop(stack)? as u32 as usize;
                let dst = pop(stack)? as u32 as u64;
                let (start, len) = match self.m.datas.get(seg).ok_or("bad data segment")? {
                    Data::Passive(s, l) => (*s, *l),
                    Data::Active(_, s, l) => (*s, *l),
                };
                let len = if self.mem.dropped.load(SeqCst) & (1 << seg.min(63)) != 0 { 0 } else { len };
                if src + n > len {
                    return Err("memory access out of bounds");
                }
                let p = self.mem.at(dst, n as u64)?;
                unsafe { core::ptr::copy_nonoverlapping(self.m.bytes[start + src..].as_ptr(), p, n) };
            }
            9 => {
                self.mem.dropped.fetch_or(1 << imm.min(63), SeqCst);
            }
            10 => {
                let n = pop(stack)? as u32 as u64;
                let src = pop(stack)? as u32 as u64;
                let dst = pop(stack)? as u32 as u64;
                let (s, d) = (self.mem.at(src, n)?, self.mem.at(dst, n)?);
                unsafe { core::ptr::copy(s, d, n as usize) };
            }
            11 => {
                let n = pop(stack)? as u32 as u64;
                let v = pop(stack)? as u8;
                let dst = pop(stack)? as u32 as u64;
                let d = self.mem.at(dst, n)?;
                unsafe { core::ptr::write_bytes(d, v, n as usize) };
            }
            _ => return Err("unsupported 0xfc instruction"),
        }
        Ok(())
    }

    fn op_fe(&mut self, sub: u8, off: u32, stack: &mut Vec<u64>) -> Result<(), Trap> {
        let pop = |s: &mut Vec<u64>| s.pop().ok_or("stack underflow");
        let k = sub as u32;
        if k == 3 {
            core::sync::atomic::fence(SeqCst);
            return Ok(());
        }
        let off = off as u64;
        // Width in bytes of each atomic op (loads, stores, then rmw groups of 7).
        let width = |k: u32| -> u64 {
            match k {
                0 | 1 => 4,
                2 => 8,
                0x10 | 0x17 => 4,
                0x11 | 0x18 => 8,
                0x12 | 0x14 | 0x19 | 0x1b => 1,
                0x13 | 0x15 | 0x1a | 0x1c => 2,
                0x16 | 0x1d => 4,
                _ => [4, 8, 1, 2, 1, 2, 4][((k - 0x1e) % 7) as usize],
            }
        };
        let w = width(k);
        match k {
            0 => {
                let _count = pop(stack)?;
                let a = pop(stack)? as u32 as u64 + off;
                self.mem.at(a, 4)?;
                stack.push(0);
            }
            1 | 2 => {
                let timeout = pop(stack)? as i64;
                let expected = pop(stack)?;
                let a = pop(stack)? as u32 as u64 + off;
                let p = self.mem.at(a, w)?;
                let now = || if w == 4 { unsafe { AtomicU32::from_ptr(p as *mut u32) }.load(SeqCst) as u64 } else { unsafe { AtomicU64::from_ptr(p as *mut u64) }.load(SeqCst) };
                if now() != (if w == 4 { expected & 0xffff_ffff } else { expected }) {
                    stack.push(1);
                } else {
                    let t0 = libsys::clock_ns();
                    let mut res = 0;
                    while now() == expected {
                        if timeout >= 0 && libsys::clock_ns() - t0 >= timeout as u64 {
                            res = 2;
                            break;
                        }
                        libsys::sleep_ns(100_000);
                    }
                    stack.push(res);
                }
            }
            0x10..=0x16 => {
                let a = pop(stack)? as u32 as u64 + off;
                if a % w != 0 {
                    return Err("unaligned atomic");
                }
                let p = self.mem.at(a, w)?;
                let v = unsafe {
                    match w {
                        1 => AtomicU8::from_ptr(p).load(SeqCst) as u64,
                        2 => AtomicU16::from_ptr(p as *mut u16).load(SeqCst) as u64,
                        4 => AtomicU32::from_ptr(p as *mut u32).load(SeqCst) as u64,
                        _ => AtomicU64::from_ptr(p as *mut u64).load(SeqCst),
                    }
                };
                stack.push(v);
            }
            0x17..=0x1d => {
                let v = pop(stack)?;
                let a = pop(stack)? as u32 as u64 + off;
                if a % w != 0 {
                    return Err("unaligned atomic");
                }
                let p = self.mem.at(a, w)?;
                unsafe {
                    match w {
                        1 => AtomicU8::from_ptr(p).store(v as u8, SeqCst),
                        2 => AtomicU16::from_ptr(p as *mut u16).store(v as u16, SeqCst),
                        4 => AtomicU32::from_ptr(p as *mut u32).store(v as u32, SeqCst),
                        _ => AtomicU64::from_ptr(p as *mut u64).store(v, SeqCst),
                    }
                }
            }
            0x1e..=0x4e => {
                let group = (k - 0x1e) / 7;
                let (v, expected) = if group == 6 {
                    let rep = pop(stack)?;
                    let exp = pop(stack)?;
                    (rep, exp)
                } else {
                    (pop(stack)?, 0)
                };
                let a = pop(stack)? as u32 as u64 + off;
                if a % w != 0 {
                    return Err("unaligned atomic");
                }
                let p = self.mem.at(a, w)?;
                macro_rules! rmw {
                    ($at:ty, $t:ty) => {{
                        let x = unsafe { <$at>::from_ptr(p as *mut $t) };
                        let v = v as $t;
                        (match group {
                            0 => x.fetch_add(v, SeqCst),
                            1 => x.fetch_sub(v, SeqCst),
                            2 => x.fetch_and(v, SeqCst),
                            3 => x.fetch_or(v, SeqCst),
                            4 => x.fetch_xor(v, SeqCst),
                            5 => x.swap(v, SeqCst),
                            _ => match x.compare_exchange(expected as $t, v, SeqCst, SeqCst) {
                                Ok(o) | Err(o) => o,
                            },
                        }) as u64
                    }};
                }
                let old = match w {
                    1 => rmw!(AtomicU8, u8),
                    2 => rmw!(AtomicU16, u16),
                    4 => rmw!(AtomicU32, u32),
                    _ => rmw!(AtomicU64, u64),
                };
                stack.push(old);
            }
            _ => return Err("unsupported atomic instruction"),
        }
        Ok(())
    }
}
