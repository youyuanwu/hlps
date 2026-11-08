# Unikraft compatibility patches

Base: `3b7cba8816c02a497e7b5aa9c901655784a01000`, from the pinned
Hyperlight-Unikraft submodule. Apply in [series](series) order.

## 0001: wake waits for caught signals

Locally authored for this experiment; not copied from BSDkrun and not submitted
upstream. The affected source retains its BSD-3-Clause notices.

The base kernel interrupts blocking system calls for fatal signals but only
wakes `pause`/`sigsuspend` for caught signals. A child with a `SIGUSR1` handler
blocked in `read(pipe)` never unwinds its read to dispatch that handler. The
Phase 1 `signals` probe reports a guest deadlock.

Extract the existing interruption helper and use it for caught, unmasked
signals as well. A process-directed signal selects one eligible thread;
thread-directed signals interrupt only that thread. Masked/ignored signals
are not forced out of their waits, and a vfork parent is not resumed while
the child borrows its stack.

Reproducer/regressions: `signals` (process and pthread handler delivery during
blocking reads, fatal signal wait status), `blocked-signals` (mask inheritance,
signalfd, and no forced handler delivery), plus the entire Phase 1 guest suite.
`SA_RESTART` remains unsupported by the base kernel; this patch does not
implement it. The test retries `EINTR` explicitly.

## 0002: record RAMFS single-user ownership

Locally authored; existing RAMFS BSD notices are retained. No upstream
submission. The base RAMFS vnode attribute handler leaves uid/gid zero,
regardless of the configured guest uid/gid, so PostgreSQL correctly rejects
its data directory when the guest uses non-root uid 1000.

Record the configured creator identity in each RAMFS node and report those
stored fields through stat. With no posix-user support, the existing zero
identity is preserved. This is a single-user filesystem model, not general
multi-user/chown support or CPIO ownership restoration.

Reproducer/regression: `pg-init` validates the initialized data directory and
a newly created temporary file against the actual guest uid/gid, then normal
PostgreSQL ownership checks run unchanged. Original Phase 1 probes validate
the default uid-0 kernel profile.

## 0003: shared anonymous mappings outlive their creator

Locally authored under the affected source's BSD-3-Clause license; not
submitted upstream. The pinned kernel tracks mmap ownership by exec image and
unmaps a process's mappings when it exits. That wrongly destroys PostgreSQL
statistics DSM segments created/pinned by transient startup or client
processes: later backends assert on a corrupt DSA segment header or fault.

Mark anonymous `MAP_SHARED` mappings as VM-scoped, not owned by one exec image.
They survive creator exit until explicit `munmap` or VM teardown. Retain
exec/process cleanup for private mappings. Preserve the anonymous shared flag
through remapping without permitting another process's private mapping, or
unowned kernel heap mappings, to grow.

Reproducer/regressions: `pg-init` reexecutes a child that creates an 8 KiB
anonymous shared mapping, reaps it, checks the surviving bytes, shrinks and
regrows the mapping, and explicitly unmaps it. The full SQL gate checks
statistics use, backend exit/reconnect, graceful teardown, and bounded RSS
growth. Ordinary Phase 1 process/TLS/shared-memory probes also run.
