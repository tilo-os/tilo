#!/usr/bin/env python3
"""A Cargo local registry holding the packaged SDK and what it builds on.

`publish_check.py` leaves each SDK crate's `.crate` in
`target/package/tmp-registry/` (`cargo package`, in `target/package/`). This
writes a local registry (Cargo's `local-registry` source: an index plus the
`.crate` files) with those crates and the crates.io packages they resolve to
without the workspace's `[patch]`. That set is not the workspace lockfile's:
a crates.io original can pull what its patched copy trims. A throwaway probe
that depends on every packaged crate (patched to its unpacked package, so
nothing needs to be published) resolves against crates.io to find it; the
workspace lockfile's crates.io packages are added for optional features. An app outside the workspace
then builds on the SDK exactly as it would once published, with version
requirements and no `[patch]`, by replacing crates.io with the registry:

    cargo build \\
      --config 'source.crates-io.replace-with="tilo-sdk"' \\
      --config 'source.tilo-sdk.local-registry="<registry>"'

`.crate` files come from Cargo's download cache (hard-linked where the file
system allows) or crates.io; index entries for crates.io packages come from
Cargo's index cache or index.crates.io. Nothing is published.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import shutil
import sys
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[2]
CARGO_HOME = pathlib.Path(os.environ.get("CARGO_HOME", pathlib.Path.home() / ".cargo"))
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
USER_AGENT = "tilo-local-registry (https://code.tilo.so/tilo/tilo)"


def index_path(name: str) -> pathlib.Path:
    name = name.lower()
    if len(name) <= 2:
        return pathlib.Path(str(len(name))) / name
    if len(name) == 3:
        return pathlib.Path("3") / name[0] / name
    return pathlib.Path(name[:2]) / name[2:4] / name


def fetch(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def crates_io_versions(name: str) -> dict[str, dict]:
    """Every index entry crates.io has for `name`, by version."""
    entries: dict[str, dict] = {}
    for cache in (CARGO_HOME / "registry" / "index").glob("index.crates.io-*/.cache"):
        path = cache / index_path(name)
        if path.is_file():
            parts = path.read_bytes()[5:].split(b"\0")
            # etag, then version and entry pairs
            for line in parts[2::2]:
                if line:
                    entry = json.loads(line)
                    entries[entry["vers"]] = entry
    return entries or fetch_versions(name)


def compatible(a: str, b: str) -> bool:
    """Whether versions `a` and `b` are semver-compatible (same leftmost non-zero part)."""
    left, right = a.split("+")[0].split("-")[0].split("."), b.split("+")[0].split("-")[0].split(".")
    for x, y in zip(left, right):
        if x != y:
            return False
        if x != "0":
            return True
    return True


def place_crate(name: str, version: str, checksum: str, out: pathlib.Path) -> None:
    target = out / f"{name}-{version}.crate"
    if target.exists():
        if hashlib.sha256(target.read_bytes()).hexdigest() != checksum:
            raise SystemExit(f"{name} {version}: checksum mismatch in {target}")
        return
    for cache in (CARGO_HOME / "registry" / "cache").glob("index.crates.io-*"):
        source = cache / f"{name}-{version}.crate"
        if source.is_file():
            if hashlib.sha256(source.read_bytes()).hexdigest() != checksum:
                raise SystemExit(f"{name} {version}: checksum mismatch in {source}")
            try:
                os.link(source, target)
            except FileExistsError:
                if hashlib.sha256(target.read_bytes()).hexdigest() != checksum:
                    raise SystemExit(f"{name} {version}: checksum mismatch in {target}")
            except OSError:
                shutil.copyfile(source, target)
            return
    data = fetch(f"https://static.crates.io/crates/{name}/{name}-{version}.crate")
    if hashlib.sha256(data).hexdigest() != checksum:
        raise SystemExit(f"{name} {version}: checksum mismatch from crates.io")
    target.write_bytes(data)


def probe_lock(crates: list[pathlib.Path], unpacked: pathlib.Path) -> dict:
    """The lockfile of a project that depends on every packaged crate, each
    patched to its unpacked package, resolved against crates.io."""
    names = [crate.name.removesuffix(".crate") for crate in crates]
    dependencies, patches = [], []
    for stem in names:
        name, _, version = stem.rpartition("-")
        dependencies.append(f'{name} = "={version}"\n')
        patches.append(f"{name} = {{ path = {json.dumps(str(unpacked / stem))} }}\n")
    with tempfile.TemporaryDirectory(prefix="tilo-sdk-probe-") as directory:
        root = pathlib.Path(directory)
        (root / "lib.rs").write_text("")
        (root / "Cargo.toml").write_text(
            '[package]\nname = "tilo-sdk-probe"\nversion = "0.0.0"\nedition = "2024"\n'
            'publish = false\n\n[lib]\npath = "lib.rs"\n\n[workspace]\n\n[dependencies]\n'
            + "".join(dependencies)
            + "\n[patch.crates-io]\n"
            + "".join(patches)
        )
        subprocess.run(["cargo", "generate-lockfile"], cwd=root, check=True)
        return tomllib.loads((root / "Cargo.lock").read_text())


def packaged_entry(crate: pathlib.Path) -> dict:
    """The index entry for a packaged `.crate`, from its normalized manifest."""
    stem = crate.name.removesuffix(".crate")
    with tarfile.open(crate) as archive:
        manifest = tomllib.load(archive.extractfile(f"{stem}/Cargo.toml"))
    package = manifest["package"]
    deps = []
    tables = [(None, manifest)]
    tables += [(target, table) for target, table in manifest.get("target", {}).items()]
    for target, table in tables:
        for key, kind in (
            ("dependencies", "normal"),
            ("build-dependencies", "build"),
            ("dev-dependencies", "dev"),
        ):
            for alias, spec in table.get(key, {}).items():
                spec = {"version": spec} if isinstance(spec, str) else spec
                dep = {
                    "name": alias,
                    "req": spec.get("version", "*"),
                    "features": spec.get("features", []),
                    "optional": spec.get("optional", False),
                    "default_features": spec.get("default-features", True),
                    "target": target,
                    "kind": kind,
                }
                if "package" in spec:
                    dep["package"] = spec["package"]
                if "registry-index" in spec:
                    dep["registry"] = spec["registry-index"]
                deps.append(dep)
    features, features2 = {}, {}
    for feature, values in manifest.get("features", {}).items():
        new_syntax = any(value.startswith("dep:") or "?/" in value for value in values)
        (features2 if new_syntax else features)[feature] = values
    entry = {
        "name": package["name"],
        "vers": package["version"],
        "deps": deps,
        "cksum": hashlib.sha256(crate.read_bytes()).hexdigest(),
        "features": features,
        "yanked": False,
        "links": package.get("links"),
        "v": 2 if features2 else 1,
    }
    if features2:
        entry["features2"] = features2
    if "rust-version" in package:
        entry["rust_version"] = package["rust-version"]
    return entry


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("registry", type=pathlib.Path, help="directory to write the registry to")
    parser.add_argument(
        "--packages",
        type=pathlib.Path,
        help="where the packaged .crate files are (default: target/package/tmp-registry, "
        "else target/package)",
    )
    args = parser.parse_args()
    out = args.registry.resolve()
    packages = args.packages or next(
        (
            directory
            for directory in (REPO / "target/package/tmp-registry", REPO / "target/package")
            if any(directory.glob("*.crate"))
        ),
        REPO / "target/package",
    )
    ours = sorted(packages.glob("*.crate"))
    if not ours:
        raise SystemExit(f"no .crate files in {packages}; run publish_check.py first")
    if args.registry.is_symlink() or (out.exists() and not out.is_dir()):
        raise SystemExit(f"registry must be a directory, not a link or file: {args.registry}")
    if any(path.resolve().is_relative_to(out) for path in (packages, REPO, CARGO_HOME)):
        raise SystemExit("registry must not contain its package inputs, repository or Cargo cache")
    out.parent.mkdir(parents=True, exist_ok=True)
    # Failed resolution/downloads must leave the working registry intact.
    # Staging beside it keeps the publication renames on one filesystem.
    directory = pathlib.Path(tempfile.mkdtemp(prefix=".tilo-registry-", dir=out.parent))
    backup = directory / "old"
    try:
        staged = directory / "new"
        build_registry(staged, ours)
        if out.exists():
            out.rename(backup)
        try:
            staged.rename(out)
        except OSError:
            if backup.exists():
                backup.rename(out)
            raise
        if backup.exists():
            shutil.rmtree(backup)
    finally:
        if backup.exists():
            print(f"previous registry retained at {backup}", file=sys.stderr)
        else:
            shutil.rmtree(directory)
    print(f"registry: {out}")
    return 0


def build_registry(out: pathlib.Path, ours: list[pathlib.Path]) -> None:
    (out / "index").mkdir(parents=True)
    entries: dict[str, list[dict]] = {}
    for crate in ours:
        entry = packaged_entry(crate)
        entries.setdefault(entry["name"], []).append(entry)
        shutil.copyfile(crate, out / crate.name)
    print(f"{len(ours)} packaged SDK crates")

    wanted: set[tuple[str, str, bool]] = set()
    for package in probe_lock(ours, REPO / "target/package")["package"]:
        if package.get("source") == CRATES_IO:
            wanted.add((package["name"], package["version"], True))
    lock = tomllib.loads((REPO / "Cargo.lock").read_text())
    root = tomllib.loads((REPO / "Cargo.toml").read_text())
    patched = set(root.get("patch", {}).get("crates-io", {}))
    for package in lock["package"]:
        if package["name"] in entries:
            continue
        source = package.get("source")
        if source == CRATES_IO:
            wanted.add((package["name"], package["version"], True))
        elif package["name"] in patched and (source is None or source.startswith("git+")):
            # A published SDK crate gets the crates.io original of a patch.
            wanted.add((package["name"], package["version"], False))
    names = sorted({name for name, _, _ in wanted})
    with concurrent.futures.ThreadPoolExecutor(16) as pool:
        index = dict(zip(names, pool.map(crates_io_versions_or_none, names)))
    chosen: list[dict] = []
    for name, version, exact in sorted(wanted):
        versions = index.get(name) or {}
        if exact and version not in versions:
            versions = index[name] = fetch_versions(name)
        if version in versions:
            chosen.append(versions[version])
        elif not exact:
            candidates = [
                v for v, e in versions.items() if compatible(v, version) and not e.get("yanked")
            ]
            if candidates:
                chosen.append(versions[max(candidates, key=version_key)])
        else:
            raise SystemExit(f"{name} {version} is locked but not on crates.io")
    # An exact lock entry and a patch's compatible pick can name the same
    # release; placing it twice races two threads onto one file.
    chosen = list({(entry["name"], entry["vers"]): entry for entry in chosen}.values())
    with concurrent.futures.ThreadPoolExecutor(16) as pool:
        list(
            pool.map(
                lambda entry: place_crate(entry["name"], entry["vers"], entry["cksum"], out),
                chosen,
            )
        )
    for entry in chosen:
        entries.setdefault(entry["name"], []).append(entry)
    print(f"{len(chosen)} crates.io packages")
    for name, versions in entries.items():
        path = out / "index" / index_path(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("".join(json.dumps(entry, separators=(",", ":")) + "\n" for entry in versions))


def fetch_versions(name: str) -> dict[str, dict]:
    lines = fetch(f"https://index.crates.io/{index_path(name).as_posix()}").splitlines()
    return {entry["vers"]: entry for entry in map(json.loads, lines)}


def crates_io_versions_or_none(name: str) -> dict[str, dict] | None:
    try:
        return crates_io_versions(name)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def version_key(version: str) -> tuple:
    core = version.split("+")[0]
    release, _, pre = core.partition("-")
    return (tuple(int(part) for part in release.split(".")), pre == "", pre)


if __name__ == "__main__":
    sys.exit(main())
