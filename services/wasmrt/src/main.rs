//! wasmrt: runs a wasm32 program on a native machine, for images that
//! have only a wasm32 variant. Its namespace holds the module as
//! `wasm:module:LEN` (a memory object) and the program's own names, which
//! it hands on to the program as its namespace. The program's `sys.call`
//! becomes the real system call, its pointers translated into its linear
//! memory; its threads are native threads, each with its own instance.
#![no_std]
#![no_main]
#![feature(core_float_math)]

extern crate alloc;

mod compile;
mod exec;
mod module;
mod sys;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use exec::{Instance, Mem};
use libsys::{Channel, Error, Handle, Memory, ns, println};
use module::Module;

libsys::entry!(main);

/// Linear memory reserved for a shared-memory program (it cannot grow past this).
const RESERVE: usize = 8 << 20;

const EXPORT_FUNC: u8 = 0;
const EXPORT_GLOBAL: u8 = 3;

/// A program thread: its instance and how it started.
pub struct Start {
    pub m: Arc<Module>,
    pub mem: Arc<Mem>,
    pub entry: u32,
    pub stack: u32,
    pub arg: u32,
}

/// Runs the start function (it initializes the shared memory once) and then
/// `ros_start` (main thread) or `ros_thread_start` (others).
fn run(s: &Start, main: bool) -> ! {
    let mut inst = Instance::new(s.m.clone(), s.mem.clone());
    let host = sys::Sys { start: s };
    let r = (|| -> Result<(), module::Trap> {
        if !main {
            let sp = s.m.export("__stack_pointer", EXPORT_GLOBAL).ok_or("no __stack_pointer export")?;
            inst.globals[sp as usize] = s.stack as u64;
        }
        if let Some(f) = s.m.start {
            inst.invoke(&host, f, &[])?;
        }
        if main {
            let f = s.m.export("ros_start", EXPORT_FUNC).ok_or("no ros_start export")?;
            inst.invoke(&host, f, &[0])?;
        } else {
            let f = s.m.export("ros_thread_start", EXPORT_FUNC).ok_or("no ros_thread_start export")?;
            inst.invoke(&host, f, &[s.entry as u64, s.arg as u64])?;
        }
        Ok(())
    })();
    match r {
        Ok(()) => libsys::exit(0),
        Err(t) => {
            println!("wasmrt: trap: {}", t);
            libsys::exit(Error::Killed.code())
        }
    }
}

/// A new program thread (THREAD_CREATE from the program).
extern "C" fn thread_main(arg: usize) -> ! {
    let s: Box<Start> = unsafe { Box::from_raw(arg as *mut Start) };
    run(&s, false)
}

pub fn spawn_thread(s: Start) -> libsys::Result<Handle> {
    let stack = alloc::vec![0u8; 64 * 1024].leak();
    let p = Box::into_raw(Box::new(s));
    let r = libsys::thread_create(thread_main, stack, p as usize);
    if r.is_err() {
        drop(unsafe { Box::from_raw(p) });
    }
    r
}

fn main(_arg: usize) -> i64 {
    // The program's bootstrap channel must be handle 0: take it first (it
    // is free since ns::load closed ours).
    let Ok((theirs, mine)) = Channel::create() else { return 6 };
    if theirs.0 != Handle(0) {
        println!("wasmrt: handle 0 is taken");
        return 7;
    }
    let Some((module, len)) = ns::names().find_map(|n| {
        let len = n.strip_prefix("wasm:module:")?.parse::<u64>().ok()?;
        Some((ns::get(n)?, len))
    }) else {
        println!("wasmrt: no module");
        return 1;
    };
    let mut bytes = alloc::vec![0u8; len as usize];
    if Memory(module).read(0, &mut bytes).is_err() {
        return 2;
    }
    let _ = module.close();
    let m = match Module::parse(bytes) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            println!("wasmrt: bad module: {}", e);
            return 3;
        }
    };
    let max = (m.mem_max as usize * 65536).min(RESERVE);
    let min = m.mem_min as usize * 65536;
    if min > max {
        println!("wasmrt: the module needs {} KiB", min / 1024);
        return 4;
    }
    let Some(mem) = Mem::new(min, max, false) else {
        println!("wasmrt: no memory for {} KiB", max / 1024);
        return 5;
    };
    let mem = Arc::new(mem);

    // The program's namespace: our names but the module.
    let names: Vec<(String, Handle)> =
        ns::names().filter(|n| !n.starts_with("wasm:")).filter_map(|n| Some((String::from(n), ns::get(n)?))).collect();
    let mut text = String::from("ruos ns 1\n");
    for (n, _) in &names {
        text += n;
        text.push('\n');
    }
    let hs: Vec<Handle> = names.iter().map(|(_, h)| *h).collect();
    if mine.write(text.as_bytes(), &hs).is_err() {
        return 8;
    }
    let _ = mine.close();
    run(&Start { m, mem, entry: 0, stack: 0, arg: 0 }, true)
}
