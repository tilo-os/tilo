#!/usr/bin/env python3
"""A test starts a child process under the environment lock.

`cargo test` runs a binary's tests as threads of one process, and a test that
changes the environment does so inside a `tilo_test_env::ScopedEnv` (or a
`TestHome`), which holds the process-wide environment lock. A child copies the
environment when it starts: started by another thread while that scope is
open, it inherits that test's values, such as a `TILO_LAUNCHER_BIN` pointing
at a recording stand-in, and the two tests read each other's effects.
`tilo_test_env::SpawnUnderEnvLock` (and `SpawnUnderEnvLockAsync` for Tokio)
starts a command under that lock: `spawn_locked`, `output_locked`,
`status_locked`.

This check finds, in test code (files under `tests/` or `benches/`, the
sources of `tilo-test-env`, and `#[cfg(test)]`/`#[test]` items elsewhere), every `.spawn()`, `.output()` and
`.status()` on a `Command`: a call chained onto `Command::new` or onto a
builder method only a command has (`.arg`, `.args`, `.env`, `.stdin`, ...),
or on a variable bound to or typed as a `Command`, or on a call of a
function that returns one. A payload under a `fixtures` directory is not test
code: the test starts it as its own process, so it holds no test's scope.
There is no baseline and no opt-out.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
# Test support: every source of the crate tests share is test code, as the
# files under `tests/` are. The trait's own implementation calls `spawn` on
# `self`, which is not a command this check can name.
SUPPORT = (Path("crates/dev/test-env/src"),)
SCANNED = ("apps", "crates", "examples", "fuzz", "os", "tools")
START = re.compile(r"\.\s*(spawn|output|status)\s*\(\s*\)")
COMMAND_TYPE = r"(?:(?:std|tokio)\s*::\s*process\s*::\s*)?Command\b"
CONSTRUCTOR = re.compile(rf"\b{COMMAND_TYPE}\s*::\s*(?:new|from)\s*\(")
# Builder methods of `std::process::Command` and `tokio::process::Command`
# that no other common type in the tree chains before `spawn`/`output`/`status`.
BUILDER = re.compile(
    r"\.\s*(?:arg|args|env|envs|env_clear|env_remove|current_dir|stdin|stdout|stderr"
    r"|uid|gid|process_group|pre_exec|arg0|kill_on_drop)\s*\("
)
RETURNS_COMMAND = re.compile(
    rf"\bfn\s+(\w+)\s*(?:<[^>{{]*>)?\s*\([^{{;]*?\)\s*->\s*(?:&\s*mut\s+)?{COMMAND_TYPE}"
)


def receiver(code: str, dot: int) -> str:
    """The expression `code[..dot]` ends with: a path or call followed by
    method calls, indexing, `?` and `.await`."""
    at = dot
    while True:
        while at > 0 and code[at - 1].isspace():
            at -= 1
        if at == 0:
            return code[:dot]
        char = code[at - 1]
        if char == "?":
            at -= 1
            continue
        if char in ")]":
            at = opening(code, at - 1)
            continue
        if rust_source.is_ident(code, at - 1) or char == ">":
            if char == ">":
                # A turbofish or generic path segment: `foo::<T>`.
                depth = 0
                while at > 0:
                    at -= 1
                    if code[at] == ">":
                        depth += 1
                    elif code[at] == "<":
                        depth -= 1
                        if depth == 0:
                            break
                continue
            while at > 0 and rust_source.is_ident(code, at - 1):
                at -= 1
            back = at
            while back > 0 and code[back - 1].isspace():
                back -= 1
            if code[back - 2 : back] == "::":
                at = back - 2
                continue
            if back > 0 and code[back - 1] == ".":
                at = back - 1
                continue
            return code[at:dot]
        return code[at:dot]


def opening(code: str, close: int) -> int:
    """Index of the bracket that `code[close]` closes."""
    pairs = {")": "(", "]": "["}
    want = pairs[code[close]]
    depth = 0
    for index in range(close, -1, -1):
        char = code[index]
        if char == code[close]:
            depth += 1
        elif char == want:
            depth -= 1
            if depth == 0:
                return index
    return 0


def root_name(expression: str) -> str | None:
    found = re.match(r"\s*(?:&\s*mut\s+|\*\s*)?\(?\s*([A-Za-z_]\w*)", expression)
    return found.group(1) if found else None


def last_call(expression: str) -> str | None:
    """The function or method `expression` ends by calling, if it does."""
    tail = expression.rstrip()
    if not tail.endswith(")"):
        return None
    head = tail[: opening(tail, len(tail) - 1)].rstrip()
    head = re.sub(r"::\s*<[^<>]*>\s*$", "", head)
    found = re.search(r"(\w+)\s*$", head)
    return found.group(1) if found else None


def is_command(expression: str, code: str, commands: set[str]) -> bool:
    if CONSTRUCTOR.search(expression):
        return True
    built = re.match(r"\s*(?:[\w:]*::)?([A-Z]\w*)\s*::\s*\w+\s*\(", expression)
    if built and built.group(1) != "Command":
        # Another type's builder (a sandbox `Launch`, a `Router`), whose
        # methods may share a command's names.
        return False
    if BUILDER.search(expression):
        return True
    if last_call(expression) in commands:
        return True
    name = root_name(expression)
    if name is None:
        return False
    rest = expression[expression.find(name) + len(name) :].lstrip()
    if rest.startswith("(") or rest.startswith("::<"):
        return name in commands
    if rest and not rest.startswith(".") and not rest.startswith("?"):
        return False
    binding = re.compile(
        rf"\blet\s+(?:mut\s+)?{re.escape(name)}\s*(?::\s*{COMMAND_TYPE}\s*)?=\s*[^;]*?"
        rf"(?:{CONSTRUCTOR.pattern}|\b(?:{'|'.join(map(re.escape, sorted(commands))) or '(?!)'})\s*\()"
    )
    typed = re.compile(
        rf"\b(?:mut\s+)?{re.escape(name)}\s*:\s*(?:&\s*(?:'\w+\s+)?mut\s+)?{COMMAND_TYPE}"
    )
    return bool(binding.search(code) or typed.search(code))


def test_code(relative: Path, code: str) -> str:
    if any(part == "fixtures" for part in relative.parts[:-1]):
        # A fixture is a payload a test writes, builds and starts as a process
        # of its own — often with a bare `rustc`, so it cannot link
        # `tilo-test-env`: its children inherit the payload's environment, which
        # the test that starts it sets, never a test thread's scope.
        return ""
    if rust_source.is_test_file(relative) or any(
        support in relative.parents for support in SUPPORT
    ):
        return code
    production = rust_source.blank_test_items(code)
    return "".join(
        c if c != p or c == "\n" else " " for c, p in zip(code, production)
    )


def own_workspace(relative: Path, root: Path) -> bool:
    """Whether `relative` belongs to the repository's Cargo workspace rather
    than to a tool with a workspace and lock of its own, whose tests cannot
    take the lock: they do not link `tilo-test-env`, and so cannot open a
    scope either."""
    return not any((root / parent / "Cargo.lock").is_file() for parent in relative.parents if parent != Path("."))


def sources(root: Path):
    names = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", *SCANNED],
        stdout=subprocess.PIPE, check=True,
    ).stdout
    for name in sorted(set(names.split(b"\0")) - {b""}):
        relative = Path(os.fsdecode(name))
        if relative.suffix != ".rs":
            continue
        if any(part in {"target", "vendor"} for part in relative.parts):
            continue
        if not own_workspace(relative, root):
            continue
        path = root / relative
        if path.is_file():
            yield relative, path.read_text(encoding="utf-8", errors="replace")


def package(relative: Path) -> Path:
    """The package directory a source belongs to: the path up to its
    `src`, `tests`, `benches` or `examples` directory."""
    parts = relative.parts
    for index, part in enumerate(parts[:-1]):
        if part in {"src", "tests", "benches", "examples"}:
            return Path(*parts[:index])
    return relative.parent


CLOSURE = re.compile(r"\blet\s+(?:mut\s+)?(\w+)\s*=\s*(?:move\s+)?\|[^|]*\|")


def command_closures(code: str, commands: set[str]) -> set[str]:
    """Closures in `code` that build a command: `let make = || { ...
    Command::new(...) ... }` or one that calls a function returning one."""
    found = set()
    calls = re.compile(rf"\b(?:{'|'.join(map(re.escape, sorted(commands))) or '(?!)'})\s*\(")
    for closure in CLOSURE.finditer(code):
        body_start = closure.end()
        rest = code[body_start:].lstrip()
        if rest.startswith("{"):
            open_ = code.index("{", body_start)
            body = code[open_ : rust_source.matching(code, open_)]
        else:
            body = code[body_start : code.find(";", body_start)]
        if CONSTRUCTOR.search(body) or calls.search(body):
            found.add(closure.group(1))
    return found


def audit(root: Path = ROOT, files=None) -> list[tuple[str, int, str]]:
    listed = files if files is not None else sources(root)
    loaded = [
        (relative, rust_source.code_only(text))
        for relative, text in listed
        if "Command" in text or START.search(text)
    ]
    # A helper that returns a command is called from its own package.
    commands: dict[Path, set[str]] = {}
    for relative, code in loaded:
        if "Command" in code:
            commands.setdefault(package(relative), set()).update(RETURNS_COMMAND.findall(code))
    found = []
    for relative, code in loaded:
        tests = test_code(relative, code)
        if not START.search(tests):
            continue
        own = commands.get(package(relative), set())
        local = own | command_closures(code, own)
        for call in START.finditer(tests):
            expression = receiver(code, call.start())
            if is_command(expression, code, local):
                line = rust_source.line_number(code, call.start())
                found.append((str(relative), line, call.group(1)))
    return found


def main() -> int:
    found = audit()
    for relative, line, method in found:
        print(
            f"{relative}:{line}: a test starts a process with `.{method}()`; use "
            f"`.{method}_locked()` (tilo_test_env::SpawnUnderEnvLock) so it starts "
            "outside every other test's environment scope",
            file=sys.stderr,
        )
    if found:
        print(f"{len(found)} test process start(s) bypass the environment lock", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
