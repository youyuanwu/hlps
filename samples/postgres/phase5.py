#!/usr/bin/env python3
"""Serial, affinity-pinned PostgreSQL evaluation with exact SQL validation."""

import argparse
from contextlib import contextmanager
import csv
from datetime import datetime, timezone
import fcntl
import json
import math
import os
from pathlib import Path
import platform
import signal
import subprocess
import threading
import time

import phase1
import phase2
import phase3
import phase4

DATA = phase1.WORK / "phase5"
CLIENT = DATA / "query-client"
COUNTS = {"select": 1000, "lookup": 1000, "commit": 300, "spill": 5}
PROFILE_COUNTS = {"select": 50, "lookup": 50, "commit": 50, "spill": 1}
REPEATS = 3


def client_inputs():
    return {str(path): phase1.digest(path) for path in (
        phase1.HERE / "tests/query-client.c",
        phase2.NATIVE / "include/libpq-fe.h",
        (phase2.NATIVE / "lib/libpq.so").resolve(),
    )}


def build():
    phase2.verify_postgres_install(False)
    phase1.run(["gcc", "-std=c11", "-O2", "-march=x86-64", "-Wall", "-Wextra", "-Werror",
                f"-I{phase2.NATIVE / 'include'}", phase1.HERE / "tests/query-client.c",
                f"-L{phase2.NATIVE / 'lib'}", f"-Wl,-rpath,{phase2.NATIVE / 'lib'}",
                "-lpq", "-o", CLIENT])
    phase1.write_json(DATA / "build.json", {
        "inputs": client_inputs(), "client_sha256": phase1.digest(CLIENT),
        "compiler": phase1.run(["gcc", "--version"], capture=True).splitlines()[0],
    })


def quantile(values, percentile):
    if not values or not 0 < percentile <= 100 or any(type(value) is not int or value <= 0 for value in values):
        raise RuntimeError("percentiles require positive timing samples")
    ordered = sorted(values)
    return ordered[math.ceil(len(ordered) * percentile / 100) - 1]


def distribution(values):
    return {"samples": len(values), "min_ns": min(values), "p50_ns": quantile(values, 50),
            "p95_ns": quantile(values, 95), "p99_ns": quantile(values, 99), "max_ns": max(values)}


def samples(path, counts=COUNTS):
    if path.stat().st_size > 1024 * 1024:
        raise RuntimeError("client CSV exceeded its bounded output budget")
    groups = {}
    with path.open(newline="") as file:
        reader = csv.DictReader(file)
        if reader.fieldnames != ["phase", "workload", "index", "elapsed_ns"]:
            raise RuntimeError("unexpected timing CSV header")
        for row in reader:
            if None in row or None in row.values():
                raise RuntimeError("malformed timing CSV row")
            key = (row["phase"], row["workload"])
            index, elapsed = int(row["index"]), int(row["elapsed_ns"])
            if elapsed <= 0:
                raise RuntimeError("nonpositive client timing")
            groups.setdefault(key, []).append((index, elapsed))
    expected = {("connection", "connect"): 1, ("first_query", "select"): 1}
    expected.update({("latency", workload): count for workload, count in counts.items()})
    expected.update({("batch", workload): 1 for workload in counts})
    if set(groups) != set(expected):
        raise RuntimeError("unexpected/missing client timing groups")
    result = {"connection_ns": groups[("connection", "connect")][0][1],
              "first_query_ns": groups[("first_query", "select")][0][1], "workloads": {}}
    for key, count in expected.items():
        values = groups[key]
        indices = [counts[key[1]]] if key[0] == "batch" else list(range(count))
        if len(values) != count or [i for i, _ in values] != indices:
            raise RuntimeError(f"incorrect sample count/indices: {key}")
    for workload, count in counts.items():
        latencies = [elapsed for _, elapsed in groups[("latency", workload)]]
        batch = groups[("batch", workload)][0][1]
        if batch < sum(latencies):
            raise RuntimeError("batch timing is shorter than its queries")
        result["workloads"][workload] = {
            "latency_ns": latencies, "batch_ns": batch,
            "throughput_per_second": count * 1e9 / batch, **distribution(latencies),
        }
    return result


class Memory:
    def __init__(self, pid, cpu):
        self.pid = pid
        self.cpu = cpu
        self.done = threading.Event()
        self.error = None
        self.peak_pss = 0
        self.peak_rss = 0
        self.peak_processes = 0
        self.races = 0
        self.count = 0

    def sample(self):
        pss = rss = processes = 0
        for pid in (self.pid, *phase2.descendants(self.pid)):
            try:
                if os.sched_getaffinity(pid) != {self.cpu}:
                    raise RuntimeError(f"server process {pid} escaped its CPU allowance")
                fields = {}
                for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()[1:]:
                    parts = line.split()
                    if parts[0] in ("Pss:", "Rss:"):
                        fields[parts[0]] = int(parts[1])
                pss += fields["Pss:"]
                rss += fields["Rss:"]
                processes += 1
            except ProcessLookupError:
                if pid == self.pid:
                    raise
                self.races += 1
            except FileNotFoundError:
                if pid == self.pid:
                    raise
                self.races += 1
        self.count += 1
        self.peak_pss = max(self.peak_pss, pss)
        self.peak_rss = max(self.peak_rss, rss)
        self.peak_processes = max(self.peak_processes, processes)
        if pss > phase4.RSS_LIMIT:
            raise RuntimeError("server process-tree PSS exceeded 1 GiB")

    def monitor(self):
        try:
            while not self.done.wait(0.05):
                self.sample()
        except (OSError, RuntimeError, ValueError, KeyError) as error:
            self.error = str(error)
            try:
                os.kill(self.pid, signal.SIGINT)
            except ProcessLookupError:
                self.error += "; server already exited"

    def __enter__(self):
        self.sample()
        self.thread = threading.Thread(target=self.monitor)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.done.set()
        self.thread.join(timeout=2)
        if self.thread.is_alive():
            raise RuntimeError("resource sampler failed to stop")
        if self.error:
            raise RuntimeError(f"resource sampler failed: {self.error}")

    def result(self):
        return {"peak_pss_kib": self.peak_pss, "peak_sum_rss_kib": self.peak_rss,
                "peak_processes": self.peak_processes, "samples": self.count,
                "exited_child_sample_races": self.races}


def profile_rows(path):
    result = {}
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "host" and parts[1].startswith("fs_"):
            if len(parts) != 6 or parts[1] in result:
                raise RuntimeError("unexpected host filesystem profile shape")
            count = int(parts[2])
            total, mean, maximum = map(float, parts[3:])
            if count <= 0 or any(not math.isfinite(value) or value < 0 for value in (total, mean, maximum)):
                raise RuntimeError("invalid host filesystem profile sample")
            result[parts[1]] = {"calls": count, "total_ms": total, "mean_us": mean, "max_us": maximum}
    if not {"fs_read_bytes", "fs_write_bytes", "fs_sync"}.issubset(result):
        raise RuntimeError("profile lacks measured read/write/sync callbacks")
    return {"operations": result, "total_calls": sum(row["calls"] for row in result.values()),
            "total_ms": sum(row["total_ms"] for row in result.values())}


def prepare(session, run_dir, name):
    session.execute(phase4.FIXTURE + "CREATE UNIQUE INDEX ON spill_fixture(id);"
                    "DROP TABLE IF EXISTS eval_commits;"
                    "CREATE TABLE eval_commits(id int primary key,counter bigint NOT NULL);"
                    "INSERT INTO eval_commits VALUES (1,0);", timeout=60)
    plan = json.loads("\n".join(session.execute(
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) " + phase4.CHECKSUM, timeout=60)))
    phase1.write_json(run_dir / f"{name}-explain.json", plan)
    session.execute("CHECKPOINT;")
    return phase4.validate_spill(plan)


def run_client(run_dir, name, port, env, checksum, cpu, profiled):
    path = run_dir / f"{name}-samples.csv"
    complete = subprocess.run(
        ["taskset", "--cpu-list", str(cpu), str(CLIENT), str(port), str(path), checksum,
         "profile" if profiled else "main"],
        env=env, capture_output=True, text=True, timeout=90)
    (run_dir / f"{name}-client.log").write_text(complete.stdout + complete.stderr)
    if complete.returncode != 0 or complete.stdout.strip() != \
            "PASS query-client: exact results and bounded timing samples":
        raise RuntimeError(f"timed SQL client failed; inspect {run_dir / f'{name}-client.log'}")
    return samples(path, PROFILE_COUNTS if profiled else COUNTS)


def storage_summary(root):
    used = phase2.bytes_in(root)
    if used > phase2.BUDGET or phase4.temp_files(root):
        raise RuntimeError("evaluation exceeded storage budget or left spill files")
    return {"storage_bytes": used, "pgdata_bytes": phase2.bytes_in(root / "pgdata")
            if (root / "pgdata").exists() else used,
            "wal_bytes": phase2.bytes_in(root / ("pgdata/pg_wal" if (root / "pgdata").exists() else "pg_wal"))}


def measure(mode, repeat, run_dir, env, checksum, server_cpu, client_cpu, profiled=False):
    name = f"{mode}-{repeat}"
    result = {"mode": mode, "repeat": repeat, "profiled": profiled}
    if mode == "native":
        with phase4.native_server(env, server_cpu) as (process, startup):
            with Memory(process.pid, server_cpu) as memory:
                with phase2.Session(name + "-setup", env, run_dir, phase4.NATIVE_PORT) as session:
                    result["spill_evidence"] = prepare(session, run_dir, name)
                result["timings"] = run_client(
                    run_dir, name, phase4.NATIVE_PORT, env, checksum, client_cpu, False)
                with phase2.Session(name + "-checkpoint", env, run_dir, phase4.NATIVE_PORT) as session:
                    session.execute("CHECKPOINT;")
        result.update(startup_ns=startup, memory=memory.result(),
                      storage=storage_summary(phase4.NATIVE_DATA))
        (run_dir / f"{name}-guest.log").write_bytes((phase4.DATA / "native.log").read_bytes())
    else:
        report = run_dir / f"{name}-profile.txt" if profiled else None
        with phase4.VM(name, env, run_dir, server_cpu, report) as vm:
            with Memory(vm.host_pid, server_cpu) as memory:
                with vm.session("setup") as session:
                    result["spill_evidence"] = prepare(session, run_dir, name)
                result["timings"] = run_client(
                    run_dir, name, phase2.PORT, env, checksum, client_cpu, profiled)
                with vm.session("checkpoint") as session:
                    session.execute("CHECKPOINT;")
        result.update(startup_ns=vm.startup_ns, memory=memory.result(), host=vm.result,
                      storage=storage_summary(phase3.STORAGE))
        rows = phase3.audit_rows(vm.audit)
        errors = [row for row in rows if not row["ok"]]
        if errors:
            raise RuntimeError(f"guest evaluation reported host filesystem errors: {errors[:3]}")
        if profiled:
            result["host_fs_profile"] = profile_rows(report)
    print(f"PASS evaluation {name}: exact SQL, spill, pinned CPU, memory/storage bounds")
    return result


def summarize(runs):
    result = {}
    for mode in ("native", "guest"):
        values = [run for run in runs if run["mode"] == mode and not run["profiled"]]
        if len(values) != REPEATS:
            raise RuntimeError("evaluation is missing a full set of serial repeats")
        item = {
            "startup": distribution([run["startup_ns"] for run in values]),
            "first_connection": distribution([run["timings"]["connection_ns"] for run in values]),
            "first_query": distribution([run["timings"]["first_query_ns"] for run in values]),
            "peak_pss_kib": max(run["memory"]["peak_pss_kib"] for run in values),
            "peak_sum_rss_kib": max(run["memory"]["peak_sum_rss_kib"] for run in values),
            "max_final_storage_bytes": max(run["storage"]["storage_bytes"] for run in values),
            "max_final_wal_bytes": max(run["storage"]["wal_bytes"] for run in values),
            "workloads": {},
        }
        for workload, count in COUNTS.items():
            records = [run["timings"]["workloads"][workload] for run in values]
            medians = [row["p50_ns"] for row in records]
            item["workloads"][workload] = {
                **distribution([t for row in records for t in row["latency_ns"]]),
                "repeat_median_min_ns": min(medians), "repeat_median_max_ns": max(medians),
                "throughput_per_second": len(records) * count * 1e9 / sum(row["batch_ns"] for row in records),
                "repeat_throughput_min": min(row["throughput_per_second"] for row in records),
                "repeat_throughput_max": max(row["throughput_per_second"] for row in records),
            }
        result[mode] = item
    return result


def environment(server_cpu, client_cpu):
    cpu = json.loads(phase1.run(["lscpu", "--json"], capture=True))["lscpu"]
    interesting = {"CPU(s):", "Model name:", "Thread(s) per core:", "Core(s) per socket:",
                   "Hypervisor vendor:", "Virtualization type:"}
    governor = Path(f"/sys/devices/system/cpu/cpu{server_cpu}/cpufreq/scaling_governor")
    musl = subprocess.run(["/lib/ld-musl-x86_64.so.1"], capture_output=True, text=True, timeout=5)
    if musl.returncode != 1 or "musl libc" not in musl.stderr:
        raise RuntimeError("cannot identify the installed guest musl interpreter")
    return {
        "platform": platform.platform(), "kernel": platform.release(),
        "cpu": {row["field"]: row["data"] for row in cpu if row["field"] in interesting},
        "server_cpu": server_cpu, "client_cpu": client_cpu,
        "frequency_governor": governor.read_text().strip() if governor.exists() else "not exposed",
        "filesystem": json.loads(phase1.run(
            ["findmnt", "--json", "--target", DATA, "--output", "TARGET,SOURCE,FSTYPE,OPTIONS"], capture=True)),
        "gcc": phase1.run(["gcc", "--version"], capture=True).splitlines()[0],
        "rustc": phase1.run(["rustc", "--version"], capture=True).strip(),
        "glibc": phase1.run(["ldd", "--version"], capture=True).splitlines()[0],
        "musl": musl.stderr.splitlines()[:3],
        "postgres": phase1.run([phase2.NATIVE / "bin/postgres", "--version"], capture=True).strip(),
        "load_average": list(os.getloadavg()),
        "meminfo": Path("/proc/meminfo").read_text(),
        "cache_conditions": "fresh server processes; host caches retained; fixture/EXPLAIN/checkpoint preparation and per-workload warmup",
        "compiler_profile": "PG/client -O2 -march=x86-64; guest assertions on, native off; Rust host dev, debug=0, opt-level=0",
        "sources": phase1.pins(),
    }


def verify():
    phase3.verify()
    phase2.verify_postgres_install(False)
    build_meta = json.loads((DATA / "build.json").read_text())
    if build_meta["inputs"] != client_inputs() or build_meta["client_sha256"] != phase1.digest(CLIENT):
        raise RuntimeError("evaluation client changed; run eval-build")
    gate = json.loads((phase4.DATA / "results.json").read_text())
    expected = {
        "phase4_sha256": phase1.digest(phase1.HERE / "phase4.py"),
        "host_sha256": phase1.digest(phase3.DATA / "storage-host"),
        "rootfs_sha256": phase1.digest(phase3.DATA / "postgres.cpio"),
        "native_postgres_sha256": phase1.digest(phase2.NATIVE / "bin/postgres"),
        "native_psql_sha256": phase1.digest(phase2.NATIVE / "bin/psql"),
    }
    if gate["status"] != "passed" or gate["inputs"] != expected:
        raise RuntimeError("current inputs have not passed Phase 4; run recovery-test first")
    meta = json.loads((phase3.DATA / "storage.json").read_text())
    for field, path in (("config_sha256", phase3.STORAGE / "pgdata/postgresql.conf"),
                        ("profile_sha256", phase3.PROFILE),
                        ("hba_sha256", phase3.STORAGE / "pgdata/pg_hba.conf")):
        if meta.get(field) != phase1.digest(path):
            raise RuntimeError("persistent configuration changed; run storage-config and recovery-test")
    if phase4.FAULT.exists() or phase4.FAULT.with_suffix(".new").exists():
        raise RuntimeError("leftover WAL fault controls; inspect before evaluation")
    return expected


@contextmanager
def client_affinity(cpu):
    previous = os.sched_getaffinity(0)
    try:
        os.sched_setaffinity(0, {cpu})
        yield
    finally:
        os.sched_setaffinity(0, previous)


def test():
    inputs = verify()
    allowed = sorted(os.sched_getaffinity(0))
    if len(allowed) < 2:
        raise RuntimeError("evaluation requires distinct allowed server/client logical CPUs")
    server_cpu, client_cpu = allowed[:2]
    run_dir = DATA / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    run_dir.mkdir()
    result = {"status": "running", "started_at": datetime.now(timezone.utc).isoformat(),
              "environment": environment(server_cpu, client_cpu), "runs": [],
              "inputs": dict(inputs, client_sha256=phase1.digest(CLIENT),
                             phase5_sha256=phase1.digest(phase1.HERE / "phase5.py"))}
    phase1.write_json(DATA / "results.json", result)
    env = phase4.environment()
    env["PGAPPNAME"] = "phase5"
    checksum = phase4.expected_checksum()
    try:
        with client_affinity(client_cpu):
            for repeat in range(REPEATS):
                order = ("native", "guest") if repeat % 2 == 0 else ("guest", "native")
                for mode in order:
                    result["runs"].append(measure(
                        mode, repeat, run_dir, env, checksum, server_cpu, client_cpu))
                    phase1.write_json(DATA / "results.json", result)
            result["summary"] = summarize(result["runs"])
            result["diagnostic"] = measure(
                "guest", "profile", run_dir, env, checksum, server_cpu, client_cpu, profiled=True)
        result.update(status="passed", finished_at=datetime.now(timezone.utc).isoformat(),
                      load_average_after=list(os.getloadavg()), run_directory=str(run_dir))
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        result.update(status="failed", error=str(error))
        raise
    finally:
        phase1.write_json(DATA / "results.json", result)
        phase1.write_json(run_dir / "results.json", result)
    print("PASS phase5: six serial pinned runs and separate filesystem profile")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "test"))
    command = parser.parse_args().command
    DATA.mkdir(parents=True, exist_ok=True)
    with (phase1.WORK / "phase1.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        {"build": build, "test": test}[command]()


if __name__ == "__main__":
    main()
