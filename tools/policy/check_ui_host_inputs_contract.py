#!/usr/bin/env python3
"""Keep the toolkit's host inputs explicit.

`tilo-ui` widgets and state receive identity, paths, accessibility settings,
locale and diagnostics from the host that runs them
(`ui::host::HostEnvironment`, `ComponentRunnerInputs`). Only the native
runner reads them from the process, so the same toolkit can be hosted where
there is no process environment to read.

This check fails on a process-environment read (`env::var`, `env::var_os`,
`env::vars`, `env::args`) in `crates/sdk/ui-core/src` and
`crates/sdk/tilo-host/src` outside:

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

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import blank_test_items  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "crates/sdk/ui-core/src"
HOST_SRC = ROOT / "crates/sdk/tilo-host/src"

NATIVE_PREFIXES = ("native/",)

# The native runner and the tools that drive it.
NATIVE_RUNNER_FILES = {
    "app/native_runner.rs",
    "app/native_runner/startup.rs",
    "application/host.rs",
    "dx12_shader_cache.rs",
    "headless.rs",
    "launch.rs",
    "native_scenario.rs",
    "offscreen.rs",
    "presentation_feedback.rs",
    "profiling/resource_trace.rs",
    "render_thread.rs",
    "render_thread/remote.rs",
    "webview/chromium/mod.rs",
    "webview/chromium/runtime.rs",
    "webview/frame_trace.rs",
    "webview/profile.rs",
    "window_icon.rs",
}

# Portable code that still reads the process directly.
PENDING: set[str] = set()

ENV_READ = re.compile(r"\benv::(?:var|var_os|vars|vars_os|args|args_os)\b")
COMMENT = re.compile(r"(?m)//.*$")


def production_source(text: str) -> str:
    """Drop comments and every item or block gated on `cfg(test)`."""
    return blank_test_items(COMMENT.sub(lambda m: " " * len(m.group()), text))


def is_exempt(relative: str) -> bool:
    return (
        relative.startswith(NATIVE_PREFIXES)
        or relative.endswith("tests.rs")
        or "/tests/" in relative
        or relative in NATIVE_RUNNER_FILES
    )


def host_audit() -> list[str]:
    """The host services crate reads the process only in its native backends."""
    violations: list[str] = []
    for path in sorted(HOST_SRC.rglob("*.rs")):
        relative = path.relative_to(HOST_SRC).as_posix()
        if relative.startswith(NATIVE_PREFIXES) or relative.endswith("tests.rs"):
            continue
        text = production_source(path.read_text(encoding="utf-8"))
        for hit in ENV_READ.finditer(text):
            line = text.count("\n", 0, hit.start()) + 1
            violations.append(
                f"crates/sdk/tilo-host/src/{relative}:{line}: reads the process environment "
                "outside the native backends"
            )
    return violations


def audit() -> list[str]:
    violations: list[str] = host_audit()
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
