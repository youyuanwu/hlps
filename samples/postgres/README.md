# PostgreSQL experiment

This directory implements the Phase 1 Hyperlight-Unikraft baseline/process
probes and the Phase 2 PostgreSQL 16.15 RAM-only prototype from
[the plan](PLAN.md). Phase 1 needs no network, host mounts, database files,
or snapshots. Phase 2 uses authenticated loopback TCP and an initialized
in-guest database; it is not a durable storage milestone.

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

This is a functional, RAM-only prototype. **Phase 3 has not started**;
host-backed storage, disk-spill validation, durable synchronization/recovery,
and snapshots are not supported or claimed.
