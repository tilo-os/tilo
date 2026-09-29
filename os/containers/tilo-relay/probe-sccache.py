#!/usr/bin/env python3
"""Prove the nested BuildKit client can write and reread the remote cache."""

import json
import os
from pathlib import Path
import subprocess
import tempfile


TIMEOUT = float(os.environ.get("TILO_CACHE_PROBE_TIMEOUT_SECONDS", "45"))
if not 0.05 <= TIMEOUT <= 45:
    raise RuntimeError("invalid compiler-cache probe timeout")


def run(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, check=check, text=True, capture_output=True, timeout=TIMEOUT)


def count(stats: dict, name: str) -> int:
    value = stats.get(name, 0)
    if isinstance(value, dict):
        value = sum(value.get("counts", {}).values())
    if type(value) is not int or value < 0:
        raise RuntimeError(f"invalid {name} counter")
    return value


def main() -> None:
    # sccache names its GHA namespace "sccache-v<version>-<SCCACHE_GHA_VERSION>".
    namespace = os.environ["SCCACHE_GHA_VERSION"]

    with tempfile.TemporaryDirectory(prefix="tilo-relay-cache-probe-") as directory:
        root = Path(directory)
        source = root / "probe.rs"
        source.write_text(
            'pub fn cache_probe() -> &\'static str { "'
            + os.environ["TILO_CACHE_PROBE_NONCE"]
            + '" }\n'
        )
        output = root / "libtilo_relay_cache_probe.rlib"
        attempts = []
        try:
            for attempt in ("write", "read"):
                run("sccache", "--stop-server", check=False)
                os.environ["SCCACHE_DIR"] = str(root / attempt)
                run("sccache", "--zero-stats")
                run(
                    "sccache", "rustc", "--crate-name", "tilo_relay_cache_probe",
                    "--crate-type", "rlib", str(source), "--emit=link", "--out-dir", directory,
                )
                payload = json.loads(run("sccache", "--show-stats", "--stats-format=json").stdout)
                location = payload.get("cache_location", "")
                if not (location.startswith("ghac, name: sccache-v") and f"-{namespace}, " in location):
                    raise RuntimeError("compiler probe did not use the bound remote cache")
                stats = payload.get("stats", {})
                counters = {name: count(stats, name) for name in (
                    "cache_hits", "cache_misses", "cache_writes", "cache_errors",
                    "cache_read_errors", "cache_write_errors",
                )}
                if any(counters[name] for name in ("cache_errors", "cache_read_errors", "cache_write_errors")):
                    raise RuntimeError("remote compiler cache reported an authentication or I/O error")
                attempts.append(counters)
                output.unlink()
        finally:
            run("sccache", "--stop-server", check=False)

        if not attempts[0]["cache_writes"]:
            raise RuntimeError("compiler probe did not populate the remote cache")
        if not attempts[1]["cache_hits"]:
            raise RuntimeError("compiler probe did not reread the remote cache")

    print("remote compiler cache probe: PASS")


if __name__ == "__main__":
    main()
