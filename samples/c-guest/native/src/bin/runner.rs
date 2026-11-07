use anyhow::{Result, ensure};

unsafe extern "C" {
    fn hyperlight_main();
}

fn main() -> Result<()> {
    ensure!(
        std::env::args_os().len() == 1,
        "usage: hyperlight-c-guest-native-runner"
    );
    unsafe { hyperlight_process::runtime::run(hyperlight_main) }
}
