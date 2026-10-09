#!/usr/bin/env python3
"""Keep bounded, unvalidated RPMs from this checkout for failure diagnosis."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

MAX_BYTES = 4 * 1024**3
MAX_FILES = 3


def retain(root, output, stage, status):
    revision = subprocess.check_output(
        ["git", "-C", str(root), "rev-parse", "HEAD"], text=True,
        env={name: value for name, value in os.environ.items() if not name.startswith("GIT_")},
    ).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision) or revision != os.environ.get("GITHUB_SHA"):
        raise ValueError("RPM retention requires the exact workflow source commit")
    files = sorted((root / "dist/rpm/build/RPMS").glob("*/*.rpm"))
    if not files:
        return
    if len(files) > MAX_FILES or len({p.name for p in files}) != len(files):
        raise ValueError("RPM retention exceeds the three-package limit")
    if any(p.is_symlink() or p.parent.is_symlink() or not p.is_file()
           or not p.name.startswith("tilo-session-")
           for p in files):
        raise ValueError("RPM retention accepts only regular Tilo package files")
    total = sum(p.stat().st_size for p in files)
    if total > MAX_BYTES:
        raise ValueError("RPM retention exceeds the 4 GiB limit")
    output.mkdir(parents=True, exist_ok=False)
    try:
        packages = []
        copied = 0
        for source in files:
            destination = output / source.name
            digest = hashlib.sha256()
            with source.open("rb") as stream, destination.open("xb") as target:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    copied += len(chunk)
                    if copied > MAX_BYTES:
                        raise ValueError("RPM retention grew beyond the 4 GiB limit")
                    target.write(chunk)
                    digest.update(chunk)
            packages.append({"name": source.name, "bytes": destination.stat().st_size,
                             "sha256": digest.hexdigest()})
        (output / "provenance.json").write_text(json.dumps({
            "schema": 1, "validation": "failed-unvalidated", "source_commit": revision,
            "architecture": os.environ["TILO_EXPECT_ARCH"], "failed_stage": stage,
            "exit_status": int(status), "max_bytes": MAX_BYTES, "max_files": MAX_FILES,
            "packages": packages,
        }, indent=2) + "\n")
    except BaseException:
        shutil.rmtree(output)
        raise


if __name__ == "__main__":
    retain(Path.cwd(), Path("/artifacts/failed-rpms"), sys.argv[1], sys.argv[2])
