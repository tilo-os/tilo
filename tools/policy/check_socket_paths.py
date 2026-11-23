#!/usr/bin/env python3
"""A local socket binds and connects through `tilo_socket` (`tilo_files::native::socket`).

The kernel takes a Unix socket's path in a fixed buffer (108 bytes on Linux),
so `UnixListener::bind(path)` and `UnixStream::connect(path)` fail for a
socket in a deep enough directory. Whether a socket works then depends on
where its profile, state root or worktree happens to live, and a holder whose
socket failed to open looks to everyone else exactly like one that is gone.
`tilo_socket` (`tilo_files::native::socket`) reaches a path of any length
through its directory and refuses loudly where it cannot, so every bind and
connect by path goes through it, with no exception: a fixed short path such as
`/run/greetd.sock` is used as it is by the same call.
"""
from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
SCANNED = ("apps", "crates", "tools")
# The one place that binds and connects by path.
HELPER = "crates/platform/tilo-socket/src/lib.rs"
CALL = re.compile(r"\bUnix(?:Listener::bind|Stream::connect)\s*\(")


def audit(root: Path = ROOT) -> tuple[Counter[str], int]:
    """The calls the tree has outside the helper, and the sources scanned."""
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
            count = len(CALL.findall(code))
            if count:
                found[str(relative)] = count
    return found, scanned


def main() -> int:
    found, scanned = audit()
    violations = [
        f"{path}: {count} socket bind/connect by path"
        for path, count in sorted(found.items())
    ]
    for violation in violations:
        print(violation)
    if violations:
        print(
            "socket paths FAILED: bind and connect through `tilo_socket`\n"
            "(re-exported as `tilo_files::native::socket`), which reaches a path of\n"
            "any length; a fixed short path is used as it is by the same call.",
            file=sys.stderr,
        )
        return 1
    print(f"socket paths OK ({scanned} sources)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
