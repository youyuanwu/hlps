import tempfile
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import postgres


class PostgresTests(unittest.TestCase):
    def test_exact_postgres_version(self):
        postgres.check_version("postgres (PostgreSQL) 16.15\n")
        for value in ("postgres (PostgreSQL) 16.4", "16.15", ""):
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                postgres.check_version(value)

    def test_rss_threshold(self):
        postgres.check_rss_growth(1000, 33768)
        postgres.check_rss_growth(1000, 900)
        with self.assertRaises(RuntimeError):
            postgres.check_rss_growth(1000, 33769)

    def test_budget_counts_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "subdir").mkdir()
            (root / "one").write_bytes(b"abc")
            (root / "subdir/two").write_bytes(b"defg")
            self.assertEqual(postgres.bytes_in(root), 7)

    def test_sql_shape_and_values(self):
        class FakeSession:
            def execute(self, _):
                return ["1|committed"]
        postgres.expect(FakeSession(), "SELECT", ["1|committed"])
        with self.assertRaises(RuntimeError):
            postgres.expect(FakeSession(), "SELECT", ["1", "committed"])

    def test_packaging_rejects_stale_flags_and_mixed_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            build = root / "postgres-guest-build"
            (build / "src/backend").mkdir(parents=True)
            installed = root / "phase2/guest-install/usr/local/bin"
            installed.mkdir(parents=True)
            (build / "src/backend/postgres").write_bytes(b"verified")
            (installed / "postgres").write_bytes(b"verified")
            expected = {"configure": ["current-flags"], "patches": []}
            (build / "experiment-inputs.json").write_text(json.dumps(expected))
            with patch.object(postgres, "WORK", root), patch.object(postgres, "DATA", root / "phase2"), \
                    patch.object(postgres, "postgres_inputs", return_value=expected):
                postgres.verify_postgres_install(True)
                (installed / "postgres").write_bytes(b"stale")
                with self.assertRaises(RuntimeError):
                    postgres.verify_postgres_install(True)
                (installed / "postgres").write_bytes(b"verified")
                (build / "experiment-inputs.json").write_text('{"configure":["old-flags"]}')
                with self.assertRaises(RuntimeError):
                    postgres.verify_postgres_install(True)

if __name__ == "__main__":
    unittest.main()
