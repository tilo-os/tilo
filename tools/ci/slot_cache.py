#!/usr/bin/env python3
"""Keep a Rust job's `target/` and Cargo downloads on its runner slot.

Every Forgejo runner slot runs one job at a time on a persistent Docker store.
A job that names a slot-cache volume (check.yml's Rust jobs mount
`tilo-slot-cache-<trust>` at /ci-slot-cache) gets that volume bind-mounted at
the ordinary `<workspace>/target` and under `$CARGO_HOME`, so paths, and with
them sccache keys, are exactly those of a cold job. `CARGO_TARGET_DIR` stays
unset: it is part of sccache's key (DEVELOPMENT.md).

  slot_cache.py mount --trust <trusted|untrusted>   after checkout, before cargo
  slot_cache.py seal                                last step, `if: always()`

The volume name carries the trust class, so a pull request's build scripts
never write a directory that a push to main builds from. Publications do not
use slot caches at all.

Checkout gives every file and directory a new mtime, and Cargo rebuilds a
workspace crate whose sources are newer than its artifacts. `mount` restores
the mtime a file had in the previous job when its Git blob is unchanged, and a
directory's when its tracked entries are, from a manifest that
only `seal` makes valid: it drops every file a step modified after `mount`, and
a job that never seals (cancelled, killed) leaves no manifest, so the next job
rebuilds workspace crates instead of trusting artifacts of unknown sources.

A cache never decides an outcome: when the volume is absent or unusable the
job builds cold, as before.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

LAYOUT = "v1"
GIB = 1 << 30
# The store must have room for a target tree beside the slot's images; the
# small quality slots (20 GiB stores) never qualify.
MIN_STORE_BYTES = 100 * GIB
# One key directory at most this big; beyond it the next job starts empty.
MAX_TARGET_BYTES = 60 * GIB
# Periodic wipe: dependency churn leaves artifacts nothing selects any more.
MAX_TARGET_AGE = 7 * 86400
# Store usage above which only the current key directory is kept, and above
# which even that is wiped before the job builds.
PRESSURE_PERCENT = 75
# Other key directories (an older builder image) kept for jobs still on it.
KEEP_OTHER_KEYS = 1
# Cargo's own entries; anything else under target/ (nextest reports, CI
# receipts, CARGO_TARGET_TMPDIR, imager fixtures) belongs to one job.
CARGO_ENTRIES = {
    "debug", "release", "thin-app", "demo", "CACHEDIR.TAG", ".rustc_info.json",
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
}
META = ".tilo-slot-cache.json"
SEALED = ".tilo-mtimes"
PENDING = ".tilo-mtimes.pending"


def log(message: str) -> None:
    print(f"[slot-cache] {message}", flush=True)


def store_usage(path: Path) -> tuple[int, int]:
    stat = os.statvfs(path)
    total = stat.f_blocks * stat.f_frsize
    free = stat.f_bavail * stat.f_frsize
    return total, total - free


def used_percent(path: Path) -> int:
    total, used = store_usage(path)
    return int(100 * used / total) if total else 100


def tree_bytes(path: Path) -> int:
    """Allocated bytes under path; unknown sizes count as over the cap."""
    try:
        out = subprocess.run(["du", "-skx", str(path)], capture_output=True, text=True,
                             timeout=120, check=True).stdout
        return int(out.split()[0]) * 1024
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return MAX_TARGET_BYTES + 1


def toolchain_key() -> str:
    """Name what Cargo's fingerprints cannot see: the compiler and system libraries.

    A builder image with other -sys library versions invalidates build-script
    outputs (pkg-config probes, bindgen) that Cargo would otherwise call fresh.
    """
    digest = hashlib.sha256()
    for command in (["rustc", "-vV"], ["uname", "-m"], ["rpm", "-qa", "--qf", "%{NEVRA}\\n"]):
        try:
            out = subprocess.run(command, capture_output=True, text=True, timeout=120).stdout
        except (OSError, subprocess.SubprocessError):
            out = ""
        digest.update("\n".join(sorted(out.splitlines())).encode())
        digest.update(b"\0")
    return digest.hexdigest()[:16]


def read_meta(directory: Path) -> dict:
    try:
        return json.loads((directory / META).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def write_json(path: Path, value: dict) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, sort_keys=True), encoding="utf-8")
    os.replace(temporary, path)


def discard(path: Path, trash: Path, background: bool) -> None:
    """Remove path; in the background it only costs a rename now."""
    if not path.exists():
        return
    if not background:
        shutil.rmtree(path, ignore_errors=True)
        return
    trash.mkdir(parents=True, exist_ok=True)
    target = trash / f"{path.name}-{time.time_ns()}"
    os.replace(path, target)


def empty_trash(trash: Path) -> None:
    if trash.is_dir() and any(trash.iterdir()):
        # Outlives this step: the job container keeps running until the job ends,
        # and whatever is left is removed by the next job's call.
        subprocess.Popen(["rm", "-rf", "--one-file-system", *map(str, trash.iterdir())],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)


def select_target(root: Path, key: str, now: float) -> tuple[Path, str]:
    """Return the key directory for this job, applying age, size and disk limits."""
    targets = root / "target"
    trash = root / "trash"
    targets.mkdir(parents=True, exist_ok=True)
    current = targets / key
    reason = "warm" if current.is_dir() else "new"

    others = sorted(
        (d for d in targets.iterdir() if d.is_dir() and d.name != key),
        key=lambda d: read_meta(d).get("last_used", 0), reverse=True)
    for stale in others[KEEP_OTHER_KEYS:]:
        log(f"dropping key {stale.name}: older than the {KEEP_OTHER_KEYS} kept")
        discard(stale, trash, background=True)
    if used_percent(root) >= PRESSURE_PERCENT:
        for other in others[:KEEP_OTHER_KEYS]:
            log(f"dropping key {other.name}: store at {used_percent(root)}%")
            discard(other, trash, background=False)
        shutil.rmtree(trash, ignore_errors=True)

    if current.is_dir():
        meta = read_meta(current)
        age = now - meta.get("created", 0)
        if age > MAX_TARGET_AGE:
            reason = f"expired after {age / 86400:.1f} days"
        elif used_percent(root) >= PRESSURE_PERCENT:
            reason = f"store at {used_percent(root)}%"
        elif (size := tree_bytes(current)) > MAX_TARGET_BYTES:
            reason = f"{size / GIB:.0f} GiB over the {MAX_TARGET_BYTES // GIB} GiB cap"
        if reason != "warm":
            log(f"wiping key {key}: {reason}")
            pressure = used_percent(root) >= PRESSURE_PERCENT
            discard(current, trash, background=not pressure)
            if pressure:
                shutil.rmtree(trash, ignore_errors=True)
            reason = "wiped"

    current.mkdir(parents=True, exist_ok=True)
    meta = read_meta(current)
    meta.setdefault("created", now)
    meta["last_used"] = now
    write_json(current / META, meta)
    for entry in current.iterdir():
        if entry.name not in CARGO_ENTRIES and entry.name not in (META, SEALED, PENDING):
            if entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry, ignore_errors=True)
            else:
                entry.unlink(missing_ok=True)
    empty_trash(trash)
    return current, reason


def tracked_entries(workspace: Path) -> dict[str, str]:
    """Map each tracked regular file to its blob id, from the index (no hashing),
    and each directory below the root that holds tracked paths, as `dir/`, to a
    digest of the names of its tracked entries."""
    out = subprocess.run(["git", "-C", str(workspace), "ls-files", "-s", "-z"],
                         capture_output=True, check=True).stdout
    entries = {}
    children: dict[str, set[str]] = {}
    for record in out.split(b"\0"):
        if not record:
            continue
        info, path = record.split(b"\t", 1)
        mode, blob, _stage = info.split()
        name = path.decode("utf-8", "surrogateescape")
        if mode in (b"100644", b"100755"):
            entries[name] = blob.decode()
        parts = name.split("/")
        for depth in range(1, len(parts)):
            children.setdefault("/".join(parts[:depth]), set()).add(parts[depth])
    for directory, names in children.items():
        listing = "\0".join(sorted(names)).encode("utf-8", "surrogateescape")
        entries[directory + "/"] = hashlib.sha1(listing).hexdigest()
    return entries


def load_manifest(path: Path) -> dict[str, tuple[str, int]]:
    try:
        text = path.read_text(encoding="utf-8", errors="surrogateescape")
    except OSError:
        return {}
    manifest = {}
    for line in text.splitlines():
        blob, mtime, name = line.split("\t", 2)
        manifest[name] = (blob, int(mtime))
    return manifest


def write_manifest(path: Path, entries: dict[str, tuple[str, int]]) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text("".join(f"{blob}\t{mtime}\t{name}\n"
                                 for name, (blob, mtime) in sorted(entries.items())),
                         encoding="utf-8", errors="surrogateescape")
    os.replace(temporary, path)


def restore_mtimes(workspace: Path, target: Path) -> tuple[int, int]:
    """Give unchanged files and directories their previous mtimes; record what
    this job builds from.

    Directories count because a build script's `rerun-if-changed` on a
    directory compares the newest mtime within it, directories included, and
    checkout creates every directory anew. A directory is unchanged when its
    tracked entries are; a step that adds a file to it changes its mtime, so
    the seal drops it.
    """
    sealed = target / SEALED
    previous = load_manifest(sealed)
    # From here on, artifacts in target may come from this checkout, which the
    # old manifest does not describe; only this job's seal can vouch again.
    sealed.unlink(missing_ok=True)
    restored = 0
    current = {}
    for name, blob in tracked_entries(workspace).items():
        path = workspace / name
        old = previous.get(name)
        try:
            if old and old[0] == blob:
                os.utime(path, ns=(old[1], old[1]))
                restored += 1
            current[name] = (blob, os.lstat(path).st_mtime_ns)
        except OSError:
            continue
    write_manifest(target / PENDING, current)
    return restored, len(current)


def bind(source: Path, destination: Path) -> None:
    source.mkdir(parents=True, exist_ok=True)
    destination.mkdir(parents=True, exist_ok=True)
    if any(destination.iterdir()):
        raise RuntimeError(f"{destination} is not empty; refusing to shadow it")
    subprocess.run(["mount", "--bind", str(source), str(destination)], check=True)
    # Never propagate into the runner's workspace volume, whose removal at the
    # end of the job must not reach through into the cache.
    subprocess.run(["mount", "--make-private", str(destination)], check=True)


def output(**values: str) -> None:
    github_output = os.environ.get("GITHUB_OUTPUT")
    if github_output:
        with open(github_output, "a", encoding="utf-8") as stream:
            for key, value in values.items():
                stream.write(f"{key}={value}\n")


def mount(args: argparse.Namespace) -> int:
    volume = Path(args.volume)
    workspace = Path(args.workspace)
    if not os.path.ismount(volume):
        log(f"{volume} is not a mounted slot-cache volume; building cold")
        output(enabled="false")
        return 0
    total, _ = store_usage(volume)
    if total < MIN_STORE_BYTES:
        log(f"slot store is {total / GIB:.0f} GiB, under {MIN_STORE_BYTES // GIB} GiB; building cold")
        output(enabled="false")
        return 0
    root = volume / LAYOUT
    root.mkdir(exist_ok=True)
    trust_file = root / "TRUST"
    recorded = trust_file.read_text(encoding="utf-8").strip() if trust_file.exists() else ""
    if recorded and recorded != args.trust:
        # A volume never changes trust class; if one did, nothing in it is usable.
        log(f"volume was written by {recorded} jobs, this job is {args.trust}; wiping it")
        shutil.rmtree(root)
        root.mkdir()
    trust_file.write_text(args.trust + "\n", encoding="utf-8")

    started = time.monotonic()
    key = toolchain_key()
    target, reason = select_target(root, key, time.time())
    bind(target, workspace / "target")
    cargo = root / "cargo"
    if time.time() - read_meta(cargo).get("created", time.time()) > MAX_TARGET_AGE:
        log("wiping Cargo downloads: older than a week")
        shutil.rmtree(cargo, ignore_errors=True)
    cargo.mkdir(exist_ok=True)
    meta = read_meta(cargo)
    meta.setdefault("created", time.time())
    write_json(cargo / META, meta)
    cargo_home = Path(args.cargo_home)
    for part in ("registry", "git"):
        bind(cargo / part, cargo_home / part)
    restored, files = restore_mtimes(workspace, workspace / "target")
    total, used = store_usage(volume)
    log(f"{args.trust} key {key} ({reason}); restored mtimes of {restored}/{files} paths; "
        f"store {used / GIB:.0f}/{total / GIB:.0f} GiB; {time.monotonic() - started:.1f}s")
    output(enabled="true", key=key, state=reason)
    return 0


def seal(args: argparse.Namespace) -> int:
    workspace = Path(args.workspace)
    target = workspace / "target"
    pending = target / PENDING
    if not pending.exists():
        log("no slot cache to seal")
        return 0
    entries = load_manifest(pending)
    kept = {}
    for name, (blob, mtime) in entries.items():
        try:
            if os.lstat(workspace / name).st_mtime_ns == mtime:
                kept[name] = (blob, mtime)
        except OSError:
            continue
    write_manifest(target / SEALED, kept)
    pending.unlink()
    log(f"sealed {len(kept)} unchanged paths ({len(entries) - len(kept)} modified since mount)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    mount_parser = sub.add_parser("mount")
    mount_parser.add_argument("--trust", choices=("trusted", "untrusted"), required=True)
    for p in (mount_parser, sub.add_parser("seal")):
        p.add_argument("--workspace", default=os.environ.get("GITHUB_WORKSPACE", os.getcwd()))
    mount_parser.add_argument("--volume", default="/ci-slot-cache")
    mount_parser.add_argument("--cargo-home", default=os.environ.get("CARGO_HOME", "/cargo-home"))
    args = parser.parse_args(argv)
    try:
        return mount(args) if args.command == "mount" else seal(args)
    except Exception as error:  # noqa: BLE001 - a cache must never fail the job
        log(f"disabled after an error: {error!r}; building cold")
        output(enabled="false")
        return 0


if __name__ == "__main__":
    sys.exit(main())
