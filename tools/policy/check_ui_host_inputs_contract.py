#!/usr/bin/env python3
"""Keep the toolkit's host inputs explicit.

`tilo-ui` widgets and state receive identity, paths, accessibility settings,
locale and diagnostics from the host that runs them
(`ui::host::HostEnvironment`, `ComponentRunnerInputs`). Only the native
runner reads them from the process, so the same toolkit can be hosted where
there is no process environment to read.

This check fails on a process-environment read (`env::var`, `env::var_os`,
`env::vars`, `env::args`) in `crates/sdk/ui-core/src`,
`crates/sdk/tilo-host/src`, the collab client below the toolkit
(`crates/collab/collaboration/src`) and the host inputs themselves
(`crates/platform/tilo-process/src/environment`) outside:

* the native modules (`src/native/`);
* test code (`*tests.rs` files and each file's trailing test module);
* the files in ``NATIVE_RUNNER_FILES`` below, which are the native runner and
  its capture tools, and the files in ``PENDING`` which still read the
  process directly.

``PENDING`` may only shrink: an entry that no longer reads the environment is
reported so it is removed.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import add_staged_argument, judged  # noqa: E402
from rust_source import blank_test_items  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
# The toolkit's sources, and the code below it that reads host inputs as the
# toolkit does: each root with the files of it that are checked (all, or one
# module and its children).
SRC = Path("crates/sdk/ui-core/src")
BELOW_TOOLKIT = (
    (Path("crates/sdk/tilo-host/src"), None),
    (Path("crates/collab/collaboration/src"), None),
    (Path("crates/platform/tilo-process/src"), "environment"),
)

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
    "surface_services.rs",
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


def host_audit(files) -> list[str]:
    """Host services, the collab client and the host inputs read the process
    only in their native backends (`native/`, `<module>/native.rs`)."""
    violations: list[str] = []
    for scope, module in BELOW_TOOLKIT:
        for relative, source in files.sources(scope):
            within = relative.relative_to(scope).as_posix()
            if module is not None and within != f"{module}.rs" and not within.startswith(f"{module}/"):
                continue
            if (
                within.startswith(NATIVE_PREFIXES)
                or within.endswith("/native.rs")
                or within.endswith("tests.rs")
            ):
                continue
            text = production_source(source)
            for hit in ENV_READ.finditer(text):
                line = text.count("\n", 0, hit.start()) + 1
                violations.append(
                    f"{scope.as_posix()}/{within}:{line}: reads the process "
                    "environment outside the native backends"
                )
    return violations


def audit(root: Path = ROOT, staged: bool = False) -> list[str]:
    files = judged(root, staged)
    violations: list[str] = host_audit(files)
    readers: set[str] = set()
    scope = SRC

    def hits(within: str, text: str) -> list:
        if is_exempt(within):
            return []
        return list(ENV_READ.finditer(production_source(text)))

    for relative, text in files.sources(scope):
        within = relative.relative_to(scope).as_posix()
        if is_exempt(within):
            continue
        code = production_source(text)
        reads = list(ENV_READ.finditer(code))
        if not reads:
            continue
        readers.add(within)
        if within in PENDING:
            continue
        for hit in reads:
            line = code.count("\n", 0, hit.start()) + 1
            violations.append(
                f"{scope.as_posix()}/{within}:{line}: reads the process environment; "
                "take the value from ui::host::HostEnvironment or the runner inputs"
            )
    # `PENDING` may only shrink, which a file the commit stages says nothing
    # about: each entry is read whether or not the commit touched it.
    for within in sorted(PENDING - readers):
        path = scope / within
        if not (root / path).is_file() or hits(within, files.read(path)):
            violations.append(
                f"{scope.as_posix()}/{within}: no longer reads the process environment; "
                "remove it from PENDING in tools/policy/check_ui_host_inputs_contract.py"
            )
    for within in sorted(NATIVE_RUNNER_FILES):
        if not (root / SRC / within).is_file():
            violations.append(
                f"{scope.as_posix()}/{within}: listed as a native runner file but missing"
            )
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    add_staged_argument(parser)
    violations = audit(staged=parser.parse_args(argv).staged)
    if violations:
        print("ui host-inputs contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print("ui host-inputs contract OK (portable toolkit code reads no process environment)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
