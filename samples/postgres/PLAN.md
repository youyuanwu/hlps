# PostgreSQL on Hyperlight-Unikraft: experiment plan

Status: **Phases 1 and 2 implemented and validated; PostgreSQL 16.15 runs as a
RAM-only server**. See [instructions and results](README.md). Persistent storage
and recovery remain unimplemented.
Prepared 2026-11-08.

## Objective

Determine whether a patched PostgreSQL server can run inside a
Hyperlight-Unikraft micro-VM, accept ordinary PostgreSQL TCP connections, and
eventually use host-backed storage for database files, WAL, and temporary spill
files without compromising transaction correctness.

There are two distinct outcomes:

1. **Functional prototype:** a real postmaster, child backends, and SQL queries
   work. An in-memory database is sufficient for this milestone.
2. **Persistent-storage candidate:** synchronization, failure handling, restart,
   and recovery tests pass with host-backed data. A successful SQL demo alone
   does not establish durability or production readiness.

Keep modifications as reviewed patch files against pinned sources. Do not
create a PostgreSQL GitHub fork, vendor its entire source tree, or add a new
PostgreSQL submodule.

## Scope and constraints

- First target: Linux x86_64 with KVM, using the existing machine locally.
- One VM, one vCPU, initially two client connections. No SMP work is required.
- Use the existing `hluk` CLI first, with a custom rootfs and application entry
  point; do not build a new VMM or container runtime.
- No Kubernetes, OCI runtime integration, replication, extensions supplied by
  users, scale-to-zero, or live snapshot restoration in the initial experiment.
- Do not change the existing [C demo](../c-guest/README.md),
  [native adapter](../../crates/hyperlight-process/README.md), or
  [benchmark implementation](../benchmark/README.md).
- Keep existing Hyperlight dependencies pinned to 0.19.0. The reference
  Hyperlight-Unikraft package is 0.21.0 but depends on Hyperlight 0.19; verify the
  actual locked dependencies and feature compatibility before any workspace
  integration. Its package version is not the Hyperlight core version.
- Build with one job. Start with 512 MiB guest scratch, at most one live VM,
  and small database fixtures. Increase scratch up to 1 GiB only when measured
  usage or explicit allocation failures justify it.
- Cap test data plus WAL/temp files at 512 MiB and abort tests on budget
  exhaustion. Keep only the active build and, if useful, one known-working
  build; do not repeatedly build every unrelated runtime image.
- Bind the test service to host loopback only, with generated test credentials
  kept out of Git. Give the guest only its project-specific writable directory
  and the required listening-port capability.
- Every test has a host-side deadline. A timeout is a failure and terminates
  only the processes created by that test.

## Evidence and initial source pins

These are starting inputs, not evidence that PostgreSQL works on Hyperlight.
Reference URLs are pinned where possible so the port can be reproduced.

| Input | Initial selection |
|-------|-------------------|
| PostgreSQL | 16.15, the current 16.x minor listed by the official versioning page when checked |
| PostgreSQL source | `https://ftp.postgresql.org/pub/source/v16.15/postgresql-16.15.tar.bz2` |
| Published SHA-256 | `c1575341fa7bd40f5274ea465b34390f4dc64cdd0770af327005caaeb9f6b7ed` |
| Hyperlight-Unikraft | `155d333bad549377e8026a596ce89c0d4cd8e9ef` |
| Its Unikraft submodule | `3b7cba8816c02a497e7b5aa9c901655784a01000` |
| Its app-elfloader submodule | `bc7d759bdd9a96165674c63177017e64f34b5096` |
| BSDkrun port reference | `b31ff8ebed68d0ea9cba88345f4a99dc13c01fb7`, `examples/unikraft-postgres/` |

Verify archive bytes against the pinned checksum when fetching; verify Git
commits and submodule identities before building. Use Hyperlight-Unikraft's
locked dependency graph, recording all additional submodule revisions and the
host/guest library versions in build metadata. Do not resolve floating `main`,
`latest`, or a newer guest kernel silently.

The BSDkrun example targets PostgreSQL 16.4. Refresh its patches for 16.15
rather than treating them as already compatible. If a 16.4 reproduction is
needed to isolate a failure, keep it as an explicitly labeled, isolated
diagnostic build, not the deliverable or an exposed service.

Important differences:

- [Hyperlight-Unikraft's concurrency model](https://github.com/hyperlight-dev/hyperlight-unikraft/blob/155d333bad549377e8026a596ce89c0d4cd8e9ef/docs/concurrency.md)
  supports `vfork()` plus `execve()`, but not ordinary `fork()`.
- Its guest configuration already enables multiprocess execution, ELF loading,
  threading, and futexes. The task is compatibility, not implementing threads
  from scratch.
- The [BSDkrun PostgreSQL example](https://github.com/tsirysndr/bsdkrun/tree/b31ff8ebed68d0ea9cba88345f4a99dc13c01fb7/examples/unikraft-postgres)
  documents a real server, but uses another platform and explicitly sets
  `fsync = off`. It is a porting reference, not a durability baseline.
- The [Unikraft Cloud example](https://github.com/unikraft-cloud/examples/tree/main/postgres)
  uses Cloud's `base-compat` runtime and an EROFS rootfs. It is not a
  Hyperlight-compatible guest artifact and does not establish that stock
  PostgreSQL works in this integration.
- The current [host filesystem protocol](https://github.com/hyperlight-dev/hyperlight-unikraft/blob/155d333bad549377e8026a596ce89c0d4cd8e9ef/docs/fs.md)
  has no explicit file/directory synchronization operation. Its rootfs is
  writable RAM storage, and host mounts are optional capabilities.

## Proposed repository layout

Phases 1 and 2 implement source pins, baseline/process probes, PostgreSQL
patches/packaging, and the concurrent SQL gate. Persistent-storage, spill,
and recovery tests below remain future work.

```text
samples/postgres/
  PLAN.md
  README.md                 build, run, limitations, and measured results
  sources.toml              exact URLs/revisions, checksums, and dependency pins
  Makefile                  fetch, patch, build, launch, and selected checks
  rootfs/                   PostgreSQL packaging and bounded test configuration
  patches/
    postgres/
      series                explicit patch application order
      *.patch
    hyperlight-unikraft/
      series
      *.patch               host filesystem/protocol changes, if required
    unikraft/
      series
      *.patch               only missing kernel fixes
    app-elfloader/
      series
      *.patch               only missing loader fixes
  tests/                    process probes, SQL, spill, and recovery checks
```

Downloaded source, patched checkouts, object files, rootfs images, credentials,
and live `PGDATA` belong in ignored experiment directories, not in source
control. Prefer `target/postgres-experiment/` for artifacts. Keep upstream Cargo
builds explicitly outside/excluded from the root workspace initially and use
their own lockfile; workspace membership changes are a later decision.

For each patch, record its purpose, source/provenance, license obligations,
upstream status, and the test that justifies it. Author patches in temporary
local Git checkouts using normal diffs or `format-patch`; no remote fork is
necessary.

Patch application must start from a verified clean checkout and follow `series`
sequentially. Check and apply each patch before checking the next. Reject
already-applied or mismatching patches; do not use fuzzy matching, ignored
failures, or partial application. Changing a source pin, patch, or relevant
build flag invalidates the corresponding generated artifacts.

## Phase 1: baseline and minimal process probes

1. Check KVM access, toolchain versions, available RAM/disk, and required tools.
   Install missing dependencies only when a selected build/check reports them
   missing; do not run privileged build scripts or weaken global device access.
2. Fetch the pinned sources and verify their identities and licenses.
3. Build only the required Hyperlight-Unikraft kernel, loader, CLI, and minimal
   rootfs, with one job. Preserve the source's locked dependencies.
4. Prove an ordinary Linux executable and a small `posix_spawn()` child work.
   Test child exit status, waiting, signal delivery, pipes, and repeated creation.
5. Probe the features PostgreSQL will need: `setsid`, `signalfd`, blocked signals,
   ELF re-execution, child TLS, stack limits, anonymous shared memory, and
   synchronization between two guest processes.
6. Compare failures against the BSDkrun fixes. The pinned Hyperlight branch and
   loader already contain their own changes; import only still-relevant fixes,
   with a reproducer for each.

If kernel/loader patches are needed, rebuild the actual guest ELF and the CLI
that embeds it. Patching source while using a prebuilt embedded kernel is not
validation. Record the guest binary checksum and its source/patch/build inputs.

**Gate:** child processes execute reliably and exchange data without hangs,
TLS corruption, or unintended parent-state changes. If this fails, stop the
PostgreSQL bring-up and fix the minimal probe first.

2026-11-08: the initial gate passed all nine native and KVM guest probes with
512 MiB scratch and the caught-signal patch. Phase 2 adds the ownership and
shared-mapping lifetime fixes; all nine baseline probes still pass.
See [the measured baseline and limitations](README.md).

## Phase 2: PostgreSQL build and functional server

Use the BSDkrun patch stack as a starting point, adapting and guarding its
single-address-space assumptions for this guest target.

| Change area | Intended approach |
|-------------|-------------------|
| Child creation | Build with `EXEC_BACKEND`; replace its fork-and-exec path with `posix_spawn()`, not a hand-written `vfork()` substitution |
| Backend executable | Resolve the known packaged executable reliably; avoid unnecessary shell/version-check subprocesses only when the image's immutable executable identity is enforced |
| System V shared memory | Provide a reviewed single-address-space implementation or compatibility layer; do not return success from unsupported IPC operations |
| Dynamic shared memory | Implement correct creation, attach, detach, and destroy semantics for shared mappings; do not unmap memory still used by another process |
| Guest identity | Prefer a functioning non-root guest identity; otherwise use a target-guarded root-check patch confined to the unikernel build |
| Stacks | Align actual application/child stacks, reported limits, and `max_stack_depth`; do not merely increase the reported limit |

Preserve data-directory ownership/exclusivity checks. The reference IPC patch
relaxes an interlock based on its one-kernel assumptions; that is not safe to
copy without enforcing one server per data directory at the host boundary.
Anonymous-memory descriptors containing guest addresses must not be reused
across VM instances or reboots.

Package Linux PIE executables where required by the loader, dependencies, and
timezone/locale data into a Hyperlight CPIO rootfs. Handle archive hard links
according to the pinned loader's support. Prepare an initial database with
matching-version native `initdb` tools during packaging if guest initialization
cannot yet run. Do not run guest-specific shared-address-space patches as the
native PostgreSQL baseline.

Start directly as an application entry point, without relying on an interpreter
driver. Explicitly disable automatic warm snapshots/cache restoration.
Use a small configuration: two connections, low shared buffers and working
memory, no parallel query workers, and initially no optional autovacuum,
replication, or logging workers. Required startup/checkpoint/WAL processes still
need to work. Record every disabled feature as a prototype limitation.

Do not import `fsync = off` as a general workaround. If necessary to isolate
process/SQL bring-up, allow it only in an explicitly labeled RAM-only smoke
profile; that profile is nonpersistent and cannot satisfy a storage gate.
Synchronization unsupported by a filesystem remains a storage-stage blocker,
not a reason to silently pretend to synchronize or disable it for host-backed
database tests.

**Gate:** a real postmaster listens through host-loopback TCP forwarding;
two simultaneous sessions can authenticate, create/insert/select, commit and
roll back, maintain distinct session settings/temp tables, and exit cleanly.
Repeated connections must not leak memory or confuse backend state.

2026-11-08: this gate passes with PostgreSQL 16.15, a non-root uid/gid 1000
kernel profile, two SCRAM-authenticated sessions, explicit commit/rollback,
settings/temp-table isolation, and 24 checked reconnects after four warm-up
connections. Measured reconnect RSS growth is zero, and graceful shutdown
returns guest status 0. The profile is explicitly RAM-only with `fsync=off`;
it does not satisfy a storage or recovery gate. See [results](README.md).

## Phase 3: persistent filesystem semantics

Use a dedicated writable host directory for `PGDATA` and WAL, with host-side
exclusive ownership for its entire VM lifetime. A second experiment host trying
the same directory must fail before launching a server. Do not share one live
data directory across native PostgreSQL, multiple VMs, or restored clones.

Add real synchronization end to end:

1. Guest POSIX/VFS `fsync` and `fdatasync` hooks.
2. Explicit host protocol operations, including directory synchronization.
3. `FsBackend` support and the directory backend's actual OS synchronization.
4. Correct propagation of unsupported operations and I/O failures to PostgreSQL.

Do not add successful default implementations for backends that cannot sync.
Validate partial writes, offsets, append/truncate behavior, rename ordering,
mode handling, required locking, and files opened with synchronous-write flags.
Closing a file or calling `write_all` is not a durability barrier.

The current directory-listing protocol also has a finite single-reply limit.
Implement pagination before claiming support for directories beyond that limit;
include such a test before expanding catalog/file-count coverage.

**Gate:** synchronization is observed at the host boundary with `fsync` enabled,
file and directory errors are surfaced, a second writer is rejected, and
PostgreSQL starts against host-backed data without relying on silent no-ops.

## Phase 4: spill, transactions, and restart validation

Keep fixtures deterministic and inside the data budget.

| Test | Required evidence |
|------|-------------------|
| SQL correctness | Exact expected rows/checksums, commits, rollbacks, and two-session isolation |
| Disk spilling | Force an external sort using low `work_mem`; JSON `EXPLAIN ANALYZE` reports disk sort space and temp I/O, not just a successful query |
| Spill correctness | Compare ordered-result checksums with the native baseline and check temp-file cleanup |
| Graceful restart | Same data and committed transactions remain after shutdown and a fresh VM boot |
| Crash recovery | Terminate only the owned host/VM process during activity; boot a new VM and verify acknowledged commits, recovery completion, and absence of stale guest-address IPC state |
| Storage failures | Inject write/sync errors and quota exhaustion; operations fail explicitly, without acknowledging unsuccessful commits |
| Backend lifecycle | Repeated connections, client disconnects, and backend termination do not corrupt other sessions or leave indefinite waits |
| Memory/disk bounds | Measure guest/host usage and enforce fixture, WAL, temp, and live-VM limits |

Use a bounded spill fixture, for example 100,000 rows with moderately sized
sortable values, and `work_mem = '64kB'`. Adjust the fixture only if necessary
to obtain measured disk spilling. Assert disk-space/temp-I/O fields in the
execution plan; a low memory setting alone is not proof of spilling.

Process termination does not simulate host power loss or loss of the OS page
cache. Instrument sync calls and, where feasible, use a controlled failure
backend to distinguish synchronized from unsynchronized writes. Document the
fault model and remaining durability limitations. Do not reboot this shared
machine or drop its caches for a test.

**Gate:** all targeted checks pass with synchronization enabled. A RAM-only
demo, `fsync = off`, or successful sequential connections cannot satisfy this
gate. If shared-memory lifecycle or storage semantics remain unreliable,
document the exact failure and stop short of a durability claim.

## Phase 5: measured evaluation and optional feature expansion

Only after the persistent-storage gate:

- Compare against native PostgreSQL of the same version, using an unmodified
  native build, equivalent configuration/data, and documented CPU allowance.
- Measure startup, first connection/query, steady SQL p50/p95/p99, throughput,
  spill-heavy queries, peak memory, disk use, and guest-to-host I/O overhead.
- Run modes sequentially; record CPU affinity, virtualization, compiler/library
  versions, storage type, cache conditions, and synchronization settings.
- Use the existing [benchmark report](../benchmark/RESULTS.md) as an example of
  reporting uncertainty, not as a PostgreSQL baseline.
- Re-enable optional workers/features individually, with tests. No replication,
  broad Linux compatibility, or production claim follows from the minimal
  server passing.

Snapshots are a separate follow-up. Before supporting them, define how files,
locks, connections, WAL, clocks, and shared-memory descriptors behave at restore.
Never restore multiple writable server clones against the same `PGDATA`.

## Deliverables and decision

The implementation should leave:

1. Source pins, ordered patch sets, and patch provenance.
2. Reproducible bounded build/rootfs recipes and binary build metadata.
3. Functional, spill, synchronization, and fresh-VM recovery tests.
4. A sample README explaining launch/cleanup and every known limitation.
5. A results document distinguishing functional success, persistence evidence,
   unsupported features, resource use, and remaining fault-model gaps.

**Go:** targeted server, spill, and storage tests pass with clear limitations.
**Partial success:** SQL works, but persistent-storage or recovery gates fail;
keep it labeled a functional prototype.
**No-go:** required process/shared-memory semantics cannot be made reliable with
a reasonably bounded patch set. Preserve reproducers instead of introducing
success-shaped fallbacks or silently disabling correctness requirements.

Do not commit credentials, live databases, downloaded source, build caches, or
logs by default. Changes to existing workspace code require targeted regression
checks; planning/documentation changes alone do not require rebuilding.
Creating forks, opening a PR, merging, or deploying infrastructure is not part
of writing this plan and requires a separate user request.

## References

- [PostgreSQL versioning policy](https://www.postgresql.org/support/versioning/)
- [Published PostgreSQL 16.15 checksum](https://ftp.postgresql.org/pub/source/v16.15/postgresql-16.15.tar.bz2.sha256)
- [Hyperlight-Unikraft reference revision](https://github.com/hyperlight-dev/hyperlight-unikraft/tree/155d333bad549377e8026a596ce89c0d4cd8e9ef)
- [Its guest configuration](https://github.com/hyperlight-dev/hyperlight-unikraft/blob/155d333bad549377e8026a596ce89c0d4cd8e9ef/defconfig-elfloader)
- [Its filesystem backend](https://github.com/hyperlight-dev/hyperlight-unikraft/blob/155d333bad549377e8026a596ce89c0d4cd8e9ef/src/hostfs.rs)
- [BSDkrun PostgreSQL build reference](https://github.com/tsirysndr/bsdkrun/blob/b31ff8ebed68d0ea9cba88345f4a99dc13c01fb7/examples/unikraft-postgres/Dockerfile)
