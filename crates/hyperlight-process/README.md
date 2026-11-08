# hyperlight-process

This crate implements the Hyperlight **0.19.0 C function-call ABI** for native,
statically linked guests on **Linux x86_64**. It contains:

- A registration-driven runtime: no function-specific Rust bindings.
- Owned, Vercode-encoded call, result, error, and host-callback messages.
- A synchronous Rust client that owns and reaps the runner subprocess.
- A sandbox-style builder, typed guest calls, and registered typed host callbacks.
- A thread-bound in-process runner using direct calls without IPC or serialization.

**This is not Hyperlight's VM runtime or a security sandbox.** Native guest
code has the process's filesystem, network, and syscall access. Invalid native
pointers, crashes, or infinite loops cannot be safely contained by this adapter.
In-process guest failures can corrupt or terminate the host itself.
Only run trusted guests, or apply separate OS-level restrictions.

## Statically link a guest

Compile your C source into a native static library, for example with `cc` in a
`build.rs`. Use the matching C API declarations and normal host libc; do not
link Hyperlight's freestanding runtime or use its VM guest compiler flags.
Link this crate into your runner executable:

```rust,no_run
unsafe extern "C" {
    fn hyperlight_main();
}

fn main() -> anyhow::Result<()> {
    unsafe { hyperlight_process::runtime::run(hyperlight_main) }
}
```

The C initializer registers function names, signatures, and callbacks. The
runtime copies that metadata, validates incoming calls, and invokes the fixed
`hl_CGuestFunc` ABI. A different statically linked guest requires rebuilding
its executable, but does not require modifying this crate.

See the [sample build script](../../samples/c-guest/native/build.rs) and
[runner entry point](../../samples/c-guest/native/src/bin/runner.rs).

## Parent process

```rust,no_run
use hyperlight_process::RunnerBuilder;

# fn main() -> anyhow::Result<()> {
let mut runner = RunnerBuilder::from_executable("my-native-runner")
    .host_function("HostLog", |message: String| {
        println!("guest -> host: {message}");
        Ok(true)
    })
    .build()?;
let greeting = runner.call::<String>("Greet", "Hyperlight".to_owned())?;
let sum = runner.call::<i64>("Add", (20_i32, 22_i32))?;
println!("{greeting}; sum = {sum}");
runner.shutdown()?;
# Ok(())
# }
```

The calling conventions match Hyperlight's `Sandbox`: `()` means no arguments,
a single value means one argument, and a tuple supplies up to 32 arguments.
Return types can be explicit (`call::<i64>`) or inferred from a typed variable.
The native API reuses `hyperlight-common`'s `ParameterTuple`,
`SupportedParameterType`, and `SupportedReturnType` traits rather than
maintaining a second set of type/tuple implementations. These are re-exported
from `hyperlight_process::func`, together with `Bytes` for `Vec<Bytes>` byte chunks.
Subprocess mode uses Vercode, not Hyperlight's FlatBuffers transport or VMM.

Host callbacks accept `FnMut + Send + 'static` closures with individually typed
arguments and either a plain return value or `anyhow::Result<T>`. Register them
once with the builder; names registered again replace earlier callbacks.
Missing callbacks, invalid argument/return signatures, and callback errors
produce explicit host-function errors without invoking a mismatched callback.

`from_executable` deliberately names the native runner executable, not a VM
guest ELF. To customize arguments, environment, or working directory, use
`RunnerBuilder::from_command(std::process::Command)`. This API mirrors builder
and calling ergonomics only: it does not implement VM snapshots or restoration.

For dynamic calls, `runner.call_dynamic(FunctionCall { ... })` uses registered
callbacks and returns a protocol `Value`. For a custom per-call dispatcher,
use `runner.call_with(call, callback)`, which overrides the registered callbacks
for that call. The earlier `call(FunctionCall, callback)` API is now named
`call_with`; its wire behavior is unchanged. `Runner::spawn` also remains
available for direct process creation.

The handshake exposes registered signatures through `Runner::functions()`.
Guest errors leave the connection usable. Invalid IPC, unexpected messages,
and a child exiting during a call are errors; do not reuse that connection.
Dropping the client terminates and reaps a remaining child. `shutdown` requests
a clean exit and checks its status.

## Direct in-process execution

Link the same native C static library into the host instead of a separate
executable. Invocation is registration-driven and needs no per-function code
generation. Both directions convert owned values to/from the same C ABI, with
**no Vercode encoding, pipe I/O, subprocess, or VM**:

```rust,no_run
use hyperlight_process::InProcessRunnerBuilder;

unsafe extern "C" {
    fn hyperlight_main();
}

# fn main() -> anyhow::Result<()> {
let mut runner = unsafe {
    InProcessRunnerBuilder::new(hyperlight_main)
        .host_function("HostLog", |message: String| {
            println!("guest -> host: {message}");
            Ok(true)
        })
        .build()?
};
let greeting = runner.call::<String>("Greet", "Hyperlight".to_owned())?;
let sum = runner.call::<i64>("Add", (20_i32, 22_i32))?;
# Ok(())
# }
```

`build` is unsafe: the guest and its pointers must obey the C ABI for the
runner's entire lifetime. The runner is neither `Send` nor `Sync`, so create,
call, and drop it on its owning thread. Host callbacks have the same typed
registration API as subprocess mode; `functions()` and `call_dynamic` are also
available. There is no per-call `call_with` override in direct mode. Drop the
runner to release adapter state; there is no child process to shut down.

The shared runtime scopes C API thread-local state to each active instance.
Registrations, host closures, errors, and result storage are instance-local.
Native C globals, heap, and libc state are **not** isolated or reset: multiple
instances linked to the same guest share its globals, and callers must prevent
races. Linking distinct guests also requires distinct initializer/global
symbols; the runner does not rename symbols or load libraries dynamically.

Recursive native execution on a thread is rejected, including calls or
construction of another direct runner from a host callback. Host callbacks
become available after guest initialization, just as in subprocess mode.
Ordinary guest/host errors leave direct instances usable. Rust host callback
panics are caught before crossing the C ABI and make only that runner unusable.
This requires panic unwinding; `panic=abort` still terminates the host.
Invalid C memory access and guest abort APIs can terminate the entire host,
and a hung guest cannot be safely interrupted by this library.

Direct mode bypasses the subprocess 16 MiB frame limit, but still copies
argument/return data for ownership and applies C ABI validation. It is not a
zero-copy or sandboxed execution mode. The dependency still includes Vercode
for the subprocess API, but direct execution never invokes its codec.

Run the [direct C demo](../../samples/c-guest/native/src/bin/inprocess.rs):

```sh
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-inprocess
```

## Subprocess protocol and shared ownership

The protocol uses pinned **Vercode 0.7.0**, not raw C structs or Hyperlight's VM
FlatBuffers transport. Each top-level message uses Vercode's little-endian
four-byte length, including its header, with a **16 MiB maximum frame size**.
Reads handle partial I/O and EOF, and decoding must consume the complete frame.
The frame limit bounds encoded input, not all possible guest allocations.

1. Runner sends `Hello` with protocol version 1 and guest signatures.
2. Parent sends a `Call` with a request ID.
3. During guest execution, runner can send `HostCall` and synchronously wait
   for the matching `HostResult`.
4. Runner sends the matching `Result`, containing a value or explicit error.
5. Parent sends `Shutdown`, or closes stdin.

Calls are sequential; callbacks must not recursively call the same runner.
There are no built-in execution deadlines. An external supervisor is needed
to terminate a hung native guest or parent callback.

Values support all C API argument and return types, including unsigned
integers, floats, bytes, byte chunks, and void returns. C strings must be UTF-8
and contain no interior NUL. Buffers are owned by Rust while guest callbacks
execute. Guest result helpers copy data and retain results until the next call.
Host-return getters consume their value once; strings and byte-vector
descriptors/data use libc allocations that the guest must `free`.
Use `hl_free_byte_chunks` for the byte-chunk getter's result.

Protocol stdout is reserved exclusively for binary messages. Guests must use
`HostLog`, `hl_log`, or stderr for logging; native `printf` to stdout corrupts
the connection. Direct mode does not reserve stdout. Abort APIs log to stderr
and terminate the current process, including the host in direct mode.
Guest callbacks and adapter API calls must execute on the runner's thread.
IPC host callbacks are available only after the initialization handshake;
attempting one during initialization reports an error without waiting for IPC.
The optional custom C dispatcher hook is not supported; use registration.

For future compatible extensions, add fields with increasing Vercode
`#[version(n)]` annotations and meaningful defaults. Keep same-version field
ordering and types stable. Unknown message/value variants are rejected:
negotiate new capabilities or bump the protocol version for breaking changes.

## Validation

From the repository root:

```sh
CARGO_BUILD_JOBS=1 cargo test -p hyperlight-process -p hyperlight-c-guest-native --locked
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-native
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-inprocess
```

Tests exercise all argument/return types and host getters through the shared
C ABI in both modes, scoped state and reentrancy rejection, panic handling,
and direct payloads larger than the IPC frame limit. Integration tests run the
unmodified C sample's strings, callbacks, integer extremes, and error paths
in subprocess and direct modes.
