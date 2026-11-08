#!/usr/bin/env python3
"""Bounded host-backed PostgreSQL and observed storage-boundary validation."""

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import time

import phase1
import phase2

HERE = phase1.HERE
WORK = phase1.WORK
DATA = WORK / "phase3"
STORAGE = DATA / "storage"
ROOTFS = DATA / "rootfs"
PROFILE = HERE / "rootfs/persistent.conf"


@contextmanager
def ownership(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError(f"storage is already owned by another experiment: {path}") from error
        yield fd
    finally:
        os.close(fd)


def inputs():
    paths = [HERE / "phase1.py", HERE / "phase2.py", HERE / "phase3.py",
             HERE / "Makefile", HERE / "storage-host.rs", HERE / "sources.toml",
             HERE / "tests/storage.c", HERE / "rootfs/init.c", HERE / "rootfs/kernel.conf", PROFILE]
    for name in (*phase1.PATCH_TARGETS, "postgres"):
        paths.extend(phase1.patch_files(name))
        series = HERE / "patches" / name / "series"
        if series.exists():
            paths.append(series)
    return {str(path): phase1.digest(path) for path in paths}


def build():
    phase1.build(HERE / "rootfs/kernel.conf", DATA)
    source = phase1.UPSTREAM / "examples/postgres_storage.rs"
    shutil.copy2(HERE / "storage-host.rs", source)
    env = dict(os.environ, CARGO_BUILD_JOBS="1",
               CARGO_TARGET_DIR=str(WORK / "cargo"), CARGO_PROFILE_DEV_DEBUG="0")
    phase1.run(["cargo", "build", "--locked", "--example", "postgres_storage"],
               cwd=phase1.UPSTREAM, env=env)
    shutil.copy2(WORK / "cargo/debug/examples/postgres_storage", DATA / "storage-host")
    phase1.run(["cargo", "test", "--locked", "--lib", "hostfs::tests"], cwd=phase1.UPSTREAM, env=env)
    phase1.run(["cargo", "test", "--locked", "--example", "postgres_storage"], cwd=phase1.UPSTREAM, env=env)
    phase1.write_json(DATA / "host.json", {
        "inputs": inputs(), "host_sha256": phase1.digest(DATA / "storage-host"),
        "kernel_sha256": phase1.digest(DATA / "kernel-build/elfloader_hyperlight-x86_64"),
    })


def package():
    host = json.loads((DATA / "host.json").read_text())
    if host["inputs"] != inputs():
        raise RuntimeError("storage build inputs changed; run storage-build")
    phase2.prepare_sources()
    phase2.verify_postgres_install(False)
    phase2.verify_postgres_install(True)
    if ROOTFS.exists():
        if not (DATA / "build.json").exists():
            raise RuntimeError("unknown partial Phase 3 rootfs; inspect before cleanup")
        shutil.rmtree(ROOTFS)
    ROOTFS.mkdir()
    for directory in ("usr/local/bin", "usr/local/lib/postgresql", "usr/local/share",
                      "lib", "etc", "tmp", "dev", "proc/self", "storage", "readonly"):
        (ROOTFS / directory).mkdir(parents=True, exist_ok=True)
    installed = phase2.DATA / "guest-install/usr/local"
    shutil.copy2(installed / "bin/postgres", ROOTFS / "usr/local/bin/postgres")
    for directory in ("lib/postgresql", "share/postgresql"):
        shutil.copytree(installed / directory, ROOTFS / "usr/local" / directory,
                        dirs_exist_ok=True, symlinks=False,
                        ignore=shutil.ignore_patterns("pgxs", "*.a"))
    shutil.copy2(Path("/lib/ld-musl-x86_64.so.1").resolve(), ROOTFS / "lib/ld-musl-x86_64.so.1")
    (ROOTFS / "proc/self/maps").symlink_to("/dev/maps")
    (ROOTFS / "etc/ld-musl-x86_64.path").write_text("/lib:/usr/local/lib:/usr/local/lib/postgresql\n")
    for source, output in ((HERE / "rootfs/init.c", "pg-init"), (HERE / "tests/storage.c", "storage-probe")):
        phase1.run(["musl-gcc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                    "-march=x86-64", "-fPIE", "-pie", "-pthread", source, "-o", ROOTFS / output])
    names = sorted(str(path.relative_to(ROOTFS)) for path in ROOTFS.rglob("*"))
    with (DATA / "postgres.cpio").open("wb") as archive:
        subprocess.run(["cpio", "-o", "-H", "newc", "--reproducible", "--owner=1000:1000"],
                       cwd=ROOTFS, input=("\n".join(names) + "\n").encode(),
                       stdout=archive, check=True)
    phase1.write_json(DATA / "build.json", {
        "inputs": inputs(), "host_sha256": host["host_sha256"],
        "kernel_sha256": host["kernel_sha256"],
        "rootfs_sha256": phase1.digest(DATA / "postgres.cpio"),
        "postgres_sha256": phase1.digest(ROOTFS / "usr/local/bin/postgres"),
    })


def initialize():
    if STORAGE.exists():
        raise RuntimeError("persistent storage already exists; never overwrite PGDATA")
    STORAGE.mkdir(mode=0o700)
    with ownership(STORAGE):
        password = DATA / "password"
        password.write_text(secrets.token_urlsafe(32) + "\n")
        password.chmod(0o600)
        phase2.check_version(phase1.run([phase2.NATIVE / "bin/postgres", "--version"], capture=True))
        phase1.run(["timeout", "--kill-after=5s", "60s", phase2.NATIVE / "bin/initdb",
                    "-D", STORAGE / "pgdata", "--username=phase2", "--encoding=UTF8",
                    "--locale=C", "--auth-local=reject", "--auth-host=scram-sha-256",
                    f"--pwfile={password}"])
        with (STORAGE / "pgdata/postgresql.conf").open("a") as config:
            config.write("\n" + (HERE / "rootfs/ram-only.conf").read_text() + "\n" + PROFILE.read_text())
        (STORAGE / "pgdata/pg_hba.conf").write_text(
            "host all all 127.0.0.1/32 scram-sha-256\nhost all all 0.0.0.0/0 reject\nhost all all ::0/0 reject\n")
        probes = STORAGE / "probes"
        (probes / "many").mkdir(parents=True)
        (probes / "fail-dir-sync").mkdir()
        for i in range(5000):
            (probes / "many" / f"entry-{i:04}").touch()
        phase1.write_json(DATA / "storage.json", {"initialized": True, "budget": phase2.BUDGET})
        write_storage_config()


def write_storage_config():
    if (STORAGE / "pgdata/postmaster.pid").exists():
        raise RuntimeError("cannot reconfigure a live or stale PostgreSQL data directory")
    config = STORAGE / "pgdata/postgresql.conf"
    marker = "\n# --- hlps Phase 3 managed profile ---\n"
    existing = config.read_text().split(marker, 1)[0]
    config.write_text(existing + marker + (HERE / "rootfs/ram-only.conf").read_text()
                      + "\n" + PROFILE.read_text())
    meta = json.loads((DATA / "storage.json").read_text())
    meta.update(config_sha256=phase1.digest(config),
                profile_sha256=phase1.digest(PROFILE),
                hba_sha256=phase1.digest(STORAGE / "pgdata/pg_hba.conf"))
    phase1.write_json(DATA / "storage.json", meta)


def configure():
    with ownership(STORAGE):
        write_storage_config()


def command(entry, audit, identity, cpu=None):
    args = ["timeout", "--kill-after=5s", "120s", DATA / "storage-host",
            DATA / "postgres.cpio", STORAGE, audit, entry, identity]
    if cpu is not None:
        args = ["taskset", "--cpu-list", str(cpu), *args]
    if not (os.access("/dev/kvm", os.R_OK) and os.access("/dev/kvm", os.W_OK)):
        if not shutil.which("setpriv"):
            raise RuntimeError("setpriv is required to drop root before the persistent VM host")
        args = ["sudo", "-n", "setpriv", f"--reuid={os.getuid()}", f"--regid={os.getgid()}",
                f"--groups={os.stat('/dev/kvm').st_gid}", *args]
    return list(map(str, args))


def verify():
    phase1.verify_upstream()
    phase1.prepare_patches()
    phase2.prepare_sources()
    meta = json.loads((DATA / "build.json").read_text())
    if meta["inputs"] != inputs():
        raise RuntimeError("Phase 3 build inputs changed; rebuild/repackage")
    for field, path in (("host_sha256", DATA / "storage-host"),
                        ("kernel_sha256", DATA / "kernel-build/elfloader_hyperlight-x86_64"),
                        ("rootfs_sha256", DATA / "postgres.cpio"),
                        ("postgres_sha256", ROOTFS / "usr/local/bin/postgres")):
        if meta[field] != phase1.digest(path):
            raise RuntimeError(f"unverified storage artifact: {path}")


def audit_rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def validate_audit(rows, postgres):
    def observed(operation, path, ok):
        return any(row["operation"] == operation and row["path"] == path
                   and row["ok"] == ok for row in rows)
    if postgres:
        if not any(row["operation"] == "sync_all" and row["ok"] and
                   row["path"].startswith("pgdata/pg_wal/") for row in rows):
            raise RuntimeError("PostgreSQL WAL synchronization not observed")
        if not observed("sync_all", "pgdata/pg_logical", True):
            raise RuntimeError("PostgreSQL subdirectory synchronization not observed")
    else:
        for operation, path, ok in (
                ("sync_all", "probes", True),
                ("sync_all", "probes/sync-write", True),
                ("sync_all", "probes/dsync-write", True),
                ("sync_all", "probes/fail-sync", False),
                ("sync_all", "probes/fail-dir-sync", False),
                ("write", "probes/partial-write", False),
                ("write", "probes/quota", False)):
            if not observed(operation, path, ok):
                raise RuntimeError(f"missing storage-boundary evidence: {operation} {path} {ok}")
        pages = [r for r in rows if r["operation"] == "list_page" and r["path"] == "probes/many"]
        if len(pages) < 40 or not all(r["ok"] for r in pages):
            raise RuntimeError("large-directory pagination not observed")


def sql_test():
    for port in (phase2.PORT, phase2.CONTROL_PORT):
        with socket.socket() as reservation:
            reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            reservation.bind(("127.0.0.1", port))
    audit = DATA / f"postgres-audit-{secrets.token_hex(4)}.jsonl"
    identity = secrets.token_hex(16)
    pgpass = DATA / "pgpass"
    pgpass.write_text(f"127.0.0.1:{phase2.PORT}:postgres:phase2:{(DATA / 'password').read_text().strip()}\n")
    pgpass.chmod(0o600)
    env = {key: value for key, value in os.environ.items() if not key.startswith("PG")}
    env.update(PGPASSFILE=str(pgpass), PGCONNECT_TIMEOUT="2", PGAPPNAME="phase3")
    with (DATA / "guest.log").open("wb") as log:
        process = subprocess.Popen(command("/pg-init", audit, identity), cwd=WORK,
                                   stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 30
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"PostgreSQL guest exited {process.returncode}; inspect {DATA / 'guest.log'}")
                ready = subprocess.run([str(phase2.NATIVE / "bin/pg_isready"), "-h", "127.0.0.1",
                                        "-p", str(phase2.PORT), "-U", "phase2", "-d", "postgres", "-t", "1"],
                                       env=env, capture_output=True, timeout=3)
                if ready.returncode == 0:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("persistent PostgreSQL startup deadline exceeded")
                time.sleep(0.1)
            denied = subprocess.run([sys.executable, str(HERE / "phase3.py"), "lock-probe"],
                                    capture_output=True, text=True, timeout=5)
            if denied.returncode == 0 or "already owned" not in denied.stderr:
                raise RuntimeError("the running VM host did not retain exclusive storage ownership")
            competing_audit = DATA / f"competing-audit-{secrets.token_hex(4)}.jsonl"
            competing = subprocess.run(command("/storage-probe", competing_audit, secrets.token_hex(16)),
                                       capture_output=True, text=True, timeout=5)
            if competing.returncode == 0 or competing_audit.exists() or \
                    not any(word in competing.stderr.lower() for word in ("wouldblock", "would block")):
                raise RuntimeError("a competing VM host was not rejected before opening storage")
            with phase2.Session("storage", env, DATA) as session:
                for name, value in (("fsync", "on"), ("synchronous_commit", "on"), ("wal_sync_method", "fsync")):
                    phase2.expect(session, f"SHOW {name};", [value])
                session.execute("DROP TABLE IF EXISTS storage_gate; CREATE TABLE storage_gate(id int primary key);"
                                "BEGIN; INSERT INTO storage_gate VALUES (42); COMMIT; CHECKPOINT;")
                phase2.expect(session, "SELECT id FROM storage_gate;", ["42"])
                phase2.expect(session, "SELECT count(*) FROM pg_stat_activity WHERE backend_type IN "
                              "('checkpointer','background writer','walwriter');", ["3"])
        finally:
            code = phase2.stop_guest(process, identity)
    output = (DATA / "guest.log").read_text()
    phase1.validate_result("postmaster-shutdown", output, code)
    if (STORAGE / "pgdata/postmaster.pid").exists():
        raise RuntimeError("postmaster lockfile left after clean shutdown")
    rows = audit_rows(audit)
    validate_audit(rows, True)
    if phase2.bytes_in(STORAGE) > phase2.BUDGET:
        raise RuntimeError("live storage budget exceeded")
    return {"audit": str(audit), "successful_syncs": sum(r["operation"].startswith("sync") and r["ok"] for r in rows),
            "storage_bytes": phase2.bytes_in(STORAGE), "guest_exit": code}


def test():
    verify()
    storage_meta = json.loads((DATA / "storage.json").read_text())
    if not storage_meta["initialized"]:
        raise RuntimeError("storage initialization is incomplete")
    for field, path in (("config_sha256", STORAGE / "pgdata/postgresql.conf"),
                        ("hba_sha256", STORAGE / "pgdata/pg_hba.conf"),
                        ("profile_sha256", PROFILE)):
        if storage_meta.get(field) != phase1.digest(path):
            raise RuntimeError("persistent configuration changed; use storage-config explicitly")
    result = {"status": "running"}
    phase1.write_json(DATA / "results.json", result)
    with ownership(STORAGE):
        for name in ("io", "renamed", "sync-write", "dsync-write", "fail-sync",
                     "partial-write", "quota", "mode-dir"):
            path = STORAGE / "probes" / name
            if path.is_dir():
                path.rmdir()
            elif path.exists():
                path.unlink()
    # The host independently locks the directory before opening the backend.
    try:
        audit = DATA / f"probe-audit-{secrets.token_hex(4)}.jsonl"
        completed = subprocess.run(command("/storage-probe", audit, secrets.token_hex(16)),
                                   cwd=WORK, capture_output=True, text=True, timeout=130)
        (DATA / "probes.log").write_text(completed.stdout + completed.stderr)
        print(completed.stdout, end="")
        phase1.validate_result("storage", completed.stdout, completed.returncode)
        validate_audit(audit_rows(audit), False)
        result["probe_audit"] = str(audit)
        result["postgres"] = sql_test()
        result["second_writer_rejected"] = True
        result["status"] = "passed"
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        result.update(status="failed", error=str(error))
        raise
    finally:
        phase1.write_json(DATA / "results.json", result)
    print("PASS phase3: observed syncs/errors, paginated directory, single writer, host-backed PostgreSQL")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "package", "init", "configure", "test", "lock-probe"))
    args = parser.parse_args()
    DATA.mkdir(parents=True, exist_ok=True)
    if args.command == "lock-probe":
        with ownership(STORAGE):
            print("storage ownership acquired")
        return
    with (WORK / "phase1.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        {"build": build, "package": package, "init": initialize,
         "configure": configure, "test": test}[args.command]()


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
