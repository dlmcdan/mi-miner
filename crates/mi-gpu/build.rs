use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Only compile Metal shaders on macOS
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "macos" {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let shader_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/shader.metal");

    if !shader_src.exists() {
        println!("cargo:warning=shader.metal not found, GPU mining will be unavailable");
        return;
    }

    // Compile .metal -> .air
    let air_path = out_dir.join("sha256d.air");
    let status = Command::new("xcrun")
        .args([
            "-sdk",
            "macosx",
            "metal",
            "-c",
            shader_src.to_str().unwrap(),
            "-o",
            air_path.to_str().unwrap(),
        ])
        .status();

    match status {
        Ok(s) if s.success() => {}
        _ => {
            println!("cargo:warning=Metal shader compilation failed (xcrun metal). GPU mining will be unavailable at runtime.");
            println!("cargo:warning=Install Xcode (not just command line tools) for Metal shader support.");
            return;
        }
    }

    // Link .air -> .metallib
    let metallib_path = out_dir.join("sha256d.metallib");
    let status = Command::new("xcrun")
        .args([
            "-sdk",
            "macosx",
            "metallib",
            air_path.to_str().unwrap(),
            "-o",
            metallib_path.to_str().unwrap(),
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "cargo:rustc-env=MI_METALLIB_PATH={}",
                metallib_path.display()
            );
            println!("cargo:rerun-if-changed=src/shader.metal");
        }
        _ => {
            println!("cargo:warning=Metal library linking failed (xcrun metallib). GPU mining will be unavailable at runtime.");
        }
    }
}
