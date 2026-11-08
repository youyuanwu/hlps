# Three-mode benchmarks

See the [2026-11-08 results summary](RESULTS.md) for the complete measured
comparison, confidence intervals, lifecycle costs, latency tails, and CSV data.

One [stateless C guest](guest.c), compiled for Hyperlight/KVM and for native
execution, with the same typed calls and host callback in each backend:

| Workload | Parameters | Purpose |
|----------|------------|---------|
| `Add` | Two i32 values, i64 result | Minimal call overhead |
| `EchoBytes` | 64 B, 1 KiB, 4 KiB, 64 KiB | Copying, serialization, transport |
| `CallHost` | Seed and 1, 10, 100 callbacks | Synchronous guest-to-host overhead |
| `Compute` | Seed and 100, 10000, 1000000 iterations | CPU work versus boundary overhead |

Compute and callbacks use the same unsigned, dependency-chained operation.
Checksums and echoed bytes are validated outside timing. There is no guest
printing, message recording, or mutable global state.

## Build

Requirements are the same as the [C demo](../c-guest/README.md). Native modes
do not need KVM access. The build script reuses its checksum-verified SDK and
VM flags, builds the benchmark ELF, and statically links the native guest.
Both C builds use Clang with `-O2`; Rust measurements should use release builds.

From the repository root:

```sh
CARGO_BUILD_JOBS=1 cargo build --release --locked -p hyperlight-benchmarks --bins
```

## Criterion: steady-state throughput and regressions

```sh
# All three modes; requires read/write access to /dev/kvm.
CARGO_BUILD_JOBS=1 cargo bench --locked -p hyperlight-benchmarks --bench modes

# A single native mode, without KVM.
HL_BENCH_MODE=inprocess CARGO_BUILD_JOBS=1 \
  cargo bench --locked -p hyperlight-benchmarks --bench modes
HL_BENCH_MODE=subprocess CARGO_BUILD_JOBS=1 \
  cargo bench --locked -p hyperlight-benchmarks --bench modes

# Filter a workload and save a comparison baseline.
HL_BENCH_MODE=inprocess CARGO_BUILD_JOBS=1 \
  cargo bench --locked -p hyperlight-benchmarks --bench modes -- \
  inprocess/add --save-baseline before
```

`HL_BENCH_MODE` accepts `all` (default), `kvm`, `subprocess`, or `inprocess`.
Each backend gets an existing, initialized runner. Criterion warms it up and
reports statistically estimated time/call and throughput; creation and shutdown
are excluded. Defaults are 50 samples, 1-second warm-up, and 3-second measurement
per case. Standard Criterion CLI options can override these.

Criterion data/baselines are stored under the Cargo target directory's
`criterion/` directory. Plotting and Rayon are disabled to keep dependencies
and resource usage low. Criterion's confidence intervals are **not** per-call
p50/p95/p99 latency.

To smoke-test every benchmark without collecting statistics, add `-- --test`.
If only root can access KVM, build as your user with `cargo bench ... --no-run`,
then run the printed benchmark executable with `sudo <executable> --bench`.
The `--bench` flag is required for statistical measurement when invoked directly;
without it Criterion runs test mode. Do not run Cargo as root. `CRITERION_HOME`
can select a separate results directory; files written under sudo are root-owned.

## Lifecycle and individual-call latency

```sh
target/release/hyperlight-benchmark --mode all > timings.csv

target/release/hyperlight-benchmark \
  --mode subprocess --workload echo \
  --samples 1000 --warmup 100 --lifecycle-samples 20 --repeats 3 > timings.csv

target/release/hyperlight-benchmark --help
```

When KVM needs elevation, run the built CLI with `sudo`, not the build.
The CLI emits CSV on stdout and compiler/CPU/kernel/affinity, paths, and
measurement configuration on stderr. It reports:

- Creation through successful guest initialization, separately from teardown.
- First `Add` call on each newly created instance, before any warm-up.
- Warmed steady-state per-call mean and nearest-rank p50/p95/p99.
- Operations/second, and round-trip MiB/second for echo (`2 * payload size`,
  excluding framing).

Creation/first-call/teardown use `--lifecycle-samples`; warmed calls use
`--samples`. Each repeat runs modes sequentially, rotating mode order between
repeats. Sample storage is bounded (100000 call samples, 10000 lifecycle samples);
warm-up may be zero and repetitions are limited to 100.
Lifecycle creation is not a cold OS/page-cache measurement. In-process creation
initializes adapter state around already-linked code; it does not create isolated
guest globals or fresh process memory.

Both harnesses prepare payloads and expected results outside timing. Timed calls
include the input clone required by the owned typed API, allocations, transport,
return conversion, and result destruction. Backend/workload dispatch is outside
the timed loop. The latency CLI adds a clock read around every invocation, so use
Criterion's batch measurements for very fast-call throughput and regressions.

## Fairness and limitations

- KVM uses 2 MiB scratch, 128 KiB buffers in both directions, and 128 pages
  (512 KiB) per pool, including spare control-buffer capacity. These settings cover all payloads;
  they are applied to every VM workload. Native subprocess frames remain limited
  to 16 MiB. Requested mode/configuration failures are explicit; no fallback.
- The shared source is not an identical binary: VM builds are freestanding and
  use Hyperlight's C runtime; native builds use host libc and the native adapter.
  Results compare **complete execution paths**, not just Vercode and FlatBuffers.
- Native in-process execution has no memory/security isolation. Multiple runners
  would share guest globals, although this benchmark guest has none.
- Run on an otherwise idle machine. Record CPU placement, governor/turbo settings,
  and whether the host itself is virtualized. A subprocess inherits the parent's
  CPU affinity; pinning both to one CPU changes scheduling costs. Metadata records
  the inherited affinity, but this suite does not change it or CPU frequency.
- Memory footprint and concurrency are not measured in this first suite.

`HL_BENCH_GUEST` and `HL_BENCH_RUNNER` override the artifact paths in either
harness; the latency CLI also accepts `--guest` and `--runner`. Overrides must
point to builds of this benchmark guest, not the greeting demo.

## Tests

```sh
CARGO_BUILD_JOBS=1 cargo test --locked -p hyperlight-benchmarks
# Also run the ignored kvm_workloads test when /dev/kvm is accessible.
```

Tests check all native workload sizes, signed boundaries, zero-loop and unsigned
overflow cases, CSV shape, invalid CLI arguments, sample limits, and percentile
calculation. The ignored KVM test runs the same workload assertions.
Another ignored test checks all-mode CSV output and rotating execution order.
