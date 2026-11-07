use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use hyperlight_host::SandboxBuilder;

fn run_demo(guest: PathBuf) -> Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .context(
            "cannot open /dev/kvm; enable virtualization/nested virtualization and grant read/write access",
        )?;

    let messages = Arc::new(Mutex::new(Vec::new()));
    let host_messages = Arc::clone(&messages);
    let mut sandbox = SandboxBuilder::from_file(&guest)
        .host_function("HostLog", move |message: String| {
            println!("guest -> host: {message}");
            host_messages.lock().unwrap().push(message);
            Ok(true)
        })
        .build()
        .with_context(|| {
            format!(
                "could not start guest {}; this demo requires Linux x86_64 and read/write access to /dev/kvm",
                guest.display()
            )
        })?;

    let greeting = sandbox
        .call::<String>("Greet", "Hyperlight".to_string())
        .context("calling the C guest's Greet function")?;
    let expected = "Hello, Hyperlight! This is a Hyperlight C guest.";
    ensure!(greeting == expected, "unexpected greeting: {greeting:?}");
    ensure!(
        *messages.lock().unwrap() == [expected],
        "the guest did not call HostLog with the expected message"
    );
    println!("host <- guest: {greeting}");

    let sum = sandbox
        .call::<i64>("Add", (20_i32, 22_i32))
        .context("calling the C guest's Add function")?;
    ensure!(sum == 42, "unexpected sum: {sum}");
    println!("host <- guest: Add(20, 22) = {sum}");

    let wide_sum = sandbox.call::<i64>("Add", (i32::MAX, i32::MAX))?;
    ensure!(
        wide_sum == 4_294_967_294,
        "addition did not preserve the 64-bit result"
    );
    println!("All guest checks passed.");
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let guest = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/guest"));
    ensure!(
        args.next().is_none(),
        "usage: hyperlight-c-guest-demo [guest-binary]"
    );
    run_demo(guest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a built C guest and read/write access to /dev/kvm"]
    fn c_guest_round_trip() -> Result<()> {
        run_demo(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/guest"))
    }
}
