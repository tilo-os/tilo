"""Place an exact measured native workload; changed inputs retain the general lane."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess


INPUTS = (
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml",
    ".config/nextest.toml", ".forgejo/workflows/check.yml", "tools/test-env.sh",
    "tools/with-test-environment.sh", "tools/with-headless-audio.sh", "tools/cargo-runner.sh",
    ".config/nextest-filters.toml", "tools/ci/nextest_filters.py", "tools/ci/clippy-check.sh",
    "tools/ci/compiler_inputs.py", "tools/ci/measure.sh", "tools/ci/process_progress.py",
    "tools/ci/cgroup_memory.py", "scripts/ci-run.sh", "tools/ensure-headless-audio.sh",
    "tools/ci/prepare-test-shard.sh", "os/dev/assert-native-arch.sh", "tools/font-catalog.py",
    ".forgejo/actions/prepare-rust-build/action.yml",
    ".forgejo/actions/restore-cargo-downloads/action.yml",
    ".forgejo/actions/merge-main/action.yml",
)
PACKAGES = ("crates/collab/collab-types", "crates/platform/tilo-clock", "crates/system/wire-frame")


def fingerprint(root: Path) -> str:
    paths = set(INPUTS)
    for directory in PACKAGES:
        paths.update(path.relative_to(root).as_posix() for path in (root / directory).rglob("*")
                     if path.is_file())
    digest = hashlib.sha256()
    for name in sorted(paths):
        digest.update(name.encode() + b"\0")
        digest.update(hashlib.sha256((root / name).read_bytes()).digest())
    return digest.hexdigest()


def placement(root: Path, names: set[str], arch: str) -> dict:
    if arch != "x86_64" or names != {"collab-types"}:
        return {}
    try:
        receipt = json.loads((root / "tools/ci/qualified-native.json").read_text())
        if receipt.get("enabled") is not True:
            return {}
        proof = receipt["qualification"]
        if (proof["cpu_quota"] != 2 or proof["memory_limit_bytes"] != 5 * 1024**3
                or proof["tests_passed"] != 82 or proof["tests_skipped"] != 0
                or not 0 < proof["cgroup_memory_peak_bytes"] < 3 * 1024**3
                or any(proof[key] != 0 for key in ("memory_max_events", "oom_events", "oom_kill_events"))):
            return {}
        # Selection uses the merged tree; an older PR can still execute its
        # older workflow, whose empty certification would exceed this envelope.
        head = os.environ.get("HEAD_SHA")
        if not head:
            return {}
        workflow = subprocess.check_output(
            ["git", "show", f"{head}:.forgejo/workflows/check.yml"], cwd=root, stderr=subprocess.DEVNULL)
        if workflow != (root / ".forgejo/workflows/check.yml").read_bytes():
            return {}
        if receipt["schema"] == 1 and receipt["input_sha256"] == fingerprint(root):
            return {"qualified_runner": "runner-quality", "qualified_builders": receipt["builders"]}
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError):
        pass
    return {}
