use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, ensure};
use hyperlight_process::RunnerBuilder;

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let executable = match arguments.next() {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()?.with_file_name("hyperlight-c-guest-native-runner"),
    };
    ensure!(
        arguments.next().is_none(),
        "usage: hyperlight-c-guest-native-demo [runner-executable]"
    );
    let messages = Arc::new(Mutex::new(Vec::new()));
    let host_messages = Arc::clone(&messages);
    let mut runner = RunnerBuilder::from_executable(executable)
        .host_function("HostLog", move |message: String| {
            println!("guest -> host: {message}");
            host_messages
                .lock()
                .map_err(|error| anyhow!("could not record host message: {error}"))?
                .push(message);
            Ok(true)
        })
        .build()?;
    let greeting = runner
        .call::<String>("Greet", "Hyperlight".to_owned())
        .context("calling native C Greet")?;
    let expected = "Hello, Hyperlight! This is a Hyperlight C guest.";
    ensure!(greeting == expected, "unexpected greeting: {greeting:?}");
    ensure!(
        *messages
            .lock()
            .map_err(|error| anyhow!("could not read host messages: {error}"))?
            == [expected],
        "guest did not call HostLog with the expected greeting"
    );
    println!("host <- guest: {expected}");

    for (left, right, expected) in [(20, 22, 42_i64), (i32::MAX, i32::MAX, 4_294_967_294)] {
        let value = runner.call::<i64>("Add", (left, right))?;
        ensure!(value == expected, "unexpected sum: {value:?}");
        if left == 20 {
            println!("host <- guest: Add(20, 22) = {expected}");
        }
    }
    runner.shutdown()?;
    println!("All native subprocess guest checks passed.");
    Ok(())
}
