# hlps

Small experiments with [Hyperlight](https://github.com/hyperlight-dev/hyperlight).

## Samples

- [C guest demo](samples/c-guest/README.md): build a freestanding C guest and
  call it from a Rust host using a Linux KVM micro-VM, or statically link the
  same guest into a native Rust subprocess or directly into its host without KVM.

## Rust workspace

- [hyperlight-process](crates/hyperlight-process/): generic native C guest adapter,
  Vercode IPC protocol, Rust subprocess client, and direct in-process runner.
- [KVM host](samples/c-guest/host/): the original Hyperlight micro-VM demo.
- [Native sample](samples/c-guest/native/): subprocess and direct statically linked demos.

```sh
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-native
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-inprocess
CARGO_BUILD_JOBS=1 cargo test --workspace --locked
```

Native execution is **not a sandbox** and does not use Hyperlight's runtime.
Use only trusted native guests. The KVM demo remains available via
`make -C samples/c-guest run`.
