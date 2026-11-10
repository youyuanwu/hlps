import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import storage


class StorageTests(unittest.TestCase):
    def test_release_metadata_is_required_before_packaging_or_running(self):
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(storage, "DATA", Path(temporary)), \
                patch.object(storage, "inputs", return_value={}), \
                patch.object(storage.runtime, "verify_upstream"), \
                patch.object(storage.runtime, "prepare_patches"), \
                patch.object(storage.postgres, "prepare_sources"):
            for filename, check in (("host.json", storage.package), ("build.json", storage.verify)):
                for settings in ({}, dict(storage.runtime.RUST_BUILD, profile="dev"),
                                 dict(storage.runtime.RUST_BUILD, opt_level=0)):
                    (Path(temporary) / filename).write_text(
                        json.dumps({"inputs": {}, "rust_build": settings}))
                    with self.assertRaisesRegex(RuntimeError, "release build"):
                        check()

    def test_writer_lock_rejects_aliases_and_is_released(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "storage"
            directory.mkdir()
            alias = root / "alias"
            alias.symlink_to(directory)
            with storage.ownership(directory):
                with self.assertRaises(RuntimeError):
                    with storage.ownership(alias):
                        pass
                code = ("import sys; from pathlib import Path; "
                        f"sys.path.insert(0, {str(storage.HERE)!r}); import storage; "
                        f"ctx=storage.ownership(Path({str(alias)!r})); ctx.__enter__()")
                denied = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=5)
                self.assertNotEqual(denied.returncode, 0)
                self.assertIn("already owned", denied.stderr)
            with storage.ownership(directory):
                pass

    def test_postgres_needs_observed_wal_and_directory_sync(self):
        rows = [{"operation": "sync_all", "path": "pgdata/pg_wal/segment", "ok": True},
                {"operation": "sync_all", "path": "pgdata/pg_logical", "ok": True}]
        storage.validate_audit(rows, True)
        for values in ([], rows[:1], rows[1:],
                       [dict(row, ok=False) for row in rows]):
            with self.assertRaises(RuntimeError):
                storage.validate_audit(values, True)

    def test_directory_requires_multiple_successful_pages(self):
        rows = [{"operation": op, "path": path, "ok": ok} for op, path, ok in (
            ("sync_all", "probes", True),
            ("sync_all", "probes/sync-write", True),
            ("sync_all", "probes/dsync-write", True),
            ("sync_all", "probes/fail-sync", False),
            ("sync_all", "probes/fail-dir-sync", False),
            ("write", "probes/partial-write", False),
            ("write", "probes/quota", False))]
        pages = [{"operation": "list_page", "path": "probes/many", "ok": True}] * 40
        storage.validate_audit(rows + pages, False)
        with self.assertRaises(RuntimeError):
            storage.validate_audit(rows + pages[:39], False)
        with self.assertRaises(RuntimeError):
            storage.validate_audit(rows + [dict(row, ok=False) for row in pages], False)

    def test_initialization_never_overwrites_existing_storage(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(storage, "STORAGE", Path(temporary)):
            with self.assertRaisesRegex(RuntimeError, "never overwrite"):
                storage.initialize()

    def test_budget_scan_surfaces_permission_errors(self):
        error = PermissionError("unreadable database subdirectory")
        def unreadable(*_, onerror, **__):
            onerror(error)
            return iter(())
        with patch.object(storage.postgres.os, "walk", unreadable):
            with self.assertRaises(PermissionError):
                storage.postgres.bytes_in(Path("/example"))

    def test_budget_does_not_follow_rootfs_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "link").symlink_to("/missing")
            self.assertEqual(storage.postgres.bytes_in(root), len("/missing"))


if __name__ == "__main__":
    unittest.main()
