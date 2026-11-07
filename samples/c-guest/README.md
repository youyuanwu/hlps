# Hyperlight C guest demo

This sample runs a freestanding C program inside a
[Hyperlight](https://github.com/hyperlight-dev/hyperlight) micro-VM. The VM has no
operating system: the guest uses Hyperlight's C API and bundled picolibc, not the
host's libc.

- `Greet(String) -> String` formats a greeting and calls the registered Rust
  `HostLog(String) -> Bool` function. The host acknowledges the message before
  the guest returns it.
- `Add(Int, Int) -> Long` adds two signed 32-bit integers with a signed 64-bit
  result, avoiding signed overflow.

## Requirements

- **Linux x86_64**, hardware virtualization enabled, and read/write access to
  `/dev/kvm`. Inside another VM, nested virtualization must be enabled.
- Rust/Cargo **1.89 or newer**.
- Clang and LLVM's `ld.lld`, GNU Make, a host C toolchain, curl, tar, and
  `sha256sum`. On Ubuntu/Debian:

  ```sh
  sudo apt install build-essential clang lld curl ca-certificates
  ```

- Internet access for the initial SDK and Cargo dependency downloads.

The native subprocess mode below requires Linux x86_64 and the build tools,
but does not require KVM or elevated permissions.

The host and guest are pinned to **Hyperlight 0.19.0**. Hyperlight checks the
guest's embedded version, so both must be upgraded together. The Makefile
downloads the released C headers and static runtime library, verifies their
SHA-256 checksums, and extracts them into the ignored `sdk/` directory. No
Hyperlight checkout or `cargo-hyperlight` installation is needed.

## Build and run

From the repository root:

```sh
make -C samples/c-guest
make -C samples/c-guest run
```

Expected output:

```text
guest -> host: Hello, Hyperlight! This is a Hyperlight C guest.
host <- guest: Hello, Hyperlight! This is a Hyperlight C guest.
host <- guest: Add(20, 22) = 42
All guest checks passed.
```

The host checks the returned greeting, the guest-to-host callback, and both
ordinary and wide integer addition. Its only optional argument is the guest
ELF path; without one it loads this sample's `build/guest`.

```sh
target/debug/hyperlight-c-guest-demo \
  samples/c-guest/build/guest
```

Cargo builds default to two concurrent jobs to limit memory usage; override
with `CARGO_BUILD_JOBS=1 make -C samples/c-guest` if needed.

## Native Rust subprocess

The same unmodified C guest can be statically linked into a normal Rust
executable using the workspace's [native runner](../../crates/hl-native-runner/).
The parent uses Vercode IPC and services `HostLog` over the subprocess pipes:

```sh
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-native
```

Expected output is the same greeting and `Add(20, 22) = 42`, followed by
`All native subprocess guest checks passed.` No sudo or KVM is needed.

The [native sample](native/) builds both the runner and the parent executable.
Its parent uses `RunnerBuilder::host_function` and typed
`runner.call::<String>("Greet", name)` / `runner.call::<i64>("Add", (left, right))`,
matching the KVM sandbox's calling conventions without changing the C guest.
The runner accepts binary IPC on stdin/stdout, not interactive commands. To
choose a different runner executable, pass its path to the parent demo:

```sh
target/debug/hyperlight-c-guest-native-demo \
  target/debug/hyperlight-c-guest-native-runner
```

This is explicit native execution, **not a fallback or equivalent isolation**
for the micro-VM. Only use trusted guests. Changing the statically linked guest
requires rebuilding its executable; the adapter itself is function-generic.
All workspace packages share the root Cargo lockfile and `target/` directory.

## Tests

```sh
# Native C tests with mocked Hyperlight APIs; no hypervisor required.
make -C samples/c-guest test

# Actual C guest/host round trip in KVM.
make -C samples/c-guest test-vm

# Actual native Rust subprocess, registration, IPC, callbacks, and errors.
make -C samples/c-guest test-native

# Adapter and wire-format tests plus native C subprocess integration.
CARGO_BUILD_JOBS=1 cargo test -p hl-native-runner -p hyperlight-c-guest-native --locked
```

The native tests exercise registration, the callback and return-value types,
empty names, the exact 255-byte greeting boundary, oversized-name errors,
negative acknowledgments, and signed integer extremes. They do not validate
the Hyperlight runtime or VM isolation; use `test-vm` for that.

## Troubleshooting

- **Cannot start the guest / permission denied:** check
  `ls -l /dev/kvm` and `test -r /dev/kvm && test -w /dev/kvm`.
  Ask your administrator to grant access (typically membership in the `kvm`
  group, followed by a new login). The sample does not change device permissions.
- **No KVM device:** enable virtualization/nested virtualization on the host.
  There is no native-execution fallback for the VM demo.
- **`No such device (os error 19)` despite `/dev/kvm` existing:** the device node
  is present, but the kernel has no usable KVM backend. In WSL2 this can happen
  when nested virtualization is not exposed to Linux. File permissions alone
  cannot fix it; enable nested virtualization in the outer host or run the
  sample on a Linux machine with functioning KVM.
- **Version mismatch:** update the host dependency, SDK URLs, checksums, and
  Cargo lockfile together, remove the old SDK cache, and rebuild.
- **Greeting too long:** `Greet` reports a guest error rather than returning a
  truncated message when the formatted greeting exceeds 255 bytes.

`make -C samples/c-guest clean` removes the compiled guest, native test, and
Cargo build output, but retains the SDK downloads.

## Files and upstream references

- [guest.c](guest.c): guest functions and `hyperlight_main` registration.
- [host/src/main.rs](host/src/main.rs): sandbox creation, host callback, and calls.
- [Makefile](Makefile): freestanding ELF build and pinned SDK downloads.
- [guest_test.c](guest_test.c): native guest behavior tests.
- [Upstream guest build guide](https://github.com/hyperlight-dev/hyperlight/blob/v0.19.0/docs/how-to-build-a-hyperlight-guest-binary.md).
- [Hyperlight 0.19.0 release](https://github.com/hyperlight-dev/hyperlight/releases/tag/v0.19.0).

Sample source is covered by the repository's [MIT license](../../LICENSE).
The downloaded Hyperlight runtime is Apache-2.0 licensed and includes picolibc;
see [upstream third-party licensing](https://github.com/hyperlight-dev/hyperlight/blob/v0.19.0/src/hyperlight_libc/third_party/README.md)
before redistributing the SDK.
