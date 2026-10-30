//! The module, read and validated with `wasmparser`: what the interpreter
//! needs. Function bodies are validated and pre-decoded (compile.rs) the
//! first time they are called: a big program (coreutils) uses only a few
//! of its functions per run. Only the
//! interpreter (exec.rs) is our own: no interpreter crate supports shared
//! memory and atomics (wasmi lists the threads proposal as planned).
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering::SeqCst};

use wasmparser::{FuncToValidate, ValidatorResources};

pub type Trap = &'static str;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuncType {
    pub params: usize,
    pub results: usize,
}

pub struct Code {
    /// Declared locals (after the parameters).
    pub locals: usize,
    /// The operators (`start`) and the whole body (`body`, for validation).
    pub start: usize,
    pub end: usize,
    pub body: usize,
    /// 0 not yet, 1 being prepared, 2 ready, 3 invalid.
    state: AtomicU8,
    check: UnsafeCell<Option<FuncToValidate<ValidatorResources>>>,
    /// The body, pre-decoded.
    func: UnsafeCell<Option<crate::compile::Func>>,
}

unsafe impl Sync for Code {}
unsafe impl Send for Code {}

pub enum Data {
    Active(u32, usize, usize),
    Passive(usize, usize),
}

pub struct Module {
    pub bytes: Vec<u8>,
    pub types: Vec<FuncType>,
    /// Type of every function (imports first).
    pub funcs: Vec<u32>,
    pub imports: Vec<(String, String)>,
    pub code: Vec<Code>,
    pub table: Vec<u32>,
    /// (initial value, mutable)
    pub globals: Vec<(u64, bool)>,
    pub exports: BTreeMap<String, (u8, u32)>,
    pub start: Option<u32>,
    pub datas: Vec<Data>,
    pub mem_min: u32,
    pub mem_max: u32,
    /// The memory is shared (imported; the program has threads).
    pub mem_shared: bool,
    /// Exception tags: the number of values each carries.
    pub tags: Vec<usize>,
}

pub struct Reader<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl Reader<'_> {
    pub fn byte(&mut self) -> Result<u8, Trap> {
        let c = *self.b.get(self.p).ok_or("unexpected end")?;
        self.p += 1;
        Ok(c)
    }

    pub fn u32(&mut self) -> Result<u32, Trap> {
        Ok(self.uleb()? as u32)
    }

    pub fn uleb(&mut self) -> Result<u64, Trap> {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let c = self.byte()?;
            v |= ((c & 0x7f) as u64) << s;
            s += 7;
            if c & 0x80 == 0 {
                return Ok(v);
            }
            if s > 63 {
                return Err("bad leb");
            }
        }
    }

    pub fn sleb(&mut self, bits: u32) -> Result<i64, Trap> {
        let (mut v, mut s) = (0i64, 0u32);
        loop {
            let c = self.byte()?;
            if s < 64 {
                v |= ((c & 0x7f) as i64) << s;
            }
            s += 7;
            if c & 0x80 == 0 {
                if s < 64 && c & 0x40 != 0 {
                    v |= -1i64 << s;
                }
                let _ = bits;
                return Ok(v);
            }
        }
    }




}

impl Module {
    /// Validates and reads a module (`wasmparser`).
    pub fn parse(bytes: Vec<u8>) -> Result<Module, Trap> {
        use wasmparser::{
            DataKind, ElementItems, ElementKind, ExternalKind, Operator, Parser, Payload, TypeRef, ValidPayload,
            Validator, WasmFeatures,
        };
        let mut validator = Validator::new_with_features(WasmFeatures::all());
        let bad = |_| "bad module";
        let mut m = Module {
            bytes: Vec::new(),
            types: Vec::new(),
            funcs: Vec::new(),
            imports: Vec::new(),
            code: Vec::new(),
            table: Vec::new(),
            globals: Vec::new(),
            exports: BTreeMap::new(),
            start: None,
            datas: Vec::new(),
            mem_min: 0,
            mem_max: 0,
            mem_shared: false,
            tags: Vec::new(),
        };
        let constant = |e: &wasmparser::ConstExpr, globals: &[(u64, bool)]| -> Result<u64, Trap> {
            let mut r = e.get_operators_reader();
            let v = match r.read().map_err(bad)? {
                Operator::I32Const { value } => value as u32 as u64,
                Operator::I64Const { value } => value as u64,
                Operator::F32Const { value } => value.bits() as u64,
                Operator::F64Const { value } => value.bits(),
                Operator::GlobalGet { global_index } => globals.get(global_index as usize).ok_or("bad global")?.0,
                Operator::RefFunc { function_index } => function_index as u64,
                _ => return Err("unsupported constant expression"),
            };
            Ok(v)
        };
        let mut elems: Vec<(u32, Vec<u32>)> = Vec::new();
        for payload in Parser::new(0).parse_all(&bytes) {
            let payload = payload.map_err(bad)?;
            let check = match validator.payload(&payload).map_err(|_| "invalid module")? {
                ValidPayload::Func(f, _) => Some(f),
                _ => None,
            };
            match payload {
                Payload::TypeSection(r) => {
                    for t in r.into_iter_err_on_gc_types() {
                        let t = t.map_err(bad)?;
                        m.types.push(FuncType { params: t.params().len(), results: t.results().len() });
                    }
                }
                Payload::ImportSection(r) => {
                    for i in r.into_imports() {
                        let i = i.map_err(bad)?;
                        match i.ty {
                            TypeRef::Func(t) => {
                                m.funcs.push(t);
                                m.imports.push((String::from(i.module), String::from(i.name)));
                            }
                            TypeRef::Memory(mt) => {
                                m.mem_min = mt.initial as u32;
                                m.mem_max = mt.maximum.unwrap_or(65536) as u32;
                                m.mem_shared = mt.shared;
                            }
                            TypeRef::Tag(t) => {
                                let n = m.types.get(t.func_type_idx as usize).ok_or("bad tag type")?.params;
                                m.tags.push(n);
                            }
                            _ => return Err("unsupported import"),
                        }
                    }
                }
                Payload::FunctionSection(r) => {
                    for t in r {
                        m.funcs.push(t.map_err(bad)?);
                    }
                }
                Payload::TagSection(r) => {
                    for t in r {
                        let t = t.map_err(bad)?;
                        m.tags.push(m.types.get(t.func_type_idx as usize).ok_or("bad tag type")?.params);
                    }
                }
                Payload::TableSection(r) => {
                    for t in r {
                        m.table = alloc::vec![u32::MAX; t.map_err(bad)?.ty.initial as usize];
                    }
                }
                Payload::MemorySection(r) => {
                    for mt in r {
                        let mt = mt.map_err(bad)?;
                        m.mem_min = mt.initial as u32;
                        m.mem_max = mt.maximum.unwrap_or(65536) as u32;
                    }
                }
                Payload::GlobalSection(r) => {
                    for g in r {
                        let g = g.map_err(bad)?;
                        let v = constant(&g.init_expr, &m.globals)?;
                        m.globals.push((v, g.ty.mutable));
                    }
                }
                Payload::ExportSection(r) => {
                    for e in r {
                        let e = e.map_err(bad)?;
                        let k = match e.kind {
                            ExternalKind::Func => 0,
                            ExternalKind::Table => 1,
                            ExternalKind::Memory => 2,
                            ExternalKind::Global => 3,
                            _ => 4,
                        };
                        m.exports.insert(String::from(e.name), (k, e.index));
                    }
                }
                Payload::StartSection { func, .. } => m.start = Some(func),
                Payload::ElementSection(r) => {
                    for e in r {
                        let e = e.map_err(bad)?;
                        let ElementKind::Active { offset_expr, .. } = e.kind else { continue };
                        let off = constant(&offset_expr, &m.globals)? as u32;
                        let mut f = Vec::new();
                        match e.items {
                            ElementItems::Functions(r) => {
                                for x in r {
                                    f.push(x.map_err(bad)?);
                                }
                            }
                            ElementItems::Expressions(_, r) => {
                                for x in r {
                                    f.push(constant(&x.map_err(bad)?, &m.globals)? as u32);
                                }
                            }
                        }
                        elems.push((off, f));
                    }
                }
                Payload::DataSection(r) => {
                    for d in r {
                        let d = d.map_err(bad)?;
                        let start = d.data.as_ptr() as usize - bytes.as_ptr() as usize;
                        m.datas.push(match d.kind {
                            DataKind::Passive => Data::Passive(start, d.data.len()),
                            DataKind::Active { offset_expr, .. } => {
                                Data::Active(constant(&offset_expr, &m.globals)? as u32, start, d.data.len())
                            }
                        });
                    }
                }
                Payload::CodeSectionEntry(body) => {
                    let mut locals = 0;
                    for l in body.get_locals_reader().map_err(bad)? {
                        locals += l.map_err(bad)?.0 as usize;
                    }
                    let ops = body.get_operators_reader().map_err(bad)?;
                    let start = ops.original_position() as usize;
                    let (body, end) = (body.range().start as usize, body.range().end as usize);
                    m.code.push(Code {
                        locals,
                        start,
                        end,
                        body,
                        state: AtomicU8::new(0),
                        check: UnsafeCell::new(check),
                        func: UnsafeCell::new(None),
                    });
                }
                _ => {}
            }
        }
        for (off, f) in elems {
            for (i, x) in f.into_iter().enumerate() {
                let at = off as usize + i;
                if at >= m.table.len() {
                    m.table.resize(at + 1, u32::MAX);
                }
                m.table[at] = x;
            }
        }
        m.bytes = bytes;
        Ok(m)
    }

    /// Function `i` (not counting imports), validated and pre-decoded on first use.
    pub fn func(&self, i: usize) -> Result<&crate::compile::Func, Trap> {
        let c = self.code.get(i).ok_or("bad function")?;
        loop {
            match c.state.load(SeqCst) {
                2 => return Ok(unsafe { (*c.func.get()).as_ref().unwrap() }),
                3 => return Err("invalid function"),
                0 if c.state.compare_exchange(0, 1, SeqCst, SeqCst).is_ok() => {
                    let ok = self.prepare(c);
                    c.state.store(if ok.is_ok() { 2 } else { 3 }, SeqCst);
                    ok?;
                }
                _ => core::hint::spin_loop(),
            }
        }
    }

    fn prepare(&self, c: &Code) -> Result<(), Trap> {
        use wasmparser::{BinaryReader, FunctionBody};
        if let Some(check) = unsafe { (*c.check.get()).take() } {
            let body = FunctionBody::new(BinaryReader::new(&self.bytes[c.body..c.end], c.body as u64));
            check.into_validator(Default::default()).validate(&body).map_err(|_| "invalid function")?;
        }
        let f = crate::compile::compile(self, c.start, c.end)?;
        unsafe { *c.func.get() = Some(f) };
        Ok(())
    }

    pub fn export(&self, name: &str, kind: u8) -> Option<u32> {
        self.exports.get(name).filter(|(k, _)| *k == kind).map(|(_, i)| *i)
    }

    /// (params, results) of a block type at `r`.
    pub fn block_type(&self, r: &mut Reader) -> Result<(usize, usize), Trap> {
        let c = r.b[r.p];
        if c == 0x40 {
            r.p += 1;
            Ok((0, 0))
        } else if c == 0x63 || c == 0x64 {
            // (ref null? heaptype)
            r.p += 1;
            r.sleb(33)?;
            Ok((0, 1))
        } else if c >= 0x69 && c <= 0x7f {
            // A value type (exnref is 0x69).
            r.p += 1;
            Ok((0, 1))
        } else {
            let t = self.types.get(r.sleb(33)? as usize).ok_or("bad block type")?;
            Ok((t.params, t.results))
        }
    }
}
