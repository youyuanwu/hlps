# PostgreSQL experiment: Phase 1

This directory implements the pinned Hyperlight-Unikraft baseline and process
probes from [the plan](PLAN.md). It does **not** build or run PostgreSQL yet.
No network access, host filesystem mounts, database files, or snapshots are
needed by these probes.

## Build and run

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
PostgreSQL is downloaded and checksum-verified but left unbuilt. BSDkrun is a
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
interrupted I/O explicitly. PostgreSQL process adaptation and durable
filesystem synchronization are still subsequent phases.
