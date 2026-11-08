use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, ensure};

fn version(program: &std::ffi::OsStr) -> Result<String> {
    let output = Command::new(program).arg("--version").output()?;
    ensure!(output.status.success(), "{program:?} --version failed");
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .next()
        .context("compiler returned an empty version")?
        .to_owned())
}

fn main() -> Result<()> {
    let manifest = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").context("CARGO_MANIFEST_DIR is missing")?,
    );
    let sample = manifest.join("../c-guest");
    let status = Command::new("make")
        .arg("-C")
        .arg(&sample)
        .args(["build/native-include/.ready", "build/benchmark-guest"])
        .status()
        .context("building the benchmark VM guest and preparing C headers")?;
    ensure!(
        status.success(),
        "benchmark guest preparation failed: {status}"
    );
    let include = sample.join("build/native-include");
    cc::Build::new()
        .compiler("clang")
        .file(manifest.join("guest.c"))
        .include(&include)
        .flag("-std=c11")
        .flag("-Wall")
        .flag("-Wextra")
        .opt_level(2)
        .warnings_into_errors(true)
        .try_compile("hl_benchmark_guest_native")
        .context("compiling the native benchmark guest")?;
    let guest = sample.join("build/benchmark-guest").canonicalize()?;
    println!("cargo:rustc-env=HL_BENCH_GUEST={}", guest.display());
    println!(
        "cargo:rustc-env=HL_BENCH_RUSTC={}",
        version(&std::env::var_os("RUSTC").context("RUSTC is missing")?)?
    );
    println!(
        "cargo:rustc-env=HL_BENCH_CLANG={}",
        version(std::ffi::OsStr::new("clang"))?
    );
    println!(
        "cargo:rustc-env=HL_BENCH_OPT_LEVEL={}",
        std::env::var("OPT_LEVEL")?
    );
    for path in [
        manifest.join("guest.c"),
        sample.join("Makefile"),
        sample.join("sdk/.ready"),
        include.join("hyperlight_guest.h"),
        include.join("macro.h"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(())
}
