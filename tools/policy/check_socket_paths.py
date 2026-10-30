#!/usr/bin/env python3
"""A local socket binds and connects through `tilo_files::native::socket`.

The kernel takes a Unix socket's path in a fixed buffer (108 bytes on Linux),
so `UnixListener::bind(path)` and `UnixStream::connect(path)` fail for a
socket in a deep enough directory. Whether a socket works then depends on
where its profile, state root or worktree happens to live, and a holder whose
door failed to open looks to everyone else exactly like one that is gone.
`tilo_files::native::socket::{bind, connect}` reach a path of any length
through its directory and refuse loudly where they cannot.

Counts in the baseline only shrink. A call whose path is fixed and short (a
system socket such as `/run/greetd.sock`, an abstract or fd-passed address)
says so with `allow-socket-path: <why>` on the same line.
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = ROOT / "tools/policy/socket_paths_baseline.txt"
SCANNED = ("apps", "crates")
# The one place that binds and connects by path.
HELPER = "crates/platform/tilo-files/src/native/socket.rs"
CALL = re.compile(r"\bUnix(?:Listener::bind|Stream::connect)\s*\(")
ALLOW = re.compile(r"allow-socket-path:\s*\S")


def baseline(path: Path = BASELINE) -> Counter[str]:
    counts: Counter[str] = Counter()
    if not path.is_file():
        return counts
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        count, _, relative = line.partition(" ")
        counts[relative.strip()] += int(count)
    return counts


def audit(root: Path = ROOT) -> tuple[Counter[str], int]:
    found: Counter[str] = Counter()
    scanned = 0
    for tree in SCANNED:
        for source in sorted((root / tree).rglob("*.rs")):
            relative = source.relative_to(root)
            if any(part in {"target", "vendor"} for part in relative.parts):
                continue
            if str(relative) == HELPER or rust_source.is_test_file(relative):
                continue
            scanned += 1
            text = source.read_text(encoding="utf-8")
            code = rust_source.code_only(rust_source.production_source(text))
            for match in CALL.finditer(code):
                start = text.rfind("\n", 0, match.start()) + 1
                end = text.find("\n", match.end())
                if ALLOW.search(text[start : end if end != -1 else len(text)]):
                    continue
                found[str(relative)] += 1
    return found, scanned


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="record every call the tree has now, for a change that adds none",
    )
    arguments = parser.parse_args()

    found, scanned = audit()
    if arguments.write_baseline:
        body = "\n".join(f"{count} {path}" for path, count in sorted(found.items()))
        BASELINE.write_text(
            "# Sockets still bound or reached by a path the kernel may refuse for its\n"
            "# length (tools/policy/check_socket_paths.py). Counts only shrink: use\n"
            "# tilo_files::native::socket::{bind, connect}, or mark a fixed short path\n"
            "# `allow-socket-path: <why>`.\n" + (body + "\n" if body else ""),
            encoding="utf-8",
        )
        print(
            f"wrote {sum(found.values())} call(s) in {len(found)} file(s)",
            file=sys.stderr,
        )
        return 0

    allowed = baseline()
    violations = [
        f"{path}: {count} socket bind/connect by path, {allowed.get(path, 0)} allowed"
        for path, count in sorted(found.items())
        if count > allowed.get(path, 0)
    ]
    for violation in violations:
        print(violation)
    if violations:
        print(
            "socket paths FAILED: bind and connect with tilo_files::native::socket,\n"
            "which reach a path of any length; a fixed short path says\n"
            "`allow-socket-path: <why>`.",
            file=sys.stderr,
        )
        return 1
    print(
        f"socket paths OK ({scanned} sources; {sum(found.values())} known, none new)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
