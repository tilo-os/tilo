#!/usr/bin/env python3
"""Fail when the documented installation matrix drifts from release tooling."""

from __future__ import annotations

import json
from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parents[1]
contract = json.loads((ROOT / "docs/install-contract.json").read_text())

assert contract["schemaVersion"] == 2
assert contract["product"] == "Tilo"
assert contract["completeSystem"]["architectures"] == ["x86_64", "aarch64"]
assert "minimumUsbGiB" not in contract["completeSystem"]
assert contract["completeSystem"]["usbCapacity"] == {
    "requirement": "expanded-image-size",
    "catalogField": "image_size",
    "unit": "bytes",
}
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
    medium = f"tilo-installer-linux-{architecture}.img.zst"
    assert f"            {medium}" in imager_workflow
assert """      - name: Verify the installer-medium release payload
        run: |
          set -euo pipefail
          medium="tilo-installer-linux-${{ matrix.arch }}.img.zst"
""" in imager_workflow
assert """            dist/tilo-installer/tilo-installer-linux-${{ matrix.arch }}.img.zst
            dist/tilo-installer/tilo-installer-linux-${{ matrix.arch }}.img.zst.sha256
            dist/tilo-installer/catalog-${{ matrix.arch }}.json
          if-no-files-found: error
""" in imager_workflow
assert """            (cd "$(dirname "$path")" && sha256sum --check --strict "$(basename "$path").sha256")""" in imager_workflow
assert 'tilo-installer-*.asc' in imager_workflow
for host in ("linux", "windows", "macos"):
    assert f"  {host}:" in imager_workflow
windows_host = next(host for host in contract["imager"]["hosts"] if host["os"].startswith("Windows"))
assert windows_host["architectures"] == ["x86_64", "aarch64"]
for architecture in windows_host["architectures"]:
    assert f"runner: imager-windows-{architecture}" in imager_workflow
    assert f"            tilo-imager-setup-windows-{architecture}.exe" in imager_workflow
assert "python3 apps/imager/packaging/generate-winget.py release-assets" in imager_workflow

# Imager's native Windows release includes `ui`, and with it the host
# services that re-export the clipboard crate. The Wayland clipboard backend
# consumes Unix file descriptors, so it must stay on the Linux target of
# `tilo-clipboard` and never enter non-Linux builds through ui or tilo-host.
for manifest in (
    "crates/sdk/ui-core/Cargo.toml",
    "crates/sdk/tilo-host/Cargo.toml",
    "crates/platform/tilo-clipboard/Cargo.toml",
):
    parsed = tomllib.loads((ROOT / manifest).read_text())
    assert "smithay-clipboard" not in parsed.get("dependencies", {}), manifest
clipboard_manifest = tomllib.loads(
    (ROOT / "crates/platform/tilo-clipboard/Cargo.toml").read_text()
)
linux_clipboard_dependencies = clipboard_manifest["target"]['cfg(target_os = "linux")'][
    "dependencies"
]
assert "smithay-clipboard" in linux_clipboard_dependencies

session_workflow = (ROOT / ".forgejo/workflows/release-desktop.yml").read_text()
packaging_docs = (ROOT / "docs/packaging.md").read_text()
rpm_publisher = (ROOT / "dist/ci/publish-forgejo-packages.sh").read_text()
rpm_signer = (ROOT / "dist/rpm/sign-rpms.sh").read_text()
rpm_signing_tests = (ROOT / "dist/rpm/test-signing-contract.sh").read_text()
rpm_repo_generator = (ROOT / "dist/rpm/make-repo-file.sh").read_text()
rpm_build_deps = (ROOT / "dist/ci/install-build-deps.sh").read_text()
rpm_release_verifier = (ROOT / "dist/ci/verify-release-artifacts.sh").read_text()
rpm_spec = (ROOT / "dist/rpm/tilo-session.spec").read_text()
assert "https://code.tilo.so/api/packages/tilo/rpm/fedora/44.repo" in packaging_docs
assert "/api/packages/$owner/rpm/fedora/44/upload" in rpm_publisher
assert "sudo rpm-ostree install tilo-session" in packaging_docs
assert "sudo rpm-ostree update" in packaging_docs
assert "sudo rpm-ostree uninstall tilo-session" in packaging_docs
assert "sudo rpm-ostree rollback" in packaging_docs
assert "public tilo organization/package visibility" in packaging_docs.lower()
assert "--netrc-file" in packaging_docs
assert "username=<Forgejo username>" in packaging_docs
assert "password=<PAT with read:package>" in packaging_docs
assert "gpgcheck=1" in packaging_docs
assert "repo_gpgcheck=0" in packaging_docs
assert "gpgkey=file:///etc/pki/rpm-gpg/tilo-rpm-signing-key.asc" in packaging_docs
assert "releases/download/desktop-v${TILO_DESKTOP_VERSION}/tilo-fedora-44.repo" in packaging_docs
assert "releases/download/desktop-v${TILO_DESKTOP_VERSION}/tilo-rpm-signing-key.asc" in packaging_docs
assert "dnf config-manager` persists credentials" in packaging_docs
assert "dnf stores the credential" not in packaging_docs.lower()
assert "TILO_RPM_SIGNING_KEY_B64" in packaging_docs
assert "unencrypted armored" in packaging_docs and "private key" in packaging_docs
assert "TILO_RPM_SIGNING_KEY_B64" in session_workflow
assert "TILO_RPM_SIGNING_FINGERPRINT" in session_workflow
assert "startsWith(github.ref, 'refs/tags/desktop-v') && '1' || '0'" in session_workflow
assert "TILO_RPM_SIGNING_KEY_ARTIFACT: ${{ matrix.arch == 'amd64' && '1' || '0' }}" in session_workflow
rpm_job = (ROOT / "dist/ci/run-package-job.sh").read_text()
assert "--env TILO_RPM_SIGNING_KEY_B64" in rpm_job
assert "if [[ ${TILO_RPM_SIGNING_ENABLED:-0} == 1 ]]" in rpm_job
assert "dist/rpm/sign-rpms.sh /artifacts" in rpm_job
assert "TILO_RPM_SIGNING_KEY_B64" in rpm_signer
assert "rpmkeys --dbpath" in rpm_signer
assert "rpmsign" in rpm_signer
assert "substituted-key" in rpm_signing_tests
assert "TILO_FAKE_RPM_MODE=unsigned" in rpm_signing_tests
assert "TILO_FAKE_RPM_MODE=wrong" in rpm_signing_tests
assert "gpgcheck=1" in rpm_repo_generator
assert "repo_gpgcheck=0" in rpm_repo_generator
assert "make-repo-file.sh" in (ROOT / "dist/ci/run-package-job.sh").read_text()
assert "rpm-sign" in rpm_build_deps
assert "expected_count=18" in rpm_release_verifier
assert "expected_count=26" in rpm_release_verifier
assert 'tilo-session-apps-${version}-1.fc44.x86_64.rpm' in rpm_release_verifier
assert "RPM is unsigned" in rpm_release_verifier
assert "make-repo-file.sh" in rpm_release_verifier
assert "cmp -- \"$expected_repo\" \"$release_dir/tilo-fedora-44.repo\"" in rpm_release_verifier
# The package's runtime publisher is a trust pin for the exact signed runtime
# shipped beside it, not administrator-owned configuration.  Keeping an old
# copy during an upgrade makes every packaged app fail closed only after login.
trust_pin = "%{_sysconfdir}/tilo/runtime-bundle-trusted-publishers.txt"
assert f"%config(noreplace) {trust_pin}" not in rpm_spec
assert f"\n{trust_pin}\n" in rpm_spec
assert "Select native release lanes" in session_workflow
assert 'TILO_DESKTOP_ARM64_RELEASES: "1"' in session_workflow
assert '[[ ${TILO_DESKTOP_ARM64_RELEASES:-0} == 1 ]]' in session_workflow
for desktop in contract["desktopSession"]:
    assert len(desktop["architectures"]) == 1
    assert len(desktop["plannedArchitectures"]) == 1
assert 'refs/tags/desktop-v* || ${FULL_NATIVE:-false} == true' in session_workflow
assert "amd64_runner=runner-large" in session_workflow
assert "arm64_runner=runner-large-arm64" in session_workflow
assert '{arch:"amd64",runner:$amd64_runner,targets:$amd64_targets}' in session_workflow
assert '{arch:"arm64",runner:$arm64_runner,targets:$arm64_targets}' in session_workflow
assert "docker.io/menci/archlinuxarm@sha256:" in session_workflow
assert "docker.io/gomasy/archlinuxarm@sha256:" in session_workflow
assert "release_arches=amd64,arm64" in session_workflow
assert "release_arches: ${{ steps.lanes.outputs.release_arches }}" in session_workflow
assert "RELEASE_ARCHES: ${{ needs.validate.outputs.release_arches }}" in session_workflow
assert 'verify-release-artifacts.sh release "$version" "$RELEASE_ARCHES"' in session_workflow
assert "TILO_LANE_TARGETS" in session_workflow
assert "while IFS='|' read -r distro image revision" in session_workflow
assert 'echo "::endgroup::"\n              exit 1' in session_workflow
assert "max-parallel:" not in session_workflow
assert "TILO_BLAKE3_PURE=1" in rpm_job
assert 'CARGO_FEATURE_PURE="$TILO_BLAKE3_PURE"' in rpm_job
assert "cargo_jobs=${TILO_CARGO_JOBS:-2}" in rpm_job
assert 'TILO_CARGO_JOBS: "6"' in session_workflow
assert "cores = '\"${CARGO_BUILD_JOBS:-2}\"" in rpm_job
assert "CARGO_PROFILE_RELEASE_LTO=thin" in rpm_job
assert "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16" in rpm_job
nix_package = (ROOT / "dist/nix/package.nix").read_text()
assert 'CARGO_PROFILE_RELEASE_LTO = "thin";' in nix_package
assert 'CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16";' in nix_package
assert "stage=docker-health" in rpm_job

targets = {item["distribution"]: item for item in contract["desktopSession"]}
assert list(targets) == ["Ubuntu", "Debian", "Fedora", "Arch Linux", "NixOS"]
for item in targets.values():
    assert len(item["architectures"]) == 1
    assert len(item["plannedArchitectures"]) == 1
    assert item["package"] == "tilo-session"

print("install contract matches the current Imager and desktop-session release lanes")
