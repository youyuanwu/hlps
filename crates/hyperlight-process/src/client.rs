use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

use crate::func::{HostFunction, ParameterTuple, RegisteredHostFunction, SupportedReturnType};
use crate::protocol::{
    CallError, CallResult, FunctionCall, FunctionSignature, Message, PROTOCOL_VERSION, Value,
    read_message, write_message,
};

/// Configure a native executable and its typed host callbacks before spawning it.
///
/// ```no_run
/// use hyperlight_process::RunnerBuilder;
///
/// # fn main() -> anyhow::Result<()> {
/// let mut runner = RunnerBuilder::from_executable("my-native-runner")
///     .host_function("HostLog", |message: String| {
///         println!("{message}");
///         Ok(true)
///     })
///     .build()?;
/// let greeting = runner.call::<String>("Greet", "Hyperlight".to_owned())?;
/// let sum = runner.call::<i64>("Add", (20_i32, 22_i32))?;
/// runner.shutdown()?;
/// # Ok(())
/// # }
/// ```
pub struct RunnerBuilder {
    command: Command,
    host_functions: BTreeMap<String, RegisteredHostFunction>,
    validation_error: Option<anyhow::Error>,
}

impl RunnerBuilder {
    /// This path is a native runner executable, not a freestanding VM guest.
    pub fn from_executable(path: impl AsRef<Path>) -> Self {
        Self::from_command(Command::new(path.as_ref()))
    }

    pub fn from_command(command: Command) -> Self {
        Self {
            command,
            host_functions: BTreeMap::new(),
            validation_error: None,
        }
    }

    /// Register a typed callback. Re-registering a name replaces the earlier callback.
    pub fn host_function<Args: ParameterTuple, Output: SupportedReturnType>(
        mut self,
        name: impl AsRef<str>,
        function: impl Into<HostFunction<Output, Args>>,
    ) -> Self {
        let name = name.as_ref();
        if name.is_empty() || name.contains('\0') {
            self.validation_error = Some(anyhow::anyhow!(
                "host function name must be nonempty and contain no NUL"
            ));
        } else {
            self.host_functions
                .insert(name.to_owned(), function.into().into());
        }
        self
    }

    pub fn build(mut self) -> Result<Runner> {
        if let Some(error) = self.validation_error {
            return Err(error);
        }
        let mut runner = Runner::spawn(&mut self.command)?;
        runner.host_functions = self.host_functions;
        Ok(runner)
    }
}

/// A synchronous client. Host callbacks must not recursively call this runner.
pub struct Runner {
    child: Child,
    input: Option<BufWriter<ChildStdin>>,
    output: BufReader<ChildStdout>,
    functions: Vec<FunctionSignature>,
    host_functions: BTreeMap<String, RegisteredHostFunction>,
    next_id: u64,
    usable: bool,
    reaped: bool,
}

impl Runner {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("starting native guest runner")?;
        let input = child.stdin.take().context("runner stdin was not piped")?;
        let output = child.stdout.take().context("runner stdout was not piped")?;
        let mut runner = Self {
            child,
            input: Some(BufWriter::new(input)),
            output: BufReader::new(output),
            functions: Vec::new(),
            host_functions: BTreeMap::new(),
            next_id: 1,
            usable: true,
            reaped: false,
        };
        match read_message(&mut runner.output)?.context("runner exited before its handshake")? {
            Message::Hello {
                protocol_version,
                functions,
            } => {
                ensure!(
                    protocol_version == PROTOCOL_VERSION,
                    "unsupported native runner protocol version: {protocol_version}"
                );
                runner.functions = functions;
            }
            message => bail!("expected runner handshake, received {message:?}"),
        }
        Ok(runner)
    }

    pub fn functions(&self) -> &[FunctionSignature] {
        &self.functions
    }

    /// Call using Hyperlight's typed argument and return conventions.
    pub fn call<Output: SupportedReturnType>(
        &mut self,
        function: &str,
        arguments: impl ParameterTuple,
    ) -> Result<Output> {
        let value = self.call_dynamic(FunctionCall {
            function: function.into(),
            arguments: arguments
                .into_value()
                .into_iter()
                .map(Value::from)
                .collect(),
            return_type: Output::TYPE.into(),
        })?;
        Output::from_value(value.into()).context("converting native guest return value")
    }

    /// Call with runtime-defined values, using the builder's registered callbacks.
    pub fn call_dynamic(&mut self, call: FunctionCall) -> Result<Value> {
        self.exchange(call, None)
    }

    /// Low-level call with a per-call callback dispatcher instead of registered callbacks.
    pub fn call_with(
        &mut self,
        call: FunctionCall,
        mut host_callback: impl FnMut(&FunctionCall) -> CallResult,
    ) -> Result<Value> {
        self.exchange(call, Some(&mut host_callback))
    }

    fn exchange(
        &mut self,
        call: FunctionCall,
        mut host_callback: Option<&mut dyn FnMut(&FunctionCall) -> CallResult>,
    ) -> Result<Value> {
        ensure!(
            self.usable,
            "runner connection is unusable after a protocol failure"
        );
        let id = self.next_id;
        self.next_id = id.checked_add(1).context("runner request ID overflow")?;
        let expected_return = call.return_type;
        self.usable = false;
        let input = self.input.as_mut().context("runner stdin is closed")?;
        write_message(input, &Message::Call { id, call })?;
        loop {
            let message = read_message(&mut self.output)?.context("runner exited during a call")?;
            match message {
                Message::Result {
                    id: response_id,
                    result,
                } if response_id == id => {
                    if let CallResult::Value(value) = &result {
                        ensure!(
                            value.value_type() == expected_return,
                            "runner returned {:?}, expected {expected_return:?}",
                            value.value_type()
                        );
                    }
                    self.usable = true;
                    return match result {
                        CallResult::Value(value) => Ok(value),
                        CallResult::Error(error) => Err(error.into()),
                    };
                }
                Message::HostCall { id, call } => {
                    let result = match host_callback.as_deref_mut() {
                        Some(callback) => callback(&call),
                        None => {
                            let result = self
                                .host_functions
                                .get_mut(&call.function)
                                .with_context(|| {
                                    format!("unregistered host function: {}", call.function)
                                })
                                .and_then(|function| function.call(call));
                            match result {
                                Ok(value) => CallResult::Value(value),
                                Err(error) => CallResult::Error(CallError {
                                    code: 17,
                                    message: format!("{error:#}"),
                                }),
                            }
                        }
                    };
                    write_message(input, &Message::HostResult { id, result })?;
                }
                message => bail!("unexpected runner message during call {id}: {message:?}"),
            }
        }
    }

    pub fn shutdown(mut self) -> Result<()> {
        ensure!(
            self.usable,
            "cannot gracefully shut down an unusable runner connection"
        );
        let input = self.input.as_mut().context("runner stdin is closed")?;
        write_message(input, &Message::Shutdown)?;
        self.input.take();
        let status = self
            .child
            .wait()
            .context("waiting for native guest runner")?;
        self.reaped = true;
        ensure!(status.success(), "native guest runner exited with {status}");
        Ok(())
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.input.take();
        if self.reaped {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if let Err(error) = self.child.kill() {
                    eprintln!("could not terminate native guest runner: {error}");
                }
            }
            Err(error) => eprintln!("could not query native guest runner status: {error}"),
        }
        if let Err(error) = self.child.wait() {
            eprintln!("could not reap native guest runner: {error}");
        }
    }
}
