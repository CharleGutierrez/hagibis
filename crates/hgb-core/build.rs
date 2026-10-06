use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native/zig/hgb_accelerate.zig");
    println!("cargo:rerun-if-changed=native/zig/hgb_mcp.zig");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    
    let zig_src = manifest_dir.join("native/zig/hgb_accelerate.zig");
    let out_lib = out_dir.join("libhgb_accelerate.a");

    let status = Command::new("zig")
        .current_dir(&manifest_dir)
        .arg("build-lib")
        .arg("-O")
        .arg("ReleaseFast")
        .arg("-static").arg("-fPIC")
        .arg(&zig_src)
        .arg(format!("-femit-bin={}", out_lib.display()))
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-search=native={}", out_dir.display());
            println!("cargo:rustc-link-lib=static=hgb_accelerate");
            println!("cargo:rustc-cfg=has_zig_accelerate");
        }
        _ => {
            println!("cargo:warning=Zig compiler not found or build failed. Falling back.");
        }
    }
}
