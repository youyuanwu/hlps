use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, ensure};

fn main() -> Result<()> {
    let manifest = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").context("CARGO_MANIFEST_DIR is missing")?,
    );
    let sample = manifest
        .parent()
        .context("native sample has no parent directory")?;
    let status = Command::new("make")
        .arg("-C")
        .arg(sample)
        .arg("build/native-include/.ready")
        .status()
        .context("preparing Hyperlight C headers with make")?;
    ensure!(
        status.success(),
        "preparing Hyperlight C headers failed: {status}"
    );
    let include = sample.join("build/native-include");
    cc::Build::new()
        .compiler("clang")
        .file(sample.join("guest.c"))
        .include(&include)
        .flag("-std=c11")
        .warnings_into_errors(true)
        .try_compile("hl_c_guest_native")
        .context("compiling the native C guest")?;
    for path in [
        sample.join("guest.c"),
        sample.join("Makefile"),
        include.join("hyperlight_guest.h"),
        include.join("macro.h"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(())
}
