# PostgreSQL evaluation: 2026-11-08

**All targeted evaluation checks passed**: three serial repeats of native
PostgreSQL 16.15 and the Hyperlight-Unikraft guest, plus a separate profiled
guest run. The persistent-storage/recovery prerequisite also passed.

## Summary and decision

- Median warmed `SELECT 1`: **49.8 us native / 411.3 us guest**, about 8.3x.
- Median indexed lookup: **64.6 us / 685.8 us**, about 10.6x.
- Median synchronous update: **3.20 ms / 4.31 ms**, about 1.35x.
- Median spill checksum: **203 ms / 4,975 ms**, about 24.5x.
- Sampled server-tree PSS: **51.1 MiB native / 135.9 MiB guest**.
- Filesystem callback body time is dominated by writes in the diagnostic.

**Go for further bounded prototype research; not a production readiness
decision.** Real PostgreSQL SQL, disk spilling, synchronization, and fresh-VM
recovery work within the tested limits. The current audited development-build
storage path is expensive, especially for spilling. Optional features,
snapshots, full POSIX descriptor/locking semantics, and physical power-loss
durability remain unvalidated or unsupported.

These ratios compare the **complete experimental stacks**, not Hyperlight's
isolation cost alone. Native PostgreSQL is unmodified and uses glibc/fork;
the guest uses musl, EXEC_BACKEND/posix_spawn, assertions, and the patched
single-address-space kernel. Its Rust host is unoptimized and every relevant
storage mutation is audited/accounted. Native has no equivalent host audit.
Do not extrapolate these measurements to an optimized or production port.

## Environment and procedure

| Setting | Value |
|---------|-------|
| Measurement window | 2026-11-08, 18:14:03 to 18:16:38 UTC |
| Host virtualization | Microsoft full virtualization; Hyperlight KVM is nested |
| CPU | Intel Xeon Platinum 8573C; 2 exposed logical CPUs, 1 core / 2 threads |
| Affinity | Every server CPU 0; benchmark client and sampler CPU 1 |
| Frequency/load control | `schedutil`; no frequency/turbo or neighbor-load control |
| Kernel / RAM | Linux `7.0.0-1019-azure`; 7.7 GiB RAM, no swap |
| Storage | `/data` on ext4, `/dev/sdb1`, `rw,relatime`; underlying device performance not controlled |
| PostgreSQL | 16.15, native source unmodified; pinned target-guarded guest patches |
| Compiler/runtime | GCC 15.2.0; glibc 2.43 native/client; musl 1.2.5 guest |
| Rust / host build | Rust 1.93.1; development profile, debug=0, opt-level=0 |
| PostgreSQL/client flags | `-O2 -march=x86-64`; guest assertions enabled, native assertions disabled |
| Synchronization | `fsync=on`, `synchronous_commit=on`, WAL method `fsync` |
| Memory/worker profile | Shared buffers 16 MiB, work_mem 64 KiB; max connections 2; optional workers/parallelism/autovacuum disabled |
| Source base | `77973d6` plus the evaluation changes; exact pins in [sources.toml](sources.toml) |
| Cache conditions | Fresh server processes, retained host page cache, fixture/EXPLAIN/checkpoint preparation, per-workload warm-up |

Modes ran sequentially in native/guest, guest/native, native/guest order.
No native server and guest VM overlapped. Host CPU 0 and client CPU 1 are
sibling threads: this is not an independent two-core allocation or an SMP
benchmark. Affinity was verified while sampling and restored afterward.
One-minute host load increased from 0.57 to 1.73, including the suite's own
activity. Shared-host scheduling and cache conditions remain uncontrolled.

Both modes recreate the same 100,000-row C-locale fixture, unique ID index,
and one-row update table before each repeat. Existing dedicated PGDATA/WAL
directories are reused; initialization history and other gate tables are
not byte-identical. Preparation is outside query timing and deliberately
warms caches. No reboot, global cache drop, snapshot, or live data clone.

The [C client](tests/query-client.c) uses native libpq and CLOCK_MONOTONIC.
Latency includes synchronous `PQexec`/protocol completion but excludes exact
result validation and CSV output. Each short workload has 50 untimed calls,
then 1,000 selects/lookups or 300 updates. Spilling has one untimed call and
five timed calls per repeat. Throughput is calls divided by whole measured
batch time, including loop/result-validation overhead and excluding CSV.
It is single-client closed-loop throughput, not saturation capacity.

## Startup and first benchmark connection

All times are milliseconds, reported as **median [min, max]**, three samples
per mode.

| Metric | Native | Guest |
|--------|-------:|------:|
| Launch to observed readiness | 114.972 [114.864, 115.107] | 451.969 [449.268, 454.324] |
| First benchmark connection | 7.212 [6.424, 7.463] | 44.013 [42.175, 44.199] |
| First benchmark SELECT 1 | 0.158 [0.154, 0.164] | 0.698 [0.684, 0.833] |

Startup includes launch wrappers, guest boot/ownership probes where relevant,
and host `pg_isready` polling at approximately 100 ms intervals. It excludes
builds and database initialization, and is **observed end-to-end readiness**,
not intrinsic PostgreSQL boot time. First connection/query timings occur
after readiness probes and fixture preparation; they are not cold-cache or
first-ever cluster queries. Three samples do not establish startup tails.

## Warmed SQL latency and throughput

Latency values are **microseconds**, pooled across the three main repeats.
Percentiles use nearest rank. No confidence interval or universal tail claim
is inferred from this finite, scheduling-dependent run.

| Workload | Mode | Samples | p50 | p95 | p99 | Calls/s |
|----------|------|--------:|----:|----:|----:|--------:|
| SELECT 1 | Native | 3,000 | 49.756 | 65.121 | 110.754 | 18,865.9 |
| SELECT 1 | Guest | 3,000 | 411.324 | 460.915 | 914.325 | 2,301.4 |
| Indexed lookup | Native | 3,000 | 64.551 | 94.422 | 709.018 | 11,577.4 |
| Indexed lookup | Guest | 3,000 | 685.826 | 983.486 | 1,995.103 | 1,400.6 |
| Synchronous update | Native | 900 | 3,201.173 | 4,322.581 | 6,157.061 | 300.5 |
| Synchronous update | Guest | 900 | 4,313.421 | 5,626.302 | 8,099.983 | 222.3 |

Lookups select deterministically distributed IDs and verify ID, payload length,
and exact payload content. Each implicit-transaction update increments one
counter and validates every returned value, including warm-up. Synchronous
commit completes before `PQexec` returns; the final counter is exactly 350.
Each guest audit contains **360 successful real WAL fsyncs** across the whole
run, including startup/preparation/checkpoint, with no host operation errors.

Between-repeat median ranges were 41.454-51.897 us native / 409.404-412.847 us
guest for SELECT, 63.409-65.731 / 685.208-687.152 us for lookup, and
3.049-3.271 / 4.230-4.419 ms for updates. Native lookup throughput varied
from **8,136.6 to 14,784.4 calls/s**, despite relatively stable medians.
This is why throughput must not be inferred from median latency or treated
as a machine-independent result.

## Disk spilling and correctness

Every timed checksum equals the independent Python/native expectation:
`6d334d76c6d5153bbf8eebf552f99702`.

| Measurement | Native | Guest |
|-------------|-------:|------:|
| Timed full ordered-result checksums | 15 | 15 |
| Median checksum, ms | 203.145 | 4,975.069 |
| Observed p95/max checksum, ms | 227.288 | 5,029.065 |
| Checksum calls/s | 4.804 | 0.201 |
| EXPLAIN disk sort space, KiB | 27,280 | 27,272 |
| Actual temp blocks read / written | 13,614 / 14,334 | 13,610 / 14,352 |

The plan requires a disk sort with exactly 100,000 actual rows and nonzero
temp reads/writes. These are measured fields, not an inference from low
work_mem. Temp files are absent after the run. With only 15 spill samples
per mode, p95 and p99 both select the maximum and are not reliable tail
estimates. This is an ordered-sort-plus-checksum workload, not raw disk
bandwidth or a general SQL throughput benchmark.

## Resource use

| Measurement | Native | Guest |
|-------------|-------:|------:|
| Sampled server-tree peak PSS, KiB | 52,293 | 139,121 |
| Sampled sum RSS, KiB | 87,832 | 140,936 |
| Maximum sampled server process count | 5 | 1 host process |
| Largest post-shutdown logical storage, bytes | 58,683,194 | 58,811,080 |
| Post-shutdown WAL bytes | 16,777,216 | 16,777,216 |

PSS is summed across the native postmaster/children to avoid shared-page
double counting; summed native RSS is not a physical-memory total. Guest
figures include the host and resident guest mappings, but not the benchmark
client. Sampling starts after readiness, covers preparation/SQL, runs every
50 ms, and cannot prove instantaneous peaks. No child-exit sampling race was
observed in these repeats.

Guest audited logical storage high-water reached **86,706,372 bytes**
(about 82.7 MiB), below 512 MiB. This includes spill growth, unlike the
post-shutdown table above. Native temp space is evidenced by EXPLAIN and
temp-file cleanup, not a sampled whole-directory high-water measurement.
Logical budgets exclude physical inode/allocation overhead and outside-mount
audit/build artifacts. Guest scratch stays capped at 512 MiB and host/tree
memory checks at 1 GiB.

## Guest-to-host filesystem diagnostic

The separate reduced profiled run is excluded from the latency tables. It
includes boot, fixture/EXPLAIN preparation, 10 warm-up and 50 timed calls for
each short workload, two checksum calls including warm-up, checkpoint, and
shutdown. Its filesystem profile records **100,395 callbacks**, totaling
**5,779.672 ms of callback body time**:

| Callback | Calls | Total ms | Mean us |
|----------|------:|---------:|--------:|
| fs_write_bytes | 48,463 | 4,642.143 | 95.8 |
| fs_read_bytes | 50,830 | 424.024 | 8.3 |
| fs_sync | 147 | 672.880 | 4,577.4 |
| fs_stat | 832 | 9.860 | 11.9 |
| fs_list_page | 55 | 12.311 | 223.8 |

Writes account for about 80% of filesystem callback body time. This supports
investigating the path-based backend, repeated metadata/accounting, auditing,
and transport granularity before expanding features. It does **not** prove
which optimization would help or how fast an optimized port would be.

The existing upstream profiler times host callback bodies. These include
backend calls, quota/accounting, and audit serialization/writes, while excluding
VM exit/reentry and some argument marshalling. Whole-life totals also include
preparation and shutdown: they are not per-query I/O costs. Neither subtracting
native latency nor this profile isolates pure Hyperlight overhead.

## Reproducibility and remaining limits

Run [the documented targets](README.md#phase-5-nativeguest-evaluation).
The successful raw run is
`target/postgres-experiment/phase5/20261108T181403.037139Z/`, with per-query
CSVs, EXPLAIN JSON, logs, diagnostic profile, and versioned `results.json`.
The top-level Phase 5 result records current script/client/host/rootfs/native
tool hashes and exact source pins; generated artifacts and credentials remain
ignored. No PostgreSQL compatibility patch was added for evaluation.

Validation: **25 Python tests, seven targeted Rust tests, C compilation with
warnings denied, Clippy with warnings denied**, default Phase 3 storage gate,
and Phase 4's SQL-time WAL faults/fresh-VM recovery gate. No server or storage
ownership lock is deliberately left held afterward.

Recovery evidence is limited to owned-host SIGKILL and injected WAL write/
sync EIO/EDQUOT with the host OS page cache intact. A failed sync can leave an
unacknowledged transaction recoverable: applications must handle the unknown
commit outcome. This is not physical power-loss, torn-write, production,
replication, SMP, or live-snapshot validation. Keep optional features disabled
until each has its own correctness/resource gate.
