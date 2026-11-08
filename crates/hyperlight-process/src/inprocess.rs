//! Direct native execution: no subprocess, IPC, or Vercode encoding.

use anyhow::{Context, Result};

use crate::func::{HostFunction, HostFunctions, ParameterTuple, SupportedReturnType, guest_call};
use crate::protocol::{FunctionCall, FunctionSignature, Value};
use crate::runtime::NativeRuntime;

/// Configure a trusted, statically linked guest for direct execution.
///
/// ```no_run
/// use hyperlight_process::InProcessRunnerBuilder;
///
/// unsafe extern "C" {
///     fn hyperlight_main();
/// }
///
/// # fn main() -> anyhow::Result<()> {
/// let mut runner = unsafe {
///     InProcessRunnerBuilder::new(hyperlight_main)
///         .host_function("HostLog", |message: String| {
///             println!("{message}");
///             Ok(true)
///         })
///         .build()?
/// };
/// let greeting = runner.call::<String>("Greet", "Hyperlight".to_owned())?;
/// let sum = runner.call::<i64>("Add", (20_i32, 22_i32))?;
/// # Ok(())
/// # }
/// ```
pub struct InProcessRunnerBuilder {
    initialize: unsafe extern "C" fn(),
    host_functions: HostFunctions,
}

impl InProcessRunnerBuilder {
    pub fn new(initialize: unsafe extern "C" fn()) -> Self {
        Self {
            initialize,
            host_functions: HostFunctions::default(),
        }
    }

    /// Re-registering a name replaces the earlier callback.
    pub fn host_function<Args: ParameterTuple, Output: SupportedReturnType>(
        mut self,
        name: impl AsRef<str>,
        function: impl Into<HostFunction<Output, Args>>,
    ) -> Self {
        self.host_functions.register(name.as_ref(), function);
        self
    }

    /// Initialize the linked guest. Host callbacks become available afterwards.
    ///
    /// # Safety
    /// The guest must obey the Hyperlight 0.19 C ABI throughout its lifetime.
    /// Its callbacks must use valid pointers, execute only on the calling thread,
    /// and not retain borrowed call parameters beyond a call. Code must remain
    /// loaded until the runner is dropped. Shared guest globals must not race
    /// across runners or threads. Native code can corrupt or terminate the host.
    pub unsafe fn build(mut self) -> Result<InProcessRunner> {
        self.host_functions.validate()?;
        Ok(InProcessRunner {
            runtime: unsafe { NativeRuntime::new(self.initialize, self.host_functions)? },
        })
    }
}

/// Thread-bound direct runner. Dropping it releases adapter state, not guest globals.
///
/// Recursive native execution on this thread is rejected, even for another instance.
/// Host callback panics become errors and render this runner unusable.
///
/// ```compile_fail
/// fn requires_send<T: Send>() {}
/// requires_send::<hyperlight_process::InProcessRunner>();
/// ```
///
/// ```compile_fail
/// fn requires_sync<T: Sync>() {}
/// requires_sync::<hyperlight_process::InProcessRunner>();
/// ```
pub struct InProcessRunner {
    runtime: NativeRuntime,
}

impl InProcessRunner {
    pub fn functions(&self) -> &[FunctionSignature] {
        self.runtime.functions()
    }

    pub fn call<Output: SupportedReturnType>(
        &mut self,
        function: &str,
        arguments: impl ParameterTuple,
    ) -> Result<Output> {
        let value = self.call_dynamic(guest_call::<Output>(function, arguments))?;
        Output::from_value(value.into()).context("converting native guest return value")
    }

    pub fn call_dynamic(&mut self, call: FunctionCall) -> Result<Value> {
        self.runtime.call(call)
    }
}
