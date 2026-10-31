// The WASI functions (`pub extern "C" fn wasi_*` in main.rs) are exports:
// the guest's wasi_snapshot_preview1 imports are wired to them.
fn main() {
    let src = std::fs::read_to_string("src/main.rs").unwrap();
    for l in src.lines() {
        if let Some(rest) = l.trim().strip_prefix("pub extern \"C\" fn ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if name.starts_with("wasi_") {
                println!("cargo:rustc-link-arg=--export={name}");
            }
        }
    }
    println!("cargo:rerun-if-changed=src/main.rs");
}
