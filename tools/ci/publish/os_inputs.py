#!/usr/bin/env python3
"""Stage the app collection and desktop payload a release published, for the OS image to embed.

    os_inputs.py apps ARCH DEST [--pointer FILE] [--source COMMIT]
    os_inputs.py desktop ARCH DEST [--pointer FILE] [--source COMMIT]

Reads the `app-collection` or `desktop-payload` pointer (tools/ci/publish/pointers.py: FILE, else
$T_POINTER_<NAME>), downloads what it names for ARCH, and refuses it unless every check passes:

- the archive's SHA-256 is the pointer's;
- it holds only what such an archive may hold, every member a plain file or directory (an app
  collection through tools/apps/check-collection-members.sh, the one allowlist of its members);
- every file in it is in its SHA256SUMS with that digest;
- it was built for ARCH from the commit the pointer names, and with --source, that commit is
  COMMIT;
- an app collection's runtime names the build its apps link (`<release>+native.<digest>`) and
  its publisher. Its tapps are signed by the app publisher and its runtime by the runtime
  publisher: the image build verifies those signatures when it installs them
  (os/scripts/pack-apps.sh, `tilo-runtime-bundle verify`), as it does for every prebuilt app.

Then DEST holds what os/image/compile.sh takes instead of compiling (TILO_APP_ARTIFACTS,
TILO_APP_RUNTIME, TILO_DESKTOP_ARTIFACTS): `app-artifacts/*.tapp` and `app-runtime/`, or
`desktop-artifacts/` (the four binaries and the payload's manifest), with `app-collection.json` or
`desktop-payload.json` beside them saying what was taken.

Exit status 3: the pointer is not set, or names nothing for ARCH; 1: it was refused.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pointers  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]
MEMBERS_CHECK = ROOT / "tools/apps/check-collection-members.sh"
DESKTOP_BINARIES = ("shell", "tilo-launcher", "tilo-runtime-mcp", "tilo-unlock-helper")
DESKTOP_MEMBERS = {"SHA256SUMS", "manifest.json", *DESKTOP_BINARIES}
HEX64 = re.compile(r"[0-9a-f]{64}")


class Refused(Exception):
    """What a pointer names can't be embedded."""


def fetch(url: str, dest: Path) -> None:
    """Download URL to DEST over HTTPS only, redirects included. Generic packages of tilo are
    anonymously readable, and the SHA-256 the pointer holds pins the bytes."""
    result = subprocess.run(["curl", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2",
                             "--fail", "--location", "--retry", "3", "--silent", "--show-error",
                             "--output", str(dest), url], capture_output=True, text=True)
    if result.returncode != 0:
        raise Refused(f"could not download {url}: {result.stderr.strip()}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(what: str, entry: dict, work: Path) -> Path:
    archive = work / "archive.tar.gz"
    fetch(entry["url"], archive)
    actual = sha256(archive)
    if actual != entry["sha256"]:
        raise Refused(f"the {what} at {entry['url']} has SHA-256 {actual}, not the pointer's {entry['sha256']}")
    return archive


def unpack(what: str, archive: Path, into: Path) -> list[str]:
    """Extract ARCHIVE into INTO; its member names as `tar -t` lists them (directories with a
    trailing slash). Links, devices and anything outside INTO are refused."""
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        for member in members:
            if not (member.isfile() or member.isdir()):
                raise Refused(f"the {what} holds {member.name}, which is not a plain file or directory")
        names = [m.name + "/" if m.isdir() and not m.name.endswith("/") else m.name for m in members]
        try:
            tar.extractall(into, filter="data")
        except tarfile.FilterError as error:
            raise Refused(f"the {what} holds an unsafe member: {error}") from None
    return names


def check_sums(what: str, root: Path) -> None:
    """Every file under ROOT but SHA256SUMS is listed there, with its digest."""
    sums = root / "SHA256SUMS"
    if not sums.is_file():
        raise Refused(f"the {what} has no SHA256SUMS")
    listed = {}
    for line in sums.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *](.+)", line)
        if not match or match.group(2).startswith("/") or ".." in Path(match.group(2)).parts:
            raise Refused(f"the {what}'s SHA256SUMS has a line that is not a digest of a file in it: {line!r}")
        listed[match.group(2)] = match.group(1)
    files = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()} - {"SHA256SUMS"}
    if set(listed) != files:
        raise Refused(f"the {what}'s SHA256SUMS lists {sorted(set(listed) - files)} and leaves out "
                      f"{sorted(files - set(listed))}")
    for name, digest in sorted(listed.items()):
        if sha256(root / name) != digest:
            raise Refused(f"{name} in the {what} is not what its SHA256SUMS says")


def recorded(what: str, document: Path, arch: str, source: str) -> None:
    """DOCUMENT (the archive's index.json or manifest.json) names ARCH and SOURCE."""
    try:
        value = json.loads(document.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise Refused(f"the {what} has no readable {document.name}: {error}") from None
    if value.get("architecture") != arch:
        raise Refused(f"the {what} is for {value.get('architecture')!r}, not {arch}")
    if value.get("source") != source:
        raise Refused(f"the {what} records source {value.get('source')!r}, not the pointer's {source}")


def resolve(name: str, arch: str, pointer_file: Path | None, source: str | None) -> tuple[dict, dict]:
    pointer = pointers.read(name, pointer_file)
    entry = pointers.entry(pointer, arch)
    if pointer["source"] is None:
        raise Refused(f"the {name} pointer names no source commit, so nothing shows whose {name} it is")
    if source is not None and pointer["source"] != source:
        raise Refused(f"the {name} pointer is from {pointer['source']}, not {source}")
    return pointer, entry


def provenance(dest: Path, name: str, pointer: dict, entry: dict, arch: str) -> None:
    record = {"pointer": name, "architecture": arch, "url": entry["url"], "sha256": entry["sha256"],
              "source": pointer["source"]}
    if "builder" in pointer:
        record["builder"] = pointer["builder"]
    (dest / f"{name}.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def stage_apps(arch: str, dest: Path, pointer_file: Path | None = None, source: str | None = None) -> dict:
    pointer, entry = resolve("app-collection", arch, pointer_file, source)
    what = f"{arch} app collection"
    with tempfile.TemporaryDirectory(prefix="os-inputs-apps.") as temp:
        work = Path(temp)
        archive = download(what, entry, work)
        tree = work / "tree"
        tree.mkdir()
        names = unpack(what, archive, tree)
        members = subprocess.run(["bash", str(MEMBERS_CHECK), "--require-tapp", "app collection"],
                                 input="".join(n + "\n" for n in names), capture_output=True, text=True)
        if members.returncode != 0:
            raise Refused(members.stderr.strip())
        check_sums(what, tree)
        recorded(what, tree / "index.json", arch, pointer["source"])
        runtimes = sorted((tree / "runtime/tilo-runtime").glob("*+native.*"))
        if not runtimes or not all((r / "bundle.toml").is_file() for r in runtimes):
            raise Refused(f"the {what} has no runtime build its apps can name (<release>+native.<digest>)")
        publisher = tree / "runtime/runtime-bundle-publisher.hex"
        if not publisher.is_file() or not HEX64.fullmatch(publisher.read_text(encoding="utf-8").strip()):
            raise Refused(f"the {what} names no runtime publisher")
        tapps = sorted((tree / "tapp").glob("*.tapp"))
        for stale in ("app-artifacts", "app-runtime"):
            shutil.rmtree(dest / stale, ignore_errors=True)
        (dest / "app-artifacts").mkdir(parents=True)
        for tapp in tapps:
            shutil.copy2(tapp, dest / "app-artifacts" / tapp.name)
        shutil.copytree(tree / "runtime", dest / "app-runtime")
    provenance(dest, "app-collection", pointer, entry, arch)
    return {"source": pointer["source"], "tapps": len(tapps)}


def stage_desktop(arch: str, dest: Path, pointer_file: Path | None = None, source: str | None = None) -> dict:
    pointer, entry = resolve("desktop-payload", arch, pointer_file, source)
    what = f"{arch} desktop payload"
    with tempfile.TemporaryDirectory(prefix="os-inputs-desktop.") as temp:
        work = Path(temp)
        archive = download(what, entry, work)
        tree = work / "tree"
        tree.mkdir()
        names = set(unpack(what, archive, tree))
        if names != DESKTOP_MEMBERS:
            raise Refused(f"the {what} holds {sorted(names)}, not exactly {sorted(DESKTOP_MEMBERS)}")
        check_sums(what, tree)
        recorded(what, tree / "manifest.json", arch, pointer["source"])
        out = dest / "desktop-artifacts"
        shutil.rmtree(out, ignore_errors=True)
        out.mkdir(parents=True)
        for binary in DESKTOP_BINARIES:
            shutil.copyfile(tree / binary, out / binary)
            (out / binary).chmod(0o755)
        shutil.copyfile(tree / "manifest.json", out / "manifest.json")
    provenance(dest, "desktop-payload", pointer, entry, arch)
    return {"source": pointer["source"]}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("what", choices=("apps", "desktop"))
    parser.add_argument("arch", choices=pointers.ARCHES)
    parser.add_argument("dest", type=Path)
    parser.add_argument("--pointer", type=Path, help="the pointer file (default: $T_POINTER_<NAME>)")
    parser.add_argument("--source", help="refuse a pointer published from another commit")
    args = parser.parse_args(argv)
    if args.source is not None and not pointers.COMMIT.fullmatch(args.source):
        parser.error(f"--source {args.source!r} is not a commit")
    stage = stage_apps if args.what == "apps" else stage_desktop
    try:
        args.dest.mkdir(parents=True, exist_ok=True)
        staged = stage(args.arch, args.dest.resolve(), args.pointer, args.source)
    except pointers.Unset as unset:
        print(f"os_inputs: {unset}", file=sys.stderr)
        return 3
    except (pointers.PointerError, Refused) as error:
        print(f"os_inputs: refused: {error}", file=sys.stderr)
        return 1
    detail = f", {staged['tapps']} apps" if "tapps" in staged else ""
    print(f"os_inputs: staged the published {args.what} of {staged['source']} for {args.arch}{detail} in {args.dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
