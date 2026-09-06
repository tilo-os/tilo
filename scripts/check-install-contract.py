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
for runner, architecture in (
    ("imager-linux-x86_64", "x86_64"),
    ("imager-linux-aarch64", "aarch64"),
):
    assert f'{{"arch":"{architecture}","runner":"{runner}"}}' in imager_workflow
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
packaging_docs = (ROOT / "docs/packaging.md").read_text()
rpm_publisher = (ROOT / "dist/ci/publish-forgejo-packages.sh").read_text()
rpm_signer = (ROOT / "dist/rpm/sign-rpms.sh").read_text()
rpm_signing_tests = (ROOT / "dist/rpm/test-signing-contract.sh").read_text()
rpm_repo_generator = (ROOT / "dist/rpm/make-repo-file.sh").read_text()
rpm_build_deps = (ROOT / "dist/ci/install-build-deps.sh").read_text()
rpm_release_verifier = (ROOT / "dist/ci/verify-release-artifacts.sh").read_text()
assert "https://code.sandbox.example/api/packages/qos/rpm/fedora/44.repo" in packaging_docs
assert "/api/packages/$owner/rpm/fedora/44/upload" in rpm_publisher
assert "sudo rpm-ostree install qos-session" in packaging_docs
assert "sudo rpm-ostree update" in packaging_docs
assert "sudo rpm-ostree uninstall qos-session" in packaging_docs
assert "sudo rpm-ostree rollback" in packaging_docs
assert "public qos organization/package visibility" in packaging_docs.lower()
assert "--netrc-file" in packaging_docs
assert "username=<Forgejo username>" in packaging_docs
assert "password=<PAT with read:package>" in packaging_docs
assert "gpgcheck=1" in packaging_docs
assert "repo_gpgcheck=0" in packaging_docs
assert "gpgkey=file:///etc/pki/rpm-gpg/qos-rpm-signing-key.asc" in packaging_docs
assert "releases/download/desktop-v${QOS_DESKTOP_VERSION}/qos-fedora-44.repo" in packaging_docs
assert "releases/download/desktop-v${QOS_DESKTOP_VERSION}/qos-rpm-signing-key.asc" in packaging_docs
assert "dnf config-manager` persists credentials" in packaging_docs
assert "dnf stores the credential" not in packaging_docs.lower()
assert "QOS_RPM_SIGNING_KEY_B64" in packaging_docs
assert "unencrypted armored" in packaging_docs and "private key" in packaging_docs
assert "QOS_RPM_SIGNING_KEY_B64" in session_workflow
assert "QOS_RPM_SIGNING_FINGERPRINT" in session_workflow
assert "startsWith(github.ref, 'refs/tags/desktop-v') && '1' || '0'" in session_workflow
assert "QOS_RPM_SIGNING_KEY_ARTIFACT: ${{ matrix.arch == 'amd64' && '1' || '0' }}" in session_workflow
rpm_job = (ROOT / "dist/ci/run-package-job.sh").read_text()
assert "--env QOS_RPM_SIGNING_KEY_B64" in rpm_job
assert "if [[ ${QOS_RPM_SIGNING_ENABLED:-0} == 1 ]]" in rpm_job
assert "dist/rpm/sign-rpms.sh /artifacts" in rpm_job
assert "QOS_RPM_SIGNING_KEY_B64" in rpm_signer
assert "rpmkeys --dbpath" in rpm_signer
assert "rpmsign" in rpm_signer
assert "substituted-key" in rpm_signing_tests
assert "QOS_FAKE_RPM_MODE=unsigned" in rpm_signing_tests
assert "QOS_FAKE_RPM_MODE=wrong" in rpm_signing_tests
assert "gpgcheck=1" in rpm_repo_generator
assert "repo_gpgcheck=0" in rpm_repo_generator
assert "make-repo-file.sh" in (ROOT / "dist/ci/run-package-job.sh").read_text()
assert "rpm-sign" in rpm_build_deps
assert "expected_count=17" in rpm_release_verifier
assert "expected_count=24" in rpm_release_verifier
assert "RPM is unsigned" in rpm_release_verifier
assert "make-repo-file.sh" in rpm_release_verifier
assert "cmp -- \"$expected_repo\" \"$release_dir/qos-fedora-44.repo\"" in rpm_release_verifier
assert "Select native release lanes" in session_workflow
assert 'QOS_DESKTOP_ARM64_RELEASES: "0"' in session_workflow
assert '[[ ${QOS_DESKTOP_ARM64_RELEASES:-0} == 1 ]]' in session_workflow
assert 'refs/tags/desktop-v* || ${FULL_NATIVE:-false} == true' in session_workflow
assert '{arch:"amd64",runner:"runner",targets:$targets}' in session_workflow
assert '{arch:"arm64",runner:"runner-arm64",targets:$targets}' in session_workflow
assert "release_arches=amd64,arm64" in session_workflow
assert "release_arches: ${{ steps.lanes.outputs.release_arches }}" in session_workflow
assert "RELEASE_ARCHES: ${{ needs.validate.outputs.release_arches }}" in session_workflow
assert 'verify-release-artifacts.sh release "$version" "$RELEASE_ARCHES"' in session_workflow
assert "QOS_LANE_TARGETS" in session_workflow
assert "while IFS='|' read -r distro image revision" in session_workflow

targets = {item["distribution"]: item for item in contract["desktopSession"]}
assert list(targets) == ["Ubuntu", "Debian", "Fedora", "Arch Linux", "NixOS"]
for item in targets.values():
    assert len(item["architectures"]) == 2
    assert item["plannedArchitectures"] == []
    assert item["package"] == "qos-session"

print("install contract matches the current Imager and desktop-session release lanes")
