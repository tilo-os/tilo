#!/usr/bin/env python3
"""Keep GStreamer the only media stack: no in-tree codec crates.

GStreamer (through `crates/platform/tilo-media`) is the one media stack. It selects
hardware encoders and decoders first, then royalty-free software ones, and
reaches H.264 in software only through Cisco's openh264 binary, which the
system's `openh264` GStreamer plugin loads at runtime. Compiling a codec into
Tilo would bypass all of that, so no workspace crate may depend on:

* `openh264` / `openh264-sys2`: builds Cisco's H.264 codec from source, which
  Cisco's patent licence does not cover (it covers only Cisco's own binaries);
* `ffmpeg-next` / `ffmpeg-sys-next`: a second, parallel media stack.

The check reads `Cargo.lock` (the resolved graph, which catches transitive
use) and every `Cargo.toml` in the repository (which catches a declaration
before the lock is regenerated).

Cisco's binary itself reaches a Tilo device only as the owner's own download
(the media-codecs extension, os/usr/share/doc/tilo/codec-pack.md). No image
Tilo ships may carry it, so outside the CI and developer build environments
named in `CISCO_ENVIRONMENTS`, no Containerfile, image package list or script
under `os/` may:

* install one of Cisco's packages by name (`CISCO_PACKAGES`);
* enable the `fedora-cisco-openh264` repository;
* run `dist/ci/install-build-deps.sh`, whose test profile installs Cisco's
  build for the video-editor suites (Containerfiles);
* build its final stage from the CI builder image, which carries it.

Resolution can still pull Cisco's `openh264` in transitively (with Fedora's
`fedora-cisco-openh264` repository enabled it satisfies `libopenh264.so`
ahead of the `noopenh264` stub), which only a resolved check sees:
os/scripts/verify-no-cisco-openh264.sh gates the frozen RPM snapshot and the
finished OS rootfs. The shipped images that install from live Fedora
repositories instead (`LIVE_REPOSITORY_IMAGES`) must, in their final stage,
remove that repository before their first install and run the same gate
after their last one.
"""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]
FORBIDDEN = {
    "openh264": "compiles Cisco's H.264 codec from source; use media::RealtimeEncoder/Decoder",
    "openh264-sys2": "compiles Cisco's H.264 codec from source; use media::RealtimeEncoder/Decoder",
    "ffmpeg-next": "a second media stack; use media (GStreamer)",
    "ffmpeg-sys-next": "a second media stack; use media (GStreamer)",
}
SKIP_DIRS = {"target", ".git", "node_modules"}

# Packages built from Cisco's OpenH264 or loading it, as
# os/scripts/verify-no-cisco-openh264.sh names them.
CISCO_PACKAGES = ("openh264", "openh264-devel", "mozilla-openh264", "gstreamer1-plugin-openh264")
CISCO_REPO = "fedora-cisco-openh264"
BUILD_DEPS = "install-build-deps"
CI_BUILDER_IMAGE = "tilo/tilo-builder"
# The only places that may install Cisco's binary: build and test
# environments, never a shipped image. Every entry must exist.
CISCO_ENVIRONMENTS = {
    "dist/ci/install-build-deps.sh": "its test profile installs the plugin for the video-editor suites",
    "os/Containerfile.builder": "the CI builder image; runs install-build-deps.sh's test profile",
    "dist/dev/Containerfile.fedora44": "the developer distrobox, built on the developer's own machine",
    "os/dev/legacy-vm-e2e.sh": "installs into the throwaway CI container of the legacy VM test",
}
# Shipped images whose final stage installs from live Fedora repositories
# rather than the frozen snapshot. Every entry must exist.
LIVE_REPOSITORY_IMAGES = {
    "crates/hosting/tilo-room-service/container/Containerfile": "the room service, on fedora-minimal",
    "os/Containerfile.legacy-guest": "the Linux legacy VM guest, on the mirrored fedora-bootc base",
}
CISCO_REPO_REMOVAL = f"rm -f /etc/yum.repos.d/{CISCO_REPO}.repo"
RESOLVED_GATE = re.compile(r"\bbash\s+\S*verify-no-cisco-openh264\.sh(?!\s+--repo)")
# A package name, or the start of an RPM file name (`openh264-2.6.0-...rpm`).
CISCO_PACKAGE_TOKEN = re.compile(
    r"(?<![\w.+-])(" + "|".join(re.escape(name) for name in CISCO_PACKAGES) + r")(?=-\d|[^\w.+/-]|$)"
)
PACKAGE_INSTALL = re.compile(
    r"\b(dnf5?|microdnf|yum|rpm-ostree)\b.*\binstall\b|\brpm\s+-[a-zA-Z]*[iU]"
)
CISCO_REPO_ENABLED = re.compile(
    r"--enablerepo[= ]\S*" + CISCO_REPO
    + r"|" + CISCO_REPO + r"\.enabled=(1|true|yes)"
    + r"|config-manager\b.*\b(enable|setopt)\b.*" + CISCO_REPO
)


@dataclass(frozen=True)
class Violation:
    path: str
    name: str
    reason: str = ""

    def __str__(self) -> str:
        if self.reason:
            return f"{self.path}: {self.reason}"
        return f"{self.path}: depends on `{self.name}` ({FORBIDDEN[self.name]})"


def lock_violations(lock: Path, root: Path = ROOT) -> list[Violation]:
    shown = str(lock.relative_to(root)) if lock.is_relative_to(root) else str(lock)
    if not lock.is_file():
        return [Violation(shown, "", "cannot inspect the resolved media dependencies: lockfile is missing or not a file")]
    data = tomllib.loads(lock.read_text(encoding="utf-8"))
    return [
        Violation(shown, package["name"])
        for package in data.get("package", [])
        if package.get("name") in FORBIDDEN
    ]


def _dependency_tables(manifest: dict) -> list[dict]:
    tables = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        tables.append(manifest.get(key, {}))
    tables.append(manifest.get("workspace", {}).get("dependencies", {}))
    for target in manifest.get("target", {}).values():
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            tables.append(target.get(key, {}))
    for section in ("patch", "replace"):
        for registry in manifest.get(section, {}).values():
            if isinstance(registry, dict):
                tables.append(registry)
    return tables


def manifest_violations(manifest_path: Path, root: Path = ROOT) -> list[Violation]:
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    shown = (
        str(manifest_path.relative_to(root))
        if manifest_path.is_relative_to(root)
        else str(manifest_path)
    )
    found = []
    package_name = manifest.get("package", {}).get("name")
    if package_name in FORBIDDEN:
        found.append(Violation(shown, package_name))
    for table in _dependency_tables(manifest):
        for key, spec in table.items():
            name = spec.get("package", key) if isinstance(spec, dict) else key
            if name in FORBIDDEN:
                found.append(Violation(shown, name))
    return found


def manifests(root: Path = ROOT) -> list[Path]:
    out = []
    for path in root.rglob("Cargo.toml"):
        if any(part in SKIP_DIRS for part in path.relative_to(root).parts):
            continue
        out.append(path)
    return sorted(out)


def is_containerfile(path: Path) -> bool:
    name = path.name
    if name.endswith(".dockerignore"):
        return False
    return name in ("Containerfile", "Dockerfile") or name.startswith(("Containerfile.", "Dockerfile."))


def image_sources(root: Path = ROOT) -> list[Path]:
    """Containerfiles anywhere, and the package lists and scripts under `os/`."""
    out = []
    for directory, subdirectories, files in os.walk(root):
        subdirectories[:] = [name for name in subdirectories if name not in SKIP_DIRS]
        under_os = Path(directory).relative_to(root).parts[:1] == ("os",)
        for name in files:
            path = Path(directory, name)
            if is_containerfile(path) or (under_os and path.suffix in (".sh", ".packages")):
                out.append(path)
    return sorted(out)


def logical_lines(text: str) -> list[str]:
    """Code lines, comments dropped and backslash continuations joined."""
    lines, pending = [], ""
    for physical in text.splitlines():
        if physical.lstrip().startswith("#"):
            continue
        stripped = physical.rstrip()
        if stripped.endswith("\\"):
            pending += stripped[:-1] + " "
            continue
        lines.append(pending + physical)
        pending = ""
    if pending:
        lines.append(pending)
    return lines


def final_base(lines: list[str]) -> str:
    """The image a Containerfile's final stage builds on, ARG defaults expanded."""
    args: dict[str, str] = {}
    stages: dict[str, str] = {}
    base = ""
    for line in lines:
        words = line.split()
        if not words:
            continue
        if words[0].upper() == "ARG" and len(words) > 1 and "=" in words[1]:
            name, value = words[1].split("=", 1)
            args.setdefault(name, value.strip("\"'"))
        elif words[0].upper() == "FROM":
            operands = [word for word in words[1:] if not word.startswith("--")]
            if not operands:
                continue
            image = re.sub(
                r"\$\{(\w+)[^}]*\}|\$(\w+)",
                lambda match: args.get(match.group(1) or match.group(2), match.group(0)),
                operands[0],
            )
            base = stages.get(image, image)
            if len(operands) >= 3 and operands[1].upper() == "AS":
                stages[operands[2]] = base
    return base


def image_violations(root: Path = ROOT) -> list[Violation]:
    found = []
    for path in image_sources(root):
        shown = str(path.relative_to(root))
        if shown in CISCO_ENVIRONMENTS:
            continue
        lines = logical_lines(path.read_text(encoding="utf-8", errors="replace"))
        for line in lines:
            named = CISCO_PACKAGE_TOKEN.search(line)
            if named and (PACKAGE_INSTALL.search(line) or path.suffix == ".packages"):
                found.append(Violation(
                    shown, named.group(1),
                    f"installs Cisco's `{named.group(1)}`; only the owner's media-codecs download "
                    "may bring it to a device, never a shipped image"))
            if CISCO_REPO_ENABLED.search(line):
                found.append(Violation(
                    shown, CISCO_REPO, f"enables `{CISCO_REPO}`; shipped images resolve without it"))
            if is_containerfile(path) and BUILD_DEPS in line:
                found.append(Violation(
                    shown, BUILD_DEPS,
                    "runs dist/ci/install-build-deps.sh, whose test profile installs Cisco's "
                    "OpenH264; it belongs to CI and developer environments only"))
        if is_containerfile(path) and CI_BUILDER_IMAGE in final_base(lines):
            found.append(Violation(
                shown, CI_BUILDER_IMAGE,
                "builds its final stage on the CI builder image, which carries Cisco's OpenH264; "
                "copy the build outputs out of it instead"))
    return found


def final_stage(lines: list[str]) -> list[str]:
    starts = [index for index, line in enumerate(lines) if line.lstrip().upper().startswith("FROM ")]
    return lines[starts[-1]:] if starts else lines


def live_repository_violations(root: Path = ROOT) -> list[Violation]:
    """Each live-repository image drops Cisco's repository and gates what it resolved."""
    found = []
    for shown in sorted(LIVE_REPOSITORY_IMAGES):
        path = root / shown
        if not path.is_file():
            continue
        stage = final_stage(logical_lines(path.read_text(encoding="utf-8", errors="replace")))
        installs = [index for index, line in enumerate(stage) if PACKAGE_INSTALL.search(line)]
        if not installs:
            continue
        removal = next((index for index, line in enumerate(stage) if CISCO_REPO_REMOVAL in line), None)
        first = stage[installs[0]]
        removed_first = removal is not None and (
            removal < installs[0]
            or (removal == installs[0] and first.index(CISCO_REPO_REMOVAL) < PACKAGE_INSTALL.search(first).start())
        )
        if not removed_first:
            found.append(Violation(
                shown, CISCO_REPO,
                f"installs from live repositories without first running `{CISCO_REPO_REMOVAL}` "
                "in the same stage"))
        last = stage[installs[-1]]
        gated = any(
            RESOLVED_GATE.search(line)
            and (index > installs[-1]
                 or (index == installs[-1]
                     and RESOLVED_GATE.search(line).start() > PACKAGE_INSTALL.search(last).start()))
            for index, line in enumerate(stage)
        )
        if not gated:
            found.append(Violation(
                shown, "verify-no-cisco-openh264",
                "does not run os/scripts/verify-no-cisco-openh264.sh on the finished rootfs "
                "after its last package install"))
    return found


def stale_environments(root: Path = ROOT) -> list[Violation]:
    return [
        Violation(path, path, f"is named in {table} but does not exist")
        for table, paths in (("CISCO_ENVIRONMENTS", CISCO_ENVIRONMENTS),
                             ("LIVE_REPOSITORY_IMAGES", LIVE_REPOSITORY_IMAGES))
        for path in sorted(paths)
        if not (root / path).is_file()
    ]


def check(root: Path = ROOT) -> list[Violation]:
    found = lock_violations(root / "Cargo.lock", root)
    for manifest in manifests(root):
        found.extend(manifest_violations(manifest, root))
    found.extend(image_violations(root))
    found.extend(live_repository_violations(root))
    return found


def main() -> int:
    found = check() + stale_environments()
    if found:
        print(
            "GStreamer is the only media stack, and no shipped image carries Cisco's OpenH264:",
            file=sys.stderr,
        )
        for violation in found:
            print(violation)
        return 1
    print(
        "No in-tree codec crates (openh264, ffmpeg-next); no shipped image installs Cisco's OpenH264.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
