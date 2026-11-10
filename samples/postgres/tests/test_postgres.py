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
            (build / "src/include").mkdir()
            config = build / "src/include/pg_config.h"
            config.write_text("/* #undef USE_ASSERT_CHECKING */\n")
            installed = root / "phase2/guest-install/usr/local/bin"
            installed.mkdir(parents=True)
            (build / "src/backend/postgres").write_bytes(b"verified")
            (installed / "postgres").write_bytes(b"verified")
            expected = {"configure": ["current-flags"], "patches": []}
            (build / "experiment-inputs.json").write_text(json.dumps(expected))
            with patch.object(postgres, "WORK", root), patch.object(postgres, "DATA", root / "phase2"), \
                    patch.object(postgres, "postgres_inputs", return_value=expected):
                postgres.verify_postgres_install(True)
                config.write_text("#define USE_ASSERT_CHECKING 1\n")
                with self.assertRaisesRegex(RuntimeError, "assertions disabled"):
                    postgres.verify_postgres_install(True)
                config.write_text("/* #undef USE_ASSERT_CHECKING */\n")
                (installed / "postgres").write_bytes(b"stale")
                with self.assertRaises(RuntimeError):
                    postgres.verify_postgres_install(True)
                (installed / "postgres").write_bytes(b"verified")
                (build / "experiment-inputs.json").write_text('{"configure":["old-flags"]}')
                with self.assertRaises(RuntimeError):
                    postgres.verify_postgres_install(True)

    def test_guest_and_native_comparison_flags_are_optimized_without_assertions(self):
        for guest in (False, True):
            args = postgres.configure_args(guest)
            self.assertIn("CFLAGS=-O2 -std=gnu17 -march=x86-64", args)
            self.assertNotIn("--enable-cassert", args)
        self.assertIn("--disable-cassert", postgres.configure_args(True))

    def test_build_generates_headers_before_compiling_under_an_outer_make(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "phase2").mkdir()
            with patch.object(postgres, "WORK", root), patch.object(postgres, "DATA", root / "phase2"), \
                    patch.object(postgres, "postgres_inputs", return_value={"configure": []}), \
                    patch.object(postgres.runtime, "run") as run:
                postgres.build_postgres(True)
                commands = [call.args[0] for call in run.call_args_list]
                headers = ["make", "-j1", "-C", "src/backend", "generated-headers"]
                self.assertIn(headers, commands)
                compilation = next(i for i, args in enumerate(commands) if "all" in args)
                self.assertLess(commands.index(headers), compilation)

if __name__ == "__main__":
    unittest.main()
