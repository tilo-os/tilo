#!/usr/bin/env python3
"""Keep the toolkit's host inputs explicit.

`tilo-ui` widgets and state receive identity, paths, accessibility settings,
locale and diagnostics from the host that runs them
(`ui::host::HostEnvironment`, `ComponentRunnerInputs`). Only the native
runner reads them from the process, so the same toolkit can be hosted where
there is no process environment to read.

This check fails on a process-environment read (`env::var`, `env::var_os`,
`env::vars`, `env::args`) in `crates/sdk/ui-core/src` outside:

* the native modules (`src/native/`);
* test code (`*tests.rs` files and each file's trailing test module);
* the files in ``NATIVE_RUNNER_FILES`` below, which are the native runner and
  its capture tools, and the files in ``PENDING`` which still read the
  process directly.

``PENDING`` may only shrink: an entry that no longer reads the environment is
reported so it is removed.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "crates/sdk/ui-core/src"

NATIVE_PREFIXES = ("native/",)

# The native runner and the tools that drive it.
NATIVE_RUNNER_FILES = {
    "app/native_runner.rs",
    "application/host.rs",
    "dx12_shader_cache.rs",
    "headless.rs",
    "launch.rs",
    "native_scenario.rs",
    "offscreen.rs",
    "presentation_feedback.rs",
    "profiling/resource_trace.rs",
    "render_thread.rs",
    "webview/chromium/mod.rs",
    "webview/frame_trace.rs",
    "webview/profile.rs",
    "webview/wpe/egl.rs",
    "webview/wpe/mod.rs",
    "window_icon.rs",
}

# Portable code that still reads the process directly.
PENDING: set[str] = set()

ENV_READ = re.compile(r"\benv::(?:var|var_os|vars|vars_os|args|args_os)\b")
TEST_ATTRIBUTE = re.compile(r"#\[cfg\((?:all\()?test\b[^\n]*\]")
COMMENT = re.compile(r"(?m)//.*$")


def blank(text: str, start: int, end: int) -> str:
    """Replace text[start:end] with spaces, keeping newlines for line numbers."""
    return text[:start] + re.sub(r"[^\n]", " ", text[start:end]) + text[end:]


def production_source(text: str) -> str:
    """Drop comments and every item or block gated on `cfg(test)`."""
    text = COMMENT.sub(lambda m: " " * len(m.group()), text)
    position = 0
    while match := TEST_ATTRIBUTE.search(text, position):
        cursor = match.end()
        brace = text.find("{", cursor)
        semicolon = text.find(";", cursor)
        if brace < 0 or (0 <= semicolon < brace):
            end = semicolon + 1 if semicolon >= 0 else len(text)
        else:
            depth = 0
            end = len(text)
            for index in range(brace, len(text)):
                if text[index] == "{":
                    depth += 1
                elif text[index] == "}":
                    depth -= 1
                    if depth == 0:
                        end = index + 1
                        break
        text = blank(text, match.start(), end)
        position = match.start() + 1
    return text


def is_exempt(relative: str) -> bool:
    return (
        relative.startswith(NATIVE_PREFIXES)
        or relative.endswith("tests.rs")
        or "/tests/" in relative
        or relative in NATIVE_RUNNER_FILES
    )


def audit() -> list[str]:
    violations: list[str] = []
    readers: set[str] = set()
    for path in sorted(SRC.rglob("*.rs")):
        relative = path.relative_to(SRC).as_posix()
        if is_exempt(relative):
            continue
        text = production_source(path.read_text(encoding="utf-8"))
        hits = [m for m in ENV_READ.finditer(text)]
        if not hits:
            continue
        readers.add(relative)
        if relative in PENDING:
            continue
        for hit in hits:
            line = text.count("\n", 0, hit.start()) + 1
            violations.append(
                f"crates/sdk/ui-core/src/{relative}:{line}: reads the process environment; "
                "take the value from ui::host::HostEnvironment or the runner inputs"
            )
    for relative in sorted(PENDING - readers):
        violations.append(
            f"crates/sdk/ui-core/src/{relative}: no longer reads the process environment; "
            "remove it from PENDING in tools/policy/check_ui_host_inputs_contract.py"
        )
    for relative in sorted(NATIVE_RUNNER_FILES):
        if not (SRC / relative).is_file():
            violations.append(
                f"crates/sdk/ui-core/src/{relative}: listed as a native runner file but missing"
            )
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("ui host-inputs contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("ui host-inputs contract OK (portable toolkit code reads no process environment)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
