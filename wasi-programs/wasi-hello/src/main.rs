//! An ordinary Rust program (std, wasm32-wasip1): arguments, environment,
//! files, the clock, stdin. On ruos it runs through the WASI layer.
use std::io::{BufRead, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("wasi-hello: {} argument(s): {}", args.len(), args[1..].join(" "));
    if let Ok(p) = std::env::var("PWD") {
        println!("wasi-hello: PWD={p}");
    }
    match args.get(1).map(String::as_str) {
        Some("write") => {
            let name = args.get(2).map(String::as_str).unwrap_or("wasi.txt");
            let mut f = std::fs::File::create(name).expect("create");
            for i in 1..=3 {
                writeln!(f, "line {i} from std::fs").unwrap();
            }
            println!("wasi-hello: wrote {name} ({} bytes)", std::fs::metadata(name).unwrap().len());
        }
        Some("read") => {
            let name = args.get(2).map(String::as_str).unwrap_or("wasi.txt");
            match std::fs::read_to_string(name) {
                Ok(t) => print!("{t}"),
                Err(e) => println!("wasi-hello: {name}: {e}"),
            }
        }
        Some("ls") => {
            let dir = args.get(2).map(String::as_str).unwrap_or(".");
            let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            println!("wasi-hello: {}", names.join(" "));
        }
        Some("stdin") => {
            let mut n = 0;
            for l in std::io::stdin().lock().lines() {
                let l = l.unwrap();
                if l == "end" {
                    break;
                }
                n += 1;
                println!("wasi-hello: got {l:?}");
            }
            println!("wasi-hello: {n} line(s)");
        }
        Some("time") => {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            let start = std::time::Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(200));
            println!("wasi-hello: year ~{}, slept {} ms", 1970 + t.as_secs() / 31_556_952, start.elapsed().as_millis() / 100 * 100);
        }
        Some("exit") => std::process::exit(args.get(2).and_then(|c| c.parse().ok()).unwrap_or(3)),
        Some("panic") => panic!("on purpose"),
        _ => {}
    }
}
