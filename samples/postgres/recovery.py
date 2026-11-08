#!/usr/bin/env python3
"""Disk spilling, fresh-VM recovery, and SQL-time WAL failure validation."""

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import stat
import subprocess
import sys
import threading
import time

import runtime
import postgres
import storage

DATA = runtime.WORK / "phase4"
NATIVE_DATA = DATA / "native-pgdata"
NATIVE_PORT = 55434
FAULT = DATA / "wal-fault.json"
ROWS = 100000
RSS_LIMIT = 1024 * 1024
FIXTURE = (
    "DROP TABLE IF EXISTS spill_fixture; "
    "CREATE TABLE spill_fixture AS SELECT i AS id, repeat(md5(i::text),4) AS value "
    f"FROM generate_series(1,{ROWS}) i; ANALYZE spill_fixture;"
)
ORDERED = 'SELECT id,value FROM spill_fixture ORDER BY value COLLATE "C",id'
CHECKSUM = (
    "SELECT md5(string_agg(id::text || ':' || value,E'\\n')) FROM ("
    + ORDERED + ") ordered;"
)


def environment():
    pgpass = DATA / "pgpass"
    pgpass.write_text(f"127.0.0.1:*:postgres:phase2:{(storage.DATA / 'password').read_text().strip()}\n")
    pgpass.chmod(0o600)
    env = {key: value for key, value in os.environ.items() if not key.startswith("PG")}
    env.update(PGPASSFILE=str(pgpass), PGCONNECT_TIMEOUT="2", PGAPPNAME="phase4")
    return env


def psql_args(port=postgres.PORT):
    return list(map(str, [postgres.NATIVE / "bin/psql", "-X", "-At", "-v", "ON_ERROR_STOP=1",
                         "-h", "127.0.0.1", "-p", port, "-U", "phase2", "-d", "postgres"]))


def ready(process, port, env, log):
    deadline = time.monotonic() + 30
    while True:
        if process.poll() is not None:
            raise RuntimeError(f"server exited {process.returncode}; inspect {log}")
        check = subprocess.run(
            list(map(str, [postgres.NATIVE / "bin/pg_isready", "-h", "127.0.0.1",
                           "-p", port, "-U", "phase2", "-d", "postgres", "-t", "1"])),
            env=env, capture_output=True, timeout=3)
        if check.returncode == 0:
            return
        if time.monotonic() >= deadline:
            raise RuntimeError(f"server readiness deadline exceeded; inspect {log}")
        time.sleep(0.1)


def free_ports(*ports):
    for port in ports:
        with socket.socket() as reservation:
            reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            reservation.bind(("127.0.0.1", port))


@contextmanager
def native_server(env, cpu=None):
    free_ports(NATIVE_PORT)
    if (NATIVE_DATA / "postmaster.pid").exists():
        raise RuntimeError("native baseline has a live/stale lockfile; inspect before reuse")
    if not NATIVE_DATA.exists():
        runtime.run(["timeout", "--kill-after=5s", "60s", postgres.NATIVE / "bin/initdb",
                    "-D", NATIVE_DATA, "--username=phase2", "--encoding=UTF8", "--locale=C",
                    "--auth-local=reject", "--auth-host=scram-sha-256",
                    f"--pwfile={storage.DATA / 'password'}"])
        with (NATIVE_DATA / "postgresql.conf").open("a") as config:
            config.write("\n" + (runtime.HERE / "rootfs/ram-only.conf").read_text()
                         + "\n" + storage.PROFILE.read_text()
                         + f"\nport={NATIVE_PORT}\n")
    with (DATA / "native.log").open("wb") as log:
        args = [str(postgres.NATIVE / "bin/postgres"), "-D", str(NATIVE_DATA)]
        if cpu is not None:
            args = ["taskset", "--cpu-list", str(cpu), *args]
        started = time.perf_counter_ns()
        process = subprocess.Popen(args,
                                   env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        try:
            ready(process, NATIVE_PORT, env, log.name)
            yield process, time.perf_counter_ns() - started
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
            try:
                code = process.wait(timeout=15)
            except subprocess.TimeoutExpired as error:
                for pid in reversed(postgres.descendants(process.pid)):
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        print(f"Cleanup: owned native process {pid} already exited", file=sys.stderr)
                process.kill()
                process.wait(timeout=5)
                raise RuntimeError("native baseline shutdown deadline exceeded") from error
            if code != 0:
                raise RuntimeError(f"native baseline exited {code}; inspect {log.name}")


@contextmanager
def native(env):
    with native_server(env):
        with postgres.Session("native", env, DATA, NATIVE_PORT) as session:
            yield session


def expected_checksum():
    rows = [(hashlib.md5(str(i).encode()).hexdigest() * 4, i) for i in range(1, ROWS + 1)]
    rows.sort()
    digest = hashlib.md5()
    for position, (value, i) in enumerate(rows):
        if position:
            digest.update(b"\n")
        digest.update(f"{i}:{value}".encode())
    return digest.hexdigest()


def validate_spill(plan):
    nodes = []
    pending = [plan[0]["Plan"]]
    while pending:
        node = pending.pop()
        nodes.append(node)
        pending.extend(node.get("Plans", []))
    sorts = [node for node in nodes if node.get("Node Type") == "Sort"
             and node.get("Sort Space Type") == "Disk"
             and node.get("Sort Space Used", 0) > 0
             and node.get("Actual Rows") == ROWS]
    if not sorts or not any(node.get("Temp Read Blocks", 0) > 0
                            and node.get("Temp Written Blocks", 0) > 0 for node in nodes):
        raise RuntimeError("external disk sort and actual temp read/write I/O were not measured")
    return {"sort_space_kib": max(node["Sort Space Used"] for node in sorts),
            "temp_read_blocks": max(node.get("Temp Read Blocks", 0) for node in nodes),
            "temp_written_blocks": max(node.get("Temp Written Blocks", 0) for node in nodes)}


def spill(session, name, expected):
    for setting, value in (("fsync", "on"), ("synchronous_commit", "on"),
                           ("wal_sync_method", "fsync"), ("block_size", "8192")):
        postgres.expect(session, f"SHOW {setting};", [value])
    session.execute("SET work_mem='64kB'; " + FIXTURE, timeout=60)
    plan = json.loads("\n".join(session.execute(
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) " + ORDERED + ";", timeout=60)))
    runtime.write_json(DATA / f"{name}-explain.json", plan)
    evidence = validate_spill(plan)
    checksum = session.execute(CHECKSUM, timeout=60)
    if checksum != [expected]:
        raise RuntimeError(f"{name} ordered-result checksum mismatch: {checksum}")
    postgres.expect(session, "SELECT count(*),min(id),max(id),"
                  "count(*) FILTER (WHERE value != repeat(md5(id::text),4)) FROM spill_fixture;",
                  [f"{ROWS}|1|{ROWS}|0"])
    evidence.update(rows=ROWS, ordered_checksum=checksum[0])
    return evidence


def temp_files(root):
    found = []
    def fail(error):
        raise error
    for directory, _, files in os.walk(root, onerror=fail, followlinks=False):
        if Path(directory).name == "pgsql_tmp":
            found.extend(str(Path(directory) / name) for name in files)
    return found


def dsm_generations():
    found = {}
    for path in sorted((storage.STORAGE / "pgdata/pg_dynshmem").iterdir()):
        if not path.name.startswith("mmap."):
            raise RuntimeError(f"unexpected DSM entry: {path}")
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size != 64:
            raise RuntimeError(f"not a bounded regular guest DSM descriptor: {path}")
        desc = path.read_bytes()
        if len(desc) != 64 or desc[:8] != b"100MSDLH" or desc[56] != 0:
            raise RuntimeError(f"malformed guest DSM descriptor: {path}")
        generation = desc[24:56].decode("ascii")
        if len(generation) != 32 or any(c not in "0123456789abcdef" for c in generation):
            raise RuntimeError(f"invalid DSM generation: {path}")
        found[path.name] = generation
    return found


def audit_snapshot(path, allow_partial=False):
    content = path.read_bytes()
    complete, _, tail = content.rpartition(b"\n")
    if tail and not allow_partial:
        raise RuntimeError(f"clean host left an incomplete audit record: {path}")
    return [json.loads(line) for line in complete.splitlines()], len(tail)


class VM:
    def __init__(self, name, env, output=DATA, cpu=None, profile=None):
        self.name = name
        self.env = env
        self.output = output
        self.cpu = cpu
        self.profile = profile
        self.identity = secrets.token_hex(16)
        self.audit = output / f"{name}-audit-{self.identity}.jsonl"
        self.log_path = output / f"{name}-guest.log"
        self.process = None
        self.crashed = False
        self.peak_rss = 0
        self.monitor_error = None
        self.done = threading.Event()

    def sample(self):
        try:
            while not self.done.wait(0.1):
                self.peak_rss = max(self.peak_rss, postgres.rss_kib(self.host_pid))
                if self.peak_rss > RSS_LIMIT:
                    raise RuntimeError("VM host exceeded the 1 GiB RSS test limit")
        except FileNotFoundError:
            if not self.done.is_set():
                self.monitor_error = "VM host exited before resource monitoring stopped"
        except (OSError, RuntimeError, StopIteration) as error:
            self.monitor_error = str(error)
            os.kill(self.host_pid, signal.SIGKILL)

    def __enter__(self):
        free_ports(postgres.PORT, postgres.CONTROL_PORT)
        self.log = self.log_path.open("wb")
        args = [*storage.command("/pg-init", self.audit, self.identity, self.cpu), str(FAULT)]
        if self.profile is not None:
            args.append(str(self.profile))
        started = time.perf_counter_ns()
        self.process = subprocess.Popen(
            args,
            cwd=runtime.WORK, stdin=subprocess.DEVNULL, stdout=self.log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 5
            while True:
                matches = [pid for pid in [self.process.pid, *postgres.descendants(self.process.pid)]
                           if Path(f"/proc/{pid}/exe").exists()
                           and Path(f"/proc/{pid}/exe").resolve() == storage.DATA / "storage-host"]
                if len(matches) == 1:
                    break
                if len(matches) > 1 or self.process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"cannot identify the single owned storage VM host; inspect {self.log_path}")
                time.sleep(0.01)
            self.host_pid = matches[0]
            self.peak_rss = postgres.rss_kib(self.host_pid)
            self.monitor = threading.Thread(target=self.sample)
            self.monitor.start()
            ready(self.process, postgres.PORT, self.env, self.log_path)
            self.startup_ns = time.perf_counter_ns() - started
            generations = dsm_generations()
            if not generations or set(generations.values()) != {self.identity}:
                raise RuntimeError("fresh VM retained stale DSM addresses/generations")
            return self
        except (RuntimeError, OSError, subprocess.SubprocessError):
            self.__exit__(None, None, None)
            raise

    def session(self, suffix="sql"):
        return postgres.Session(f"{self.name}-{suffix}", self.env, self.output)

    def crash(self):
        self.done.set()
        self.monitor.join(timeout=2)
        os.kill(self.host_pid, signal.SIGKILL)
        self.crashed = True
        self.process.wait(timeout=5)
        if self.process.returncode == 0:
            raise RuntimeError("owned SIGKILL unexpectedly reported successful host exit")
        with storage.ownership(storage.STORAGE):
            if not (storage.STORAGE / "pgdata/postmaster.pid").exists():
                raise RuntimeError("owned-host crash did not leave the expected postmaster lockfile")
            stale = dsm_generations()
            if not stale or set(stale.values()) != {self.identity}:
                raise RuntimeError("crash did not preserve testable stale DSM descriptors")
        return {"host_exit": self.process.returncode, "stale_descriptors": len(stale),
                "stale_generation": self.identity}

    def __exit__(self, *_):
        self.done.set()
        if hasattr(self, "monitor"):
            self.monitor.join(timeout=2)
        try:
            if not self.crashed:
                code = postgres.stop_guest(self.process, self.identity)
                runtime.validate_result("postmaster-shutdown", self.log_path.read_text(), code)
            if self.monitor_error:
                raise RuntimeError(self.monitor_error)
        finally:
            self.log.close()
        with storage.ownership(storage.STORAGE):
            used = postgres.bytes_in(storage.STORAGE)
        rows, tail = audit_snapshot(self.audit, allow_partial=self.crashed)
        peak_storage = max([used, *(r["storage_bytes"] for r in rows)])
        if peak_storage > postgres.BUDGET:
            raise RuntimeError("VM storage exceeded the 512 MiB budget")
        self.result = {"audit": str(self.audit), "peak_host_rss_kib": self.peak_rss,
                       "peak_storage_bytes": peak_storage, "final_storage_bytes": used,
                       "successful_syncs": sum(r["ok"] and r["operation"].startswith("sync") for r in rows),
                       "interrupted_audit_tail_bytes": tail}
        if not self.crashed and (storage.STORAGE / "pgdata/postmaster.pid").exists():
            raise RuntimeError("clean VM shutdown left a postmaster lockfile")


def lifecycle(vm):
    with vm.session("observer") as observer:
        victim = vm.session("victim")
        try:
            victim.execute("BEGIN; INSERT INTO recovery_gate VALUES (901);")
            postgres.expect(observer, "SELECT count(*) FROM recovery_gate WHERE id=901;", ["0"])
            pid = int(victim.execute("SELECT pg_backend_pid();")[0])
            postgres.expect(observer, f"SELECT pg_terminate_backend({pid},5000);", ["t"])
            try:
                victim.execute("SELECT 1;")
            except RuntimeError as error:
                if "psql exited during SQL" not in str(error):
                    raise
            else:
                raise RuntimeError("terminated backend still accepted SQL")
            if "terminating connection due to administrator command" not in Path(victim.log.name).read_text():
                raise RuntimeError("backend termination was not explicitly reported to the client")
        finally:
            victim.disconnect()
        postgres.expect(observer, "SELECT count(*) FROM recovery_gate WHERE id=901;", ["0"])
        for i in range(12):
            with vm.session(f"reconnect-{i}") as client:
                postgres.expect(client, "SELECT count(*) FROM recovery_gate;", ["8"])
        disconnected = vm.session("disconnect")
        try:
            disconnected.execute("BEGIN; INSERT INTO recovery_gate VALUES (902);")
        finally:
            disconnected.disconnect()
        with vm.session("after-disconnect") as client:
            postgres.expect(client, "SELECT count(*) FROM recovery_gate WHERE id=902;", ["0"])
    return {"reconnects": 12, "backend_termination_rollback": True, "disconnect_rollback": True}


def assert_recovered(vm, expected, stale=None, commits=16, require_redo=True):
    with vm.session() as session:
        postgres.expect(session, "SELECT id FROM recovery_gate ORDER BY id;", list(map(str, range(1, commits + 1))))
        postgres.expect(session, "SELECT pg_is_in_recovery();", ["f"])
        if session.execute(CHECKSUM, timeout=60) != [expected]:
            raise RuntimeError("spill fixture changed across a fresh VM restart")
    if stale is not None:
        text = vm.log_path.read_text()
        for marker in ("database system was interrupted", "automatic recovery in progress",
                       "checkpoint starting: end-of-recovery"):
            if marker not in text:
                raise RuntimeError(f"crash recovery evidence missing: {marker}")
        replayed = "redo starts at" in text and "redo done at" in text
        if not replayed and (require_redo or "redo is not required" not in text):
            raise RuntimeError("required WAL replay/recovery completion was not observed")
        if stale in dsm_generations().values():
            raise RuntimeError("stale guest-address DSM survived recovery")
        rows, _ = audit_snapshot(vm.audit, allow_partial=True)
        if not any(r["ok"] and r["operation"] == "stale_guest_lock" for r in rows):
            raise RuntimeError("stale guest lock cleanup was not observed under host ownership")
        if not any(r["ok"] and r["operation"].startswith("sync")
                   and r["path"].startswith("pgdata/pg_wal/") for r in rows):
            raise RuntimeError("recovery WAL synchronization was not observed")


def fault_test(operation, env, expected, results):
    with VM(f"fault-{operation}", env) as vm:
        assert_recovered(vm, expected)
        with vm.session("prepare") as session:
            session.execute("CHECKPOINT;")
        temporary = FAULT.with_suffix(".new")
        runtime.write_json(temporary, {"operation": operation})
        temporary.replace(FAULT)
        failed = subprocess.run(
            [*psql_args(), "-c", f"BEGIN; INSERT INTO fault_gate VALUES ('{operation}'); COMMIT;"],
            env=env, capture_output=True, text=True, timeout=20)
        (DATA / f"fault-{operation}-client.log").write_text(failed.stdout + failed.stderr)
        if failed.returncode == 0 or "COMMIT" in failed.stdout:
            raise RuntimeError(f"{operation} failure was acknowledged as a successful commit")
        message = "could not fsync file" if operation == "sync" else "could not write to log file"
        errno_text = "Quota exceeded" if operation == "quota" else "I/O error"
        if "PANIC:" not in failed.stderr or message not in failed.stderr or errno_text not in failed.stderr:
            raise RuntimeError(f"{operation} did not propagate the expected PostgreSQL WAL error")
        rows, _ = audit_snapshot(vm.audit, allow_partial=True)
        injected = [r for r in rows if r["operation"] == f"fault_{operation}" and not r["ok"]
                    and r["path"].startswith("pgdata/pg_wal/")]
        if len(injected) != 1 or FAULT.exists() or not failed.stderr.strip():
            raise RuntimeError(f"{operation} fault was not injected/consumed and reported exactly once")
        crash = vm.crash()
    results[operation] = dict(vm.result, client_exit=failed.returncode,
                             commit_acknowledged=False, crash=crash)
    with VM(f"recover-{operation}", env) as recovered:
        assert_recovered(recovered, expected, vm.identity, require_redo=False)
        with recovered.session("outcome") as session:
            # A sync error is an unknown commit outcome, not proof of rollback.
            outcome = session.execute(f"SELECT count(*) FROM fault_gate WHERE id='{operation}';")
            if outcome not in (["0"], ["1"]) or (operation != "sync" and outcome != ["0"]):
                raise RuntimeError(f"{operation} fault recovery produced an unexpected transaction outcome")
            results[operation]["rows_after_recovery"] = int(outcome[0])
            session.execute("CHECKPOINT;")
    results[operation]["recovery"] = recovered.result


def test():
    storage.verify()
    postgres.verify_postgres_install(False)
    meta = json.loads((storage.DATA / "storage.json").read_text())
    for field, path in (("config_sha256", storage.STORAGE / "pgdata/postgresql.conf"),
                        ("hba_sha256", storage.STORAGE / "pgdata/pg_hba.conf"),
                        ("profile_sha256", storage.PROFILE)):
        if meta.get(field) != runtime.digest(path):
            raise RuntimeError("persistent configuration changed; run storage-config")
    if FAULT.exists() or FAULT.with_suffix(".new").exists():
        raise RuntimeError("unconsumed WAL fault control; inspect before restarting")
    result = {"status": "running", "fsync": True, "fault_model": "owned process SIGKILL; one-shot WAL EIO/EDQUOT"}
    runtime.write_json(DATA / "results.json", result)
    env = environment()
    expected = expected_checksum()
    try:
        with native(env) as session:
            result["native_spill"] = spill(session, "native", expected)
        if temp_files(NATIVE_DATA) or postgres.bytes_in(NATIVE_DATA) > postgres.BUDGET:
            raise RuntimeError("native spill left temp files or exceeded storage budget")
        with VM("spill", env) as vm:
            with vm.session() as session:
                for name in ("fsync", "synchronous_commit"):
                    postgres.expect(session, f"SHOW {name};", ["on"])
                result["guest_spill"] = spill(session, "guest", expected)
                session.execute("DROP TABLE IF EXISTS recovery_gate; CREATE TABLE recovery_gate(id int primary key);"
                                "DROP TABLE IF EXISTS fault_gate; CREATE TABLE fault_gate(id text primary key);")
                for i in range(1, 9):
                    session.execute(f"BEGIN; INSERT INTO recovery_gate VALUES ({i}); COMMIT;")
                session.execute("BEGIN; INSERT INTO recovery_gate VALUES (900); ROLLBACK; CHECKPOINT;")
                result["backend_memory_bytes"] = int(session.execute(
                    "SELECT sum(total_bytes) FROM pg_backend_memory_contexts;")[0])
            if temp_files(storage.STORAGE / "pgdata"):
                raise RuntimeError("guest external sort left temporary files")
            result["lifecycle"] = lifecycle(vm)
        result["spill_vm"] = vm.result
        with VM("graceful-restart", env) as restarted:
            assert_recovered(restarted, expected, commits=8)
            with restarted.session() as session:
                session.execute("CHECKPOINT;")
        result["graceful_restart"] = restarted.result
        with VM("crash", env) as crashed:
            assert_recovered(crashed, expected, commits=8)
            session = crashed.session("uncommitted")
            try:
                session.execute("CHECKPOINT;")
                for i in range(9, 17):
                    session.execute(f"BEGIN; INSERT INTO recovery_gate VALUES ({i}); COMMIT;")
                result["acknowledged_commits"] = list(range(1, 17))
                session.execute("BEGIN; INSERT INTO recovery_gate VALUES (903);")
                active = subprocess.Popen(
                    [*psql_args(), "-c", "SELECT pg_sleep(30);"], env=env,
                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                try:
                    deadline = time.monotonic() + 5
                    while True:
                        session.execute("SELECT pg_stat_clear_snapshot();")
                        if session.execute("SELECT count(*) FROM pg_stat_activity "
                                           "WHERE query='SELECT pg_sleep(30);' AND state='active';") == ["1"]:
                            break
                        if time.monotonic() >= deadline:
                            raise RuntimeError("crash workload did not become active")
                        time.sleep(0.05)
                    result["crash"] = crashed.crash()
                finally:
                    if active.poll() is None:
                        active.kill()
                    active.communicate(timeout=5)
            finally:
                session.disconnect()
        result["crash_vm"] = crashed.result
        with VM("crash-recovery", env) as recovered:
            assert_recovered(recovered, expected, crashed.identity)
            with recovered.session() as session:
                session.execute("CHECKPOINT;")
        result["crash_recovery"] = recovered.result
        result["faults"] = {}
        for operation in ("write", "sync", "quota"):
            fault_test(operation, env, expected, result["faults"])
        if temp_files(storage.STORAGE / "pgdata"):
            raise RuntimeError("recovery left spill temp files")
        result["inputs"] = {
            "phase4_sha256": runtime.digest(runtime.HERE / "recovery.py"),
            "host_sha256": runtime.digest(storage.DATA / "storage-host"),
            "rootfs_sha256": runtime.digest(storage.DATA / "postgres.cpio"),
            "native_postgres_sha256": runtime.digest(postgres.NATIVE / "bin/postgres"),
            "native_psql_sha256": runtime.digest(postgres.NATIVE / "bin/psql"),
        }
        result["status"] = "passed"
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        result.update(status="failed", error=str(error))
        raise
    finally:
        runtime.write_json(DATA / "results.json", result)
    print("PASS recovery: measured native/guest spill, lifecycle, fresh-VM restart/recovery, WAL faults")


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
