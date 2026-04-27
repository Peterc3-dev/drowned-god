// Compile GLSL compute shaders to SPIR-V at build time using glslc.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    for shader in &["copy.comp", "triad.comp"] {
        let src = PathBuf::from("src/shaders").join(shader);
        let spv = out_dir.join(format!("{}.spv", shader));
        println!("cargo:rerun-if-changed={}", src.display());
        let status = Command::new("glslc")
            .args(["-O", "-fshader-stage=comp", "-o"])
            .arg(&spv)
            .arg(&src)
            .status()
            .expect("glslc not found in PATH");
        assert!(status.success(), "glslc failed for {}", shader);
    }
}
