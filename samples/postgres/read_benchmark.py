#!/usr/bin/env python3
"""Full-table read comparisons using standard pgbench and psql clients."""

import argparse
from datetime import datetime, timezone
from decimal import Decimal
import fcntl
import hashlib
import json
import os
import re
import struct
import subprocess

import evaluate
import postgres
import recovery
import runtime
import storage

DATA = runtime.WORK / "read-benchmark"
ROWS = recovery.ROWS
PAYLOAD = ROWS * 132
COPY_BYTES = ROWS * 142 + 21
WORKLOADS = {
    "select": "SELECT id,value FROM spill_fixture;",
    "copy": "COPY (SELECT id,value FROM spill_fixture) TO STDOUT (FORMAT BINARY);",
    "aggregate": "SELECT count(*),sum(length(value)) FROM spill_fixture;",
}
REPEATS = 3
WARMED = 3
TRANSACTIONS = WARMED + 1


def clients():
    result = {}
    for name in ("pgbench", "psql"):
        path = postgres.NATIVE / "bin" / name
        if not path.is_file():
            raise RuntimeError(f"missing {name}; run make pg-native")
        version = runtime.run([path, "--version"], capture=True).strip()
        if version != f"{name} (PostgreSQL) 16.15":
            raise RuntimeError(f"unexpected standard PostgreSQL client: {version}")
        result[name] = {"version": version, "sha256": runtime.digest(path)}
    result["libpq_sha256"] = runtime.digest((postgres.NATIVE / "lib/libpq.so").resolve())
    return result


def expected_copy_hash():
    digest = hashlib.sha256(b"PGCOPY\n\xff\r\n\0" + struct.pack("!II", 0, 0))
    for i in range(1, ROWS + 1):
        value = hashlib.md5(str(i).encode()).hexdigest().encode() * 4
        digest.update(struct.pack("!hiii", 2, 4, i, 128))
        digest.update(value)
    digest.update(struct.pack("!h", -1))
    return digest.hexdigest()


def pgbench_times(output, path):
    for pattern in (rf"^number of transactions actually processed:\s*{TRANSACTIONS}/{TRANSACTIONS}\s*$",
                    r"^number of failed transactions:\s*0\s+\(0(?:\.0+)?%\)\s*$",
                    r"^number of clients:\s*1\s*$", r"^number of threads:\s*1\s*$",
                    r"^query mode:\s*simple\s*$"):
        if len(re.findall(pattern, output, re.MULTILINE)) != 1:
            raise RuntimeError("pgbench did not report the exact successful single-client workload")
    if path.stat().st_size > 8192:
        raise RuntimeError("pgbench transaction log exceeded its bound")
    rows = [line.split() for line in path.read_text().splitlines()]
    if len(rows) != TRANSACTIONS:
        raise RuntimeError("missing/extra pgbench transaction samples")
    times = []
    for index, row in enumerate(rows):
        if len(row) != 6 or not all(value.isdecimal() for value in row) or \
                int(row[0]) != 0 or int(row[1]) != index + 1 or \
                int(row[2]) <= 0 or int(row[3]) != 0 or int(row[5]) >= 1000000:
            raise RuntimeError("invalid pgbench transaction log shape or failure")
        times.append(int(row[2]) * 1000)
    return times


def psql_times(output):
    times = []
    for line in output.splitlines():
        match = re.fullmatch(r"Time: ([0-9]+(?:\.[0-9]+)?) ms(?: \([0-9:.]+\))?", line)
        if not match:
            raise RuntimeError(f"unexpected psql timing output: {line!r}")
        elapsed = Decimal(match[1]) * 1000000
        if elapsed <= 0 or elapsed != int(elapsed):
            raise RuntimeError("invalid psql timing sample")
        times.append(int(elapsed))
    if len(times) != TRANSACTIONS:
        raise RuntimeError("missing/extra psql COPY samples")
    return times


def samples(times, workload):
    if len(times) != TRANSACTIONS or any(type(value) is not int or value <= 0 for value in times):
        raise RuntimeError("invalid standard-client timings")
    payload = 0 if workload == "aggregate" else PAYLOAD
    return [{"cache": "fresh" if index == 0 else "warmed", "index": index,
             "elapsed_ns": elapsed, "payload_bytes": payload,
             "rows_per_second": ROWS * 1e9 / elapsed,
             "payload_mb_per_second": payload * 1e3 / elapsed}
            for index, elapsed in enumerate(times)]


def validate_plan(plan):
    nodes = []
    pending = [plan[0]["Plan"]]
    while pending:
        node = pending.pop()
        nodes.append(node)
        pending.extend(node.get("Plans", []))
    scans = [node for node in nodes if node.get("Node Type") == "Seq Scan"
             and node.get("Relation Name") == "spill_fixture"]
    if len(scans) != 1 or scans[0].get("Actual Rows") != ROWS or \
            scans[0].get("Actual Loops") != 1 or \
            any(node.get("Node Type") in ("Gather", "Gather Merge", "Sort") for node in nodes):
        raise RuntimeError("read workload is not a single serial full-table scan")
    scan = scans[0]
    hits, reads = scan.get("Shared Hit Blocks", 0), scan.get("Shared Read Blocks", 0)
    if hits < 0 or reads < 0 or hits + reads <= 0 or scan.get("Temp Written Blocks", 0) != 0:
        raise RuntimeError("scan lacks buffer evidence or unexpectedly writes temp files")
    return {"actual_rows": scan["Actual Rows"], "shared_hit_blocks": hits,
            "shared_read_blocks": reads, "execution_time_ms": plan[0]["Execution Time"]}


def probe(session, workload, run_dir, name):
    session.execute("SET enable_indexscan=off; SET enable_indexonlyscan=off; SET enable_bitmapscan=off;")
    sql = WORKLOADS["select" if workload == "copy" else workload]
    result = []
    for index in range(2):
        plan = json.loads("\n".join(session.execute(
            "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) " + sql, timeout=30)))
        runtime.write_json(run_dir / f"{name}-buffer-{index}.json", plan)
        result.append(validate_plan(plan))
    return result


def prepare(mode, env, run_dir, server_cpu):
    def initialize(session, name):
        evaluate.prepare(session, run_dir, name)
        if session.execute(recovery.CHECKSUM, timeout=60) != [recovery.expected_checksum()]:
            raise RuntimeError(f"{mode} read fixture checksum mismatch")

    if mode == "native":
        with recovery.native_server(env, server_cpu):
            with postgres.Session("read-setup-native", env, run_dir, recovery.NATIVE_PORT) as session:
                initialize(session, "native-setup")
    else:
        with recovery.VM("read-setup-guest", env, run_dir, server_cpu) as vm:
            with vm.session("setup") as session:
                initialize(session, "guest-setup")


def buffer_counts(session):
    values = session.execute("SELECT heap_blks_read,heap_blks_hit FROM pg_statio_user_tables "
                             "WHERE relname='spill_fixture';")
    if len(values) != 1 or not re.fullmatch(r"[0-9]+\|[0-9]+", values[0]):
        raise RuntimeError("missing/invalid table buffer counters")
    return list(map(int, values[0].split("|")))


def verify_export(port, env, run_dir, name, expected):
    path = run_dir / f"{name}-validation.bin"
    args = [str(postgres.NATIVE / "bin/psql"), "-X", "-q", "-w", "-v", "ON_ERROR_STOP=1",
            "-h", "127.0.0.1", "-p", str(port), "-U", "phase2", "-d", "postgres",
            "-c", "COPY (SELECT id,value FROM spill_fixture ORDER BY id) TO STDOUT (FORMAT BINARY);"]
    with path.open("xb") as file:
        completed = subprocess.run(args, stdout=file, stderr=subprocess.PIPE, text=True,
                                   env=dict(env, PGOPTIONS="-c statement_timeout=30s"), timeout=35)
    (run_dir / f"{name}-validation.log").write_text(completed.stderr)
    if completed.returncode != 0 or path.stat().st_size != COPY_BYTES or runtime.digest(path) != expected:
        raise RuntimeError(f"psql export failed exact independent row/byte hash validation: {name}")
    path.unlink()
    return {"rows": ROWS, "copy_bytes": COPY_BYTES, "sha256": expected,
            "scope": "separate untimed ordered binary COPY, not pgbench per-row validation"}


def run_client(port, workload, env, run_dir, name, cpu):
    script = run_dir / f"{name}.sql"
    common = ["-h", "127.0.0.1", "-p", str(port), "-U", "phase2"]
    if workload == "copy":
        script.write_text("\\timing on\n\\o /dev/null\n" + (WORKLOADS[workload] + "\n") * TRANSACTIONS)
        args = [str(postgres.NATIVE / "bin/psql"), "-X", "-q", "-w", "-v", "ON_ERROR_STOP=1",
                *common, "-d", "postgres", "--file", str(script)]
    else:
        script.write_text(WORKLOADS[workload] + "\n")
        prefix = run_dir / f"{name}-transactions"
        args = [str(postgres.NATIVE / "bin/pgbench"), *common, "-n", "-c", "1", "-j", "1",
                "-s", "1", "-M", "simple", "-t", str(TRANSACTIONS), "-l", "--log-prefix", str(prefix),
                "-f", str(script), "postgres"]
    timed_env = dict(env, PGOPTIONS="-c statement_timeout=30s -c enable_indexscan=off "
                     "-c enable_indexonlyscan=off -c enable_bitmapscan=off")
    completed = subprocess.run(["taskset", "--cpu-list", str(cpu), *args], env=timed_env,
                               capture_output=True, text=True, timeout=90)
    (run_dir / f"{name}-client.log").write_text(completed.stdout + completed.stderr)
    if completed.returncode != 0:
        raise RuntimeError(f"standard read client failed; inspect {run_dir / f'{name}-client.log'}")
    if workload == "copy":
        times = psql_times(completed.stdout)
    else:
        paths = list(run_dir.glob(prefix.name + ".*"))
        if len(paths) != 1:
            raise RuntimeError("pgbench did not produce exactly one transaction log")
        times = pgbench_times(completed.stdout, paths[0])
    return samples(times, workload)


def measure(mode, repeat, workload, run_dir, env, server_cpu, client_cpu, expected):
    name = f"{mode}-{repeat}-{workload}"
    result = {"mode": mode, "repeat": repeat, "workload": workload,
              "client": "psql" if workload == "copy" else "pgbench"}

    def run(session, port):
        before = buffer_counts(session)
        result["samples"] = run_client(port, workload, env, run_dir, name, client_cpu)
        session.execute("SELECT pg_stat_clear_snapshot();")
        after = buffer_counts(session)
        reads, hits = [value - old for value, old in zip(after, before)]
        if reads < 0 or hits < 0 or reads + hits == 0:
            raise RuntimeError("standard-client scans lack valid heap buffer evidence")
        result["timed_scans_buffers"] = {"heap_read_blocks": reads, "heap_hit_blocks": hits,
                                        "scope": f"all {TRANSACTIONS} timed scans combined"}
        postgres.expect(session, WORKLOADS["aggregate"], [f"{ROWS}|{ROWS * 128}"])
        result["output_validation"] = verify_export(port, env, run_dir, name, expected)
        result["buffers_after_timing"] = probe(session, workload, run_dir, name)

    if mode == "native":
        with recovery.native_server(env, server_cpu) as (process, startup):
            with evaluate.Memory(process.pid, server_cpu) as memory:
                with postgres.Session(name + "-observer", env, run_dir, recovery.NATIVE_PORT) as session:
                    run(session, recovery.NATIVE_PORT)
        result.update(startup_ns=startup, memory=memory.result(),
                      storage=evaluate.storage_summary(recovery.NATIVE_DATA))
        (run_dir / f"{name}-server.log").write_bytes((recovery.DATA / "native.log").read_bytes())
    else:
        with recovery.VM(name, env, run_dir, server_cpu) as vm:
            with evaluate.Memory(vm.host_pid, server_cpu) as memory:
                with vm.session("observer") as session:
                    run(session, postgres.PORT)
        result.update(startup_ns=vm.startup_ns, memory=memory.result(), host=vm.result,
                      storage=evaluate.storage_summary(storage.STORAGE))
        if any(not row["ok"] for row in storage.audit_rows(vm.audit)):
            raise RuntimeError("read benchmark encountered failed host filesystem operations")
    print(f"PASS {result['client']} {name}: 1 first + {WARMED} repeated scans, output verified separately",
          flush=True)
    return result


def summarize(runs):
    summary = {}
    if len(runs) != 2 * REPEATS * len(WORKLOADS):
        raise RuntimeError("read benchmark is missing mode/repeat/workload runs")
    for mode in ("native", "guest"):
        summary[mode] = {}
        for workload in WORKLOADS:
            selected = [run for run in runs if run["mode"] == mode and run["workload"] == workload]
            if sorted(run["repeat"] for run in selected) != list(range(REPEATS)):
                raise RuntimeError("missing/duplicate read benchmark repeats")
            summary[mode][workload] = {}
            for cache, count in (("fresh", REPEATS), ("warmed", REPEATS * WARMED)):
                values = [row for run in selected for row in run["samples"] if row["cache"] == cache]
                if len(values) != count:
                    raise RuntimeError("missing read benchmark cache-state samples")
                elapsed = [row["elapsed_ns"] for row in values]
                summary[mode][workload][cache] = {
                    **evaluate.distribution(elapsed),
                    "rows_per_second": count * ROWS * 1e9 / sum(elapsed),
                    "payload_mb_per_second": sum(row["payload_bytes"] for row in values) * 1e3 / sum(elapsed),
                }
    return summary


def test():
    inputs = evaluate.verify_server()
    tool_meta = clients()
    allowed = sorted(os.sched_getaffinity(0))
    if len(allowed) < 2:
        raise RuntimeError("read benchmark needs distinct allowed server/client logical CPUs")
    server_cpu, client_cpu = allowed[:2]
    run_dir = DATA / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    run_dir.mkdir()
    result = {
        "status": "running", "started_at": datetime.now(timezone.utc).isoformat(),
        "environment": evaluate.environment(server_cpu, client_cpu), "clients": tool_meta,
        "inputs": dict(inputs, script_sha256=runtime.digest(runtime.HERE / "read_benchmark.py"),
                       evaluate_sha256=runtime.digest(runtime.HERE / "evaluate.py")),
        "runs": [], "run_directory": str(run_dir),
        "timing_scope": "pgbench transaction log latencies for SELECT/aggregate; psql timing for COPY to /dev/null; "
                        "connection/startup and separate correctness validation excluded; no first-row metric",
        "cache_conditions": "separate fresh server per workload/repeat, no fixture reads before first sample; "
                            "three subsequent repeated scans on the same connection; host page cache retained",
    }
    result["environment"]["cache_conditions"] = result["cache_conditions"]
    runtime.write_json(DATA / "results.json", result)
    env = dict(recovery.environment(), PGAPPNAME="read-benchmark", LC_ALL="C")
    expected = expected_copy_hash()
    try:
        with evaluate.client_affinity(client_cpu):
            for mode in ("native", "guest"):
                prepare(mode, env, run_dir, server_cpu)
            for repeat in range(REPEATS):
                order = ("native", "guest") if repeat % 2 == 0 else ("guest", "native")
                for mode in order:
                    for workload in WORKLOADS:
                        result["runs"].append(measure(
                            mode, repeat, workload, run_dir, env, server_cpu, client_cpu, expected))
                        runtime.write_json(DATA / "results.json", result)
        result.update(status="passed", summary=summarize(result["runs"]),
                      finished_at=datetime.now(timezone.utc).isoformat(),
                      load_average_after=list(os.getloadavg()))
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        result.update(status="failed", error=str(error))
        raise
    finally:
        runtime.write_json(DATA / "results.json", result)
        runtime.write_json(run_dir / "results.json", result)
    print("PASS read benchmark: standard pgbench/psql, native/guest, output hashes and serial scans")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("test",))
    parser.parse_args()
    DATA.mkdir(parents=True, exist_ok=True)
    with (runtime.WORK / "phase1.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        test()


if __name__ == "__main__":
    main()
