# Host filesystem patch

Base: Hyperlight-Unikraft `155d333bad549377e8026a596ce89c0d4cd8e9ef`.
Locally authored for the persistent storage backend; not submitted upstream. The affected Apache-2.0
copyright/license notices remain in the pinned source.

[series](series) adds:

- Explicit `FsBackend::sync`, with an unsupported/error default, never a
  successful no-op. The directory backend invokes the opened OS file's
  `sync_all` or `sync_data`; directory synchronization uses `sync_all`.
- A registered `fs_sync(mount, path, data_only)` operation with errno propagation.
- Actual Unix uid/gid appended to stat replies. Constructors retain uid/gid 0
  for existing non-directory backends.
- Bounded `fs_list_page` replies, in lexical cursor order. The directory
  backend retains only a page of entries, propagates enumeration errors, and
  refuses undecodable names instead of skipping them.
- Public capability-confined `DirBackend::open` for the experiment's observed
  and quota-limited wrapper. All operations remain beneath its directory.
- A host snapshot-contract bump. No experiment profile restores snapshots.

Existing `fs_list` remains registered for old callers. New guest kernels
require the appended stat fields and new host functions; build both together.
New pages are stable while a directory is unchanged. Modifying a directory
during enumeration is not a snapshot of that directory.

Tests: directory backend sync and confinement, 5,000-entry bounded pagination,
error/quota forwarding, actual KVM file/directory sync error probes, and the
PostgreSQL fsync-on storage gate.
