use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, ensure};
use hyperlight_process::InProcessRunnerBuilder;

unsafe extern "C" {
    fn hyperlight_main();
}

fn main() -> Result<()> {
    ensure!(
        std::env::args_os().len() == 1,
        "usage: hyperlight-c-guest-inprocess-demo"
    );
    let messages = Arc::new(Mutex::new(Vec::new()));
    let host_messages = Arc::clone(&messages);
    let mut runner = unsafe {
        InProcessRunnerBuilder::new(hyperlight_main)
            .host_function("HostLog", move |message: String| {
                println!("guest -> host: {message}");
                host_messages
                    .lock()
                    .map_err(|error| anyhow!("could not record host message: {error}"))?
                    .push(message);
                Ok(true)
            })
            .build()?
    };
    let greeting = runner.call::<String>("Greet", "Hyperlight".to_owned())?;
    let expected = "Hello, Hyperlight! This is a Hyperlight C guest.";
    ensure!(greeting == expected, "unexpected greeting: {greeting:?}");
    ensure!(
        *messages
            .lock()
            .map_err(|error| anyhow!("could not read host messages: {error}"))?
            == [expected],
        "guest did not call HostLog with the expected greeting"
    );
    println!("host <- guest: {greeting}");
    let sum = runner.call::<i64>("Add", (20_i32, 22_i32))?;
    ensure!(sum == 42, "unexpected sum: {sum}");
    println!("host <- guest: Add(20, 22) = {sum}");
    let wide_sum = runner.call::<i64>("Add", (i32::MAX, i32::MAX))?;
    ensure!(
        wide_sum == 4_294_967_294,
        "addition did not preserve the 64-bit result"
    );
    println!("All in-process guest checks passed.");
    Ok(())
}
