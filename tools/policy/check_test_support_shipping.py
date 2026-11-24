#!/usr/bin/env python3
"""Keep the daemon's unconfined test start out of everything that ships.

`tilo-runtime`'s `test-support` feature (and `tilo-daemon`'s, which turns it
on) compiles in a start of a tilolang process with no confinement at all,
used where Windows has no AppContainer (Wine) by the walk that drives the
daemon there (`tilo-runtime-unconfined-test`). A shipping daemon must not
have it: nothing could then switch the sandbox off at run time, because the
code is not there.

So, for both crates:

- `test-support` is not a default feature, and no crate's default feature
  turns it on;
- no normal or build dependency on them enables it for a target that can be
  Windows (dev-dependencies do, and the workspace's feature unification does
  for Linux alone, where the start does not exist);
- no release build script enables it, or `--all-features`, on a build.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]
GUARDED = ("tilo-runtime", "tilo-daemon")
FEATURE = "test-support"
# The scripts that build what ships: the Windows daemon and imager, the Mac app, the apps and
# their runtimes, and the OS image.
RELEASE_SCRIPTS = (
    "tools/ci/windows/build.sh",
    "tools/ci/windows/cross-arm64.sh",
    "apps/imager/packaging/package-windows.sh",
    "dist/macos/build-app.sh",
    "dist/apps/package-apps.sh",
    "os/image/compile.sh",
)
NOT_MANIFESTS = frozenset({"target", ".git", "node_modules", "vendor"})
BUILD_FLAG = re.compile(r"(?<![\w-])(--all-features|(?:[\w-]+/)?test-support)(?![\w-])")


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    what: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: {self.what}\n"
            "  Why: test-support compiles in the daemon's unconfined test start, which a shipping\n"
            "  daemon must not have.\n"
            "  Fix: enable it from a dev-dependency or a test build only."
        )


def windows_can_apply(table: str) -> bool:
    """Whether a `[target.<cfg>.…]` table (or a plain one, '') can apply to a Windows build."""
    if not table or "windows" in table:
        return True
    return not any(
        token in table
        for token in ('target_os = "linux"', 'target_os = "macos"', 'target_os = "android"', "unix")
    )


def line_of(text: str, needle: str) -> int:
    for number, line in enumerate(text.splitlines(), 1):
        if needle in line:
            return number
    return 1


def manifests(root: Path) -> list[Path]:
    return sorted(
        path
        for path in root.rglob("Cargo.toml")
        if not NOT_MANIFESTS & set(path.relative_to(root).parts)
    )


def manifest_violations(path: Path, root: Path) -> list[Violation]:
    text = path.read_text(encoding="utf-8")
    try:
        manifest = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return []
    shown = path.relative_to(root).as_posix()
    found: list[Violation] = []
    name = manifest.get("package", {}).get("name")
    features = manifest.get("features", {})
    for enabled in features.get("default", []):
        if (name in GUARDED and enabled == FEATURE) or enabled in {
            f"{crate}/{FEATURE}" for crate in GUARDED
        }:
            found.append(Violation(shown, line_of(text, "default"), f"`{enabled}` is a default feature"))
    tables = [("", kind, manifest.get(kind, {})) for kind in ("dependencies", "build-dependencies")]
    for cfg, target in manifest.get("target", {}).items():
        tables += [(cfg, kind, target.get(kind, {})) for kind in ("dependencies", "build-dependencies")]
    for cfg, kind, dependencies in tables:
        if not windows_can_apply(cfg):
            continue
        for key, spec in dependencies.items():
            if not isinstance(spec, dict):
                continue
            crate = spec.get("package", key)
            if crate in GUARDED and FEATURE in spec.get("features", []):
                where = f"[target.'{cfg}'.{kind}]" if cfg else f"[{kind}]"
                found.append(
                    Violation(shown, line_of(text, f"{key} ="), f"{where} enables {crate}'s {FEATURE}")
                )
    return found


def script_violations(path: Path, root: Path) -> list[Violation]:
    shown = path.relative_to(root).as_posix()
    found = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        code = line.split("#", 1)[0]
        if "cargo" not in code or not re.search(r"(?<![\w-])build(?![\w-])", code):
            continue
        found.extend(
            Violation(shown, number, f"a release build enables `{match.group(1)}`")
            for match in BUILD_FLAG.finditer(code)
        )
    return found


def audit(root: Path = ROOT) -> list[Violation]:
    found: list[Violation] = []
    for path in manifests(root):
        found.extend(manifest_violations(path, root))
    for script in RELEASE_SCRIPTS:
        path = root / script
        if path.is_file():
            found.extend(script_violations(path, root))
    return found


def main() -> int:
    found = audit()
    for violation in found:
        print(violation)
    if found:
        return 1
    print("No shipping build compiles in the daemon's unconfined test start", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
