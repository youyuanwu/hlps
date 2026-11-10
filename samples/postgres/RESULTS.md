# PostgreSQL release-build evaluation: 2026-11-10

**All comparisons use optimized release hosts and equivalent PostgreSQL
assertion settings.** Six serial SQL/spill runs, a separate filesystem diagnostic,
and 72 standard-client read scans passed, along with the runtime, SQL,
storage, and fresh-VM recovery checks.

## Summary

| Workload / metric | Native | Hyperlight guest | Guest/native |
|-------------------|-------:|-----------------:|-------------:|
| SELECT 1 median | 50.910 us | 123.108 us | 2.42x latency |
| Indexed lookup median | 63.764 us | 201.914 us | 3.17x latency |
| Synchronous update median | 3.257 ms | 3.779 ms | 1.16x latency |
| Spill checksum median | 203.016 ms | 2,003.290 ms | 9.87x latency |
| Repeated pgbench table SELECT | 310.54 MB/s | 67.73 MB/s | 4.59x lower throughput |
| Repeated psql binary COPY | 308.55 MB/s | 64.13 MB/s | 4.81x lower throughput |
| Repeated aggregate scan median | 27.219 ms | 73.941 ms | 2.72x latency |

The sample successfully runs real PostgreSQL, synchronized persistent data,
disk spilling, and fresh-VM recovery. It is not a production or physical
power-loss durability certification. These ratios compare **complete stacks**,
not isolated Hyperlight transition costs.

## Build equivalence and environment

| Setting | Value |
|---------|-------|
| SQL/spill window | 2026-11-10, 06:34:25-06:35:41 UTC |
| Read window | 2026-11-10, 06:35:41-06:36:12 UTC |
| Host | Microsoft full virtualization; Hyperlight KVM therefore runs nested |
| CPU | Intel Xeon Platinum 8573C; 2 exposed logical CPUs, 1 core / 2 threads |
| Affinity | Every server CPU 0; client and sampler CPU 1 |
| Frequency policy | `schedutil`; no frequency/turbo or neighbor-load control |
| Kernel / memory | Linux `7.0.0-1019-azure`; 7.7 GiB RAM, no swap |
| Filesystem | `/data` on ext4, `/dev/sdb1`, `rw,relatime` |
| PostgreSQL | 16.15; native source unmodified; pinned target-guarded guest patches |
| GCC / libc | GCC 15.2.0; glibc 2.43 native/client; musl 1.2.5 guest |
| Rust host | Rust 1.93.1, `cargo --release`, opt-level=3, debug=0 |
| Rust checks | Debug assertions/overflow checks disabled; incremental compilation disabled |
| PostgreSQL/client flags | `-O2 -march=x86-64`; PostgreSQL assertions disabled in **both** builds |
| Kernel build | Performance-optimized Unikraft configuration |
| Durability | `fsync=on`, `synchronous_commit=on`, `wal_sync_method=fsync` |
| Server profile | Shared buffers 16 MiB; work_mem 64 KiB; max connections 2; optional workers/parallelism/autovacuum disabled |
| Source identities | [sources.toml](sources.toml) and per-run script/build hashes |

Build metadata records the actual verified Cargo settings, PostgreSQL configure
arguments, configuration-header hashes, and executable/rootfs identities.
Packaging and measurements require verified release host metadata, matching
artifacts, and PostgreSQL assertions disabled in both builds. Ordinary
validation/test assertions are enabled. Generated PostgreSQL headers are
explicitly built before server compilation under the outer Makefile.

**Instrumentation differs between modes:** hostfs mutation audits,
quota accounting, storage ownership, and real synchronization are enabled;
native PostgreSQL has no equivalent hostfs audit. Profiling is off in measured
runs and on only in a separate diagnostic. Native uses glibc/fork and the guest
uses musl/EXEC_BACKEND/posix_spawn and the single-address-space kernel. Release
mode does not make these execution paths identical or isolate virtualization.

Modes run serially in native/guest, guest/native, native/guest order. The CPUs
are sibling threads, not two independent physical cores. No build or second
server ran concurrently with the measurements. One-minute load was 1.33 before
SQL / 1.51 afterward, then 1.34 after reads. Shared-host scheduling and physical
storage/cache conditions are not controlled.

## SQL and spill procedure

Both servers recreate the same 100,000-row C-locale fixture, unique ID index,
and one-row update table before each repeat. Dedicated PGDATA/WAL directories
are reused, not byte-identical clones. Preparation, EXPLAIN, and checkpointing
are outside query timing and warm caches; the host page cache is retained.

The [libpq timing client](tests/query-client.c) measures synchronous
`PQexec` completion, excluding result validation and CSV output. Short
workloads have 50 untimed calls followed by 1,000 selects/lookups or 300
autocommit updates. Spilling has one untimed and five measured checksums.
Every result is checked. Batch throughput includes loop/result-validation
cost but excludes CSV output; it is single-client closed-loop throughput.

### Startup and first benchmark connection

Milliseconds, **median [min, max]**, three samples per mode:

| Metric | Native | Guest |
|--------|-------:|------:|
| Launch to observed readiness | 114.933 [114.678, 115.236] | 376.589 [355.837, 521.951] |
| First benchmark connection | 7.464 [6.431, 8.852] | 22.627 [22.419, 23.729] |
| First benchmark SELECT 1 | 0.165 [0.156, 0.166] | 0.429 [0.406, 0.453] |

Startup includes launch wrappers, guest boot/ownership probes where relevant,
and approximately 100 ms readiness polling. It excludes build/initialization
and is not intrinsic kernel boot time. Benchmark connection/query timings
follow readiness probes and fixture preparation, not cold-cache or first-ever
cluster access. Three samples do not establish startup tails.

### Warmed latency and throughput

Latency values are **microseconds**, pooled across three repeats using
nearest-rank percentiles. No confidence interval or universal tail claim is
inferred from this finite, scheduling-dependent run.

| Workload | Mode | Samples | p50 | p95 | p99 | Calls/s |
|----------|------|--------:|----:|----:|----:|--------:|
| SELECT 1 | Native | 3,000 | 50.910 | 64.407 | 84.622 | 19,213.0 |
| SELECT 1 | Guest | 3,000 | 123.108 | 151.655 | 276.835 | 7,624.7 |
| Indexed lookup | Native | 3,000 | 63.764 | 86.714 | 167.295 | 14,124.6 |
| Indexed lookup | Guest | 3,000 | 201.914 | 292.294 | 624.789 | 4,497.7 |
| Synchronous update | Native | 900 | 3,257.106 | 4,279.890 | 6,143.833 | 294.7 |
| Synchronous update | Guest | 900 | 3,778.807 | 5,341.320 | 7,052.453 | 249.3 |

Lookup IDs and payloads are deterministic and checked. Each update validates
the exact returned counter, ending at 350 including warm-up. Every guest audit
contains **360 successful OS WAL fsyncs** across startup/preparation/updates/
checkpoint/shutdown, with no failed host operations.

Between-repeat median ranges were 49.531-51.802 us native /
122.571-123.983 us guest for SELECT, 63.187-64.560 / 198.519-204.916 us for
lookup, and 3.197-3.301 / 3.754-3.802 ms for updates. Native lookup throughput
varied from 12,483.9 to 15,133.1 calls/s. Throughput must not be inferred from
median latency.

### Disk spilling

Every full ordered-result checksum equals
`6d334d76c6d5153bbf8eebf552f99702`, matching native and independent expectations.

| Metric | Native | Guest |
|--------|-------:|------:|
| Timed checksums | 15 | 15 |
| Median, ms | 203.016 | 2,003.290 |
| Observed p95/max, ms | 215.418 | 2,018.787 |
| Checksums/s | 4.891 | 0.499 |
| EXPLAIN disk sort, KiB | 27,280 | 27,280 |
| Temp blocks read / written | 13,614 / 14,334 | 13,614 / 14,334 |

JSON EXPLAIN requires a disk sort, exactly 100,000 actual rows, and nonzero
temp reads/writes. Temp files are absent afterward. With 15 samples per mode,
p95 and p99 both select the maximum; they are not reliable tail estimates.
This is sort-plus-checksum work, not raw disk bandwidth.

### SQL/spill resource use

| Metric | Native | Guest |
|--------|-------:|------:|
| Sampled peak server-tree PSS, KiB | 52,441 | 135,916 |
| Sampled sum RSS, KiB | 84,456 | 137,732 |
| Maximum server process count | 5 | 1 host process |
| Maximum post-shutdown logical storage, bytes | 58,839,630 | 75,776,713 |
| Maximum post-shutdown WAL bytes | 16,777,216 | 33,554,432 |

Native summed RSS double-counts shared pages; PSS is the better physical-memory
comparison. Guest figures include resident guest mappings and host, not the
client. Sampling starts after readiness, covers preparation/SQL every 50 ms,
and cannot prove instantaneous peaks. No child-exit sampling race was observed.

Guest audited storage high-water was **103,680,197 bytes**, below 512 MiB.
Unlike final storage, that includes spill growth. Native temporary usage is
evidenced by EXPLAIN/cleanup, not a sampled whole-directory high-water.
The servers use separate PGDATA/WAL directories with different retained WAL
segment counts; final storage is not a controlled footprint benchmark. Logical budgets omit
physical allocation/inode overhead and outside-mount logs/builds.

## Guest-to-host filesystem diagnostic

The separate reduced profiled run includes boot, fixture/EXPLAIN preparation,
10 warm-up and 50 measured calls per short workload, two checksum calls
including warm-up, checkpoint, and shutdown. It is excluded from the latency
tables. It records **100,275 filesystem callbacks** and **4,140.075 ms** of
callback body time:

| Callback | Calls | Total ms | Mean us |
|----------|------:|---------:|--------:|
| fs_write_bytes | 48,406 | 3,151.731 | 65.1 |
| fs_read_bytes | 50,841 | 202.010 | 4.0 |
| fs_sync | 147 | 760.289 | 5,172.0 |
| fs_stat | 758 | 2.571 | 3.4 |
| fs_list_page | 55 | 6.890 | 125.3 |

Writes account for about 76% of callback body time in this diagnostic.
That body includes backend/accounting/audit work but **excludes VM exit/reentry**
and some marshalling. Totals also include preparation/shutdown. They neither
measure pure transition overhead nor profile the read-only benchmark directly.
The stateless backend reopens/seeks/closes files per request. These measurements
do not establish the contribution of that behavior to the total query latency.

## Full-table read throughput (2026-11-10)

**All 72 scans passed** using standard pgbench 16.15 for SELECT/aggregate and
psql 16.15 for binary COPY. No custom read client/parser/build step is used.
Every mode/workload/repeat boots a separate server, then runs one first and
three subsequent scans on the same connection. Preparation occurs in separate
server lifetimes; no fixture scan precedes the first timed sample.

The 100,000-row fixture contains four-byte IDs and 128-byte values:
**13.2 MB logical payload per transfer**. pgbench consumes complete SELECT
results through custom SQL scripts with `-n -c 1 -j 1 -s 1 -M simple -t 4 -l`;
it does not print rows or need pgbench tables. psql uses `\timing on` and
`\o /dev/null` for binary COPY.

| Workload | Mode | First median ms | Repeated median ms | Repeated MB/s | Repeated rows/s |
|----------|------|----------------:|-------------------:|--------------:|----------------:|
| pgbench SELECT | Native | 55.953 | 43.331 | 310.54 | 2,352,597 |
| pgbench SELECT | Guest | 206.172 | 194.157 | 67.73 | 513,079 |
| psql binary COPY | Native | 47.568 | 42.488 | 308.55 | 2,337,505 |
| psql binary COPY | Guest | 210.417 | 205.961 | 64.13 | 485,808 |

There are three first-scan and nine repeated samples per row. Rates use total
logical bytes or rows divided by total elapsed time, not reciprocal median.
MB is decimal 1,000,000 bytes, not TCP/protocol wire bandwidth. Repeated
SELECT ranges were 34.663-44.323 ms native / 192.774-201.593 ms guest; COPY
ranges were 41.968-44.312 / 204.507-208.086 ms. These finite samples do not
establish reliable p99s.

Timing comes directly from pgbench transaction logs and psql COPY timings,
at microsecond resolution. Connection/startup and correctness checks are
excluded. There is **no first-row metric or per-row validation inside timing**.
pgbench/libpq can buffer a full result; psql handles COPY streaming.

After timing, each run verifies the exact aggregate and exports rows ordered
by ID through untimed psql binary COPY. All **14,200,021 output bytes** must
match independent SHA-256
`d1f6015927ccbe4bc4571f4aa5f67c51be6da21fc2a303e246b6b82656f6af50`.
The temporary file is deleted after success. This checks every row/payload
in a separate transfer, not every timed pgbench result or COPY byte.

### Scan control and buffer behavior

The pgbench control query `SELECT count(*),sum(length(value)) FROM spill_fixture`
returns only `100000,12800000`, checked outside timing:

| Metric | Native | Guest |
|--------|-------:|------:|
| First-scan median, ms | 28.659 | 78.075 |
| Repeated median, ms | 27.219 | 73.941 |
| Repeated scanned rows/s | 3,607,938 | 1,355,887 |

The aggregate is about 2.72x slower by median, whereas returning all rows has
a larger gap. This is consistent with scan-side and result-delivery costs,
but the control also computes text lengths; subtracting it does not isolate
network or transition overhead.

Heap-statistic snapshots around each complete four-scan client show
**8,000 read blocks / 192 hits** in both modes and all workloads. These are
combined, untimed counters after client disconnect and before correctness
queries, not per-transaction measurements.

The heap is 16 MiB at 8 KiB per block. Under the 16 MiB shared-buffer profile,
the repeated scans do not retain the entire heap. **Repeated/warmed does not
mean all-buffer-hit.** PostgreSQL buffer misses do not prove physical-disk
reads: the host page cache is retained and warmed by setup and repeated scans.
No global cache manipulation or cold-disk claim is made.

Post-timing JSON EXPLAIN ANALYZE requires one serial 100,000-row Seq Scan,
positive buffer evidence, and no Gather/Sort/temp writes. COPY uses the
equivalent SELECT plan. Instrumented plan time is not streamed-result time.

### Cache-hit control: separating storage and result delivery

A cache-hit diagnostic on **2026-11-10 at approximately 06:45 UTC** keeps the
same 100,000 rows entirely in PostgreSQL's session-local buffers. It used the
same release binaries, server/client CPU affinity, serial native/guest
execution, and standard psql client. Preparation uses session-local temporary
data without modifying persistent data or configuration.

Each workload used its own session, with preparation outside timing:

```sql
SET temp_buffers = '32MB';
CREATE TEMP TABLE cache_control AS
SELECT id, value FROM spill_fixture;
```

The session then ran either the aggregate or binary COPY four times, using
psql `\timing` and directing output to `/dev/null`:

```sql
SELECT count(*), sum(length(value)) FROM cache_control;
-- Or, in a separate prepared session:
COPY (SELECT id, value FROM cache_control) TO STDOUT (FORMAT BINARY);
```

An untimed post-check returned exactly `100000|12800000`. EXPLAIN ANALYZE
confirmed a serial 100,000-row scan with **2,048 local-buffer hits, zero
local-buffer reads, and no temp writes** for each case. The temporary relation
fits in the 32 MiB local buffer allocation; these are all-cache-hit controls,
unlike the persistent-table benchmark's mostly missed shared buffers.

| Fully cached workload | Native median, ms | Guest median, ms | Guest/native |
|-----------------------|------------------:|-----------------:|-------------:|
| Aggregate scan | 25.541 | 27.331 | 1.07x |
| Binary COPY to client | 41.093 | 154.853 | 3.77x |

These are nearest-rank medians of **four samples per case**, without
independent server-start repeats or a reliable tail estimate. Aggregate ranges were
25.086-27.322 ms native / 26.156-29.579 ms guest; COPY ranges were
38.629-49.755 / 154.488-158.214 ms.

**What this establishes:**

- Scanning/computation can run near native speed when the data is already
  inside PostgreSQL's buffers. The persistent-table guest aggregate took
  73.941 ms, versus 27.331 ms in this cache-hit control.
- Returning all rows is substantially slower even without data-file
  reads during the scan: cached guest COPY took 154.853 ms, versus
  41.093 ms native. Removing storage access does not remove the return-path
  gap.
- The inspected persistent hostfs path performs a host call and data copying
  for reads; its stateless backend reopens, seeks, reads, and closes files
  per request. Guest socket sends/polls also cross host-call boundaries.
  Those paths add more than PostgreSQL's own scanning work.

**What it does not establish:** an exact percentage attributable to VM
exit/reentry, marshalling, copying, host filesystem/socket work, or scheduling.
Local temporary buffers and regular shared buffers are different execution
paths; subtracting these timings is not a controlled decomposition of
overhead. The filesystem profile above also excludes transition time and
does not profile this control directly. Nested virtualization may contribute,
but its contribution was not isolated.

Raw SQL scripts, timings, checks, EXPLAIN JSON, and
`results.json` are preserved under
`target/postgres-experiment/read-benchmark/cache-control-20261110T064541.494751Z/`.
No server was left running; session-local tables were dropped on disconnect.

### Read-test resource use

Peak sampled server-tree PSS was **30,373 KiB native / 115,875 KiB guest**.
Guest audited storage high-water was **75,814,368 bytes**. An observer
connection collects untimed counters/validation beside the one-client
benchmark, within the two-connection cap. Servers run serially with the
512 MiB scratch/storage and 1 GiB sampled-memory limits. Each scan
has a 30-second SQL deadline, client 90 seconds, and guest VM 120 seconds.

## Reproducibility and validation

With initialized storage, build and run [the documented targets](README.md#performance):

```sh
make -C samples/postgres pg-guest storage-build storage-package recovery-test
make -C samples/postgres benchmark-build check benchmark read-benchmark
```

Successful current evidence is ignored under:

- SQL/spill/profile: `target/postgres-experiment/phase5/20261110T063425.151132Z/`.
- Standard-client reads: `target/postgres-experiment/read-benchmark/20261110T063541.385661Z/`.

The run directories contain raw latencies, standard-tool logs/SQL, EXPLAIN,
resource/buffer measurements, output hashes, and versioned results.
Top-level result files point to these release runs.

Validation passed: **40 Python tests**, all nine native and nine KVM process
probes, concurrent SQL/authentication/24 reconnects, persistent storage errors/
pagination/single-writer checks, fresh-VM recovery/WAL failures, seven targeted
Rust tests **in release mode**, six SQL/spill runs plus profiling, and all
72 read scans with 18 exact export hashes. No server or ownership lock is
deliberately left running/held.

These are instrumented release comparisons on one nested, shared Azure VM,
not universal performance guarantees. Recovery covers owned-host SIGKILL and
WAL write/sync EIO/EDQUOT with host page cache intact. A failed sync can leave
an unacknowledged transaction recoverable, so its outcome is unknown. Physical
power loss, arbitrary torn writes, complete POSIX descriptors/locking,
snapshots, replication, SMP, and production readiness remain unproven.
