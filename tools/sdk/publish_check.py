#!/usr/bin/env python3
"""What stands between Tilo's builder crates and crates.io, and their publication.

The builder crates are every workspace crate in the dependency closure of
ROOTS (the toolkit, the app SDK and tilolang: its compiler, runtime, lexer,
command and browser build), including the forks they depend on under their
own names (tilo-winit, tilo-accesskit-winit, tilo-vello*, tilo-taffy,
tilo-cosmic-text, tilo-automerge-fork). For each one this prints what publishing still needs:

- `publish = false` (the workspace default);
- a missing `description`, `license` or `repository`;
- path dependencies without a `version`;
- dependencies on crates the workspace patches whose crates.io original the
  crates cannot build on (see UPSTREAM_SUFFICES): a published crate gets the
  crates.io original, so such a patch must first be published as a `tilo-`
  fork (or upstreamed);
- git dependencies, which crates.io refuses.

Each crate is in one of four states:

  ready    marked `publish = true`, nothing blocks it and every crate it
           depends on is ready: it is packaged and published.
  waiting  marked, and its own manifest is complete, but it uses a patched
           crate that is not published yet, or depends on a crate that is
           not ready. It joins the set once those are.
  pending  not marked (`publish = false`).
  BLOCKED  marked, but its own manifest is not publishable (no description,
           a path dependency without a version, a git dependency...). That
           is a mistake in the tree, never something to wait for, and it
           fails every mode but `--report-only`.

Every published crate carries one version: the release tag's (vX.Y.Z
publishes X.Y.Z). Nothing in the tree carries it; for the length of a run
every manifest of the workspace is rewritten so each ready crate is at that
version and every dependency on one asks for exactly it (`=X.Y.Z`), and the
rewrite is read back through `cargo metadata` before anything is packaged.
One version, pinned exactly, is what makes the crates one release: a project
that names two of them cannot get two releases' halves.

Modes:

  --report-only  the report alone (t's sdk-policy check): BLOCKED crates are
                 a note there, so a half-made manifest never stops unrelated
                 work.
  (default)      the report, then every ready crate packaged together with
                 `cargo publish --dry-run` at the stand-in version 0.0.0, or
                 at the tag's on a release tag (Cargo's local overlay
                 registry resolves their dependencies on each other), and
                 each package's size checked against crates.io's limit.
                 `--verify` also builds every package from its packaged
                 sources alone, in dependency order, against crates.io and
                 the packages before it. The `.crate` files stay in
                 `target/package/`, where tools/sdk/local_registry.py builds
                 a registry from them.
  --publish      the same at the release tag's version, then `cargo publish`
                 of every ready crate not yet on crates.io at that version,
                 in dependency order, with CARGO_REGISTRY_TOKEN. A crate
                 already there is skipped, so re-running a tag finishes a
                 release that stopped halfway (crates.io limits how fast new
                 names are taken: a first release stops on that limit and a
                 later run carries on).

`--check-names` asks crates.io about every name: one that exists and is not
ours (its repository is not this one) fails, since a project resolving our
version would get another owner's crate.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[2]
REPOSITORY = "https://code.tilo.so/tilo/tilo"
USER_AGENT = f"tilo-publish-check ({REPOSITORY})"
# The entry crates: Rust apps depend on `tilo-app` (and the toolkit, `tilo-ui`, for what they
# draw themselves); tilolang is its compiler, runtime and lexer, the `tilolang` command
# (tilolang-cli) and the browser build (tilolang-web). A root that is not in the workspace is
# skipped. The web entry's sign-in runtime (tilo-browser-runtime) is tilo.sh's own page, not
# something anyone builds on, so it is not one.
ROOTS = (
    "tilo-ui",
    "tilo-app",
    "tilolang",
    "tilolang-runtime",
    "tilolang-lex",
    "tilolang-cli",
    "tilolang-web",
)
# The version a run that releases nothing packages at: no release carries it.
STAND_IN = "0.0.0"
# crates.io refuses an upload larger than this (its `max_upload_size`, 10 MiB).
MAX_CRATE_BYTES = 10 * 1024 * 1024


def metadata(repo: pathlib.Path, *, locked: bool = True) -> dict:
    command = ["cargo", "metadata", "--format-version", "1"]
    if locked:
        command.append("--locked")
    done = subprocess.run(command, cwd=repo, check=False, capture_output=True, text=True)
    if done.returncode != 0:
        raise SystemExit(f"`{' '.join(command)}` in {repo} failed:\n{done.stderr}")
    return json.loads(done.stdout)


def sdk_closure(meta: dict) -> set[str]:
    packages = {package["id"]: package for package in meta["packages"]}
    nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}
    pending = [
        package_id
        for package_id, package in packages.items()
        if package["name"] in ROOTS and package["source"] is None
    ]
    seen: set[str] = set()
    while pending:
        package_id = pending.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        for dep in nodes[package_id]["deps"]:
            if any(kind["kind"] in (None, "build") for kind in dep["dep_kinds"]):
                pending.append(dep["pkg"])
    return {packages[package_id]["name"] for package_id in seen}

# Patched crates whose crates.io original a published crate builds on
# unchanged. Every patch in the closure must be listed here or be a
# blocker; the reason says why the original suffices. wgpu is not one:
# tilo-vello compiles its shaders with `Device::create_compute_pipeline_async`,
# which only the patched wgpu has, so it waits for a published fork.
UPSTREAM_SUFFICES = {
    "accesskit": "dependency trim (zbus's async-io backend follows a feature); same API",
    "accesskit_atspi_common": "dependency trim; the crates.io release, patched only to stay one copy",
    "accesskit_consumer": "dependency trim; the crates.io release, patched only to stay one copy",
    "accesskit_unix": "dependency trim (zbus's async-io backend follows a feature) and a "
    "read-only tree only Tilo's launcher turns on (TILO_ATSPI_READ_ONLY); same API",
    "atspi": "dependency trim (async-io or tokio selects zbus's backend); same API",
    "atspi-common": "dependency trim; same API",
    "atspi-proxies": "dependency trim; same API",
    "genawaiter-proc-macro": "drops the unmaintained proc-macro-error; same macro output",
    "hexane": "automerge's column store giving back the capacity a split slab no longer uses; "
    "same API, an app on the release holds more memory after long edits",
    "pulldown-cmark": "code size only: the HTML entity table without relocated pointers; same API",
    "regex-syntax": "code size only: Unicode tables as string literals and offsets; same API",
    "zbus": "one worker thread for the blocking API's runtime instead of one per CPU; same API, "
    "an app on the release keeps more idle threads after a blocking D-Bus call",
    "libspa-sys": "Fedora 44 PipeWire header fix; only the optional `pipewire` feature builds it",
    "serde_json": "code size only: generic `Value` code compiled once, in serde_json, for apps that "
    "share one runtime library; same API and behaviour (vendor/serde_json/TILO-PATCH.md)",
    "rustls-native-certs": "Tilo OS's single trust bundle; the original's discovery is right elsewhere",
    "wayland-backend": "a compositor (server) fix; SDK apps are Wayland clients",
    "wgpu-hal": "Metal presentation observer (only the optional `presentation-feedback` "
    "feature uses it), the Windows DX12 shader cache and a Vulkan device-loss switch only tests "
    "set; SDK apps use none of them. It also compiles Metal shaders without fast math; on the "
    "release a partial redraw on macOS can differ from a full one by a channel step at edges",
    "workspace-hack": "hakari's feature union for this workspace; crates.io's "
    "workspace-hack 0.1.0 is its empty placeholder",
}

# The forks the crates depend on under their own names. They sit outside the
# workspace (vendored sources are neither linted nor formatted as Tilo code),
# so packaging makes them members for its duration: Cargo packages several
# crates together only within one workspace.
FORKS = (
    "vendor/winit",
    "vendor/accesskit_winit",
    "vendor/vello",
    "vendor/vello_encoding",
    "vendor/vello_shaders",
    "vendor/automerge",
    "vendor/taffy",
    "vendor/cosmic-text",
)


def own_blockers(package: dict, local: set[str]) -> list[str]:
    """What the crate's own manifest lacks to be published: fixed in that manifest, by whoever
    marked it publishable."""
    found = []
    for field in ("description", "license", "repository"):
        if not package.get(field):
            found.append(f"no {field}")
    for dep in package["dependencies"]:
        if dep["kind"] == "dev":
            continue
        name = dep["name"]
        if dep.get("path") and dep["req"] == "*":
            found.append(f"path dependency {name} has no version")
        if dep.get("source", "") and dep["source"].startswith("git+"):
            found.append(f"git dependency {name}")
        if name in local and not dep.get("path"):
            found.append(f"workspace dependency {name} is not by path")
    return sorted(set(found))


def patch_blockers(package: dict, patched: set[str]) -> list[str]:
    """The patched crates it builds on whose crates.io original does not suffice: a fork of each
    must be published first."""
    return sorted({
        f"uses patched {dep['name']}"
        for dep in package["dependencies"]
        if dep["kind"] != "dev" and dep["name"] in patched and dep["name"] not in UPSTREAM_SUFFICES
    })


def needed_locals(package: dict, local: set[str]) -> set[str]:
    """The crates of the set this one cannot be published before: its normal and build
    dependencies by path, and its dev-dependencies that carry a version (Cargo keeps those in the
    published manifest; one without a version is dropped from it)."""
    return {
        dep["name"]
        for dep in package["dependencies"]
        if dep["name"] in local
        and dep.get("path")
        and (dep["kind"] != "dev" or dep["req"] != "*")
    }


def foreign_names(names: list[str]) -> set[str]:
    """Names crates.io holds for a crate whose repository is not this one."""
    foreign = set()
    for name in names:
        crate = crates_io(f"/crates/{name}")
        if crate is None:
            continue
        crate = crate.get("crate") if isinstance(crate, dict) else None
        if not isinstance(crate, dict) or not isinstance(crate.get("name"), str) or not crate["name"]:
            raise ValueError(f"crates.io returned invalid crate metadata for {name}; "
                             "cannot determine who holds the name")
        if (crate.get("repository") or "").rstrip("/") != REPOSITORY:
            foreign.add(name)
    return foreign


def crates_io(path: str) -> dict | None:
    """crates.io's API answer at PATH, or None where it has nothing there."""
    request = urllib.request.Request(
        f"https://crates.io/api/v1{path}", headers={"User-Agent": USER_AGENT}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def on_crates_io(name: str, version: str) -> bool:
    return crates_io(f"/crates/{name}/{version}") is not None


def release_version() -> str:
    """The release tag's version (vX.Y.Z, maybe with a pre-release part), from t's T_REF."""
    match = re.fullmatch(
        r"tag:v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)", os.environ.get("T_REF", "")
    )
    if not match:
        raise SystemExit(
            "the crates are released from a release tag (vX.Y.Z); this run is for "
            + (os.environ.get("T_REF") or "a working tree")
        )
    return match.group(1)


def scratch_tree() -> pathlib.Path:
    """A copy of the tree to package from, in the job's scratch: packaging rewrites manifests
    (the forks as members, every version), which t's jobs cannot do in their read-only sources
    and a developer's checkout must never be left holding. Build output, git and editor state
    stay behind; the copy's builds go to the original's target directory."""
    work = pathlib.Path(tempfile.mkdtemp(prefix="tilo-crates-", dir=os.environ.get("TMPDIR")))
    left_out = {"target", ".git", ".claude", "node_modules"}
    shutil.copytree(
        REPO,
        work / "tree",
        symlinks=True,
        ignore=lambda directory, names: [
            name for name in names if pathlib.Path(directory) == REPO and name in left_out
        ],
    )
    return work


def forks_as_members(tree: pathlib.Path) -> None:
    """Make FORKS members of TREE's workspace: Cargo packages several crates together only
    within one. A fork that is its own workspace (for its tests) is not one in TREE."""
    for fork in FORKS:
        manifest = tree / fork / "Cargo.toml"
        text = manifest.read_text()
        stripped = re.sub(r"(?m)^\[workspace\]\n", "", text)
        if stripped != text:
            manifest.write_text(stripped)
    manifest = tree / "Cargo.toml"
    text = manifest.read_text()
    for fork in FORKS:
        entry = f'    "{fork}",\n'
        if entry not in text:
            raise SystemExit(f"{fork} is not in the workspace's exclude list")
        text = text.replace(entry, "", 1)
    added = "".join(f'    "{fork}",\n' for fork in FORKS)
    text = text.replace("\nmembers = [\n", "\nmembers = [\n" + added, 1)
    manifest.write_text(text)


HEADER = re.compile(r"^\s*\[\s*([^\[\]]+?)\s*\]\s*(#.*)?$")
# An array of tables (`[[bin]]`, `[[test]]`): no dependency is written in one.
ARRAY = re.compile(r"^\s*\[\[")
DEP_TABLE = re.compile(r"(^|\.)(dev-|build-)?dependencies$")
PATH = re.compile(r'\bpath\s*=\s*"([^"]+)"')
VERSION = re.compile(r'\bversion\s*=\s*"[^"]*"')


def rewrite_manifest(text: str, base: pathlib.Path, published: set[pathlib.Path], version: str,
                     own: bool) -> str:
    """TEXT, a manifest in BASE, with its package at VERSION (when OWN) and every dependency by
    path on a directory of PUBLISHED asking for exactly VERSION where it names a version at all
    (a dependency without one is a blocker the report names, and is left so)."""
    request = f'version = "={version}"'

    def published_path(line: str) -> bool:
        match = PATH.search(line)
        return bool(match) and (base / match.group(1)).resolve() in published

    lines = text.splitlines(keepends=True)
    out: list[str] = []
    table = ""
    # A dependency written as its own table (`[dependencies.name]`): its lines, held until the
    # table ends, since its path may come after its version.
    held: list[str] | None = None

    def flush():
        nonlocal held
        if held is not None:
            if any(published_path(line) for line in held):
                held = [VERSION.sub(request, line, count=1) if re.match(r"\s*version\s*=", line)
                        else line for line in held]
            out.extend(held)
            held = None

    for line in lines:
        if ARRAY.match(line):
            flush()
            table = "[[]]"
            out.append(line)
            continue
        header = HEADER.match(line)
        if header:
            flush()
            table = header.group(1).replace(" ", "")
            dotted = table.rsplit(".", 1)
            if len(dotted) == 2 and DEP_TABLE.search(dotted[0]):
                held = [line]
                continue
            out.append(line)
            continue
        if held is not None:
            held.append(line)
            continue
        if own and table == "package":
            if re.match(r"\s*version\s*\.\s*workspace\s*=", line):
                raise SystemExit(f"{base}/Cargo.toml: the package's version comes from the "
                                 "workspace, which carries none")
            if re.match(r'\s*version\s*=\s*"', line):
                line = VERSION.sub(f'version = "{version}"', line, count=1)
        elif DEP_TABLE.search(table) and "{" in line and published_path(line):
            line = VERSION.sub(request, line, count=1)
        out.append(line)
    flush()
    return "".join(out)


def at_version(tree: pathlib.Path, names: set[str], version: str) -> None:
    """Every manifest of TREE's workspace rewritten so the crates NAMES are at VERSION and
    everything that depends on one by path and version asks for exactly it. The rewrite is read
    back through `cargo metadata`, so a dependency written in a form it missed fails here rather
    than in a published manifest."""
    meta = metadata(tree, locked=False)
    local = [package for package in meta["packages"] if package["source"] is None]
    published = {pathlib.Path(p["manifest_path"]).parent.resolve()
                 for p in local if p["name"] in names}
    manifests = {pathlib.Path(p["manifest_path"]): p["name"] in names for p in local}
    manifests[tree / "Cargo.toml"] = manifests.get(tree / "Cargo.toml", False)
    for manifest, own in manifests.items():
        text = manifest.read_text()
        new = rewrite_manifest(text, manifest.parent.resolve(), published, version, own)
        if new != text:
            manifest.write_text(new)
    staged = metadata(tree, locked=False)
    wrong = []
    for package in staged["packages"]:
        if package["source"] is not None:
            continue
        if package["name"] in names and package["version"] != version:
            wrong.append(f"{package['name']} is at {package['version']}")
        for dep in package["dependencies"]:
            if (dep["name"] in names and dep.get("path") and dep["req"] != "*"
                    and dep["req"] != f"={version}"):
                wrong.append(f"{package['name']} asks for {dep['name']} {dep['req']}")
    if wrong:
        raise SystemExit(f"the version {version} did not reach every manifest: "
                         + "; ".join(sorted(set(wrong))))


def packaged(target: pathlib.Path, name: str, version: str) -> pathlib.Path:
    stem = f"{name}-{version}.crate"
    found = [path for path in (target / "package" / stem, target / "package/tmp-registry" / stem)
             if path.is_file()]
    if not found:
        raise SystemExit(f"cargo left no {stem} in {target / 'package'}")
    return max(found, key=lambda path: path.stat().st_mtime)


def too_big(crates: dict[str, pathlib.Path]) -> list[str]:
    """Each package over crates.io's limit, with the files that make it so."""
    over = []
    for name, crate in sorted(crates.items()):
        size = crate.stat().st_size
        if size <= MAX_CRATE_BYTES:
            continue
        with tarfile.open(crate) as archive:
            members = sorted((m for m in archive.getmembers() if m.isfile()),
                             key=lambda m: m.size, reverse=True)[:10]
        files = "\n".join(f"      {m.size / 1048576:8.2f} MiB  {m.name}" for m in members)
        over.append(f"  {name}: {size / 1048576:.2f} MiB packed, over crates.io's "
                    f"{MAX_CRATE_BYTES // 1048576} MiB; its biggest files (unpacked):\n{files}")
    return over


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--report-only", action="store_true", help="the report alone")
    mode.add_argument("--publish", action="store_true",
                      help="publish at the release tag's version (needs CARGO_REGISTRY_TOKEN)")
    parser.add_argument("--verify", action="store_true",
                        help="also build every package from its packaged sources (slow)")
    parser.add_argument("--check-names", action="store_true",
                        help="fail on a name crates.io holds for another project")
    parser.add_argument("--version", help="package at this version (default: the release "
                        f"tag's on a tag, else {STAND_IN})")
    args = parser.parse_args()
    if args.publish:
        version = release_version()
        if args.version and args.version != version:
            raise SystemExit(f"a publication is at its tag's version {version}, not {args.version}")
        if not os.environ.get("CARGO_REGISTRY_TOKEN"):
            raise SystemExit('publishing needs secrets = ["CARGO_REGISTRY_TOKEN"]')
    elif args.version:
        version = args.version
    elif os.environ.get("T_REF", "").startswith("tag:"):
        version = release_version()
    else:
        version = STAND_IN

    meta = metadata(REPO)
    closure = sdk_closure(meta)
    root = tomllib.loads((REPO / "Cargo.toml").read_text())
    patched = set(root.get("patch", {}).get("crates-io", {}))
    local_packages = {
        package["name"]: package
        for package in meta["packages"]
        if package["source"] is None and package["name"] in closure
    }
    # A published manifest names its optional path dependencies too, even
    # ones no feature turns on here: each must be published as well.
    every_local = {p["name"]: p for p in meta["packages"] if p["source"] is None}
    pending = list(local_packages)
    while pending:
        for dep in every_local[pending.pop()]["dependencies"]:
            name = dep["name"]
            if dep["kind"] != "dev" and dep.get("path") and name not in local_packages:
                local_packages[name] = every_local[name]
                pending.append(name)
    sdk = {name: package for name, package in local_packages.items() if name not in patched}

    # Ready is a fixed point: a marked crate with nothing in its way, all of whose crates of the
    # set are ready too.
    marked = {name for name, package in sdk.items() if package.get("publish") != []}
    own = {name: own_blockers(sdk[name], set(sdk)) for name in sdk}
    patches = {name: patch_blockers(sdk[name], patched) for name in sdk}
    needs = {name: needed_locals(sdk[name], set(sdk)) for name in sdk}
    ready = {name for name in marked if not own[name] and not patches[name]}
    while True:
        dropped = {name for name in ready if not needs[name] <= ready}
        if not dropped:
            break
        ready -= dropped

    failing: list[str] = []
    print(f"Builder crate set: {len(sdk)} workspace crates ({len(closure)} packages in the "
          f"closure of {', '.join(ROOTS)}); {len(ready)} ready, at {version}")
    for name in sorted(sdk):
        if name in ready:
            print(f"  ready    {name}")
        elif name in marked and own[name]:
            failing.append(name)
            print(f"  BLOCKED  {name} (marked publish): {'; '.join(own[name] + patches[name])}")
        elif name in marked:
            waits = patches[name] + [f"needs {dep}" for dep in sorted(needs[name] - ready)]
            print(f"  waiting  {name}: {'; '.join(waits)}")
        else:
            print(f"  pending  {name}: {'; '.join(['publish = false'] + own[name] + patches[name])}")
    in_closure = sorted(name for name in patched if name in closure)
    unlisted = [name for name in in_closure if name not in UPSTREAM_SUFFICES]
    print("Patched crates a published crate builds on the crates.io original of:")
    for name in in_closure:
        if name in UPSTREAM_SUFFICES:
            print(f"  {name}: {UPSTREAM_SUFFICES[name]}")
    if unlisted:
        print(f"Patched crates the set needs as published forks: {', '.join(unlisted)}")
    if args.check_names:
        foreign = foreign_names(sorted(sdk))
        if foreign:
            # A marked crate would resolve to the other project's wherever our version is asked
            # for; one not marked yet must be renamed before it is.
            failing.extend(sorted(foreign & marked))
            print(f"Names crates.io holds for another project, to publish as tilo-* instead "
                  f"({len(foreign)}): " + ", ".join(
                      name + ("" if name in marked else " (not marked yet)")
                      for name in sorted(foreign)))
    if failing:
        # A report lists what blocks publication; only a run that packages fails on it.
        level = "note" if args.report_only else "error"
        print(f"{level}: marked publishable but not publishable: {', '.join(failing)}",
              file=sys.stderr)
        if not args.report_only:
            return 1
    if args.report_only or not ready:
        return 0

    target = pathlib.Path(meta["target_directory"])
    work = scratch_tree()
    try:
        return package(work / "tree", target, ready, version, args)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def package(tree: pathlib.Path, target: pathlib.Path, ready: set[str], version: str,
            args: argparse.Namespace) -> int:
    """Package READY from TREE, a copy of the tree, at VERSION; check each package's size, and
    publish what crates.io lacks with --publish. The builds go to TARGET, the tree's own."""
    os.environ["CARGO_TARGET_DIR"] = str(target)
    # Online, whatever the job's default: as members the forks bring dependencies the
    # workspace's lock file does not hold, and packaging resolves against crates.io as a
    # project using the packages will.
    os.environ["CARGO_NET_OFFLINE"] = "false"
    forks_as_members(tree)
    at_version(tree, ready, version)
    for name in ready:
        for stale in (target / "package" / f"{name}-{version}.crate",
                      target / "package/tmp-registry" / f"{name}-{version}.crate"):
            stale.unlink(missing_ok=True)
    # Not --locked: the version rewrite and the forks' dev-dependencies change the copy's lock
    # file. --allow-dirty: the copy holds no git history, and its rewrite is uncommitted anyway.
    command = ["cargo", "publish", "--dry-run", "--allow-dirty"]
    if not args.verify:
        command.append("--no-verify")
    for name in sorted(ready):
        command += ["-p", name]
    print("$ " + " ".join(command), flush=True)
    if subprocess.run(command, cwd=tree, check=False).returncode != 0:
        return 1
    crates = {name: packaged(target, name, version) for name in ready}
    over = too_big(crates)
    if over:
        print("error: packages crates.io would refuse:\n" + "\n".join(over), file=sys.stderr)
        return 1
    biggest = max(crates, key=lambda name: crates[name].stat().st_size)
    print(f"{len(crates)} packages at {version}, the biggest {biggest} at "
          f"{crates[biggest].stat().st_size / 1048576:.2f} MiB (crates.io takes up to "
          f"{MAX_CRATE_BYTES // 1048576} MiB)", flush=True)
    if not args.publish:
        return 0
    remaining = sorted(name for name in ready if not on_crates_io(name, version))
    for name in sorted(ready - set(remaining)):
        print(f"{name}@{version} is already on crates.io; skipping it")
    if not remaining:
        print(f"every ready crate is on crates.io at {version}")
        return 0
    # Cargo publishes several packages in dependency order and waits for each to be in the
    # index before the next. The token reaches cargo through CARGO_REGISTRY_TOKEN alone.
    command = ["cargo", "publish", "--allow-dirty", "--no-verify"]
    for name in remaining:
        command += ["-p", name]
    print("$ " + " ".join(command), flush=True)
    if subprocess.run(command, cwd=tree, check=False).returncode != 0:
        return 1
    print(f"{len(remaining)} crates published to crates.io at {version}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
