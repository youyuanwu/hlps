#!/usr/bin/env python3
"""PostgreSQL 16.15 RAM-only guest build and two-session SQL gate."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import secrets
import selectors
import shutil
import socket
import subprocess
import sys
import tarfile
import time

import phase1

HERE = phase1.HERE
WORK = phase1.WORK
DATA = WORK / "phase2"
NATIVE_SOURCE = WORK / "sources/postgres-native"
GUEST_SOURCE = WORK / "sources/postgres-guest"
NATIVE = WORK / "native-pg"
ROOTFS = DATA / "rootfs"
PROFILE = HERE / "rootfs/ram-only.conf"
PORT = 55432
CONTROL_PORT = 55433
BUDGET = 512 * 1024**2
FLAGS = ["--without-icu", "--without-readline", "--without-zlib",
         "--without-lz4", "--without-zstd", "--without-openssl"]


def prepare_sources():
    spec = phase1.pins()["postgres"]
    archive = WORK / "sources" / f"postgresql-{spec['version']}.tar.bz2"
    if phase1.digest(archive) != spec["sha256"]:
        raise RuntimeError("PostgreSQL source archive checksum mismatch")
    for source in (NATIVE_SOURCE, GUEST_SOURCE):
        if not source.exists():
            source.mkdir()
            with tarfile.open(archive) as tar:
                prefix = f"postgresql-{spec['version']}/"
                for member in tar.getmembers():
                    if member.name == prefix[:-1]:
                        continue
                    if not member.name.startswith(prefix):
                        raise RuntimeError("Unexpected PostgreSQL archive layout")
                    member.name = member.name[len(prefix):]
                    tar.extract(member, source, filter="data")
        if not (source / ".git").exists():
            phase1.run(["git", "init", "-q"], cwd=source)
            phase1.run(["git", "add", "."], cwd=source)
            phase1.run(["git", "-c", "user.name=Experiment source import",
                        "-c", "user.email=experiment@localhost", "commit", "-q",
                        "-m", "Import verified PostgreSQL 16.15 source archive",
                        "-m", "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"],
                       cwd=source)
    native_tree = phase1.run(["git", "rev-parse", "HEAD^{tree}"],
                            cwd=NATIVE_SOURCE, capture=True).strip()
    guest_tree = phase1.run(["git", "rev-parse", "HEAD^{tree}"],
                           cwd=GUEST_SOURCE, capture=True).strip()
    if native_tree != guest_tree:
        raise RuntimeError("Native and guest source baselines differ")
    phase1.run(["git", "diff", "--exit-code", "HEAD"], cwd=NATIVE_SOURCE)
    phase1.prepare_patch_target("postgres", GUEST_SOURCE)
    return native_tree


def configure_args(guest):
    source = GUEST_SOURCE if guest else NATIVE_SOURCE
    prefix = "/usr/local" if guest else str(NATIVE)
    cflags = "-O2 -std=gnu17 -march=x86-64"
    args = [source / "configure", f"--prefix={prefix}", *FLAGS, f"CFLAGS={cflags}"]
    if guest:
        args.extend(["CC=musl-gcc", "CPPFLAGS=-DEXEC_BACKEND -DHYPERLIGHT_UNIKRAFT",
                     "--disable-rpath", "--enable-cassert", "--enable-depend"])
    return args


def postgres_inputs(guest):
    source = GUEST_SOURCE if guest else NATIVE_SOURCE
    return {"configure": list(map(str, configure_args(guest)[1:])),
              "source_tree": phase1.run(["git", "rev-parse", "HEAD^{tree}"],
                                       cwd=source, capture=True).strip(),
              "patches": [{"name": p.name, "sha256": phase1.digest(p)}
                          for p in phase1.patch_files("postgres")] if guest else []}


def verify_postgres_install(guest):
    directory = WORK / ("postgres-guest-build" if guest else "postgres-native-build")
    stamp = directory / "experiment-inputs.json"
    if json.loads(stamp.read_text()) != postgres_inputs(guest):
        raise RuntimeError("PostgreSQL build inputs changed; rebuild before packaging")
    installed = (DATA / "guest-install/usr/local/bin/postgres") if guest else NATIVE / "bin/postgres"
    if phase1.digest(directory / "src/backend/postgres") != phase1.digest(installed):
        raise RuntimeError("Installed PostgreSQL does not match the verified build")


def build_postgres(guest):
    directory = WORK / ("postgres-guest-build" if guest else "postgres-native-build")
    directory.mkdir(exist_ok=True)
    args = configure_args(guest)
    config = directory / "config.status"
    expected = list(map(str, args[1:]))
    inputs = postgres_inputs(guest)
    stamp = directory / "experiment-inputs.json"
    # PostgreSQL's Makefiles do not track changes to configure/compiler flags.
    if config.exists():
        import shlex
        actual = shlex.split(phase1.run([config, "--config"], capture=True))
        changed = stamp.exists() and json.loads(stamp.read_text()) != inputs
        if actual != expected or changed or (guest and not stamp.exists()):
            phase1.run(["make", "-j1", "clean"], cwd=directory)
            phase1.run(args, cwd=directory)
    else:
        phase1.run(args, cwd=directory)
    if guest:
        install = DATA / "guest-install"
        install.mkdir(exist_ok=True)
        for target in ("src/backend", "src/timezone", "src/pl/plpgsql"):
            phase1.run(["make", "-j1", "-C", target, "all", "install",
                        f"DESTDIR={install}"], cwd=directory)
    else:
        phase1.run(["make", "-j1"], cwd=directory)
        phase1.run(["make", "-j1", "install"], cwd=directory)
    phase1.write_json(stamp, inputs)


def bytes_in(path):
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def inputs():
    paths = [HERE / "phase1.py", HERE / "phase2.py", HERE / "Makefile",
             HERE / "sources.toml", HERE / "rootfs/init.c",
             HERE / "rootfs/kernel.conf", PROFILE, HERE / "patches/postgres/series"]
    paths.extend(phase1.patch_files("postgres"))
    for name in phase1.PATCH_TARGETS:
        paths.extend(phase1.patch_files(name))
        series = HERE / "patches" / name / "series"
        if series.exists():
            paths.append(series)
    return {str(path): phase1.digest(path) for path in paths}


def check_version(output):
    expected = f"postgres (PostgreSQL) {phase1.pins()['postgres']['version']}"
    if output.strip() != expected:
        raise RuntimeError(f"Unexpected native PostgreSQL version: {output.strip()}")


def package():
    prepare_sources()
    verify_postgres_install(False)
    verify_postgres_install(True)
    kernel = json.loads((DATA / "kernel.json").read_text())
    patches = {name: [{"name": p.name, "sha256": phase1.digest(p)}
                      for p in phase1.patch_files(name)] for name in phase1.PATCH_TARGETS}
    if kernel["inputs"] != phase1.build_inputs() or \
            kernel["overlay_sha256"] != phase1.digest(HERE / "rootfs/kernel.conf") or \
            kernel["patches"] != patches:
        raise RuntimeError("Guest kernel inputs changed; run pg-kernel before packaging")
    if kernel["kernel_sha256"] != phase1.digest(DATA / "kernel-build/elfloader_hyperlight-x86_64") or \
            kernel["cli_sha256"] != phase1.digest(DATA / "hluk"):
        raise RuntimeError("Guest kernel/CLI changed since its verified build")
    check_version(phase1.run([NATIVE / "bin/postgres", "--version"], capture=True))
    if ROOTFS.exists():
        if not (DATA / "build.json").exists():
            raise RuntimeError("Unrecognized/partial rootfs; preserve it or remove this specific "
                               "generated phase2/rootfs directory before repackaging")
        shutil.rmtree(ROOTFS)
    ROOTFS.mkdir()
    for directory in ("usr/local/bin", "usr/local/lib/postgresql", "usr/local/share",
                      "lib", "etc", "tmp", "dev", "proc/self"):
        (ROOTFS / directory).mkdir(parents=True, exist_ok=True)
    installed = DATA / "guest-install/usr/local"
    shutil.copy2(installed / "bin/postgres", ROOTFS / "usr/local/bin/postgres")
    for directory in ("lib/postgresql", "share/postgresql"):
        shutil.copytree(installed / directory, ROOTFS / "usr/local" / directory,
                        dirs_exist_ok=True, symlinks=False,
                        ignore=shutil.ignore_patterns("pgxs", "*.a"))
    shutil.copy2(Path("/lib/ld-musl-x86_64.so.1").resolve(),
                 ROOTFS / "lib/ld-musl-x86_64.so.1")
    (ROOTFS / "proc/self/maps").symlink_to("/dev/maps")
    (ROOTFS / "etc/passwd").write_text("phase2:x:1000:1000:guest:/pgdata:/bin/false\n")
    (ROOTFS / "etc/group").write_text("phase2:x:1000:\n")
    (ROOTFS / "etc/ld-musl-x86_64.path").write_text("/lib:/usr/local/lib:/usr/local/lib/postgresql\n")
    phase1.run(["musl-gcc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                "-march=x86-64", "-fPIE", "-pie", "-pthread", HERE / "rootfs/init.c",
                "-o", ROOTFS / "pg-init"])
    elf = phase1.run(["readelf", "-h", "-l", ROOTFS / "usr/local/bin/postgres"], capture=True)
    if "DYN (Position-Independent Executable file)" not in elf:
        raise RuntimeError("PostgreSQL guest must be PIE")
    if "/lib/ld-musl-x86_64.so.1" not in elf:
        raise RuntimeError("PostgreSQL guest must use packaged musl")
    password = DATA / "password"
    password.write_text(secrets.token_urlsafe(32) + "\n")
    password.chmod(0o600)
    phase1.run(["timeout", "--kill-after=5s", "60s", NATIVE / "bin/initdb",
                "-D", ROOTFS / "pgdata", "--username=phase2", "--encoding=UTF8",
                "--locale=C", "--auth-local=reject", "--auth-host=scram-sha-256",
                f"--pwfile={password}", "--no-sync"])
    with (ROOTFS / "pgdata/postgresql.conf").open("a") as config:
        config.write("\n" + PROFILE.read_text())
    (ROOTFS / "pgdata/pg_hba.conf").write_text(
        "host all all 127.0.0.1/32 scram-sha-256\n"
        "host all all 0.0.0.0/0 reject\n"
        "host all all ::0/0 reject\n")
    if (ROOTFS / "pgdata/postmaster.pid").exists() or \
            any((ROOTFS / "pgdata/pg_dynshmem").iterdir()):
        raise RuntimeError("Initial rootfs contains live/stale process or DSM state")
    if bytes_in(ROOTFS / "pgdata") > BUDGET or bytes_in(ROOTFS) > BUDGET:
        raise RuntimeError("Initial rootfs exceeds the 512 MiB experiment budget")
    names = sorted(str(p.relative_to(ROOTFS)) for p in ROOTFS.rglob("*"))
    with (DATA / "postgres.cpio").open("wb") as output:
        subprocess.run(["cpio", "-o", "-H", "newc", "--reproducible", "--owner=1000:1000"],
                       cwd=ROOTFS, input=("\n".join(names) + "\n").encode(),
                       stdout=output, check=True)
    phase1.write_json(DATA / "build.json", {
        "version": phase1.pins()["postgres"]["version"],
        "inputs": inputs(),
        "archive_sha256": phase1.pins()["postgres"]["sha256"],
        "kernel_sha256": phase1.digest(DATA / "kernel-build/elfloader_hyperlight-x86_64"),
        "cli_sha256": phase1.digest(DATA / "hluk"),
        "guest_uid": 1000,
        "kernel": json.loads((DATA / "kernel.json").read_text()),
        "postgres_guest_build": postgres_inputs(True),
        "postgres_native_build": postgres_inputs(False),
        "native_initdb_sha256": phase1.digest(NATIVE / "bin/initdb"),
        "native_psql_sha256": phase1.digest(NATIVE / "bin/psql"),
        "postgres_sha256": phase1.digest(ROOTFS / "usr/local/bin/postgres"),
        "rootfs_sha256": phase1.digest(DATA / "postgres.cpio"),
        "profile_sha256": phase1.digest(PROFILE),
        "pgdata_bytes": bytes_in(ROOTFS / "pgdata"),
        "rootfs_bytes": bytes_in(ROOTFS),
        "patches": [{"name": p.name, "sha256": phase1.digest(p)}
                    for p in phase1.patch_files("postgres")],
        "profile": "RAM-only; fsync=off; no mounts or warm snapshots",
    })


class Session:
    def __init__(self, name, env):
        self.log = (DATA / f"psql-{name}.log").open("wb")
        self.process = subprocess.Popen(
            [str(NATIVE / "bin/psql"), "-X", "-qAt", "-F", "|",
             "-v", "ON_ERROR_STOP=1", "-h", "127.0.0.1", "-p", str(PORT),
             "-U", "phase2", "-d", "postgres"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, env=env)
        self.buffer = b""
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)

    def execute(self, sql):
        marker = "HL_DONE_" + secrets.token_hex(8)
        self.process.stdin.write((sql + "\n\\echo " + marker + "\n").encode())
        self.process.stdin.flush()
        lines = []
        deadline = time.monotonic() + 10
        while True:
            if b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                value = line.decode().rstrip("\r")
                if value == marker:
                    return lines
                if value:
                    lines.append(value)
                continue
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                raise RuntimeError("SQL statement exceeded its 10-second deadline")
            chunk = os.read(self.process.stdout.fileno(), 4096)
            if not chunk:
                raise RuntimeError(f"psql exited during SQL; inspect {self.log.name}")
            self.buffer += chunk
            if len(self.buffer) > 65536 or len(lines) > 1000:
                raise RuntimeError("SQL output exceeded the bounded test budget")

    def close(self):
        try:
            if self.process.poll() is None:
                self.process.stdin.write(b"\\q\n")
                self.process.stdin.flush()
            code = self.process.wait(timeout=10)
            if code != 0:
                raise RuntimeError(f"psql exited {code}; inspect {self.log.name}")
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            self.process.stdin.close()
            self.process.stdout.close()
            self.selector.close()
            self.log.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def expect(session, sql, expected):
    actual = session.execute(sql)
    if actual != expected:
        raise RuntimeError(f"SQL mismatch for {sql!r}: {actual!r}, expected {expected!r}")


def descendants(pid):
    found = []
    pending = [pid]
    while pending:
        parent = pending.pop()
        path = Path(f"/proc/{parent}/task/{parent}/children")
        try:
            children = list(map(int, path.read_text().split()))
        except FileNotFoundError:
            continue
        found.extend(children)
        pending.extend(children)
    return found


def cli_pid(process):
    for pid in descendants(process.pid):
        comm = Path(f"/proc/{pid}/comm")
        if comm.exists() and comm.read_text().strip() == "hluk":
            return pid
    raise RuntimeError("Cannot locate this test's running hluk process")


def rss_kib(pid):
    status = Path(f"/proc/{pid}/status").read_text()
    return int(next(line.split()[1] for line in status.splitlines()
                    if line.startswith("VmRSS:")))


def check_rss_growth(before, after, limit=32768):
    if after - before > limit:
        raise RuntimeError(f"Reconnect RSS growth {after - before} KiB exceeded {limit} KiB")


def stop_guest(process, identity):
    if process.poll() is None:
        try:
            with socket.create_connection(("127.0.0.1", CONTROL_PORT), timeout=2) as control:
                control.sendall((identity + "\n").encode())
            process.wait(timeout=15)
        except (OSError, subprocess.TimeoutExpired):
            if process.poll() is not None:
                return process.returncode
            owned = descendants(process.pid)
            for pid in reversed(owned):
                killed = subprocess.run(["sudo", "-n", "kill", "-KILL", str(pid)],
                                        capture_output=True, text=True)
                if killed.returncode and Path(f"/proc/{pid}").exists():
                    raise RuntimeError(f"Cannot terminate owned process {pid}: {killed.stderr}")
                if killed.returncode:
                    print(f"Cleanup: owned process {pid} already exited", file=sys.stderr)
            if process.poll() is None:
                phase1.run(["sudo", "-n", "kill", "-KILL", process.pid])
            process.wait(timeout=5)
            raise RuntimeError("Guest failed its graceful shutdown deadline")
    return process.returncode


def test():
    phase1.verify_upstream()
    phase1.prepare_patches()
    prepare_sources()
    metadata = json.loads((DATA / "build.json").read_text())
    if metadata["inputs"] != inputs():
        raise RuntimeError("Phase 2 inputs changed; rebuild/repackage before SQL tests")
    for key, path in (("kernel_sha256", DATA / "kernel-build/elfloader_hyperlight-x86_64"),
                      ("cli_sha256", DATA / "hluk"),
                      ("rootfs_sha256", DATA / "postgres.cpio"),
                      ("native_initdb_sha256", NATIVE / "bin/initdb"),
                      ("native_psql_sha256", NATIVE / "bin/psql"),
                      ("profile_sha256", PROFILE)):
        if phase1.digest(path) != metadata[key]:
            raise RuntimeError(f"Changed artifact/input; rebuild before SQL tests: {path}")
    for port in (PORT, CONTROL_PORT):
        with socket.socket() as reservation:
            reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            reservation.bind(("127.0.0.1", port))
    password = (DATA / "password").read_text().strip()
    pgpass = DATA / "pgpass"
    pgpass.write_text(f"127.0.0.1:{PORT}:postgres:phase2:{password}\n")
    pgpass.chmod(0o600)
    env = {key: value for key, value in os.environ.items() if not key.startswith("PG")}
    env.update(PGPASSFILE=str(pgpass), PGCONNECT_TIMEOUT="2", PGAPPNAME="phase2-client")
    identity = secrets.token_hex(16)
    command = ["timeout", "--kill-after=5s", "180s", DATA / "hluk",
               "run", "--cold", "--scratch-mb", "512",
               "--initrd", DATA / "postgres.cpio", "--entry", "/pg-init",
               "--port", str(PORT), "--port", str(CONTROL_PORT),
               "--env", f"HL_PG_VM_ID={identity}",
               "--env", "PATH=/usr/local/bin:/usr/bin:/bin"]
    if not (os.access("/dev/kvm", os.R_OK) and os.access("/dev/kvm", os.W_OK)):
        command = ["sudo", "-n", *command]
    result = {"status": "running", "profile": "RAM-only", "port": PORT,
              "max_rss_growth_kib": 32768}
    phase1.write_json(DATA / "results.json", result)
    with (DATA / "guest.log").open("wb") as log:
        print("+ launching one fresh RAM-only PostgreSQL guest", flush=True)
        process = subprocess.Popen(list(map(str, command)), cwd=WORK, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + 30
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"Guest exited {process.returncode}; inspect {DATA / 'guest.log'}")
                ready = subprocess.run(
                    [str(NATIVE / "bin/pg_isready"), "-h", "127.0.0.1", "-p",
                     str(PORT), "-U", "phase2", "-d", "postgres", "-t", "1"],
                    env=env, capture_output=True, text=True, timeout=3)
                if ready.returncode == 0:
                    break
                if ready.returncode not in (1, 2):
                    raise RuntimeError(f"Readiness check failed: {ready.stderr}")
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"PostgreSQL readiness deadline; inspect {DATA / 'guest.log'}")
                time.sleep(0.1)
            host_pid = cli_pid(process)
            result["host_pid"] = host_pid
            with socket.create_connection(("127.0.0.1", CONTROL_PORT), timeout=2) as control:
                control.sendall(b"wrong-identity\n")
            wrong = DATA / "pgpass-wrong"
            wrong.write_text(f"127.0.0.1:{PORT}:postgres:phase2:wrong-password\n")
            wrong.chmod(0o600)
            rejected = subprocess.run(
                [str(NATIVE / "bin/psql"), "-X", "-h", "127.0.0.1", "-p", str(PORT),
                 "-U", "phase2", "-d", "postgres", "-c", "SELECT 1"],
                env=dict(env, PGPASSFILE=str(wrong)), capture_output=True, text=True, timeout=10)
            if rejected.returncode == 0 or "password authentication failed" not in rejected.stderr:
                raise RuntimeError("Wrong-password authentication was not rejected as expected")
            result["wrong_password_rejected"] = True
            with Session("a", env) as a, Session("b", env) as b:
                pids = [a.execute("SELECT pg_backend_pid();"),
                        b.execute("SELECT pg_backend_pid();")]
                if len(pids[0]) != 1 or len(pids[1]) != 1 or pids[0] == pids[1]:
                    raise RuntimeError(f"Expected distinct simultaneous backends: {pids}")
                result["backend_pids"] = [int(values[0]) for values in pids]
                expect(a, "SELECT count(*) FROM pg_stat_activity WHERE backend_type='client backend';", ["2"])
                workers = set(a.execute("SELECT backend_type FROM pg_stat_activity;"))
                if not {"checkpointer", "background writer", "walwriter"} <= workers:
                    raise RuntimeError(f"Required auxiliary processes missing: {workers}")
                result["auxiliary_processes"] = sorted(workers - {"client backend"})
                expect(a, "SHOW fsync;", ["off"])
                expect(a, "SHOW server_version_num;", ["160015"])
                expect(a, "SELECT current_user;", ["phase2"])
                expect(a, "SHOW max_connections;", ["2"])
                expect(a, "SHOW max_stack_depth;", ["512kB"])
                a.execute("CREATE TABLE gate (id int PRIMARY KEY, value text);")
                a.execute("BEGIN; INSERT INTO gate VALUES (1, 'committed');")
                expect(b, "SELECT count(*) FROM gate;", ["0"])
                a.execute("COMMIT;")
                expect(b, "SELECT id, value FROM gate ORDER BY id;", ["1|committed"])
                a.execute("BEGIN; INSERT INTO gate VALUES (2, 'rolled-back'); ROLLBACK;")
                expect(b, "SELECT count(*) FROM gate;", ["1"])
                b.execute("BEGIN; UPDATE gate SET value='temporary'; ROLLBACK;")
                expect(a, "SELECT value FROM gate;", ["committed"])
                b.execute("BEGIN; UPDATE gate SET value='from-b'; COMMIT;")
                expect(a, "SELECT value FROM gate;", ["from-b"])
                b.execute("UPDATE gate SET value='committed';")
                a.execute("SET application_name='phase2_a'; SET timezone='UTC';"
                          "CREATE TEMP TABLE isolated (v int); INSERT INTO isolated VALUES (11);")
                b.execute("SET application_name='phase2_b'; SET timezone='GMT';"
                          "CREATE TEMP TABLE isolated (v int); INSERT INTO isolated VALUES (22);")
                expect(a, "SELECT current_setting('application_name'),current_setting('timezone'),v FROM isolated;",
                       ["phase2_a|UTC|11"])
                expect(b, "SELECT current_setting('application_name'),current_setting('timezone'),v FROM isolated;",
                       ["phase2_b|GMT|22"])
                result["transactions_and_session_isolation"] = True
                size = a.execute("SELECT (SELECT sum(pg_database_size(oid)) FROM pg_database)"
                                 " + (SELECT sum(size) FROM pg_ls_waldir());")
                if len(size) != 1 or int(size[0]) > BUDGET:
                    raise RuntimeError(f"Live database/WAL budget exceeded: {size}")
                result["database_and_wal_bytes"] = int(size[0])
            for i in range(4):
                with Session(f"warmup-{i}", env) as session:
                    expect(session, "SELECT count(*) FROM gate;", ["1"])
            baseline_rss = rss_kib(host_pid)
            for i in range(24):
                with Session(f"reconnect-{i}", env) as session:
                    expect(session, "SELECT current_setting('application_name');", ["phase2-client"])
                    expect(session, "SELECT to_regclass('pg_temp.isolated') IS NULL;", ["t"])
                    expect(session, "SELECT count(*) FROM gate;", ["1"])
            final_rss = rss_kib(host_pid)
            result.update(reconnections=24, warmup_connections=4,
                          rss_before_kib=baseline_rss, rss_after_kib=final_rss)
            check_rss_growth(baseline_rss, final_rss, result["max_rss_growth_kib"])
        except (RuntimeError, OSError, subprocess.SubprocessError) as error:
            result.update(status="failed", error=str(error))
            raise
        finally:
            try:
                result["guest_exit"] = stop_guest(process, identity)
            finally:
                phase1.write_json(DATA / "results.json", result)
    output = (DATA / "guest.log").read_text()
    try:
        for case in ("shared-creator-exit", "guest-ownership", "postmaster-shutdown"):
            phase1.validate_result(case, output, result["guest_exit"])
        if "uid=1000" not in output or "PG_INIT rejected invalid shutdown identity" not in output:
            raise RuntimeError("Non-root identity or control authentication was not observed")
    except RuntimeError as error:
        result.update(status="failed", error=str(error))
        phase1.write_json(DATA / "results.json", result)
        raise
    result["status"] = "passed"
    phase1.write_json(DATA / "results.json", result)
    print("PASS phase2-sql: two sessions, transactions, isolation, 24 reconnects, clean shutdown")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "kernel", "build-native", "build-guest", "package", "test"))
    command = parser.parse_args().command
    DATA.mkdir(parents=True, exist_ok=True)
    with (WORK / "phase1.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if command == "prepare":
            prepare_sources()
        elif command == "kernel":
            phase1.build(overlay=HERE / "rootfs/kernel.conf", output=DATA)
        elif command.startswith("build-"):
            prepare_sources()
            build_postgres(command == "build-guest")
        elif command == "package":
            package()
        else:
            test()


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
