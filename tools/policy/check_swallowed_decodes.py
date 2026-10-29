#!/usr/bin/env python3
"""Decode and parse errors in session and app-hosting code are heard.

A message from a peer, a stored record or a surface file that fails to decode
is an error: the sender should be told, or the failure logged and shown. When
the result of a decode is thrown away instead, the failure looks like an
absent value and whatever depended on it silently does nothing (a shell that
could not read a new-form surface hid a broken read path behind it).

This check finds, in the production sources of the crates listed in SCANNED,
a call to a decoding function (``decode``, ``parse``, ``from_str``,
``from_slice``, ``from_bytes``, ``deserialize``, ``read_message``, ...) whose
error is discarded in the same statement:

* ``.ok()`` anywhere in the call's method chain (``.ok()?``,
  ``filter_map(|x| decode(x).ok())``);
* ``.unwrap_or_default()``, ``.unwrap_or(..)``, or ``.unwrap_or_else(|_| ..)``;
* ``let _ = decode(..)``;
* ``if let Ok(..) = decode(..) { .. }`` or ``while let Ok(..)`` with no else;
* ``let Ok(..) = decode(..) else { .. }`` whose else neither logs nor
  returns an error;
* ``match decode(..) { .. Err(_) => .. }`` whose error arm neither logs nor
  returns an error.

A decode whose absence is genuinely fine (a hint, a cache, an optional
environment override) says why with an allow comment above the statement,
or on one of its lines up to the call::

    // optional-decode: a stale cache entry is rebuilt from the source

What remains is counted per file in ``swallowed_decodes_baseline.toml``: a
count may only fall, and a file that sheds some records it (``--update``).
"""

from __future__ import annotations

import re
import sys
import tomllib
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import blank_test_items, is_test_file, lex  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path("tools/policy/swallowed_decodes_baseline.toml")
SCANNED = (
    Path("crates/session/shell"),
    Path("crates/collab/tilo-action-broker"),
    Path("crates/collab/collaboration"),
    Path("crates/platform/tilolang-host"),
    Path("crates/platform/tilolang-tilo"),
    Path("crates/platform/tilo-ui-host"),
    Path("crates/hosting/tilo-room-host"),
    Path("crates/runtime/runtime-portal"),
    Path("apps/tilolang"),
)
ALLOW = re.compile(r"//\s*optional-decode:\s*\S")

DECODE = re.compile(
    r"(?<![\w.])(?:[\w:]*::)?(?P<name>"
    r"decode\w*|\w+_decode|parse(?:_\w+)?|\w+_parse|from_str|from_slice|from_bytes|from_reader"
    r"|from_value|from_json\w*|from_toml\w*|deserialize\w*|read_message|read_packed_message"
    r"|from_utf8|get_root\w*|from_segments)"
    r"\s*(?:::\s*<[^;{}()]*?>\s*)?\("
    r"|\.(?P<method>parse|decode\w*|deserialize\w*|get_root\w*)\s*(?:::\s*<[^;{}()]*?>\s*)?\("
)
METHOD = re.compile(r"\s*\??\s*\.\s*(\w+)\s*(?:::\s*<[^;{}()]*?>\s*)?(\()?")
SWALLOWING = {"ok", "unwrap_or_default", "unwrap_or"}
CLOSE = {"(": ")", "[": "]", "{": "}"}
# What an else block does when it hears the failure.
HEARD = re.compile(
    r"\b(?:log|tracing|warn|error|eprintln|panic|unreachable|bail|anyhow|Err|reject\w*|report\w*|"
    r"fail\w*|refuse\w*)\b"
)


def matching(code: str, open_at: int) -> int:
    """Index just past the bracket that closes the one at ``open_at``."""
    depth = 0
    for index in range(open_at, len(code)):
        char = code[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
            if depth == 0:
                return index + 1
    return len(code)


def statement_start(code: str, at: int) -> int:
    """Start of the statement or expression the offset ``at`` sits in."""
    depth = 0
    index = at - 1
    while index >= 0:
        char = code[index]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth == 0:
                return index + 1
            depth -= 1
        elif char == ";" and depth == 0:
            return index + 1
        index -= 1
    return 0


def chain_swallows(code: str, end: int) -> bool:
    """Whether the method chain starting at ``end`` discards the error."""
    while match := METHOD.match(code, end):
        name = match.group(1)
        if match.group(2) is None:
            # A field access or `.await`, the chain goes on.
            end = match.end()
            continue
        close = matching(code, match.end() - 1)
        if name in SWALLOWING:
            return True
        if name == "unwrap_or_else" and re.match(r"\(\s*\|\s*_\s*\|", code[match.end() - 1 : close]):
            return True
        end = close
    return False


def converted_later(code: str, at: int) -> bool:
    """Whether the expression around the call turns the absence back into an
    error (`.and_then(|s| s.parse().ok()).ok_or_else(|| invalid(..))?`)."""
    depth = 0
    index = at
    while index < len(code):
        char = code[index]
        if char in "([{":
            if char == "{" and depth <= 0:
                return False
            depth += 1
        elif char in ")]}":
            depth -= 1
            if depth < -4:
                return False
        elif char == ";" and depth <= 0:
            return False
        elif char == "." and depth <= 0 and re.match(r"\.\s*ok_or(?:_else)?\s*\(", code[index:]):
            return True
        index += 1
    return False


def block_after(code: str, at: int) -> tuple[int, int] | None:
    brace = code.find("{", at)
    if brace < 0:
        return None
    return brace, matching(code, brace)


def pattern_swallows(code: str, prefix: str, call_end: int) -> bool:
    """`if let Ok(..) = decode() { }` with no else, or a let-else that hears nothing."""
    if re.search(r"\b(?:if|while)\s+let\s+Ok\s*\(", prefix):
        block = block_after(code, call_end)
        if block is None:
            return False
        rest = code[block[1] :]
        return not re.match(r"\s*else\b", rest)
    if re.search(r"(?:^|[\s;{}])let\s+Ok\s*\(", prefix):
        tail = re.match(r"[^;{]*?\belse\s*\{", code[call_end:])
        if not tail:
            return False
        brace = call_end + tail.end() - 1
        return not HEARD.search(code[brace : matching(code, brace)])
    if re.search(r"\bmatch\s*$", prefix):
        block = block_after(code, call_end)
        if block is None:
            return False
        body = code[block[0] + 1 : block[1] - 1]
        for arm in re.finditer(r"\bErr\s*\(\s*(?:_|\.\.)\s*\)\s*(?:if\b[^=]*)?=>", body):
            if not HEARD.search(arm_body(body, arm.end())):
                return True
    return False


def arm_body(body: str, at: int) -> str:
    """The expression of the match arm whose `=>` ends at ``at``."""
    depth = 0
    for index in range(at, len(body)):
        char = body[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                return body[at:index]
            depth -= 1
            if depth == 0 and char == "}" and not body[at:index].strip().startswith(("Some", "Ok")):
                if body[at:index + 1].lstrip().startswith("{"):
                    return body[at : index + 1]
        elif char == "," and depth == 0:
            return body[at:index]
    return body[at:]


def findings_in(text: str) -> list[tuple[int, str]]:
    code, _ = lex(text)
    code = blank_test_items(code)
    lines = text.split("\n")
    found: list[tuple[int, str]] = []
    for call in DECODE.finditer(code):
        name = call.group("name") or call.group("method")
        before = code[max(0, call.start() - 4) : call.start()]
        if re.search(r"\bfn\s*$", code[max(0, call.start() - 8) : call.start()]):
            continue
        call_end = matching(code, call.end() - 1)
        start = statement_start(code, call.start())
        prefix = code[start : call.start()]
        how = None
        if chain_swallows(code, call_end) and not converted_later(code, call_end):
            how = "error discarded"
        elif re.match(r"\s*let\s+_\s*(?::[^=]*)?=\s*$", prefix) and not before.endswith("."):
            how = "result bound to `_`"
        elif pattern_swallows(code, prefix, call_end):
            how = "error branch ignored"
        if how is None:
            continue
        line = code.count("\n", 0, call.start()) + 1
        first = start + len(code[start : call.start()]) - len(code[start : call.start()].lstrip())
        opening = code.count("\n", 0, first) + 1
        # The comment may sit above the statement or on any of its lines up
        # to the call.
        nearby = lines[max(0, min(opening, line) - 2) : line]
        if any(ALLOW.search(entry) for entry in nearby):
            continue
        found.append((line, f"{name}: {how}"))
    return found


def violations(root: Path) -> list[str]:
    found: list[str] = []
    for top in SCANNED:
        base = root / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            relative = path.relative_to(root)
            if "target" in relative.parts or "examples" in relative.parts or is_test_file(relative):
                continue
            for line, message in findings_in(path.read_text(encoding="utf-8")):
                found.append(f"{relative.as_posix()}:{line}: swallowed decode ({message})")
    return found


def load_baseline(root: Path) -> dict[str, int]:
    path = root / BASELINE
    if not path.exists():
        return {}
    return {name: int(count) for name, count in tomllib.loads(path.read_text()).get("files", {}).items()}


def write_baseline(root: Path, counts: Counter[str]) -> None:
    lines = [
        "# Decode and parse results whose error is discarded, per file, in session and",
        "# app-hosting code. Counts may only fall; tools/policy/check_swallowed_decodes.py",
        "# --update records a file that shed some. Report the error, or mark a genuinely",
        "# optional read with `// optional-decode: <why>`.",
        "[files]",
    ]
    lines += [f'"{name}" = {count}' for name, count in sorted(counts.items())]
    (root / BASELINE).write_text("\n".join(lines) + "\n")


def main(argv: list[str]) -> int:
    root = ROOT
    found = violations(root)
    counts = Counter(entry.split(":", 1)[0] for entry in found)
    if "--update" in argv:
        write_baseline(root, counts)
        print(f"recorded {sum(counts.values())} swallowed decode(s)", file=sys.stderr)
        return 0
    if "--list" in argv:
        print("\n".join(found))
        return 0
    baseline = load_baseline(root)
    grown = [entry for entry in found if counts[entry.split(":", 1)[0]] > baseline.get(entry.split(":", 1)[0], 0)]
    shed = sorted(name for name, count in baseline.items() if counts.get(name, 0) < count)
    if grown:
        print("\n".join(grown))
        print(
            "\na decode or parse error is discarded: propagate it, answer the peer, or log it "
            "and show the state; a genuinely optional read says why with "
            "`// optional-decode: <why>`.",
            file=sys.stderr,
        )
        return 1
    if shed:
        for name in shed:
            print(f"{name}: swallowed decodes fell below {BASELINE}; record it with "
                  "tools/policy/check_swallowed_decodes.py --update")
        return 1
    print(f"Swallowed decodes contract OK ({sum(counts.values())} left, none new)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
