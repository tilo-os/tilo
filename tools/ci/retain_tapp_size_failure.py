#!/usr/bin/env python3
"""Bounded, nonpublishing ELF evidence after a shipped TAPP size gate fails.

Never archive dist/app-release/work: it contains publisher private identities.
Only copy catalog-named ELF files and their adjacent .map/.sym/.debug sidecars.
No Cargo, linker, stripping, signing, network, or environment snapshot runs here.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/perf"))
import tapp_size_ratchet as ratchet  # noqa: E402

MAX_TOTAL = 256 * 1024 * 1024
MAX_ELF = 128 * 1024 * 1024
MAX_SIDECAR = 32 * 1024 * 1024
MAX_JSON = 1024 * 1024
MAX_APPS = 64
SIDECARS = (".map", ".sym", ".debug")
MACHINES = {"aarch64": 183, "x86_64": 62}


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=root, text=True).strip()


def regular(root: Path, relative: Path):
    """No absolute/escaping paths, symlinks, directories or special files."""
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError("diagnostic path escapes repository")
    cursor = root
    for part in relative.parts:
        cursor /= part
        if cursor.is_symlink():
            raise ValueError("symlinked diagnostic path")
    fd = os.open(cursor, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError("diagnostic input is not a regular file")
    return os.fdopen(fd, "rb")


def inspect(root: Path, relative: Path, limit: int, arch: str | None = None) -> dict:
    digest = hashlib.sha256()
    with regular(root, relative) as stream:
        size = os.fstat(stream.fileno()).st_size
        if not 0 < size <= limit:
            raise ValueError("diagnostic input exceeds file bound or is empty")
        header = stream.read(20)
        if arch is not None and (len(header) < 20 or header[:6] != b"\x7fELF\x02\x01"
                                 or int.from_bytes(header[18:20], "little") != MACHINES[arch]):
            raise ValueError("diagnostic ELF target mismatch")
        digest.update(header)
        read = len(header)
        while block := stream.read(1024 * 1024):
            read += len(block)
            if read > limit:
                raise ValueError("diagnostic input grew beyond bound")
            digest.update(block)
        if read != size:
            raise ValueError("diagnostic input changed size")
    return {"bytes": size, "sha256": digest.hexdigest()}


def retain(root: Path, *, arch: str, event_source: str, base_source: str,
           builder: str, run_id: str, attempt: str) -> dict:
    if arch not in MACHINES or not re.fullmatch(r"[0-9a-f]{40}", event_source):
        raise ValueError("exact source and supported architecture required")
    if not re.fullmatch(r"registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}", builder):
        raise ValueError("immutable builder identity required")
    if not re.fullmatch(r"[1-9][0-9]*", run_id) or not re.fullmatch(r"[1-9][0-9]*", attempt):
        raise ValueError("positive workflow API run ID and attempt required")
    source = git(root, "rev-parse", "HEAD")
    parents = git(root, "show", "-s", "--format=%P", "HEAD").split()
    if source != event_source:
        if not re.fullmatch(r"[0-9a-f]{40}", base_source) or parents != [event_source, base_source]:
            raise ValueError("tested source is not the expected PR merge")
    elif base_source:
        # merge-main legitimately does nothing when this head already contains
        # the selected main. Prove that relationship, rather than invent a merge.
        if not re.fullmatch(r"[0-9a-f]{40}", base_source):
            raise ValueError("invalid selected PR base")
        subprocess.run(["git", "merge-base", "--is-ancestor", base_source, source],
                       cwd=root, check=True)
    subprocess.run(["git", "diff", "--quiet", "HEAD", "--"], cwd=root, check=True)
    release = Path("dist/app-release")
    index_path = release / "index.json"
    index_info = inspect(root, index_path, MAX_JSON)
    with regular(root, index_path) as stream:
        index = json.load(stream)
    if index.get("source") != event_source or index.get("architecture") != arch:
        raise ValueError("packaging index source/target mismatch")
    apps = index.get("apps", [])
    if not isinstance(apps, list) or not 0 < len(apps) <= MAX_APPS:
        raise ValueError("packaging index population exceeds bound")
    budget = ratchet.load(root)
    limits = budget.app_totals()
    failed, seen, binaries_seen = [], set(), set()
    for app in apps:
        name, binary = app["name"], app["binary"]
        if not re.fullmatch(r"[a-z0-9][a-z0-9_-]*", name) or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", binary):
            raise ValueError("invalid catalog binary name")
        if binary in {"manifest.json", "libruntime_dylib.so"}:
            raise ValueError("reserved diagnostic filename")
        with regular(root, Path("apps") / name / "tilo-app.toml") as stream:
            catalog = tomllib.loads(stream.read(MAX_JSON + 1).decode())
        if catalog["package"]["id"] != app["id"] or Path(catalog["entrypoint"]["bin"]).name != binary:
            raise ValueError("packaging entry differs from source catalog")
        if app["id"] in seen or binary in binaries_seen:
            raise ValueError("duplicate package or binary identity")
        seen.add(app["id"])
        binaries_seen.add(binary)
        filename = app["file"]
        if not isinstance(filename, str) or Path(filename).name != filename or not filename.endswith(f"-{arch}.tapp"):
            raise ValueError("invalid TAPP filename")
        tapp = inspect(root, release / "tapp" / filename, MAX_ELF)
        if tapp["sha256"] != app["sha256"]:
            raise ValueError("packaged TAPP differs from index hash")
        limit = limits.get((app["id"], arch))
        if limit is None or tapp["bytes"] > limit:
            failed.append({"id": app["id"], "binary": binary, "name": name,
                           "tapp": {"file": filename, **tapp}, "budget": limit})
    if not failed:
        raise ValueError("no oversized or unbudgeted TAPP; refuse success-path retention")
    for app in failed:
        binary = Path("target/thin-app") / app["binary"]
        # package-apps copies this ELF verbatim into the signed payload staging.
        target = inspect(root, binary, MAX_ELF, arch)
        staged = inspect(root, release / "work" / f"payload-{app['name']}" / "bin" / app["binary"], MAX_ELF, arch)
        if target != staged:
            raise ValueError("retained ELF differs from signed payload staging")
        app["elf"] = target
    output = root / "target/ci/tapp-size-failure"
    for relative in (Path("target"), Path("target/ci")):
        if (root / relative).is_symlink():
            raise ValueError("symlinked diagnostic output parent")
    # Never upload an old cache's evidence if collection fails this time.
    if output.exists() or output.is_symlink():
        raise ValueError("diagnostic output already exists")
    output.mkdir(parents=True, mode=0o700)
    manifest = {"schema": "tilo.tapp-size-failure/v1", "event_source": event_source,
                "tested_source": source, "tested_tree": git(root, "rev-parse", "HEAD^{tree}"),
                "base_source": base_source or None, "builder_image": builder,
                "architecture": arch, "target": f"{arch}-unknown-linux-gnu",
                "profile": "thin-app", "run_id": int(run_id), "run_attempt": int(attempt),
                "index": index_info, "failed_apps": failed, "files": [], "omitted": [],
                "max_total_bytes": MAX_TOTAL, "max_manifest_bytes": MAX_JSON, "total_bytes": 0,
                "sidecars": "Only already-existing adjacent .map/.sym/.debug files; no maps generated.",
                "build_inputs": {}}
    for relative in ("Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml",
                     f"{ratchet.DIRECTORY}/budget.toml"):
        manifest["build_inputs"][relative] = inspect(root, Path(relative), MAX_JSON)
    for path in sorted((root / ratchet.DIRECTORY / "raises").glob("*.toml")):
        relative = path.relative_to(root)
        manifest["build_inputs"][str(relative)] = inspect(root, relative, MAX_JSON)

    def copy(relative: Path, kind: str, required: bool = False, expected: dict | None = None) -> None:
        destination = None
        created = False
        try:
            info = inspect(root, relative, MAX_ELF if kind == "elf" else MAX_SIDECAR,
                           arch if kind == "elf" or relative.suffix == ".debug" else None)
            if expected is not None and info != expected:
                raise ValueError("diagnostic ELF changed after payload verification")
            if manifest["total_bytes"] + info["bytes"] > MAX_TOTAL:
                raise ValueError("total diagnostic byte limit reached")
            destination = output / relative.name
            with regular(root, relative) as stream, destination.open("xb") as saved:
                created = True
                digest, count = hashlib.sha256(), 0
                while block := stream.read(1024 * 1024):
                    count += len(block)
                    if count > info["bytes"]:
                        raise ValueError("diagnostic input changed during copy")
                    digest.update(block)
                    saved.write(block)
            if count != info["bytes"] or digest.hexdigest() != info["sha256"]:
                destination.unlink()
                raise ValueError("diagnostic input changed during copy")
            manifest["total_bytes"] += count
            manifest["files"].append({"path": destination.name, "input": str(relative),
                                      "kind": kind, **info})
        except (OSError, ValueError) as error:
            if created and destination is not None and destination.exists():
                destination.unlink()
            manifest["omitted"].append({"input": str(relative), "required": required,
                                        "reason": type(error).__name__ + ": " + str(error)})

    for app in failed:
        binary = Path("target/thin-app") / app["binary"]
        copy(binary, "elf", required=True, expected=app["elf"])
    # Required app evidence takes priority over optional shared/runtime evidence.
    binaries = [Path("target/thin-app") / app["binary"] for app in failed]
    runtime = Path("target/thin-app/libruntime_dylib.so")
    copy(runtime, "elf")
    for binary in [*binaries, runtime]:
        for suffix in SIDECARS:
            relative = binary.with_name(binary.name + suffix)
            if (root / relative).exists() or (root / relative).is_symlink():
                copy(relative, "sidecar")
    manifest["complete_required_binaries"] = not any(item["required"] for item in manifest["omitted"])
    encoded = json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n"
    if len(encoded) > MAX_JSON:
        raise ValueError("diagnostic manifest exceeds bound")
    (output / "manifest.json").write_bytes(encoded)
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--event-source", required=True)
    parser.add_argument("--base-source", default="")
    args = parser.parse_args()
    manifest = retain(ROOT, arch=args.arch, event_source=args.event_source,
                      base_source=args.base_source,
                      builder=os.environ.get("DIAGNOSTIC_BUILDER_IMAGE", ""),
                      run_id=os.environ.get("GITHUB_RUN_ID", ""),
                      attempt=os.environ.get("GITHUB_RUN_ATTEMPT", ""))
    print(f"retained {len(manifest['files'])} diagnostic files, {manifest['total_bytes']} bytes; "
          f"required binaries complete={manifest['complete_required_binaries']}")


if __name__ == "__main__":
    main()
