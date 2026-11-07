# hlps

Small experiments with [Hyperlight](https://github.com/hyperlight-dev/hyperlight).

## Samples

- [C guest demo](samples/c-guest/README.md): build a freestanding C guest and
  call it from a Rust host using a Linux KVM micro-VM, or statically link the
  same guest into a native Rust subprocess without KVM.

## Rust workspace

- [hl-native-runner](crates/hl-native-runner/): generic native C guest adapter,
  Vercode IPC protocol, and reusable Rust subprocess client.
- [KVM host](samples/c-guest/host/): the original Hyperlight micro-VM demo.
- [Native sample](samples/c-guest/native/): statically linked runner and parent demo.

```sh
CARGO_BUILD_JOBS=1 make -C samples/c-guest run-native
CARGO_BUILD_JOBS=1 cargo test --workspace --locked
```

Native execution is **not a sandbox** and does not use Hyperlight's runtime.
Use only trusted native guests. The KVM demo remains available via
`make -C samples/c-guest run`.
