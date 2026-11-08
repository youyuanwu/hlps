# Unikraft compatibility patches

Base: `3b7cba8816c02a497e7b5aa9c901655784a01000`, from the pinned
Hyperlight-Unikraft submodule. Apply in [series](series) order.

## 0001: wake waits for caught signals

Locally authored for this experiment; not copied from BSDkrun and not submitted
upstream. The affected source retains its BSD-3-Clause notices.

The base kernel interrupts blocking system calls for fatal signals but only
wakes `pause`/`sigsuspend` for caught signals. A child with a `SIGUSR1` handler
blocked in `read(pipe)` never unwinds its read to dispatch that handler. The
`signals` process probe reproduces the guest deadlock.

Extract the existing interruption helper and use it for caught, unmasked
signals as well. A process-directed signal selects one eligible thread;
thread-directed signals interrupt only that thread. Masked/ignored signals
are not forced out of their waits, and a vfork parent is not resumed while
the child borrows its stack.

Reproducer/regressions: `signals` (process and pthread handler delivery during
blocking reads, fatal signal wait status), `blocked-signals` (mask inheritance,
signalfd, and no forced handler delivery), plus the entire native/KVM process suite.
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
PostgreSQL ownership checks run unchanged. The runtime probes validate
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
growth. Native/KVM process/TLS/shared-memory probes also run.

## 0004: hostfs synchronization, ownership, pagination and close semantics

Locally authored; retains the affected BSD notices. Not submitted upstream.
Paired with [the host patch](../hyperlight-unikraft/README.md).

Replace hostfs's successful fsync stub with an explicit host call. The legacy
VFS fdatasync path uses this same full fsync, which is a stronger durability
barrier rather than a no-op. Synchronous writes (O_SYNC/O_DSYNC) also execute
the full barrier before returning.

Hostfs has no guest write cache: mark its vnodes to avoid VFS's implicit
fsync-on-close, which would turn closing read-only or removed files into
unrequested path-based syncs. Explicit sync errors propagate. Ensure close
still unlocks/releases resources after errors for other filesystems.

Set create/mkdir modes on the host, report uid/gid from the single stat reply,
and make append file position track the actual end. Add bounded per-open
directory page state and support rewind without relisting the whole directory
on every entry. Reject malformed or unsupported backend page replies.

The hostfs protocol is still path-based, not an OS file-description protocol.
Closing and reopening files across renames is tested; POSIX semantics for
unlinked-open files and arbitrary seekdir cookies are not implemented here.
Unsupported hostfs record locks fail EOPNOTSUPP instead of silently reporting
success. PostgreSQL's data-directory O_EXCL interlock remains intact, and an
additional host inode lock exclusively owns all writable storage during a VM.

Tests: native helper/audit tests; KVM offsets/truncate/append/modes/rename,
O_SYNC/O_DSYNC barriers, file/directory sync errors, partial writes, quota,
read-only rejection, record-lock refusal, and 5,000 entries plus rewind.
PostgreSQL runs with fsync and synchronous_commit enabled and observed WAL,
relation, and directory OS synchronization. Previous guest profiles also run.
