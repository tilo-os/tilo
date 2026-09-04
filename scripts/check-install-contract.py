#!/usr/bin/env python3
"""Fail when the documented installation matrix drifts from release tooling."""

from __future__ import annotations

import json
from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parents[1]
contract = json.loads((ROOT / "docs/install-contract.json").read_text())

assert contract["schemaVersion"] == 1
assert contract["product"] == "qOS"
assert contract["completeSystem"]["architectures"] == ["x86_64", "aarch64"]
assert contract["completeSystem"]["alongside"] == {
    "rootMinimumGiB": 32,
    "efiMinimumGiB": 2,
    "efiMinimumFreeGiB": 1,
    "resizesExistingFilesystems": False,
}

imager_workflow = (ROOT / ".forgejo/workflows/release-imager.yml").read_text()
for runner, architecture in (("runner", "x86_64"), ("runner-arm64", "aarch64")):
    assert f"arch: {architecture}\n            runner: {runner}" in imager_workflow
    medium = f"qos-installer-linux-{architecture}.img.zst"
    assert f"            {medium}" in imager_workflow
assert """      - name: Verify the installer-medium release payload
        run: |
          set -euo pipefail
          medium="qos-installer-linux-${{ matrix.arch }}.img.zst"
""" in imager_workflow
assert """            dist/qos-installer/qos-installer-linux-${{ matrix.arch }}.img.zst
            dist/qos-installer/qos-installer-linux-${{ matrix.arch }}.img.zst.sha256
            dist/qos-installer/catalog-${{ matrix.arch }}.json
          if-no-files-found: error
""" in imager_workflow
assert """            (cd "$(dirname "$path")" && sha256sum --check --strict "$(basename "$path").sha256")""" in imager_workflow
assert 'qos-installer-*.asc' in imager_workflow
for host in ("linux", "windows", "macos"):
    assert f"  {host}:" in imager_workflow

# Imager's native Windows release includes `ui`. The Wayland clipboard backend
# consumes Unix file descriptors, so it must never enter non-Linux builds.
ui_manifest = tomllib.loads((ROOT / "ui/Cargo.toml").read_text())
assert "smithay-clipboard" not in ui_manifest["dependencies"]
linux_ui_dependencies = ui_manifest["target"]['cfg(target_os = "linux")']["dependencies"]
assert "smithay-clipboard" in linux_ui_dependencies

session_workflow = (ROOT / ".forgejo/workflows/package-desktop.yml").read_text()
assert "Restore the ARM64 include only when" in session_workflow
assert "arch: amd64" in session_workflow
assert "arch: arm64" not in session_workflow

targets = {item["distribution"]: item for item in contract["desktopSession"]}
assert list(targets) == ["Ubuntu", "Debian", "Fedora", "Arch Linux", "NixOS"]
for item in targets.values():
    assert len(item["architectures"]) == 1
    assert len(item["plannedArchitectures"]) == 1
    assert item["package"] == "qos-session"

print("install contract matches the current Imager and desktop-session release lanes")
