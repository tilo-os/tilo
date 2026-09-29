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
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
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


@dataclass(frozen=True)
class Violation:
    path: str
    name: str

    def __str__(self) -> str:
        return f"{self.path}: depends on `{self.name}` ({FORBIDDEN[self.name]})"


def lock_violations(lock: Path, root: Path = ROOT) -> list[Violation]:
    if not lock.is_file():
        return []
    data = tomllib.loads(lock.read_text(encoding="utf-8"))
    shown = str(lock.relative_to(root)) if lock.is_relative_to(root) else str(lock)
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


def check(root: Path = ROOT) -> list[Violation]:
    found = lock_violations(root / "Cargo.lock", root)
    for manifest in manifests(root):
        found.extend(manifest_violations(manifest, root))
    return found


def main() -> int:
    found = check()
    if found:
        print("GStreamer is the only media stack; in-tree codec crates are not allowed:")
        for violation in found:
            print(f"  {violation}")
        return 1
    print("No in-tree codec crates (openh264, ffmpeg-next).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
