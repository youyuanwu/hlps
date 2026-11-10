# Unikraft compatibility patches

Base: `3b7cba8816c02a497e7b5aa9c901655784a01000`, from the pinned
Hyperlight-Unikraft submodule. Apply in [series](series) order.

## 0001: wake waits for caught signals

Locally authored for this experiment; not copied from BSDkrun and not submitted
upstream. The affected source retains its BSD-3-Clause notices.

The interruption helper wakes blocking I/O for caught, unmasked signals,
allowing a `SIGUSR1` handler to run in a pipe-reading child.
A process-directed signal selects one eligible thread;
thread-directed signals interrupt only that thread. Masked/ignored signals
are not forced out of their waits, and a vfork parent is not resumed while
the child borrows its stack.

Validation: `signals` (process and pthread handler delivery during
blocking reads, fatal signal wait status), `blocked-signals` (mask inheritance,
signalfd, and no forced handler delivery), plus the entire native/KVM process suite.
`SA_RESTART` remains unsupported by the base kernel; this patch does not
implement it. The test retries `EINTR` explicitly.

## 0002: record RAMFS single-user ownership

Locally authored; RAMFS BSD notices are retained. No upstream submission.

Each RAMFS node records the configured creator identity and reports those
fields through stat. Without posix-user support, the identity is zero.
This is a single-user filesystem model, not general
multi-user/chown support or CPIO ownership restoration.

Validation: `pg-init` validates the initialized data directory and
a newly created temporary file against the actual guest uid/gid, then normal
PostgreSQL's own ownership checks validate the guest identity. The runtime
probes validate the default uid-0 kernel profile.

## 0003: shared anonymous mappings outlive their creator

Locally authored under the affected source's BSD-3-Clause license; not
submitted upstream.

Anonymous `MAP_SHARED` mappings are VM-scoped, not owned by one exec image.
They survive creator exit until explicit `munmap` or VM teardown, supporting
PostgreSQL statistics DSM segments created by transient startup/client
processes. Private mappings receive exec/process cleanup. Remapping preserves
the anonymous shared flag without permitting another process's private
mapping, or unowned kernel heap mappings, to grow.

Validation: `pg-init` reexecutes a child that creates an 8 KiB
anonymous shared mapping, reaps it, checks the surviving bytes, shrinks and
regrows the mapping, and explicitly unmaps it. The full SQL gate checks
statistics use, backend exit/reconnect, graceful teardown, and bounded RSS
growth. Native/KVM process/TLS/shared-memory probes also run.

## 0004: hostfs synchronization, ownership, pagination and close semantics

Locally authored; retains the affected BSD notices. Not submitted upstream.
Paired with [the host patch](../hyperlight-unikraft/README.md).

Hostfs fsync uses an explicit host call. The VFS fdatasync path uses the same
full fsync barrier. Synchronous writes (O_SYNC/O_DSYNC) also execute
the full barrier before returning.

Hostfs has no guest write cache: its vnodes skip implicit fsync-on-close.
Explicit sync errors propagate. Close unlocks/releases resources even after
sync errors for other filesystems.

Create/mkdir modes are applied on the host, stat replies report uid/gid,
and append file position tracks the actual end. Bounded per-open directory
page state supports rewind without relisting the whole directory on every
entry. Malformed or unsupported backend page replies are rejected.

The hostfs protocol is path-based, not an OS file-description protocol.
Closing and reopening files across renames is tested; POSIX semantics for
unlinked-open files and arbitrary seekdir cookies are not implemented here.
Unsupported hostfs record locks fail EOPNOTSUPP instead of silently reporting
success. PostgreSQL's data-directory O_EXCL interlock remains intact, and an
additional host inode lock exclusively owns all writable storage during a VM.

Tests: native helper/audit tests; KVM offsets/truncate/append/modes/rename,
O_SYNC/O_DSYNC barriers, file/directory sync errors, partial writes, quota,
read-only rejection, record-lock refusal, and 5,000 entries plus rewind.
PostgreSQL runs with fsync and synchronous_commit enabled and observed WAL,
relation, and directory OS synchronization. Runtime and RAM-backed SQL
validation also pass.
