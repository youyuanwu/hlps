# PostgreSQL on Hyperlight-Unikraft

**PostgreSQL 16.15 runs successfully inside a Hyperlight-Unikraft KVM guest.**
The sample demonstrates authenticated SQL sessions, transactions, host-backed
PGDATA/WAL, real synchronization, disk spilling, and recovery in a fresh VM.
The same validation suites reproduce these results; the
[performance report](RESULTS.md) compares the guest with unmodified native
PostgreSQL.

| Demonstrated capability | Verified result |
|-------------------------|-----------------|
| Process model | All nine native and nine KVM process/signal/TLS/stack/shared-memory probes pass |
| Real SQL server | Two authenticated concurrent backends, commits/rollbacks, session isolation, 24 reconnects |
| Non-root execution | Guest uid/gid 1000; PostgreSQL's root and ownership checks remain intact |
| Persistent storage | `fsync=on`, `synchronous_commit=on`, observed OS file/directory and WAL barriers |
| Storage semantics | Offsets, append/truncate, permissions, rename/reopen, explicit I/O/quota/read-only errors |
| Directory handling | All 5,000 entries over bounded pages, including rewind |
| Disk spilling | 100,000-row external sorts, measured temp I/O, exact native/Python checksum, temp cleanup |
| Fresh-VM recovery | Sixteen acknowledged commits survive restart and owned-host SIGKILL; stale DSM addresses are not attached |
| Failure handling | SQL-time WAL write/sync EIO and EDQUOT fail without a commit acknowledgement |
| Measurements | Six serial native/guest runs plus a separate filesystem profile, with exact results and resource bounds |

This is a working, bounded sample, **not a production PostgreSQL port or a
power-loss durability certification**. A sync error can leave an unacknowledged
transaction recoverable from the host page cache. See [limits](#limits).

## Build and run the persistent demo

Use Linux x86_64 with KVM and run builds as an ordinary user.

For a **new** sample data directory:

```sh
make -C samples/postgres setup
make -C samples/postgres persistent-demo
```

`setup` checks resources, fetches verified sources, builds native PostgreSQL
tools and the patched guest server, builds/packages the persistent host, and
initializes PGDATA once. It deliberately refuses to overwrite existing
storage. If this sample was already initialized, skip `setup`:

```sh
make -C samples/postgres persistent-demo
```

The demo boots a guest against its dedicated host directory, runs actual
filesystem probes, connects through native `psql` over authenticated loopback
TCP, commits row `42`, reads it back, checkpoints, and shuts down cleanly.
Successful output includes:

```text
PASS storage-offsets-modes-rename
PASS storage-synchronous-write
PASS storage-sync-errors
PASS storage-write-errors
PASS storage-pagination
PASS storage
PASS persistent-sql: observed syncs/errors, paginated directory, single writer, host-backed PostgreSQL
```

Failures return a nonzero exit and leave diagnostics in the ignored artifact
directory. A successful compile or zero guest exit alone cannot satisfy the
checks. No server is deliberately left running after a demo or test.

### Requirements

Preflight checks at least 2 GiB available RAM and 4 GiB free disk, plus the
existing Git/C/Rust tools. Debian/Ubuntu builds also need `flex`, `bison`, `m4`,
`musl-tools`, and `cpio`. Missing dependencies are reported, not automatically
installed. The benchmark additionally needs two allowed logical CPUs,
`taskset`, and `findmnt`.

No Docker, Kraft, Kubernetes, or additional runtime images are required.
When direct KVM access is unavailable, deadline-wrapped guest runs use
`sudo -n`. The persistent host uses `sudo -n setpriv` to drop to the invoking
uid/gid and supply process-local access to the existing KVM group. Device
permissions, global group membership, and unrelated infrastructure are not
changed.

## Validation commands

The suites are organized by what they validate, not by an implementation
sequence. With initialized persistent storage, run everything serially:

```sh
make -C samples/postgres validate
```

Or select a capability:

| Target | Purpose |
|--------|---------|
| `check` | Helper assertions, exact thresholds/output shapes, artifact guards, and command wiring |
| `process-test` | Build and run all nine native and KVM runtime probes |
| `ram-demo` | Build/package/run the RAM-backed server and concurrent SQL/session/reconnect checks |
| `persistent-demo` | Rebuild/package the persistent host, verify managed configuration, run storage and SQL checks |
| `storage-test` | Repeat persistent filesystem/SQL validation against verified existing builds |
| `recovery-test` | Native/guest spill correctness, graceful restart, crash recovery, lifecycle, and WAL faults |
| `benchmark-build` | Compile the bounded native libpq timing client |
| `benchmark` | Three rotating native/guest repeats and a separate guest filesystem profile |

The RAM demo explicitly uses `fsync=off`: its database is a fresh RAM copy and
SQL changes disappear on exit. It is not a fallback for persistent/recovery
validation, which always uses synchronization.

Build/configuration targets are also available individually:

```sh
make -C samples/postgres preflight fetch
make -C samples/postgres runtime-build process-test-native process-test-guest
make -C samples/postgres pg-kernel pg-native pg-guest pg-package sql-test
make -C samples/postgres storage-build storage-package
# Only for never-initialized storage:
make -C samples/postgres storage-init
# Update the managed persistent configuration while the server is stopped:
make -C samples/postgres storage-config
```

Tests reject changed source/build/configuration hashes; rebuild the matching
artifacts instead of editing metadata or bypassing guards. `benchmark` also
requires a current successful recovery result for the same inputs.

## What the checks exercise

### Runtime and SQL

[runtime.py](runtime.py) builds the pinned kernel/CLI and runs
[process.c](tests/process.c) as the same ordinary musl PIE natively and in KVM.
The cases are `hello`, `spawn`, `pipes`, `signals`, `blocked-signals`, `setsid`,
`tls`, `stack`, and `shared`. They check 32 child reexecutions, eight pipe
exchanges, caught/masked signals, independent globals/TLS, actual stack
touching, and 32 cross-process semaphore rounds.

[postgres.py](postgres.py) builds native tools from an unmodified source tree
and the guest server from a separately patched tree. Its SQL suite rejects
wrong credentials, requires distinct simultaneous backends and the necessary
auxiliary processes, verifies transactions/dirty-read refusal and independent
settings/temp tables, and checks reconnect RSS growth against the unchanged
32 MiB limit.

[init.c](rootfs/init.c) is a Linux PIE supervisor: it checks guest ownership and
shared-mapping lifetime, starts PostgreSQL with `posix_spawn`, and reaps it.
A separate loopback control thread accepts shutdown only with the current
VM identity. It does not rely on the pinned CLI's blocking stdin callback.

### Filesystem and recovery

[storage.py](storage.py), [storage-host.rs](storage-host.rs), and
[storage.c](tests/storage.c) retain the observed, quota-limited host backend.
Successful audit entries follow actual OS synchronization, not stubbed success.
File/directory/synchronous-write errors, partial-write EIO, EDQUOT, and EROFS
are propagated. Unsupported hostfs record locks/flock and mount-wide sync fail
explicitly. Bounded lexical pagination holds at most 128 entries per page.

[recovery.py](recovery.py) requires an actual external sort and nonzero temp
reads/writes in JSON EXPLAIN ANALYZE, checks every ordered-result checksum
against native PostgreSQL and an independent expectation, and verifies cleanup.
The fixture's checksum is `6d334d76c6d5153bbf8eebf552f99702`.

Restart tests verify exact committed rows and the same fixture in a fresh VM.
Crash tests kill only the identified host in this launcher's process tree,
with an open transaction and another active session. Recovery must complete
WAL replay and an end-of-recovery checkpoint, discard old DSM generations,
and leave uncommitted rows absent. Backend termination/client disconnect
rollbacks and twelve additional reconnects are checked.

One-shot WAL write EIO, sync EIO, and EDQUOT occur during real SQL commits.
Each must produce the expected PostgreSQL error, failed client exit, no
`COMMIT` acknowledgement, and successful fault-free fresh-VM recovery.
Write/quota test rows are absent afterward; the sync-error outcome is explicitly
allowed to be unknown rather than misreported as rollback.

### Performance

[evaluate.py](evaluate.py) and [query-client.c](tests/query-client.c) retain
the compiled libpq measurement path. Each mode runs three times in rotating
order, with 1,000 selects, 1,000 indexed lookups, 300 synchronous updates, and
five spill checksums per repeat, after the existing warm-ups.

Servers inherit one logical CPU and clients/samplers another; affinity is
verified and restored. Timed `PQexec` round trips exclude result validation
and CSV output. Separate batch timing measures single-client closed-loop
throughput. Main runs retain storage auditing but disable the profiler; a
reduced diagnostic records filesystem callback counts/body time separately.
See [RESULTS.md](RESULTS.md) for all measured values and caveats.

## Data, configuration, and ownership

Everything generated remains ignored beneath `target/postgres-experiment/`:
sources, builds, CPIOs, logs/audits, passwords, result metadata, and databases.
Existing internal directory names, configuration markers, result formats, and
the `phase2` test database role are preserved so saved data and measurements
need no migration. The maintained entry points are the descriptive commands
above.

The persistent directory is [storage.STORAGE](storage.py); its PGDATA is
mounted at `/storage/pgdata`. Application/kernel files remain RAM-backed, and
live PGDATA is never copied into a rootfs image. Native initialization uses
real synchronization, not `--no-sync`. Generated SCRAM credentials/pgpass files
are private and never checked in. PostgreSQL/control listen on host loopback
ports 55432/55433; the serial native comparison uses 55434.

Initialization/configuration and the VM host independently lock the storage
directory inode. The host retains ownership for its entire lifetime, even if
its launcher exits. Aliases and actual competing VM hosts are rejected before
writable backend opening. Never run unrelated/native tools against this
PGDATA while a guest owns it; advisory locks do not stop noncooperating tools.

Configuration updates reject live or stale `postmaster.pid`. For crash recovery,
the exclusively owning host removes only a recognized guest lockfile and syncs
PGDATA. Malformed/native lockfiles are refused. PostgreSQL's existing mmap
cleanup removes old DSM descriptors without dereferencing their addresses;
fresh identities are verified after real crashes. An unconsumed fault-control
file stops the next test rather than being silently cleared.

## Reproducible builds and bounds

[sources.toml](sources.toml) pins PostgreSQL 16.15, Hyperlight-Unikraft, all kernel
submodules, and the BSDkrun reference. Locally authored ordered patches preserve
upstream notices and document their provenance:
[PostgreSQL](patches/postgres/README.md),
[Unikraft](patches/unikraft/README.md), and
[Hyperlight-Unikraft](patches/hyperlight-unikraft/README.md).
No source fork or PostgreSQL submodule is required.

Builds use one job and explicit GNU C17/baseline `-march=x86-64`, avoiding the
tested GCC C23 constructor and distro x86-64-v3 boot failures. Input stamps
invalidate stale objects. The rebuilt kernel is embedded before its CLI/host
is compiled; frozen RAM/persistent executables remain valid when another
runtime profile is built. Unexpected source edits or changed prepared patch
stacks are rejected, never discarded or reset.

The configuration keeps 512 MiB guest scratch, a 512 MiB logical storage
budget, 16 MiB shared buffers, 64 KiB work_mem, two connections, and a 512 KiB
PostgreSQL stack-depth limit. Required auxiliary processes stay enabled;
optional workers/parallelism/autovacuum remain disabled. The guest reports a
1 MiB stack limit despite musl reporting 8 MiB; that report is not permission
to increase PostgreSQL stack depth.

Process probes have 30-second VM deadlines, RAM SQL 180 seconds, and persistent
VMs 120 seconds. Startup has 30 seconds, normal SQL 10 seconds, bounded
fixture/sort/checksum work 60 seconds, fault clients 20 seconds, and shutdown
15 seconds. The benchmark client has a 90-second deadline. Resource sampling
and strict disk scans fail explicitly; cleanup targets only owned processes.
Physical allocation/inode overhead is not the logical-file budget metric.

## Limits

- One cooperative vCPU and one address space; ordinary `fork` and `SA_RESTART`
  are unsupported. Exec children must be PIE. Native execution of the
  guest-specific PostgreSQL binary is not supported.
- Hostfs is path-based, not a complete OS file-description protocol. Tested
  renames close/reopen files; unlinked-open/renamed-open descriptors, arbitrary
  seekdir cookies, and full guest record locks are not implemented.
- SIGKILL retains the host OS page cache. Injected WAL errors model the host
  boundary, not power failure, arbitrary torn writes, or physical disk filling.
  Handle unknown commit outcomes instead of blindly retrying non-idempotent SQL.
- No live snapshots, writable clones of the same PGDATA, replication, SMP, or
  broad production PostgreSQL compatibility is claimed.
- Finite reconnect and sampled memory checks do not prove every lifetime path
  leak-free or instantaneous peaks. Performance compares complete instrumented
  stacks, not isolation-only overhead.
