# PostgreSQL 16.15 guest patch stack

Base: the exact archive and SHA-256 in [sources.toml](../../sources.toml).
Apply in [series](series) order with `git apply --check`, then `git apply`,
before checking the next patch.

These are locally authored, target-guarded adaptations of the ideas in the
[BSDkrun 16.4 port](https://github.com/tsirysndr/bsdkrun/tree/b31ff8ebed68d0ea9cba88345f4a99dc13c01fb7/examples/unikraft-postgres).
They are not unchanged imports and have not been submitted upstream.
PostgreSQL's source copyright/license notices remain in the verified source
trees; the reference is MIT and its original per-file notices also apply.
No PostgreSQL source fork or submodule is needed.

All changes require `HYPERLIGHT_UNIKRAFT`; ordinary Linux builds keep the
original code. The guest additionally requires `EXEC_BACKEND`. Native initdb
and client tools are built from a separate, unmodified source tree.

| Patch | Purpose and reference |
|-------|-----------------------|
| 0001 | `posix_spawn` child creation instead of fork/exec; clean failed parameter files; reuse the resolved self executable path. Inspired by `exec-backend-posix-spawn.patch` and `exec-backend-self-path.patch`. |
| 0002 | Anonymous main shared memory in one address space, validated child header, child detach without unmapping, and target-only mmap GUC support under EXEC_BACKEND. Replaces the reference's `no-sysv-ipc.patch` ENOSYS fallback. |
| 0003 | DSM descriptors name anonymous mappings; attach validates a per-VM generation; detach preserves other users; destroy resolves even mappings not attached by its caller. Inspired by `dsm-single-address-space.patch`, with additional lifetime, bounds, and error handling. |

The reference `allow-root.patch` is **not used**. The guest kernel profile
sets a fixed uid/gid 1000; PostgreSQL's own root and data-directory ownership
checks remain intact. Executable-path reuse assumes the tested fixed package
is not upgraded or replaced during its VM lifetime; it is not a general
replacement for PostgreSQL's installation/version check.

Scope: one fresh VM and exclusive host experiment ownership, the RAM-backed SQL
demo or bounded host-backed storage/recovery validation, and no
warm snapshots. Raw address descriptors are invalid for another VM generation.
A stale data-directory shared-memory interlock is a fatal error, not a
successful "unused" result. The persistent host removes only recognized stale
guest lockfiles while exclusively owning storage. PostgreSQL's existing mmap
DSM startup cleanup removes old descriptors without attaching their addresses;
[Recovery tests](../../recovery.py) verify fresh identities across actual crashes. This is not a
production PostgreSQL replacement on Linux or a power-loss durability proof.

The SQL gate validates required auxiliary processes, independent concurrent
backends, transactions, settings/temp-table isolation, repeated attach/detach,
bounded reconnect RSS growth, authentication, and graceful postmaster exit.
Parallel workers and other optional DSM workloads remain disabled.
