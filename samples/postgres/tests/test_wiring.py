from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

import evaluate
import postgres
import recovery
import runtime
import storage


class WiringTests(unittest.TestCase):
    def test_input_fingerprints_use_current_script_names(self):
        with patch.object(runtime, "digest", return_value="sha256"):
            for inputs, expected in (
                    (runtime.build_inputs, {"runtime.py"}),
                    (postgres.inputs, {"runtime.py", "postgres.py"}),
                    (storage.inputs, {"runtime.py", "postgres.py", "storage.py"})):
                scripts = {Path(path).name for path in inputs() if Path(path).suffix == ".py"}
                self.assertEqual(scripts, expected)

    def test_all_validation_commands_load(self):
        for module in (runtime, postgres, storage, recovery, evaluate):
            with self.subTest(command=module.__name__):
                result = subprocess.run([sys.executable, "-B", module.__file__, "--help"],
                                        capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("usage:", result.stdout)


if __name__ == "__main__":
    unittest.main()
