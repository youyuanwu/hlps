# PostgreSQL experiment

This directory implements the Hyperlight-Unikraft baseline/process probes,
PostgreSQL 16.15 RAM-only prototype, and host-backed storage/recovery gates from
[the plan](PLAN.md). Phase 1 needs no network, host mounts, database files,
or snapshots. Phase 2 uses authenticated loopback TCP and an initialized
in-guest database. Phase 3 enables real host-backed synchronization; Phase 4
measures disk spilling and fresh-VM recovery with SQL-time WAL errors.
Power-loss durability and production readiness are not established.

## Phase 1: build and run

First target: Linux x86_64 with KVM. Run builds as an ordinary user.

```sh
make -C samples/postgres phase1
```

Individual steps:

```sh
make -C samples/postgres preflight fetch
make -C samples/postgres build
make -C samples/postgres check test-native test-guest
```

Preflight checks tools, KVM access, and at least 2 GiB available RAM and 4 GiB
free disk. Missing dependencies are reported, not automatically installed.
On Debian/Ubuntu the additional kernel/probe tools are typically `flex`,
`bison`, `m4`, `musl-tools`, and `cpio`, alongside the existing C/Rust toolchains.
No Docker, Kraft, cross compiler, or additional runtime images are required.
If the user cannot access KVM directly, only the deadline-wrapped guest runs
use `sudo -n`; builds never do. Device permissions are not changed.

[sources.toml](sources.toml) pins the host, all three kernel submodules, the
PostgreSQL archive/checksum, the BSDkrun reference, and Hyperlight 0.19.0.
Phase 1 downloads/checks PostgreSQL but does not build it. BSDkrun is a
sparse, pinned local reference checkout, not a new remote fork or dependency.
Licenses/notices stay with downloaded sources. Hyperlight-Unikraft is Apache
2.0; Unikraft and libelf include BSD licenses and per-file notices; musl uses
MIT; PostgreSQL uses its PostgreSQL license. The loader's individual source
notices also apply. Binary redistribution needs those notices, not just this
summary.

The kernel build uses upstream's defconfig and library list, with one job,
but uses the local toolchain rather than upstream's Ubuntu 24.04 Docker
builder. GNU C17 is explicit: newer GCC's C23 default changes old-style
function declarations and fails this kernel's constructor calls. This is a
compiler-mode selection, not a kernel compatibility patch. Local binaries
are not claimed to match upstream's prebuilt checksums.
Both kernel and probe also specify `-march=x86-64`: distro compilers may
default to x86-64-v3 instructions that the guest's CPU setup cannot execute.

The source-built x86_64 ELF **replaces the kernel embedded by the CLI before
the CLI is rebuilt**. Cargo uses upstream's own lockfile and an isolated
target directory. The root workspace excludes this checkout; its existing
dependencies and members remain unchanged.

## Probes and gate

[tests/process.c](tests/process.c) is an ordinary, dynamically linked musl
Linux PIE. The same executable source runs natively and inside the guest.
The minimal CPIO contains it, its musl interpreter, directories, and a
`/proc/self/maps` link to Unikraft's `/dev/maps`; there is no shell or driver.

| Probe | Requirement |
|-------|-------------|
| `hello` | Ordinary ELF entry point and stdout |
| `spawn` | 32 self-reexecutions via `posix_spawn`, exact exit code/wait/reaping, unchanged parent TLS/globals |
| `pipes` | Eight children exchange and verify 128-byte payloads, descriptor closure and EOF |
| `signals` | Process- and thread-directed `SIGUSR1` handlers interrupt pipe reads; a child exits from `SIGTERM` with the correct wait status |
| `blocked-signals` | Mask inheritance through exec, child `signalfd`, nonblocking `EAGAIN`, parent signal consumption and mask restoration |
| `setsid` | Child session/group creation without changing the parent's session/group |
| `tls` | Two pthreads have independent TLS across yielding; reexecuted child starts with clean TLS |
| `stack` | Parent and child report limits, actually touch 1 MiB of stack, and report pthread stack size |
| `shared` | Parent/child exchange 32 exact values using process-shared semaphores |

For `shared`, the guest deliberately uses anonymous shared memory and a
within-VM address passed to the reexecuted child, reflecting Unikraft's single
address space. Native Linux uses a temporary file-backed shared mapping because
`exec` discards anonymous mappings. This is a target-specific compatibility
probe, **not** a portable PostgreSQL DSM implementation; addresses are never
saved across VM instances.

Each guest probe boots a fresh VM with 512 MiB scratch, `--cold`, no mounts,
and no networking. GNU `timeout` imposes a 30-second host deadline and a
5-second forced-termination grace period. Tests are serial and stop at the
first failure. A zero exit code alone is insufficient: each case must print
exactly one matching `PASS` line. A timeout, missing marker, wrong wait status,
or failed assertion is an explicit failure.

The gate is all nine guest probes passing, not compilation or native success.
This gate does not establish PostgreSQL compatibility, storage durability,
multi-vCPU execution, scheduler fairness, or support for ordinary `fork`.
Stack touching is a bounded smoke check, not permission to configure
PostgreSQL beyond the actual loader/child stack allocation.

## Artifacts and patches

Everything generated stays under the ignored
`target/postgres-experiment/` directory:

- `preflight.json`: resource/toolchain evidence.
- `sources/`: verified sources and PostgreSQL archive.
- `kernel-build/`, `cargo/`: one active kernel/CLI build.
- `rootfs/`, `phase1.cpio`, `native-phase1`: minimal application artifacts.
- `build.json`: source, submodule, patch, lockfile, configuration, and
  kernel/CLI/rootfs checksums.
- `native-*.log`, `guest-*.log`, and result JSON: actual probe output.

Only source pins, build recipes, probes, documentation, and justified patches
belong in Git. Patch stacks, when needed, use ordered
`patches/{hyperlight-unikraft,unikraft,app-elfloader}/series`. The recipe checks
then applies each patch sequentially to the pinned clean checkout, and rejects
unexpected changes or changed/already-applied patch stacks. It never resets or
discards source edits. A previously prepared checkout may be reused only when
its recorded patch order, hashes, and resulting diff match. Patch-file Git
whitespace attributes allow unified-diff context prefixes; patch application
still validates the actual source edits.

Guest tests reject changed build inputs or binary checksums; rebuild after
changing source pins, patches, configuration, recipes, or probes.

## Results (2026-11-08)

All **nine native and nine KVM guest probes pass**, including 32 repeated
child creations, eight pipe exchanges, and 32 cross-process semaphore rounds.
The kernel and CLI were built locally from the pinned sources with one job;
no PostgreSQL build, database service, or storage test was started.

Three bring-up issues were resolved:

1. The GCC 15 C23 default rejected kernel constructor calls: select GNU C17.
2. The distro compiler's x86-64-v3 default caused guest shutdown at boot:
   select baseline x86-64 and invalidate stale kernel objects when flags change.
3. Caught `SIGUSR1` left a pipe-reading child blocked forever: apply the
   [focused signal-wakeup patch](patches/unikraft/README.md) and rebuild both
   the actual kernel and the embedding CLI.

The pinned kernel already supplies the tested session, signalfd, exec, and
vfork/TLS paths; no BSDkrun kernel/loader patch was imported wholesale.

Guest stack observations: `RLIMIT_STACK` reports **1 MiB**, while musl's
`pthread_getattr_np` reports **8 MiB** for both entry and exec child. Both
actually touched 1 MiB successfully. The configured loader entry stack is
2 MiB and the exec child allocation is 8 MiB: reported pthread size alone
must not be used to raise PostgreSQL's stack-depth setting.

Remaining limitations include one cooperative vCPU, unsupported ordinary
`fork`, and unsupported `SA_RESTART` in the base kernel. The probe retries
interrupted I/O explicitly. Phase 2 below implements process adaptation;
durable filesystem synchronization remains subsequent work.

## Phase 2: PostgreSQL RAM-only prototype

```sh
make -C samples/postgres phase2
```

Selected steps, all serial:

```sh
make -C samples/postgres pg-kernel pg-native pg-guest pg-package
make -C samples/postgres check pg-test
```

The initial native PostgreSQL build takes longer than a cached run. It uses
unmodified verified 16.15 sources to provide matching initdb/client tools.
The guest uses a separate source tree, an
[ordered target-guarded patch stack](patches/postgres/README.md),
`EXEC_BACKEND`, musl, GNU C17, baseline x86-64, and assertions. ICU, readline,
compression libraries, and SSL are configured out. Only the guest server,
timezone data, and builtin PL/pgSQL are built/packaged; no Docker is needed.
Patch/configuration changes invalidate old objects before rebuilding.

[rootfs/kernel.conf](rootfs/kernel.conf) selects uid/gid 1000 and the
`phase2` guest identity. It overlays, rather than modifies, upstream's pinned
defconfig. There is **no PostgreSQL root-check removal**. Native and guest
data-directory ownership checks remain intact.

The Phase 2 kernel and copied CLI are separate artifacts under
`target/postgres-experiment/phase2/`. Kernel profiles reuse the one Cargo
cache to save disk, but the Phase 2 CLI embeds its own rebuilt kernel and is
copied before another profile can overwrite the cache binary. `make phase1`
restores/rebuilds the original kernel profile; this does not change the copied
Phase 2 CLI. Both profiles record the actual kernel/configuration checksums.

The [RAM-only configuration](rootfs/ram-only.conf) sets:

- 512 MiB scratch, two client connections, 16 MiB shared buffers, and
  conservative 512 KiB maximum stack depth.
- Anonymous main shared memory and VM-scoped DSM; no host filesystem mounts.
- PostgreSQL on `127.0.0.1:55432`, with generated SCRAM-SHA-256 credentials.
- Required startup/checkpointer/background-writer/WAL-writer processes;
  no autovacuum, parallel workers, WAL senders, or logging collector.
- `fsync=off`, bounded temp/WAL settings, and a 512 MiB database/WAL test budget.
  This deliberately nonpersistent profile **cannot satisfy a durability gate**.

[rootfs/init.c](rootfs/init.c) is an ordinary Linux PIE supervisor, not an
interpreter driver or VMM. It spawns the postmaster and reaps it. A separate
loopback-only control listener at `127.0.0.1:55433` accepts only the current
VM's random identity before requesting PostgreSQL fast shutdown. Both ports
are explicit capabilities; outbound networking is denied. The pinned CLI's
stdin host callback blocks the host while reading, so this supervisor does
not use stdin as a live control channel.

`pg-test` boots fresh with `--cold`, holds the exclusive experiment lock,
rejects occupied ports, and tests:

1. Wrong-password rejection and two authenticated simultaneous, distinct
   backends.
2. Real auxiliary processes and exact PostgreSQL/profile settings.
3. Table creation, inserts, committed visibility, rollback, and no dirty reads.
4. Distinct application/timezone settings and same-named independent temp tables.
5. Four warm-up plus 24 fresh connections without stale temp/settings state.
6. At most 32 MiB additional host RSS after warm-up, and a bounded live
   database/WAL footprint.
7. Invalid control-identity rejection, graceful postmaster shutdown, and zero
   guest exit status.

Readiness, SQL, clients, shutdown, and the overall VM have bounded host
deadlines; the VM deadline is 180 seconds. Timeout/error cleanup targets only
this test's process tree. The finite RSS test detects large reconnect leaks,
not all possible lifetime leaks.

`pg-package` regenerates only the known generated rootfs/seed and credentials.
Each run boots its own RAM copy; SQL changes are lost on VM exit. Unrecognized partial packaging directories are not silently overwritten.
Credentials, sources, binaries, CPIOs, native/guest build caches, and live
results stay ignored. Do not directly execute the guest-specific binary on
Linux, reuse DSM descriptors across VMs, attach persistent PGDATA, or use
snapshots with this prototype.

Artifacts under `target/postgres-experiment/phase2/` include `kernel.json`,
`build.json`, `guest.log`, `psql-*.log`, and `results.json`. Source/patch,
configuration, native tool, kernel, CLI, and rootfs identities are checked
before tests. The JSON status is the observed gate result, not an inference
from a successful compile.

### Phase 2 results (2026-11-08)

**Gate passed**, including repeated fresh-VM runs and a run after restoring the
original Phase 1 kernel profile.

| Measurement/check | Result |
|-------------------|--------|
| PostgreSQL | 16.15; uid/gid 1000; root/ownership checks unchanged |
| Simultaneous authenticated backends | 2 distinct sessions |
| Transactions | Explicit commit and rollback in both sessions; no dirty reads |
| Session isolation | Independent settings and same-named temp tables |
| Reconnects | 4 warm-up + 24 checked connections |
| Host RSS before / after reconnects | 183,544 / 183,544 KiB; no measured growth |
| Database plus WAL | 38,805,557 bytes, below 512 MiB budget |
| Shutdown | PostgreSQL fast shutdown and guest exit 0 |
| Existing baseline | All 9 native and 9 KVM process probes pass |
| Helper/native validation | 10 Python tests; native guard-off syntax checks with and without EXEC_BACKEND |

Two additional [kernel fixes](patches/unikraft/README.md) were necessary beyond
Phase 1's caught-signal patch: RAMFS nodes needed real stored single-user
ownership, and shared anonymous DSM mappings needed to survive their creating
process's exit. A creator-exit/remap/unmap probe and a new-file ownership probe
pass before PostgreSQL starts. Private mappings still receive process/exec
cleanup; the reconnect test remains bounded.

Ordinary native PostgreSQL sources are unmodified. The guest-specific root
bypass initially explored during bring-up was removed, and both PostgreSQL's
main/root-check source and data-directory ownership-check source are
byte-identical to the verified native source.

Phase 2 alone is a functional, RAM-only prototype;
disk-spill validation, crash recovery, and snapshots remain later work. Phase 3
below supplies the initial host-backed storage/synchronization boundary.

## Phase 3: host-backed storage boundary

After the previous phases have built the native/guest PostgreSQL tools:

```sh
make -C samples/postgres storage-build storage-package storage-init
make -C samples/postgres phase3
```

`storage-init` is deliberately one-time: it refuses to overwrite existing
PGDATA. Subsequent `phase3` runs reuse the dedicated
`target/postgres-experiment/phase3/storage/` directory. To update the managed
test configuration independently use `make -C samples/postgres storage-config`.
Configuration updates require exclusive ownership and reject a live/stale
postmaster lockfile; they never initialize or erase an existing database.

The [persistent profile](rootfs/persistent.conf) enables `fsync`,
`synchronous_commit`, and `wal_sync_method=fsync`. It keeps the bounded
connections/memory/WAL/temp settings of the earlier prototype. Optional
writeback hints are disabled because the pinned kernel lacks `sync_file_range`;
actual file/directory synchronization is not disabled. PGDATA, WAL, temp
files, and DSM descriptors live only below this experiment's host directory.
The application/kernel rootfs remains RAM-backed.

The [host patch](patches/hyperlight-unikraft/README.md) wires an explicit
`FsBackend::sync` through `fs_sync` to the real OS file's `sync_all`/`sync_data`.
Unsupported backends return EOPNOTSUPP, not success. The
[kernel patch](patches/unikraft/README.md) forwards VFS `fsync`; its legacy
`fdatasync`, O_SYNC, and O_DSYNC paths use full fsync, a stronger barrier.
Hostfs create/mkdir permissions and stat ownership reach the real filesystem.
Paginated listings retain at most 128 entries per host page and bounded
per-open guest page state instead of re-sending the entire directory.

[storage-host.rs](storage-host.rs) is a small instrumented host using the
existing Hyperlight-Unikraft library/API, not a new VMM or runtime. It opens a
capability-confined directory backend, records sync/error results, enforces a
512 MiB logical-file-size budget, and exposes only `/storage` plus a read-only
view for testing. It uses the same loopback PostgreSQL/control ports and
fresh-VM control identity as Phase 2. No outbound networking or snapshots.

Both configuration/initialization and the running VM host take an exclusive
lock on the storage directory **inode**, rejecting aliases and second writers.
The VM host retains its own lock until guest teardown, even if its launcher
exits. Directly running an unguarded native PostgreSQL or modifying the storage
directory from unrelated host tools is prohibited by the experiment workflow;
an advisory lock is not a defense against noncooperating host programs.

The persistent host runs as the invoking user's uid/gid. When direct KVM
access is unavailable, `sudo -n setpriv` drops root before launching it and
supplies the existing KVM group only to that process. Device permissions and
system user/group membership are not changed. This keeps new database files
owned by the experiment user. Budget scans fail explicitly on unreadable
directories, rather than skipping them.

The gate runs [storage probes](tests/storage.c) before PostgreSQL:

- Exact offsets, readbacks, truncate, append file position, chmod, mkdir modes,
  O_EXCL collision, and rename followed by reopen.
- Actual file/directory synchronization and O_SYNC/O_DSYNC barriers.
- Injected EIO on file/directory sync and synchronous write; partial write
  followed by EIO with exact surviving bytes; EDQUOT on an oversized write.
- Read-only write rejection and explicit refusal of unsupported hostfs
  record locks/flock, rather than the kernel's previous success stubs.
- All 5,000 directory entries in order, multiple transport pages, and rewind.
- PostgreSQL startup with synchronization enabled, a committed row, checkpoint,
  required auxiliary processes, and graceful shutdown.
- Concurrent writer rejection by both the Python guard and another actual VM
  host, before it opens the writable backend.

Each VM has a 120-second host deadline, readiness/SQL/shutdown have their own
deadlines, builds use one job, and tests run serially. The managed probe files
are the only files test setup resets; PostgreSQL data is not erased.
Rebuilding rootfs images never copies live PGDATA into them.

Generated metadata, `probes.log`, `guest.log`, and JSONL operation audits remain
under the ignored Phase 3 artifact directory. A successful audit entry is
written only after the actual sync API returns successfully; failure entries
record the propagated error. The gate requires WAL and directory sync evidence,
not just `SHOW fsync` output or a successful SQL response.

### Limits

This is an initial persistent-storage **boundary**, not a production/durability
certification. The protocol remains path-based: correct OS-file-description
semantics for open files that are unlinked/renamed, arbitrary seekdir cookies,
and full guest record locking are not implemented. Tested PostgreSQL accesses
use stable paths or close/reopen across renames, with exclusive whole-directory
ownership. Hostfs mount-wide sync is explicitly unsupported; callers must
sync the relevant file and directory descriptors.

Injected failures model write/sync/quota errors at the host boundary, not loss
of the host page cache, machine power failure, or every physical filesystem
fault. No host reboot or global cache manipulation is performed. Phase 3 alone
does not establish crash recovery, acknowledged-commit survival, or measured
spilling; the bounded Phase 4 tests below exercise those paths. Broader
filesystem/descriptor compatibility remains unsupported; snapshots are forbidden.

### Phase 3 results (2026-11-08)

**Targeted storage-boundary gate passed**, including repeat runs after switching
back to the earlier kernel profiles.

| Check | Observed result |
|-------|-----------------|
| PostgreSQL profile | 16.15, `fsync=on`, `synchronous_commit=on`, WAL method `fsync` |
| Real OS synchronization | 60 successful calls in the final SQL/checkpoint/shutdown run |
| Synchronized directories | Logical, transaction, commit-timestamp, and multixact directories |
| Storage errors | File/directory/synchronous-write EIO, partial-write EIO, EDQUOT, EROFS surfaced |
| Directory listing | 5,000 exact entries over bounded pages; rewind passes |
| Ownership | Second Python guard and actual competing VM host rejected |
| Data budget | 39,494,900 logical bytes, including PGDATA/WAL and probes; below 512 MiB |
| Shutdown | Committed query/checkpoint succeeds; guest exits 0; host lock released |
| Regressions | 9 native + 9 KVM Phase 1 probes; Phase 2 concurrent SQL/reconnect gate |
| Validation | 16 Python tests, 5 targeted Rust tests, Clippy with warnings denied |

The final persistent host runs as uid/gid 1000 rather than root. Generated
database files remain readable/manageable by the invoking user, and strict
disk scans do not skip inaccessible subdirectories.

This passes Phase 3's limited gate, not Phase 4's spill/crash-recovery/durability
gate. The path-based descriptor and advisory-lock limitations above remain
explicit follow-ups.

## Phase 4: measured spill and fresh-VM recovery

With the previously initialized persistent storage:

```sh
make -C samples/postgres phase4
# Repeat only the recovery/spill tests without rebuilding:
make -C samples/postgres recovery-test
```

`phase4` rebuilds the instrumented host/rootfs, runs helper tests and the
Phase 3 gate, then runs [phase4.py](phase4.py). It does not reinitialize PGDATA
or change the persistent configuration. Source/build/configuration hashes are
verified first. The tests recreate only their `spill_fixture`, `recovery_gate`,
and `fault_gate` tables in this dedicated experiment database.

An unmodified native PostgreSQL 16.15 server is started serially on loopback
port 55434, against a separate generated `phase4/native-pgdata` directory.
It uses the same conservative configuration, C locale, credentials, and
synchronization settings. It is a correctness baseline, not a performance
comparison. No native server and guest VM run concurrently.

Both servers sort 100,000 deterministic rows with 128-character sortable
values at `work_mem=64kB`. JSON `EXPLAIN (ANALYZE,BUFFERS)` must report a
disk-backed sort, positive disk space, exactly 100,000 actual rows, and
nonzero temp reads **and** writes. The entire ordered result is checksummed
against both native PostgreSQL and a separately computed Python expectation.
Exact counts, ID bounds, row payloads, and temp-file cleanup are also checked.

The guest gate then checks:

- Sixteen individually acknowledged transactions survive a fresh VM:
  eight across graceful shutdown and eight more written after the last
  checkpoint before the crash.
- `SIGKILL` targets only the identified host executable in this launcher's
  process tree, with an uncommitted transaction and another active SQL session.
  The fresh VM completes actual WAL replay and an end-of-recovery checkpoint;
  uncommitted rows are absent.
- Terminating a backend and abruptly disconnecting a client roll back their
  open transactions without breaking the other session; twelve reconnects
  still see the exact committed rows.
- One-shot WAL write EIO, sync EIO, and EDQUOT occur during real SQL commits.
  Each produces the expected PostgreSQL `PANIC`, failed client exit, and no
  `COMMIT` acknowledgement. Each is followed by owned-host termination and
  a fresh, fault-free VM recovery.

### Crash ownership and ephemeral IPC

The VM host takes the existing exclusive storage-directory inode lock
**before** examining a leftover `postmaster.pid`. Only the recognized guest
format (guest data path, port, loopback address, empty socket directory, and
zero SysV interlock) may be removed. Malformed/native lockfiles are refused,
never reinterpreted as dead host PIDs. The removal and real PGDATA-directory
sync are audited. This depends on the existing single-writer workflow; it
does not protect against unrelated native tools ignoring the advisory lock.

No PostgreSQL patch was added for crash cleanup. Its existing mmap DSM
startup cleanup removes old descriptor files without mapping their saved
addresses. The test requires actual old-generation descriptors after the
crash and only fresh-generation descriptors once the new VM is ready.
Other stale locks and unrecognized partial files still require inspection.

The optional fault-control file is outside mounted storage, atomically armed,
limited to WAL writes/syncs, and consumed exactly once. A leftover control
file aborts the next recovery test instead of silently clearing it. EDQUOT is
a controlled host-boundary quota error, not filling this shared machine's
physical disk; the real 512 MiB logical budget remains independently enforced.

`SIGKILL` can interrupt writing the host's JSONL audit. Only an unterminated
final record is permitted for a crashed host or a live snapshot, and its byte
count is recorded for crashed hosts. Completed malformed records and partial
records after clean shutdown fail. Only complete successful sync records
count as synchronization evidence.

### Bounds, results, and limits (2026-11-08)

**Targeted Phase 4 gate passed**, including repeated fresh-VM runs.

| Measurement/check | Observed result |
|-------------------|-----------------|
| Profile | PostgreSQL 16.15; `fsync=on`, `synchronous_commit=on`, WAL method `fsync` |
| Rows / ordered checksum | 100,000 / `6d334d76c6d5153bbf8eebf552f99702`, matching native and Python |
| Native disk sort | 27,280 KiB; 13,614 temp blocks read / 14,334 written |
| Guest disk sort | 27,272 KiB; 13,610 temp blocks read / 14,352 written |
| Restart / crash | Exact committed rows and fixture checksum survive; actual WAL replay completes |
| Ephemeral IPC | Old-generation descriptors discarded; no stale addresses attached |
| SQL fault outcomes | No commit acknowledged; write/EDQUOT rows absent after recovery |
| Sync-error outcome | Unacknowledged row recovered; **unknown commit outcome**, not rollback proof |
| Sampled host RSS | Below 130 MiB; 1 GiB hard test limit |
| Logical storage high-water | Below 81 MiB; 512 MiB enforced budget |
| Queried backend memory | About 2.12 MiB; not total guest memory |
| Regressions / helpers | 9 native + 9 KVM probes; Phase 2 and Phase 3 gates; 20 Python and 7 targeted Rust tests; Clippy with warnings denied |

Each VM retains the 120-second host deadline and 512 MiB scratch cap. SQL
normally has a 10-second deadline (60 seconds for bounded fixture/sort/checksum
work); fault clients have 20 seconds, startup 30 seconds, and shutdown
15 seconds. Hosts run serially. Host RSS is sampled every 100 ms from process
identification until teardown; samples cannot prove instantaneous peaks.
Audited logical-file accounting captures spill growth and verifies cleanup.
The native fixture directory has its own 512 MiB check.

Ignored `target/postgres-experiment/phase4/` artifacts include native/guest
EXPLAIN JSON, client/guest logs, unique per-VM audits, credentials, the native
baseline database, and `results.json` with hashes and measured outcomes.
No server is deliberately left running after the gate.

This is a persistent-storage **candidate under the tested fault model**,
not a production or power-loss durability certification. SIGKILL preserves
the host OS page cache; a sync error can leave a transaction recoverable even
though the client never received its commit acknowledgement. Applications
must handle that ambiguity rather than blindly retrying a non-idempotent
transaction. No power-off, global cache dropping, arbitrary torn writes,
live snapshots, full POSIX descriptor semantics, or broad locking support
is claimed. Phase 5 evaluation and optional features remain separate work.
