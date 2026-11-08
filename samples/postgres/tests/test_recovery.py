import unittest
from pathlib import Path
import tempfile
from unittest.mock import patch

import recovery


class RecoveryTests(unittest.TestCase):
    def test_spill_requires_actual_disk_sort_and_both_temp_io_directions(self):
        node = {"Node Type": "Sort", "Sort Space Type": "Disk", "Sort Space Used": 1024,
                "Actual Rows": recovery.ROWS, "Temp Read Blocks": 12, "Temp Written Blocks": 16}
        evidence = recovery.validate_spill([{"Plan": node}])
        self.assertEqual(evidence["sort_space_kib"], 1024)
        for change in ({"Sort Space Type": "Memory"}, {"Sort Space Used": 0},
                       {"Temp Read Blocks": 0}, {"Temp Written Blocks": 0}, {"Actual Rows": 1}):
            with self.assertRaises(RuntimeError):
                recovery.validate_spill([{"Plan": dict(node, **change)}])

    def test_nested_sort_is_validated(self):
        node = {"Node Type": "Sort", "Sort Space Type": "Disk", "Sort Space Used": 10,
                "Actual Rows": recovery.ROWS, "Temp Read Blocks": 1, "Temp Written Blocks": 1}
        self.assertEqual(recovery.validate_spill([{"Plan": {"Plans": [node]}}])["temp_read_blocks"], 1)

    def test_stale_descriptor_identity_is_bounded_and_not_followed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "pgdata/pg_dynshmem"
            directory.mkdir(parents=True)
            descriptor = directory / "mmap.1"
            generation = "a" * 32
            payload = b"100MSDLH" + (4096).to_bytes(8, "little") * 2 + generation.encode() + b"\0" * 8
            with patch.object(recovery.storage, "STORAGE", root):
                descriptor.write_bytes(payload)
                self.assertEqual(recovery.dsm_generations(), {"mmap.1": generation})
                for content in (payload[:56], b"invalid!" + payload[8:], payload[:24] + b"z" * 32 + payload[56:]):
                    descriptor.write_bytes(content)
                    with self.assertRaises(RuntimeError):
                        recovery.dsm_generations()
                descriptor.unlink()
                descriptor.symlink_to("/missing")
                with self.assertRaises(RuntimeError):
                    recovery.dsm_generations()

    def test_only_interrupted_audit_tail_is_permitted_and_counted(self):
        with tempfile.TemporaryDirectory() as temporary:
            audit = Path(temporary) / "audit"
            audit.write_bytes(b'{"ok":true}\n{"operation":')
            rows, tail = recovery.audit_snapshot(audit, allow_partial=True)
            self.assertEqual(rows, [{"ok": True}])
            self.assertEqual(tail, len(b'{"operation":'))
            with self.assertRaises(RuntimeError):
                recovery.audit_snapshot(audit)
            audit.write_bytes(b'not-json\n')
            with self.assertRaises(ValueError):
                recovery.audit_snapshot(audit, allow_partial=True)


if __name__ == "__main__":
    unittest.main()
