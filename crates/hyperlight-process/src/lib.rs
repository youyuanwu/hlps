//! Native, statically linked execution of Hyperlight 0.19 C guests.
//! This adapter is not a sandbox: guest code has the runner's process privileges.

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("the native Hyperlight C adapter currently supports Linux x86_64 only");

pub mod abi;
pub mod client;
pub mod func;
pub mod inprocess;
pub mod protocol;
pub mod runtime;

pub use client::{Runner, RunnerBuilder};
pub use inprocess::{InProcessRunner, InProcessRunnerBuilder};
