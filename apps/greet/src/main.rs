//! greet: an example Function (doc/serverless.md). Each request gets
//! `INSTANCE: hello REQUEST (#N here)`: N counts requests in this
//! instance's memory, which a Function cannot count on (instances come and
//! go). With `kv:counter` it adds `, #M in kv:counter`: a count that lasts.
//! `sleep SECONDS` waits that long before answering (to try `timeout`).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use libsys::kv::Kv;
use libsys::{args, func};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let (_, env) = args::load();
    let pod = env.iter().find_map(|e| e.strip_prefix(b"POD=")).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default();
    let counter = Kv::get_ns("counter");
    let mut here = 0u64;
    func::serve(|req| {
        let req = String::from_utf8_lossy(req).into_owned();
        if let Some(s) = req.strip_prefix("sleep ").and_then(|s| s.trim().parse::<u64>().ok()) {
            libsys::sleep_ms(s * 1000);
        }
        here += 1;
        let mut out = format!("{pod}: hello {req} (#{here} here");
        if let Some(kv) = counter {
            let n = kv.get("count").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + 1;
            let _ = kv.put("count", &format!("{n}"));
            out += &format!(", #{n} in kv:counter");
        }
        out += ")";
        out.into_bytes()
    })
}
