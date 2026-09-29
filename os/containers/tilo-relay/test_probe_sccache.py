#!/usr/bin/env python3
"""Executable contract tests for the nested remote-cache probe."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest


HERE = Path(__file__).resolve().parent
PROBE = HERE / "probe-sccache.py"
FAKE = r'''#!/usr/bin/env python3
import json, os, pathlib, sys, time
state = pathlib.Path(os.environ["TILO_FAKE_SCCACHE_STATE"])
log = pathlib.Path(os.environ["TILO_FAKE_SCCACHE_LOG"])
with log.open("a") as stream:
    stream.write(" ".join(sys.argv[1:]) + "\n")
args = sys.argv[1:]
mode = os.environ.get("TILO_FAKE_SCCACHE_MODE", "success")
if args == ["--stop-server"] or args == ["--zero-stats"]:
    raise SystemExit(0)
if args and args[0] == "rustc":
    if mode == "timeout":
        time.sleep(2)
    if mode == "auth":
        print("remote authentication failed", file=sys.stderr)
        raise SystemExit(2)
    count = int(state.read_text()) if state.exists() else 0
    state.write_text(str(count + 1))
    out_dir = pathlib.Path(args[args.index("--out-dir") + 1])
    (out_dir / "libtilo_relay_cache_probe.rlib").write_bytes(b"probe")
    raise SystemExit(0)
if args == ["--show-stats", "--stats-format=json"]:
    count = int(state.read_text())
    location = "Local disk" if mode == "local" else (
        "ghac, name: sccache-v0.test-" + os.environ["SCCACHE_GHA_VERSION"] + ", prefix: /sccache/"
    )
    stats = {"cache_hits": int(count > 1), "cache_misses": int(count == 1),
             "cache_writes": int(count == 1), "cache_errors": 0,
             "cache_read_errors": 0, "cache_write_errors": int(mode == "io")}
    print(json.dumps({"cache_location": location, "stats": stats}))
    raise SystemExit(0)
raise SystemExit(2)
'''


class ProbeTests(unittest.TestCase):
    def run_probe(self, mode: str, timeout: str = "1"):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        client = root / "sccache"
        client.write_text(textwrap.dedent(FAKE))
        client.chmod(0o755)
        log = root / "commands.log"
        env = os.environ.copy()
        env.update(
            PATH=str(root) + os.pathsep + env["PATH"],
            SCCACHE_GHA_VERSION="tilo-relay-v1",
            TILO_CACHE_PROBE_NONCE="test-run",
            TILO_CACHE_PROBE_TIMEOUT_SECONDS=timeout,
            TILO_FAKE_SCCACHE_MODE=mode,
            TILO_FAKE_SCCACHE_STATE=str(root / "state"),
            TILO_FAKE_SCCACHE_LOG=str(log),
        )
        started = time.monotonic()
        result = subprocess.run(
            [sys.executable, str(PROBE)], env=env, text=True,
            capture_output=True, timeout=5,
        )
        return result, log.read_text().splitlines(), time.monotonic() - started

    def test_remote_write_then_hit_passes_and_stops_server(self):
        result, commands, _ = self.run_probe("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("remote compiler cache probe: PASS", result.stdout)
        self.assertEqual(commands[-1], "--stop-server")

    def test_failed_or_untrusted_probes_are_rejected_and_stop_server(self):
        for mode, timeout, stderr in (
            ("local", "1", "did not use the bound remote cache"),
            ("io", "1", "authentication or I/O error"),
            # A failing client still stops the server.
            ("auth", "1", None),
            # A hung client is bounded by the probe timeout.
            ("timeout", "0.05", None),
        ):
            with self.subTest(mode=mode):
                result, commands, elapsed = self.run_probe(mode, timeout)
                self.assertNotEqual(result.returncode, 0)
                if stderr is not None:
                    self.assertIn(stderr, result.stderr)
                if mode == "timeout":
                    self.assertLess(elapsed, 2)
                self.assertEqual(commands[-1], "--stop-server")

if __name__ == "__main__":
    unittest.main()
