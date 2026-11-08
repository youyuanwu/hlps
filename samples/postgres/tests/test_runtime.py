import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("runtime", Path(__file__).parents[1] / "runtime.py")
runtime = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runtime)


class ResultTests(unittest.TestCase):
    def test_exact_success(self):
        runtime.validate_result("spawn", "diagnostic\nPASS spawn\n", 0)

    def test_missing_wrong_duplicate_and_timeout_fail(self):
        for output, code in (("", 0), ("PASS pipes\n", 0),
                             ("PASS spawn\nPASS spawn\n", 0),
                             ("PASS spawn\n", 1), ("PASS spawn\n", 124)):
            with self.subTest(output=output, code=code), self.assertRaises(RuntimeError):
                runtime.validate_result("spawn", output, code)

    def test_patch_series_order_and_rejection(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "patches/unikraft"
            directory.mkdir(parents=True)
            for name in ("one.patch", "two.patch"):
                (directory / name).touch()
            with patch.object(runtime, "HERE", root):
                (directory / "series").write_text("# ordered\ntwo.patch\none.patch\n")
                self.assertEqual([p.name for p in runtime.patch_files("unikraft")],
                                 ["two.patch", "one.patch"])
                for content in ("../one.patch\n", "one.patch\none.patch\n", "missing.patch\n"):
                    (directory / "series").write_text(content)
                    with self.assertRaises(RuntimeError):
                        runtime.patch_files("unikraft")

    def test_submodule_identity_and_locked_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            spec = runtime.pins()["hyperlight-unikraft"]
            status = "\n".join(f" {commit} {path}" for path, commit in
                               spec["submodules"].items())
            lock = root / "Cargo.lock"
            lock.write_text('[[package]]\nname="hyperlight-host"\nversion="0.19.0"\n'
                            '[[package]]\nname="hyperlight-common"\nversion="0.19.0"\n')

            def output(args, **kwargs):
                if args[1] == "rev-parse":
                    return spec["commit"] + "\n"
                if args[1] == "submodule":
                    return status
                return ""

            with patch.object(runtime, "UPSTREAM", root), patch.object(runtime, "run", output):
                self.assertEqual(runtime.verify_upstream(), spec["submodules"])
                lock.write_text(lock.read_text().replace("0.19.0", "0.20.0"))
                with self.assertRaises(RuntimeError):
                    runtime.verify_upstream()
                status = status.replace(" ", "+", 1)
                with self.assertRaises(RuntimeError):
                    runtime.verify_upstream()

    def test_patches_checked_and_applied_sequentially(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "patches/unikraft"
            directory.mkdir(parents=True)
            (directory / "series").write_text("one.patch\ntwo.patch\n")
            for name in ("one.patch", "two.patch"):
                (directory / name).write_text(name)
            calls = []

            def output(args, **kwargs):
                calls.append(args)
                return ""

            with patch.object(runtime, "HERE", root), patch.object(runtime, "WORK", root), \
                    patch.object(runtime, "UPSTREAM", root), patch.object(runtime, "run", output):
                runtime.prepare_patches()
                applies = [args for args in calls if args[1] == "apply"]
                self.assertEqual(applies, [
                    ["git", "apply", "--check", directory / "one.patch"],
                    ["git", "apply", directory / "one.patch"],
                    ["git", "apply", "--check", directory / "two.patch"],
                    ["git", "apply", directory / "two.patch"],
                ])
                calls.clear()
                runtime.prepare_patches()
                self.assertFalse(any(args[1] == "apply" for args in calls))
                (directory / "series").write_text("two.patch\none.patch\n")
                with patch.object(runtime, "run", return_value="existing diff"):
                    with self.assertRaises(RuntimeError):
                        runtime.prepare_patches()
                (directory / "one.patch").write_text("changed")
                with patch.object(runtime, "run", return_value="unexpected diff"):
                    with self.assertRaises(RuntimeError):
                        runtime.prepare_patches()


if __name__ == "__main__":
    unittest.main()
