#!/usr/bin/env python3
"""Verify frozen RPMs using the builder that will consume them, without Docker pull."""

import argparse
import difflib
import json
import platform
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def verify(ref, builder):
    if not builder.strip():
        raise ValueError("the relay BuildKit builder name is required")
    arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(platform.machine())
    if arch is None:
        raise ValueError("RPM snapshot verification requires a supported native build host")
    if not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", ref):
        raise ValueError("RPM snapshot must be pinned by its sha256 digest")
    # imagetools fetches manifests/configs using existing Docker registry auth;
    # it does not download the RPM filesystem layer into the Docker image store.
    metadata = json.loads(subprocess.check_output(
        ["docker", "buildx", "imagetools", "inspect", ref,
         "--format", "{{json .Image}}"], text=True,
    ))
    if isinstance(metadata, dict) and "architecture" not in metadata:
        keys = [f"linux/{arch}"]
        if arch == "arm64":
            keys.append("linux/arm64/v8")
        candidates = [metadata[key] for key in keys if key in metadata]
        metadata = candidates[0] if len(candidates) == 1 else None
    if not isinstance(metadata, dict) or (
        metadata.get("architecture") != arch or metadata.get("os") != "linux"
        or metadata.get("variant", "") not in ({"", "v8"} if arch == "arm64" else {""})
    ):
        raise ValueError(f"frozen RPM snapshot OCI metadata does not match native linux/{arch}")

    repo = Path(__file__).resolve().parents[2]
    expected = subprocess.check_output(
        [sys.executable, str(repo / "os/dev/derive-rpm-package-set.py")], text=True,
    )
    with tempfile.TemporaryDirectory(prefix="tilo-rpm-buildkit-") as temporary:
        work = Path(temporary)
        context = work / "context"
        context.mkdir()
        (context / "Dockerfile").write_text(
            "ARG RPMS_REF\nFROM ${RPMS_REF} AS rpms\nFROM scratch\n"
            "COPY --from=rpms /repo/tilo-direct-package-specs.txt /actual\n",
            encoding="utf-8",
        )
        output = work / "output"
        subprocess.run(
            ["docker", "buildx", "build", "--builder", builder,
             "--platform", f"linux/{arch}", "--build-arg", f"RPMS_REF={ref}",
             "--output", f"type=local,dest={output}", str(context)], check=True,
        )
        manifest = output / "actual"
        if not manifest.is_file() or manifest.is_symlink():
            raise ValueError("frozen RPM snapshot has no regular package manifest")
        actual = manifest.read_bytes().decode("utf-8")
        if actual != expected:
            sys.stderr.writelines(difflib.unified_diff(
                actual.splitlines(keepends=True), expected.splitlines(keepends=True),
                fromfile="snapshot", tofile="current consumers",
            ))
            raise ValueError("frozen RPM snapshot does not match the current Containerfiles")
    print("Frozen RPM snapshot package manifest matches every consumer.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ref")
    parser.add_argument("--builder", required=True)
    args = parser.parse_args()
    try:
        verify(args.ref, args.builder)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"RPM snapshot verification failed: {error}", file=sys.stderr)
        sys.exit(1)
