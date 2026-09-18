#!/usr/bin/env python3
"""Metadata checks for the separate, complete Windows guest RPM snapshot."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys


def immutable_ref(value):
    if not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", value):
        raise ValueError("expected a full immutable OCI digest reference")
    return value


def package_roots(path):
    roots = []
    for line in Path(path).read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9+_-]*\.(x86_64|i686|noarch)", line):
            raise ValueError(f"invalid architecture-qualified package root: {line!r}")
        if line in roots:
            raise ValueError(f"duplicate package root: {line}")
        roots.append(line)
    if not roots:
        raise ValueError("empty package root manifest")
    return sorted(roots)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def collect_rpms(caches, repo):
    """Merge fresh DNF cache locations; never silently choose a name collision."""
    repo = Path(repo)
    for cache in caches:
        for path in sorted(Path(cache).rglob("*.rpm")):
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"cache RPM must be a regular file: {path}")
            target = repo / path.name
            if target.exists() or target.is_symlink():
                if target.is_symlink() or not target.is_file() or digest(path) != digest(target):
                    raise ValueError(f"different RPMs collide at filename: {path.name}")
            else:
                shutil.copyfile(path, target)


def read_rpm_header(path):
    result = subprocess.run(
        ["rpm", "-qp", "--qf",
         "%{NAME}\\t%{EPOCHNUM}\\t%{VERSION}\\t%{RELEASE}\\t%{ARCH}\\t%{SOURCEPACKAGE}\\n",
         str(path)], capture_output=True, text=True, check=False)
    fields = result.stdout.rstrip("\n").split("\t")
    if result.returncode or len(fields) != 6 or any(not field or "\n" in field for field in fields):
        raise ValueError(f"cannot read valid RPM header: {path.name}")
    name, epoch, version, release, arch, source = fields
    if arch not in ("x86_64", "i686", "noarch") or source == "1":
        raise ValueError(f"unsupported guest RPM header architecture/type: {path.name}: {arch}")
    if not epoch.isdecimal():
        raise ValueError(f"invalid RPM epoch: {path.name}")
    return {"name": name, "epoch": epoch, "version": version,
            "release": release, "architecture": arch}


def rpm_inventory(repo):
    result = {}
    identities = set()
    for path in sorted(Path(repo).glob("*.rpm")):
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"RPM must be a regular, non-symlink file: {path.name}")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+~^-]*\.(x86_64|i686|noarch)\.rpm", path.name):
            raise ValueError(f"unexpected guest RPM filename/architecture: {path.name}")
        if path.stat().st_size == 0:
            raise ValueError(f"empty RPM: {path.name}")
        header = read_rpm_header(path)
        identity = tuple(header[key] for key in ("name", "epoch", "version", "release", "architecture"))
        if identity in identities:
            raise ValueError(f"duplicate RPM NEVRA: {identity}")
        identities.add(identity)
        expected_name = f"{header['name']}-{header['version']}-{header['release']}.{header['architecture']}.rpm"
        if path.name != expected_name:
            raise ValueError(f"RPM filename does not match its header: {path.name}")
        result[path.name] = {"sha256": digest(path), **header}
    if not result:
        raise ValueError("snapshot has no RPMs")
    return result


def snapshot_contract(roots_path, repo, buildroot_ref):
    result = {
        "schema_version": 1,
        "distribution": "fedora",
        "releasever": "44",
        "architecture": "x86_64",
        "transaction_base": "empty-installroot",
        "install_weak_deps": True,
        "buildroot_ref": immutable_ref(buildroot_ref),
        "package_roots": package_roots(roots_path),
        "rpms": rpm_inventory(repo),
    }
    trust = Path(repo) / 'development-wine'
    if trust.exists():
        if trust.is_symlink() or not trust.is_dir():
            raise ValueError('development Wine trust must be a regular directory')
        names = {'public.asc', 'paired-contract.json', 'development-wine.json', 'reference.txt'}
        if {p.name for p in trust.iterdir()} != names:
            raise ValueError('development Wine trust has missing or unexpected files')
        for name in names:
            if (trust / name).is_symlink() or not (trust / name).is_file():
                raise ValueError('development Wine trust must contain regular files')
        result['development_wine'] = {
            'reference': immutable_ref((trust / 'reference.txt').read_text().strip()),
            'files': {name: digest(trust / name) for name in sorted(names)},
            'development_only': True,
        }
    return result


def verify_snapshot(roots_path, repo, buildroot_ref):
    repo = Path(repo)
    stored = json.loads((repo / "windows-guest-contract.json").read_text())
    expected = snapshot_contract(roots_path, repo, buildroot_ref)
    if stored != expected:
        changed = sorted(key for key in set(stored) | set(expected)
                         if stored.get(key) != expected.get(key))
        raise ValueError(f"guest snapshot contract mismatch: {', '.join(changed)}")
    return stored


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("roots", "record", "verify", "reference", "collect"))
    parser.add_argument("--roots", type=Path)
    parser.add_argument("--repo", type=Path)
    parser.add_argument("--buildroot-ref")
    parser.add_argument("--cache", type=Path, action="append", default=[])
    args = parser.parse_args()
    try:
        if args.operation == "collect":
            if args.repo is None or not args.cache:
                parser.error("--repo and --cache are required")
            collect_rpms(args.cache, args.repo)
        elif args.operation == "reference":
            print(immutable_ref(args.buildroot_ref or ""))
        elif args.operation == "roots":
            if args.roots is None:
                parser.error("--roots is required")
            print("\n".join(package_roots(args.roots)))
        else:
            if args.roots is None or args.repo is None or args.buildroot_ref is None:
                parser.error("--roots, --repo and --buildroot-ref are required")
            if args.operation == "verify":
                verify_snapshot(args.roots, args.repo, args.buildroot_ref)
            else:
                result = snapshot_contract(args.roots, args.repo, args.buildroot_ref)
                (args.repo / "windows-guest-contract.json").write_text(
                    json.dumps(result, indent=2, sort_keys=True) + "\n")
    except (ValueError, OSError) as error:
        print(f"Windows guest contract rejected: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
