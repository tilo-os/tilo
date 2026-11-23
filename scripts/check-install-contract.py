#!/usr/bin/env python3
"""Fail when the documented installation matrix drifts from release tooling."""

from __future__ import annotations

import json
import os
import re
import subprocess
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

# Staged media writing is a development route, and this file is the copy the
# release tooling publishes to the landing page verbatim. The stage has to be
# stated by the data itself, next to the claim it qualifies, so that neither the
# copy nor the page can present a route under development as a support
# commitment: a reader that only looks at these fields still sees the stage.
staged_installer = contract["completeSystem"]["stagedLiveInstaller"]
assert staged_installer == {
    "classification": "development",
    "liveInstallerModes": ["guided-alongside"],
    "targetMinimumGiB": 34,
    "reclaimsStagingAfterFirstSession": True,
    "resizesExistingFilesystems": False,
}
assert any(
    "development guided-import route" in principle for principle in contract["principles"]
)
assert any(
    "Development routes are under development" in principle
    for principle in contract["principles"]
)

# The installer release is t's: each host's downloads come from its package check on both
# architectures, and the release check requires every one, with the media, before a tag publishes.
t_checks = {check["name"]: check for check in tomllib.loads((ROOT / "t.toml").read_text())["check"]}
release_check = (ROOT / "tools/ci/imager-release/assets.sh").read_text()
for host in ("linux", "windows", "macos"):
    assert t_checks[f"package/{host}-{{arch}}"]["matrix"]["arch"] == ["x86_64", "aarch64"]
assert 'tilo-installer-linux-$arch.img.zst' in release_check
assert 'tilo-imager-setup-windows-$arch.exe' in release_check
assert 'apps/imager/packaging/generate-winget.py' in release_check
windows_host = next(host for host in contract["imager"]["hosts"] if host["os"].startswith("Windows"))
assert windows_host["architectures"] == ["x86_64", "aarch64"]

# People follow the Homebrew commands the landing page prints, so those commands
# have to name the tap the release workflow actually publishes. The contract is
# the copy the site consumes; derive every field in it from the publisher here
# instead of restating a destination by hand in a second place.
def workflow_env_value(text: str, name: str) -> str:
    """The literal value of a workflow env entry, or of its `||` fallback."""
    match = re.search(
        rf"^  {name}: (?:\$\{{\{{[^}}]*\|\|\s*'([^']+)'\s*\}}\}}|(\S+))$", text, re.M
    )
    assert match, f"release workflow has no literal value for {name}"
    return match.group(1) or match.group(2)

homebrew_tap_repository = workflow_env_value(imager_workflow, "TILO_HOMEBREW_TAP_REPOSITORY")
homebrew_tap_name = workflow_env_value(imager_workflow, "TILO_HOMEBREW_TAP_NAME")
release_server = workflow_env_value(imager_workflow, "TILO_RELEASE_SERVER")
assert re.fullmatch(r"[A-Za-z0-9_.-]+/homebrew-[A-Za-z0-9_.-]+", homebrew_tap_repository)
assert re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", homebrew_tap_name)
homebrew = contract["imager"]["homebrew"]
assert homebrew["tap"] == {
    "name": homebrew_tap_name,
    "repository": homebrew_tap_repository,
    "url": f"{release_server}/{homebrew_tap_repository}.git",
    "command": f"brew tap {homebrew_tap_name} {release_server}/{homebrew_tap_repository}.git",
}
# The formula and cask names are the ones Homebrew audits in the tap, so read
# them out of the templates that produce those files.
formula_template = (ROOT / "apps/imager/packaging/homebrew/tilo-imager.rb.in").read_text()
cask_template = (ROOT / "apps/imager/packaging/homebrew/tilo-imager-cask.rb.in").read_text()
formula_name = re.search(r'bin\.install "([^"]+)"', formula_template).group(1)
cask_name = re.search(r'^cask "([^"]+)" do$', cask_template, re.M).group(1)
# The formula and the cask share one name, because that is the single name a
# person types and Homebrew resolves.
assert formula_name == cask_name == "tilo-imager"
assert homebrew["formula"]["name"] == formula_name
assert homebrew["cask"]["name"] == cask_name
assert homebrew["formula"]["host"] == "Linux" and homebrew["cask"]["host"] == "macOS"
for host_key, flags in (("formula", ""), ("cask", "--cask ")):
    command = homebrew[host_key]
    assert command["install"] == f"brew install {flags}{command['name']}"
    assert command["upgrade"] == f"brew upgrade {flags}{command['name']}"
    assert command["uninstall"] == f"brew uninstall {flags}{command['name']}"

# Both release scripts read the destination from the workflow instead of
# repeating it, so the tap the page prints, the tap the README prints and the
# tap the install tests exercise cannot disagree.
for script in (
    "apps/imager/packaging/publish-homebrew-tap.sh",
    "apps/imager/packaging/publish-site-release.sh",
):
    text = (ROOT / script).read_text()
    assert f"TILO_HOMEBREW_TAP_REPOSITORY:-{homebrew_tap_repository}" in text, script
    assert f"TILO_HOMEBREW_TAP_NAME:-{homebrew_tap_name}" in text, script

site_publisher = (ROOT / "apps/imager/packaging/publish-site-release.sh").read_text()
assert '.imager.homebrew.tap.command' in site_publisher
assert '.imager.homebrew.tap.repository' in site_publisher
assert '.imager.homebrew.tap.name' in site_publisher
assert 'landing page Homebrew tap does not match the release tap' in site_publisher
assert "grep -R -h -o -- 'brew tap" in site_publisher
assert "'brew install --cask tilo-imager' 'brew install tilo-imager'" in site_publisher

# The install tests run the same tap people get told to run.
assert imager_workflow.count('brew tap "$TILO_HOMEBREW_TAP_NAME"') == 2
assert "brew tap tilo/" not in imager_workflow

# Every host/architecture the install contract declares is one the release
# workflow builds and requires, and every download it declares is a file the
# publication check carries and the landing-page promotion links. A host whose
# artifacts are declared and built but never mapped is how Windows ARM64 stayed
# declared, built, and absent from the download matrix.
required_matrix = imager_workflow.split("          required=(\n", 1)[1].split("          )\n", 1)[0]
required_assets = {
    line.strip() for line in required_matrix.splitlines() if line.strip()
}
assert required_assets, "the publication check requires no artifact"

declared_entries = {}
for host in contract["imager"]["hosts"]:
    assert host["architectures"], f"{host['os']} declares no architecture"
    assert host["downloads"], f"{host['os']} declares no download"
    for download in host["downloads"]:
        assert set(download) == {"name", "asset", "pageKey"}, download
        assert "{arch}" in download["asset"], download
        assert "{archKey}" in download["pageKey"], download
        for architecture in host["architectures"]:
            release_file = download["asset"].replace("{arch}", architecture)
            assert release_file in required_assets, f"{release_file} is not a release prerequisite"
            runner = "windows" if host["os"].startswith("Windows") else host["os"].lower()
            if runner == "linux":
                # The Linux jobs come from the plan's native runner matrix,
                # not a per-host label.
                job = {"x86_64": "runner-release", "aarch64": "runner-large-arm64"}[architecture]
                assert f'"arch":"{architecture}","runner":"{job}"' in imager_workflow, release_file
            else:
                assert f"runner: imager-{runner}-{architecture}" in imager_workflow, release_file
            arch_key = "X64" if architecture == "x86_64" else "Arm64"
            entry = download["pageKey"].replace("{archKey}", arch_key)
            assert entry not in declared_entries, f"{entry} is declared twice"
            declared_entries[entry] = release_file

# Windows ARM64 was the host this gap dropped: declared, built by its own
# runner, required by the publication check, and absent from the page.
assert {
    entry: release_file
    for entry, release_file in declared_entries.items()
    if entry.startswith("windowsArm64")
} == {
    "windowsArm64Setup": "tilo-imager-setup-windows-aarch64.exe",
    "windowsArm64Standalone": "tilo-imager-windows-aarch64.exe",
    "windowsArm64Portable": "tilo-imager-windows-aarch64.zip",
}

# The publisher may not keep a second copy of this list: it has to derive every
# entry from the contract, and it has to verify the manifest it publishes rather
# than a hard-kept subset of it.
assert ".imager.hosts[] | .os as $os | .downloads[]" in site_publisher
assert ".pageKey" in site_publisher
assert "jq -r '.assets | keys[]' \"$manifest\"" in site_publisher
for entry in declared_entries:
    assert f'"{entry} "' not in site_publisher, entry

# Imager's native Windows release includes `ui`, and with it the host
# services that re-export the clipboard crate. `arboard` reads and writes a
# Wayland selection over data-control and falls back to X11, so it must stay on
# the targets that have those protocols and never enter the page's build.
for manifest in (
    "crates/ui/ui-core/Cargo.toml",
    "crates/sdk/tilo-process-services/Cargo.toml",
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
assert "arboard" in linux_clipboard_dependencies
assert "wayland-data-control" in linux_clipboard_dependencies["arboard"]["features"]

# The package jobs build the same immutable app store as releases. Exercise the job's own
# preflight before it fetches or compiles anything: an absent authority must fail at once,
# without weakening the staging checks (dist/rpm/stage-app-store.sh).
package_job = (ROOT / "dist/ci/package-job.sh").read_text()
# The Fedora-only setup starts at the condition around its trust root; a run that names only
# other distributions skips it and needs no key.
job_preflight, job_expensive = package_job.split("\nif ((needs_app_store)); then\n", 1)
assert "bash dist/ci/fetch-package-env-runtime.sh" in job_expensive
assert "dist/ci/run-package-job.sh" in job_expensive
assert "fetch-package-env-runtime" not in job_preflight
assert "run-package-job" not in job_preflight
owner_check = "require_registry_owner() {" + job_preflight.split("require_registry_owner() {", 1)[1]
owner_check += "\nrequire_registry_owner\n"
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
    assert (result.returncode == 0) == accepted, "package-job registry-owner preflight"

packaging_docs = (ROOT / "docs/packaging.md").read_text()
rpm_signer = (ROOT / "dist/rpm/sign-rpms.sh").read_text()
rpm_signing_tests = (ROOT / "dist/rpm/test-signing-contract.sh").read_text()
rpm_repo_generator = (ROOT / "dist/rpm/make-repo-file.sh").read_text()
rpm_build_deps = (ROOT / "dist/ci/install-build-deps.sh").read_text()
rpm_release_verifier = (ROOT / "dist/ci/verify-release-artifacts.sh").read_text()
rpm_spec = (ROOT / "dist/rpm/tilo-session.spec").read_text()
assert "https://code.tilo.so/api/packages/tilo/rpm/fedora/44.repo" in packaging_docs
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
for desktop in contract["desktopSession"]:
    assert len(desktop["architectures"]) == 1
    assert len(desktop["plannedArchitectures"]) == 1
    assert any(
        arch in desktop["plannedArchitectures"]
        for arch in ("arm64", "aarch64", "aarch64-linux")
    ), desktop["distribution"]
getting_started = (ROOT / "docs/getting-started.md").read_text()
release_pipeline = (ROOT / "docs/release-pipeline.md").read_text()
for name, page in (
    ("docs/getting-started.md", getting_started),
    ("docs/packaging.md", packaging_docs),
    ("docs/release-pipeline.md", release_pipeline),
):
    assert "when enabled" not in page, name
    assert "not release-guarded" not in page, name
package_targets = (ROOT / "dist/ci/package-targets.txt").read_text()
assert "amd64|arch|docker.io/menci/archlinuxarm@sha256:" in package_targets
assert "arm64|arch|docker.io/gomasy/archlinuxarm@sha256:" in package_targets
assert "TILO_BLAKE3_PURE=1" in rpm_job
assert 'CARGO_FEATURE_PURE="$TILO_BLAKE3_PURE"' in rpm_job
assert "cargo_jobs=${TILO_CARGO_JOBS:-2}" in rpm_job
assert "cores = '\"${CARGO_BUILD_JOBS:-2}\"" in rpm_job
assert "CARGO_PROFILE_RELEASE_LTO=thin" in rpm_job
assert "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16" in rpm_job
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

print("install contract matches the current Imager and desktop-session release jobs")
