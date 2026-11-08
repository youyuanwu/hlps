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
