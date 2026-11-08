import csv
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import phase5


class EvaluationTests(unittest.TestCase):
    def test_nearest_rank_percentiles_and_invalid_samples(self):
        values = list(range(100, 0, -1))
        self.assertEqual(phase5.quantile(values, 50), 50)
        self.assertEqual(phase5.quantile(values, 95), 95)
        self.assertEqual(phase5.quantile(values, 99), 99)
        for sample in ([], [0], [-1], [float("nan")]):
            with self.assertRaises(RuntimeError):
                phase5.quantile(sample, 50)

    def write_samples(self, path, counts):
        with path.open("w", newline="") as file:
            writer = csv.writer(file)
            writer.writerow(["phase", "workload", "index", "elapsed_ns"])
            writer.writerow(["connection", "connect", 0, 10])
            writer.writerow(["first_query", "select", 0, 10])
            for workload, count in counts.items():
                for i in range(count):
                    writer.writerow(["latency", workload, i, 10])
                writer.writerow(["batch", workload, count, count * 10 + 1])

    def test_exact_csv_counts_shapes_and_batch_bounds(self):
        counts = {"select": 3, "lookup": 3, "commit": 2, "spill": 1}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "samples.csv"
            self.write_samples(path, counts)
            result = phase5.samples(path, counts)
            self.assertEqual(result["workloads"]["select"]["samples"], 3)
            original = path.read_text()
            for content in (original.replace("latency,select,0,10", "latency,select,1,10"),
                            original.replace("latency,select,0,10\n", ""),
                            original.replace("batch,select,3,31", "batch,select,3,29"),
                            original.replace("latency,select,0,10", "latency,select,0,0"),
                            original.replace("phase,workload", "phase,unexpected")):
                path.write_text(content)
                with self.assertRaises(RuntimeError):
                    phase5.samples(path, counts)

    def test_profile_requires_real_io_counts_and_finite_durations(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "profile"
            text = ("host fs_read_bytes 4 2.000 500.0 1000.0\n"
                    "host fs_write_bytes 2 1.000 500.0 1000.0\n"
                    "host fs_sync 1 1.000 1000.0 1000.0\n")
            path.write_text(text)
            self.assertEqual(phase5.profile_rows(path)["total_calls"], 7)
            for content in ("", text.replace("fs_sync", "fs_other"),
                            text.replace("2.000", "nan"),
                            text.replace("fs_read_bytes 4", "fs_read_bytes 0")):
                path.write_text(content)
                with self.assertRaises(RuntimeError):
                    phase5.profile_rows(path)

    def test_pss_sum_affinity_and_exact_memory_limit(self):
        memory = phase5.Memory(101, 7)
        with patch.object(phase5.phase2, "descendants", return_value=[102]), \
                patch.object(phase5.os, "sched_getaffinity", return_value={7}), \
                patch.object(Path, "read_text", return_value="header\nRss: 100 kB\nPss: 80 kB\n"):
            memory.sample()
            self.assertEqual(memory.result()["peak_pss_kib"], 160)
            self.assertEqual(memory.result()["peak_sum_rss_kib"], 200)
            with patch.object(phase5.phase4, "RSS_LIMIT", 159):
                with self.assertRaisesRegex(RuntimeError, "exceeded"):
                    memory.sample()
            with patch.object(phase5.os, "sched_getaffinity", return_value={7, 8}):
                with self.assertRaisesRegex(RuntimeError, "CPU allowance"):
                    memory.sample()

    def test_client_affinity_is_restored_after_failure(self):
        with patch.object(phase5.os, "sched_getaffinity", return_value={2, 3}), \
                patch.object(phase5.os, "sched_setaffinity") as affinity:
            with self.assertRaises(RuntimeError):
                with phase5.client_affinity(3):
                    raise RuntimeError("test")
            self.assertEqual(affinity.call_args_list[0].args, (0, {3}))
            self.assertEqual(affinity.call_args_list[-1].args, (0, {2, 3}))


if __name__ == "__main__":
    unittest.main()
