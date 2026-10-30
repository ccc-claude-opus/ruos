// Per-architecture link settings (linker script for the native kernels,
// exported entry points for wasm32) and the boot images: every file in
// $ROS_BOOT (set by run.sh to build/boot-<platform>) is embedded.
use std::env;
use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    match arch.as_str() {
        "riscv64" | "aarch64" => {
            let ld = format!("{dir}/src/arch/{arch}/linker.ld");
            println!("cargo:rerun-if-changed={ld}");
            println!("cargo:rustc-link-arg=-T{ld}");
        }
        "wasm32" => {
            for e in ["kernel_dtb_buf", "kernel_boot", "kernel_irq", "kernel_syscall", "kernel_fault", "kernel_scratch", "kernel_remote", "kernel_remote_buf", "kernel_snapshot", "kernel_snapshot_len"] {
                println!("cargo:rustc-link-arg=--export={e}");
            }
        }
        _ => {}
    }

    println!("cargo:rerun-if-env-changed=ROS_BOOT");
    let mut out = String::from("pub static IMAGES: &[(&str, &[u8])] = &[\n");
    if let Ok(boot) = env::var("ROS_BOOT") {
        println!("cargo:rerun-if-changed={boot}");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&boot)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            println!("cargo:rerun-if-changed={}", f.display());
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            writeln!(out, "    ({name:?}, include_bytes!({:?})),", f.display().to_string()).unwrap();
        }
    }
    out.push_str("];\n");
    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("boot_images.rs");
    std::fs::write(dest, out).unwrap();
}
