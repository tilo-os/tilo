#!/usr/bin/env python3
"""Remote fixture: pinned runtime bytes and an empty, owned daemon profile.

Run through runtime-bundle's signal_runtime_fixture example in an isolated
native build container. It never links, registers, sends, or accesses a personal account.
The Rust launcher verifies the signed bundle before selecting the probe step.
"""
import argparse
import errno
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("signal_staging", ROOT / "os/scripts/stage-signal-runtime.py")
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)


def prepare(work):
    assets = work / "assets"
    assets.mkdir()
    pin = json.loads(staging.PIN.read_text())
    for entry in pin["assets"].values():
        destination = assets / entry["name"]
        size = 0
        with urllib.request.urlopen(entry["url"], timeout=30) as response, destination.open("wb") as output:
            while block := response.read(1024 * 1024):
                size += len(block)
                staging.require(size <= 256 * 1024 * 1024, "fixture asset exceeds size bound")
                output.write(block)
        staging.verified_asset(assets, entry)
    staging.stage(assets, work / "signal-provider", pin, "x86_64")


def command(bundle, profile):
    provider = bundle / "lib/signal-provider"
    libraries = ":".join(str(path) for path in (bundle / "lib", bundle / "lib/lib64",
                                                bundle / "lib/usr/lib64", provider / "java/lib",
                                                provider / "java/lib/server"))
    return [str(bundle / "lib/lib64/ld-linux-x86-64.so.2"), "--library-path", libraries,
            str(provider / "java/bin/java"), "--enable-native-access=ALL-UNNAMED",
            "-Duser.home=" + str(profile), "-classpath",
            str(provider / "signal/tilo") + ":" + str(provider / "signal/lib/*"),
            "org.asamk.signal.Main", "--data-dir", str(profile)]


def probe(work, prefix=()):
    bundle = work / "bundle"
    profile = work / "empty-fixture-profile"
    profile.mkdir(mode=0o700)
    environment = {"LD_LIBRARY_PATH": str(bundle / "lib"), "LANG": "C.UTF-8"}
    base = [*prefix, *command(bundle, profile)]
    version = subprocess.run(base + ["--version"], env=environment,
                             capture_output=True, timeout=20, check=True)
    staging.require(version.stdout.strip() == b"signal-cli 0.14.8", "fixture Signal version differs")
    with tempfile.TemporaryDirectory(prefix="tilo-signal-fixture-", dir="/tmp") as socket_directory, \
            tempfile.TemporaryFile() as stderr:
        endpoint = Path(socket_directory) / "fixture.sock"
        daemon = subprocess.Popen(base + ["daemon", "--socket", str(endpoint), "--receive-mode", "manual"],
                                  env=environment, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=stderr,
                                  start_new_session=True)
        try:
            deadline = time.monotonic() + 20
            # bind() publishes the pathname before listen() accepts clients.
            # Observe readiness through the same connection used for the RPC.
            while True:
                staging.require(daemon.poll() is None, "empty fixture daemon exited before accepting RPC")
                staging.require(time.monotonic() < deadline, "empty fixture daemon did not accept RPC")
                stream = socket.socket(socket.AF_UNIX)
                stream.settimeout(5)
                try:
                    stream.connect(str(endpoint))
                    break
                except OSError as error:
                    stream.close()
                    if error.errno not in (errno.ENOENT, errno.ECONNREFUSED):
                        raise
                    time.sleep(0.02)
            with stream:
                stream.sendall(b'{"jsonrpc":"2.0","id":"fixture-empty","method":"listAccounts","params":{}}\n')
                with stream.makefile("rb") as reader:
                    reply = reader.readline(65537)
                staging.require(len(reply) <= 65536 and reply.endswith(b"\n"), "invalid fixture reply frame")
                decoded = json.loads(reply)
                staging.require(isinstance(decoded, dict) and decoded.get("jsonrpc") == "2.0"
                                and "error" not in decoded and decoded.get("id") == "fixture-empty"
                                and decoded.get("result") == [],
                                "fixture must contain no authenticated accounts")
        except Exception as error:
            stderr.seek(0, os.SEEK_END)
            stderr.seek(max(0, stderr.tell() - 4096))
            detail = stderr.read().decode("utf-8", errors="replace").strip()
            raise RuntimeError(f"empty fixture probe failed (daemon status {daemon.poll()}): {error}; {detail}") from error
        finally:
            if daemon.poll() is None:
                os.killpg(daemon.pid, signal.SIGTERM)
                try:
                    daemon.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(daemon.pid, signal.SIGKILL)
                    daemon.wait(timeout=3)
    print("Signal/Java runtime: exact version, owned empty daemon and bounded JSON-RPC passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("prepare", "probe"))
    parser.add_argument("work", type=Path)
    args = parser.parse_args()
    staging.require(args.work.is_absolute(), "fixture work root must be absolute")
    (prepare if args.operation == "prepare" else probe)(args.work)


if __name__ == "__main__":
    main()
