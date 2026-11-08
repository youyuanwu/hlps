# Three-mode benchmark results: 2026-11-08

All **33 Criterion cases** completed: 11 workloads in each of KVM, native
subprocess, and native in-process mode. Each case collected 50 samples.
A separate latency/lifecycle pass completed three rotating repeats for all modes.

## Summary

- **Small calls:** in-process `Add` took about **270 ns**, versus **20.3 us**
  for KVM and **15.6 us** for subprocess in Criterion. That is approximately
  **75x** and **58x** faster, respectively.
- **Large transfers:** at 64 KiB, KVM took **39.8 us**, beating subprocess's
  **63.5 us**. In-process took **7.37 us**. Subprocess is not always faster
  than KVM; payload size and the complete transport path matter.
- **Host callbacks:** 100 callbacks took **1.88 ms** in KVM, **1.62 ms** in
  subprocess, and **16.0 us** in-process.
- **CPU-bound work:** one million dependent operations took **3.013 ms**,
  **3.000 ms**, and **2.969 ms**. The gap shrank to approximately **1.5%**
  for KVM and **1.1%** for subprocess relative to in-process.
- **Instance reuse matters:** median repeat-average creation was **2.80 ms**
  for KVM, **551 us** for subprocess, and **1.33 us** for the in-process
  adapter. These do not provide equivalent isolation.

These are measurements on one virtualized machine, not universal performance
guarantees. In particular, the separate latency pass did not reproduce
Criterion's small-call ordering between KVM and subprocess.

## Environment and procedure

| Setting | Value |
|---------|-------|
| Measurement window | 2026-11-08, 04:42:36 to 04:45:56 UTC |
| Host | Microsoft full virtualization; KVM guests therefore run nested |
| CPU model | Intel Xeon Platinum 8573C |
| Exposed topology | 2 logical CPUs; guest-visible 1 core, 2 threads |
| CPU affinity | CPUs 0-1; subprocess inherited the parent's affinity |
| Frequency policy | `schedutil`; frequency/turbo not pinned or changed |
| Memory | 7.7 GiB RAM, no swap; about 6.1 GiB available |
| Kernel | `7.0.0-1019-azure` |
| Rust | `1.93.1`, release optimization level 3 |
| Clang | Ubuntu `21.1.8`; both C builds use `-O2` |
| Libraries | Hyperlight 0.19.0, Vercode 0.7.0, Criterion 0.7.0 |
| KVM configuration | 2 MiB scratch, 128 KiB buffers, 128 pages per direction's pool |
| Source | Base revision `0478d2d8d4c2497744bb89f57007f5bb337f99e9` plus the benchmark suite in this working tree |

The same [stateless C source](guest.c) implements all workloads. Each backend
uses the same typed signatures and `HostStep` operation. Payloads and expected
results are prepared outside timing; timed calls include required input copies,
allocations, transport, return conversion, and result destruction.
There is no guest printing or message recording.

Criterion ran modes sequentially in KVM, subprocess, in-process order, with
one second of warm-up and a three-second measurement target per case.
The three one-million-operation cases needed approximately 3.8 seconds to
complete linear sampling; Criterion warned and extended collection rather than
reducing the 50 samples. All cases used linear sampling.
The Criterion pass took approximately 155 seconds.

The subsequent latency pass used 100 warm-up calls, 1000 individual timed calls
per workload per repeat, and 20 creation/first-call/teardown samples per repeat.
Its mode orders were:

1. KVM, subprocess, in-process.
2. Subprocess, in-process, KVM.
3. In-process, KVM, subprocess.

That pass took approximately 45 seconds. No builds or other benchmarks were
launched concurrently by this suite. Host scheduling and neighboring VM activity
were not controlled. One-minute load average was 0.14 before and 1.24 after the
combined run; the ending load includes the benchmark's own activity.

## Criterion: every case

All times below are **microseconds per guest invocation**, written as
`estimate [95% confidence interval]`. The estimate is Criterion's linear
regression slope, matching its displayed time estimate. These intervals describe
statistical uncertainty within this run; they are **not per-call percentiles**
and do not capture all between-run or machine-to-machine variation.

| Workload | KVM | Subprocess | In-process |
|----------|----:|-----------:|-----------:|
| `add` | 20.315 [19.939, 20.761] | 15.574 [14.442, 16.455] | 0.270 [0.265, 0.278] |
| `echo/64` | 20.198 [19.898, 20.571] | 16.741 [15.950, 17.200] | 0.278 [0.276, 0.282] |
| `echo/1024` | 19.843 [19.729, 20.023] | 17.407 [16.729, 17.835] | 0.311 [0.309, 0.317] |
| `echo/4096` | 21.978 [21.607, 22.319] | 18.814 [18.189, 19.128] | 0.535 [0.529, 0.546] |
| `echo/65536` | 39.798 [39.426, 40.336] | 63.507 [63.143, 63.740] | 7.373 [7.338, 7.418] |
| `callbacks/1` | 36.576 [36.278, 37.117] | 32.387 [30.515, 33.698] | 0.460 [0.456, 0.470] |
| `callbacks/10` | 207.169 [205.352, 209.539] | 177.183 [168.848, 183.151] | 1.886 [1.870, 1.916] |
| `callbacks/100` | 1875.135 [1849.797, 1904.423] | 1617.906 [1550.647, 1664.177] | 16.031 [15.911, 16.211] |
| `compute/100` | 20.973 [20.790, 21.219] | 16.201 [15.030, 16.970] | 0.552 [0.549, 0.556] |
| `compute/10000` | 50.494 [50.279, 50.914] | 51.707 [50.771, 52.406] | 30.009 [29.956, 30.066] |
| `compute/1000000` | 3013.293 [3006.256, 3021.394] | 3000.049 [2996.097, 3004.311] | 2968.526 [2964.949, 2972.645] |

For `Add`, these estimates correspond to approximately 49,226 calls/second
for KVM, 64,211 for subprocess, and 3,709,639 for in-process.
The slight 64 B versus 1 KiB KVM inversion should not be interpreted as larger
payloads intrinsically being faster.

### Byte throughput

**Round-trip MiB/second**, counting `2 * payload size` per call, excluding
framing. This is effective echo throughput, not measured physical memory
bandwidth or a zero-copy transfer rate.

| Payload bytes | KVM | Subprocess | In-process |
|--------------:|----:|-----------:|-----------:|
| 64 | 6.0 | 7.3 | 438.7 |
| 1024 | 98.4 | 112.2 | 6275.6 |
| 4096 | 355.5 | 415.2 | 14608.9 |
| 65536 | 3140.9 | 1968.3 | 16953.2 |

The 64 KiB result shows that the KVM transport can outperform subprocess
serialization and pipe I/O for this workload, despite slower minimal calls.
This does not isolate the codec's contribution: VMM transitions, scheduling,
allocations, and copying are all included.

### Callback scaling

Using `(callbacks/100 - callbacks/1) / 99`, the approximate marginal cost
per additional callback was **18.6 us** for KVM, **16.0 us** for subprocess,
and **0.157 us** for in-process. This includes dispatch, conversion, transport,
and the host's dependent operation; it is not a measurement of a bare Rust
function call. Each table entry still includes one outer guest invocation.

## Lifecycle and first-call costs

Values are **microseconds**, shown as the median of the three repeat-average
times, followed by `[minimum repeat average, maximum repeat average]`.
They are not confidence intervals. Each repeat has only 20 lifecycle samples;
this is not sufficient to establish reliable lifecycle tail percentiles.

| Phase | KVM | Subprocess | In-process |
|-------|----:|-----------:|-----------:|
| Create through initialization | 2802.875 [2624.135, 3057.380] | 550.547 [548.790, 559.169] | 1.329 [1.265, 1.483] |
| First `Add`, before warm-up | 315.051 [297.903, 343.146] | 33.334 [30.038, 38.685] | 0.876 [0.851, 0.995] |
| Teardown | 4984.663 [4981.949, 5274.621] | 104.658 [102.580, 104.698] | 0.589 [0.589, 0.609] |

Creation and teardown are excluded from steady-state results. KVM teardown
was approximately 5 ms here, reinforcing the importance of runner reuse.
Creation is not a cold page-cache or cold OS-start measurement.
In-process creation initializes adapter state around already-linked code;
it does not create fresh isolated guest globals, heap, or process memory.

## Individual-call latency and variability

These numbers come from the **separate instrumented latency pass**, not
Criterion. Values are microseconds and are the median of the three repeats'
reported statistic. Percentiles are not pooled across repeats.

### `Add` latency

| Mode | Mean | p50 | p95 | p99 | Range of repeat means |
|------|-----:|----:|----:|----:|---------------------:|
| KVM | 19.071 | 18.698 | 19.906 | 27.938 | 18.991 to 20.827 |
| Subprocess | 26.365 | 25.721 | 34.388 | 37.572 | 25.668 to 26.817 |
| In-process | 0.315 | 0.290 | 0.444 | 0.533 | 0.302 to 0.317 |

Subprocess `Add` was faster than KVM in Criterion, but slower in all three
latency repeats. The instrumented pass changes the timing/sampling pattern,
and scheduling/frequency conditions can also differ between passes. This
discrepancy is not explained by the data alone and should not be attributed
solely to clock-read overhead. Do not treat the small-call subprocess/KVM
ranking as stable without additional controlled runs.
In-process remained substantially faster in both harnesses.

### p99 for every steady-state case

Each entry is the median repeat p99, in microseconds, from 1000 calls per
repeat. Full mean/p50/p95/p99 values for every repeat are in the latency CSV.

| Workload | KVM | Subprocess | In-process |
|----------|----:|-----------:|-----------:|
| `add` | 27.938 | 37.572 | 0.533 |
| `echo/64` | 28.627 | 37.135 | 0.455 |
| `echo/1024` | 26.394 | 36.350 | 0.371 |
| `echo/4096` | 28.887 | 27.077 | 0.604 |
| `echo/65536` | 52.526 | 75.836 | 9.178 |
| `callbacks/1` | 49.555 | 42.242 | 0.548 |
| `callbacks/10` | 211.485 | 200.609 | 1.992 |
| `callbacks/100` | 2587.308 | 1774.143 | 22.036 |
| `compute/100` | 26.837 | 25.917 | 0.636 |
| `compute/10000` | 61.311 | 58.858 | 36.084 |
| `compute/1000000` | 3152.683 | 3165.597 | 3120.721 |

## Data and reproduction

- [Criterion estimates CSV](results/2026-11-08-criterion.csv): 33 rows,
  with sample count, estimate kind, nanosecond estimate, 95% interval, and rates.
- [Latency/lifecycle CSV](results/2026-11-08-latency.csv): 126 rows,
  including each mode, case, and repeat rather than just the summary above.
- Raw Criterion samples, the `full-2026-11-08` baseline, logs, and before/after
  environment metadata remain locally in the ignored
  `../../target/benchmark-results/2026-11-08-full/` directory.

From the repository root:

```sh
CARGO_BUILD_JOBS=1 cargo bench --locked -p hyperlight-benchmarks --bench modes --no-run

# Use the executable printed by Cargo; substitute its hash if different.
sudo env HL_BENCH_MODE=all \
  CRITERION_HOME="$PWD/target/benchmark-results/2026-11-08-full/criterion" \
  target/release/deps/modes-797d01a5b4caa8cf \
  --bench --sample-size 50 --warm-up-time 1 --measurement-time 3 \
  --save-baseline full-2026-11-08 --noplot

CARGO_BUILD_JOBS=1 cargo build --release --locked -p hyperlight-benchmarks --bins
sudo target/release/hyperlight-benchmark --mode all --workload all \
  --samples 1000 --warmup 100 --lifecycle-samples 20 --repeats 3 \
  > timings.csv
```

Choose a new results directory/baseline name when collecting a new run rather
than overwriting this baseline. Running the measurements with sudo was necessary
for this machine's KVM permissions; Cargo was run as the normal user.

## Interpretation limits

- Native execution is not a security sandbox; in-process faults can terminate
  or corrupt the host. Its speed does not make its isolation equivalent to KVM.
- The C source is shared, but binaries and runtimes are not: the VM build is
  freestanding and uses Hyperlight's C runtime; native builds use host libc and
  the adapter. These are end-to-end execution-path comparisons.
- There was one full Criterion pass, not multiple independently repeated full
  passes. The latency repeats expose some variation but do not replace that.
- Frequency, CPU placement, SMT contention, and external host activity were
  not controlled. The machine is virtualized, so these are not bare-metal KVM
  overhead estimates.
- Memory footprint, concurrency, contention between multiple guests, and
  production application workloads were not measured.

For trusted guests with frequent small calls or callbacks, in-process execution
has the clearest performance advantage in this run. For CPU-heavy guests,
choose the isolation model first: measured compute throughput was very similar.
For subprocess versus KVM, benchmark the actual payload and callback mix;
neither was uniformly faster.
