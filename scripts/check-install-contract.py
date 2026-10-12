#!/usr/bin/env python3
"""Fail when the documented installation matrix drifts from release tooling."""

from __future__ import annotations

import json
import os
import subprocess
import textwrap
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
# Standalone host-session runs build the same immutable app store as releases.
# Exercise the actual preflight shell before any checkout/download/build step:
# an absent authority must fail immediately, without weakening staging checks.
host_session_workflow = (ROOT / ".forgejo/workflows/host-session-e2e.yml").read_text()
host_rpms = host_session_workflow.split("  rpms:\n", 1)[1].split("  host-session:\n", 1)[0]
assert "TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY: ${{ vars.TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY }}" in host_rpms
preflight, expensive_steps = host_rpms.split("      # A full clone", 1)
assert "if: ${{ !inputs.rpm-artifact && !inputs.rpm-source-run }}" in preflight
assert "Fetch the env-runtime environment image" in expensive_steps
assert "dist/ci/run-package-job.sh" in expensive_steps
owner_preflight = preflight.split("      - name: Validate the app-store registry owner\n", 1)[1]
owner_check = textwrap.dedent(owner_preflight.split("        run: |\n", 1)[1])
for owner, accepted in (
    (None, False),
    ("", False),
    ("invalid", False),
    ("0" * 64, False),
    ("ab" * 31, False),
    ("ab" * 33, False),
    ("ab" * 32, True),
    ("AB" * 32, True),
):
    check_env = os.environ.copy()
    check_env.pop("TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY", None)
    if owner is not None:
        check_env["TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY"] = owner
    result = subprocess.run(["bash", "-c", owner_check], env=check_env, capture_output=True)
    assert (result.returncode == 0) == accepted, "host-session registry-owner preflight"

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
assert "isolated broker" in packaging_docs
assert "desktop-rpm-broker.md" in packaging_docs
assert "TILO_RPM_SIGNING_FINGERPRINT" in session_workflow
package_job = session_workflow.split("  package:\n", 1)[1].split("  sign-rpm:\n", 1)[0]
signing_job = session_workflow.split("  sign-rpm:\n", 1)[1].split("  host-session-e2e:\n", 1)[0]
assert "    needs: validate\n" in package_job
assert "secrets.TILO_RPM_SIGNING" not in session_workflow
assert "enable-openid-connect" not in package_job
assert "enable-openid-connect: true" in signing_job
assert "if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/desktop-v')" in signing_job
assert "needs: [validate, package]" in signing_job
assert "dist/rpm/sign-rpms.sh signed-fedora" in signing_job
assert "vars.TILO_RPM_SIGNING_FINGERPRINT" in signing_job
validation = session_workflow.split("  validate:\n", 1)[1].split("  package:\n", 1)[0]
assert 'run: tools/ci/receipt.sh verify "$GITHUB_SHA" origin' in validation
assert "needs: [validate, package, sign-rpm, host-session-e2e]" in session_workflow
assert "'signed-fedora-44-amd64' || 'fedora-44-amd64'" in session_workflow
rpm_job = (ROOT / "dist/ci/run-package-job.sh").read_text()
assert "--env TILO_RPM_SIGNING" not in rpm_job
assert "ACTIONS_ID_TOKEN_REQUEST_" in rpm_job
assert "dist/rpm/sign-rpms.sh" not in rpm_job
assert 'exec python3 "$(dirname -- "$0")/sign-rpms-broker.py"' in rpm_signer
rpm_broker = (ROOT / "dist/rpm/sign-rpms-broker.py").read_text()
assert "'/usr/local/libexec/tilo-desktop-rpm-broker'" in rpm_broker
assert "trusted_broker(BROKER, env)" in rpm_broker
assert "private-key environment is forbidden" in rpm_broker
assert "broker changed package header or payload" in rpm_broker
assert "'rpmkeys', '--dbpath'" in rpm_broker
assert "'gpg', '--homedir'" in rpm_broker
assert "'--checksig', '--verbose'" in rpm_broker
assert "verify_published" in rpm_broker
assert "OpenPGP private authority" in (ROOT / "docs/desktop-rpm-broker.md").read_text()
assert "substituted-key" in rpm_signing_tests
assert "TILO_FAKE_RPM_MODE=unsigned" in rpm_signing_tests
assert "TILO_FAKE_RPM_MODE=wrong" in rpm_signing_tests
assert "gpgcheck=1" in rpm_repo_generator
assert "repo_gpgcheck=0" in rpm_repo_generator
assert "make-repo-file.sh" in signing_job
assert "rpm-sign" in rpm_build_deps
assert "expected_count=20" in rpm_release_verifier
assert "expected_count=30" in rpm_release_verifier
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
package_targets = (ROOT / "dist/ci/package-targets.txt").read_text()
assert "amd64|arch|docker.io/menci/archlinuxarm@sha256:" in package_targets
assert "arm64|arch|docker.io/gomasy/archlinuxarm@sha256:" in package_targets
assert "amd64_targets=$(dist/ci/package-targets.sh amd64)" in session_workflow
assert "arm64_targets=$(dist/ci/package-targets.sh arm64)" in session_workflow
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
