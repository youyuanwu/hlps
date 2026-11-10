import hashlib
from pathlib import Path
import struct
from subprocess import CompletedProcess
import tempfile
import unittest
from unittest.mock import patch

import read_benchmark as benchmark


class ReadBenchmarkTests(unittest.TestCase):
    def test_pgbench_requires_all_transactions_and_exact_log_shape(self):
        output = ("number of transactions actually processed: 4/4\n"
                  "number of failed transactions: 0 (0.000%)\n"
                  "number of clients: 1\nnumber of threads: 1\nquery mode: simple\n")
        log = "".join(f"0 {i} 1234 0 1700000000 123456\n" for i in range(1, 5))
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "transactions"
            path.write_text(log)
            self.assertEqual(benchmark.pgbench_times(output, path), [1234000] * 4)
            for content in (output.replace("4/4", "3/4"), output.replace("0 (", "1 ("),
                            output.replace("clients: 1", "clients: 2"), output.replace("simple", "prepared")):
                with self.assertRaises(RuntimeError):
                    benchmark.pgbench_times(content, path)
            for content in (log.replace("0 1 1234", "0 0 1234"),
                            log.replace("0 1 1234", "1 1 1234"),
                            log.replace("0 1 1234", "0 1 failed"),
                            log.replace("0 1 1234", "0 1 0"),
                            log.replace("1700000000 123456", "1700000000 1000000"),
                            log + "0 5 1234 0 1700000000 123456\n",
                            "\n".join(log.splitlines()[:3])):
                path.write_text(content)
                with self.assertRaises(RuntimeError):
                    benchmark.pgbench_times(output, path)

    def test_psql_timings_reject_missing_extra_or_nontiming_output(self):
        output = "Time: 123.456 ms\n" * 4
        self.assertEqual(benchmark.psql_times(output), [123456000] * 4)
        self.assertEqual(benchmark.psql_times("Time: 1234.567 ms (00:01.235)\n" * 4),
                         [1234567000] * 4)
        for content in ("", "Time: 123.456 ms\n" * 3, output * 2,
                        output.replace("123.456", "0.000"), output + "ERROR: query failed\n",
                        output.replace("123.456", "nan")):
            with self.assertRaises(RuntimeError):
                benchmark.psql_times(content)

    def test_expected_binary_hash_has_exact_postgres_record_encoding(self):
        data = b"PGCOPY\n\xff\r\n\0" + b"\0" * 8
        for i in range(1, 3):
            payload = hashlib.md5(str(i).encode()).hexdigest().encode() * 4
            data += b"\0\2" + struct.pack("!i", 4) + struct.pack("!i", i)
            data += struct.pack("!i", len(payload)) + payload
        data += b"\xff\xff"
        with patch.object(benchmark, "ROWS", 2):
            self.assertEqual(benchmark.expected_copy_hash(), hashlib.sha256(data).hexdigest())

    def test_export_validation_requires_success_size_hash_and_removes_only_success(self):
        def export(args, *, stdout, **kwargs):
            stdout.write(b"abc")
            return CompletedProcess(args, 0, stderr="")

        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(benchmark, "COPY_BYTES", 3), \
                patch.object(benchmark.subprocess, "run", side_effect=export):
            root = Path(temporary)
            expected = hashlib.sha256(b"abc").hexdigest()
            result = benchmark.verify_export(55434, {}, root, "good", expected)
            self.assertEqual(result["copy_bytes"], 3)
            self.assertFalse((root / "good-validation.bin").exists())
            with self.assertRaises(RuntimeError):
                benchmark.verify_export(55434, {}, root, "bad-hash", "0" * 64)
            self.assertTrue((root / "bad-hash-validation.bin").exists())
            with patch.object(benchmark, "COPY_BYTES", 4):
                with self.assertRaises(RuntimeError):
                    benchmark.verify_export(55434, {}, root, "bad-size", expected)
            with patch.object(benchmark.subprocess, "run",
                              return_value=CompletedProcess([], 1, stderr="COPY failed")):
                with self.assertRaises(RuntimeError):
                    benchmark.verify_export(55434, {}, root, "bad-exit", expected)

    def test_sample_shapes_do_not_claim_first_row_or_transferred_control_data(self):
        for workload in benchmark.WORKLOADS:
            rows = benchmark.samples([2000000] * 4, workload)
            self.assertEqual(rows[0]["cache"], "fresh")
            self.assertEqual([row["index"] for row in rows], [0, 1, 2, 3])
            self.assertNotIn("first_row_ns", rows[0])
            self.assertEqual(rows[1]["rows_per_second"], 50000000)
            self.assertEqual(rows[0]["payload_bytes"], 0 if workload == "aggregate" else benchmark.PAYLOAD)
        for times in ([], [1, 2, 3], [1, 2, 3, 0], [1, 2, 3, float("nan")]):
            with self.assertRaises(RuntimeError):
                benchmark.samples(times, "select")

    def test_plan_requires_actual_serial_seqscan_and_buffers(self):
        scan = {"Node Type": "Seq Scan", "Relation Name": "spill_fixture", "Actual Rows": benchmark.ROWS,
                "Actual Loops": 1, "Shared Hit Blocks": 8, "Shared Read Blocks": 32}
        plan = [{"Plan": {"Node Type": "Aggregate", "Plans": [scan]}, "Execution Time": 2.0}]
        self.assertEqual(benchmark.validate_plan(plan)["shared_read_blocks"], 32)
        for change in ({"Node Type": "Index Scan"}, {"Actual Rows": 1}, {"Actual Loops": 2},
                       {"Shared Hit Blocks": 0, "Shared Read Blocks": 0}, {"Temp Written Blocks": 1},
                       {"Shared Hit Blocks": -1}):
            with self.assertRaises(RuntimeError):
                benchmark.validate_plan([{"Plan": dict(scan, **change), "Execution Time": 2}])
        for node in ("Gather", "Gather Merge", "Sort"):
            with self.assertRaises(RuntimeError):
                benchmark.validate_plan([{"Plan": {"Node Type": node, "Plans": [scan]},
                                          "Execution Time": 2}])

    def test_summary_rejects_missing_or_duplicate_runs(self):
        with self.assertRaises(RuntimeError):
            benchmark.summarize([])
        runs = [{"mode": "native", "workload": "select", "repeat": 0}
                for _ in range(2 * benchmark.REPEATS * len(benchmark.WORKLOADS))]
        with self.assertRaises(RuntimeError):
            benchmark.summarize(runs)

    def test_summary_separates_first_repeated_and_nontransfer_control(self):
        runs = []
        for mode in ("native", "guest"):
            for repeat in range(benchmark.REPEATS):
                for workload in benchmark.WORKLOADS:
                    runs.append({"mode": mode, "workload": workload, "repeat": repeat,
                                 "samples": benchmark.samples([2000000] * 4, workload)})
        summary = benchmark.summarize(runs)
        self.assertEqual(summary["guest"]["select"]["fresh"]["samples"], 3)
        self.assertEqual(summary["guest"]["select"]["warmed"]["samples"], 9)
        self.assertEqual(summary["guest"]["select"]["warmed"]["payload_mb_per_second"], 6600)
        self.assertEqual(summary["native"]["aggregate"]["fresh"]["payload_mb_per_second"], 0)


if __name__ == "__main__":
    unittest.main()
