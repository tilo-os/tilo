#!/usr/bin/env python3
"""Exercise the image builder's profile gates and build arguments without a build."""

import os
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
BUILD_INTERCEPTED = 73
KERNEL_PIN = "registry.invalid/qos/kernel@sha256:" + "1" * 64
RPMS_PIN = "registry.invalid/qos/rpms@sha256:" + "2" * 64
TARGET_REF = "code.podesta.ai/qos/qos:board-fixture"


class ImageBuildProfileTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="qos image build test ")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.os_dir = self.root / "os"
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.log = self.root / "podman-arguments"
        self.script = self.os_dir / "dev/build-image.sh"
        # Run the complete production script, normalizing a Windows checkout's
        # line endings only. All profile/ref decisions remain production code.
        self.write("os/dev/build-image.sh", (REPO / "os/dev/build-image.sh").read_text())
        self.write("os/dev/hardware_profile.py", (REPO / "os/dev/hardware_profile.py").read_text())
        self.write("os/dev/windows_provider.py", (REPO / "os/dev/windows_provider.py").read_text())
        self.write("os/hardware/profiles/lenovo-83ed.json", (REPO / "os/hardware/profiles/lenovo-83ed.json").read_text())
        self.write("os/dev/_common.sh", """\
set -euo pipefail
QOS_REPO="$QOS_TEST_REPO"
QOS_OS_DIR="$QOS_REPO/os"
QOS_OUTPUT="$QOS_OS_DIR/output"
QOS_IMAGE="${QOS_IMAGE:-localhost/qos-fixture:latest}"
QOS_DISK="$QOS_OUTPUT/qcow2/disk.qcow2"
QOS_INSTALLER_IMAGE=localhost/qos-installer:fixture
QOS_INSTALLER_OUTPUT="$QOS_OUTPUT/installer"
QOS_INSTALLER_DISK="$QOS_INSTALLER_OUTPUT/qcow2/disk.qcow2"
msg() { printf '%s\\n' "$*" >&2; }
die() { printf '%s\\n' "$*" >&2; exit 1; }
""")
        # These external checks are unrelated to forwarding inputs. A failing
        # podman stub ensures execution never reaches any disk-image operation.
        for path in (
            "os/dev/cache-github-assets.sh",
            "os/dev/verify-rpm-snapshot-package-set.sh",
            "os/tests/containerfile_sources.sh",
            "os/tests/app_store.sh",
        ):
            self.write(path, "#!/bin/bash\nexit 0\n", executable=True)
        self.write("os/base-snapshot.env", "BASE_REF=registry.invalid/qos/base:fixture\n")
        self.write("os/rpms-snapshot.env", f"RPMS_REF={RPMS_PIN}\n")
        self.write("os/kernel-snapshot.env", f"KERNEL_RPMS_REF={KERNEL_PIN}\n")
        self.write("bin/uname", """\
#!/bin/sh
[ "$#" = 1 ] && [ "$1" = -m ] || exit 72
printf '%s\\n' "$QOS_TEST_ARCH"
""", executable=True)
        self.write("bin/podman", f"""\
#!/bin/sh
printf '%s\\0' "$@" > "$QOS_TEST_PODMAN_LOG"
[ "$1" = build ] || exit 72
exit {BUILD_INTERCEPTED}
""", executable=True)
        # Do not inherit credentials, signing settings, profile overrides, or
        # shell startup hooks from the developer/CI environment.
        self.env = {
            "PATH": f"{self.bin_dir}{os.pathsep}{os.defpath}",
            "HOME": str(self.root),
            "LANG": "C.UTF-8",
            "TMPDIR": str(self.root / "scratch"),
            "QOS_TEST_REPO": str(self.root),
            "QOS_TEST_ARCH": "aarch64",
            "QOS_TEST_PODMAN_LOG": str(self.log),
            "SOURCE_DATE_EPOCH": "1700000000",
            "QOS_GENERATION": "42",
            "QOS_WITH_BROWSER": "0",
            "FEX_PAYLOAD_REF": "registry.invalid/qos/fex@sha256:" + "3" * 64,
            "WINDOWS_GUEST_REF": "registry.invalid/qos/guest@sha256:" + "4" * 64,
            # Syntactically valid test-only owner key; nothing is signed or built.
            "QOS_OFFICIAL_REGISTRY_OWNER_PUBKEY": "a" * 64,
        }

    def write(self, relative_path, content, executable=False):
        path = self.root / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")
        if executable:
            path.chmod(0o755)
        return path

    def run_builder(self, *arguments, **overrides):
        self.log.unlink(missing_ok=True)
        return subprocess.run(
            ["/bin/bash", str(self.script), *arguments],
            cwd=self.root,
            env=self.env | overrides,
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )

    def build_arguments(self, result):
        self.assertEqual(result.returncode, BUILD_INTERCEPTED, result.stderr)
        self.assertTrue(self.log.is_file(), "the real builder never reached podman")
        arguments = self.log.read_bytes().decode().split("\0")[:-1]
        self.assertEqual(arguments[0], "build")
        values = [arguments[index + 1] for index, arg in enumerate(arguments) if arg == "--build-arg"]
        self.assertEqual(len(values), len({value.partition("=")[0] for value in values}),
                         "duplicate build arguments may override the reviewed input")
        return dict(value.split("=", 1) for value in values)

    def assert_rejected_before_build(self, result, diagnostic):
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(diagnostic, result.stderr)
        self.assertFalse(self.log.exists(), "invalid profile reached podman")

    def test_default_pins_and_generic_profile_reach_the_first_build(self):
        arguments = self.build_arguments(self.run_builder())
        self.assertEqual(arguments["KERNEL_RPMS_REF"], KERNEL_PIN)
        self.assertEqual(arguments["RPMS_REF"], RPMS_PIN)
        self.assertEqual(arguments["QOS_HARDWARE_PROFILE"], "generic")
        self.assertEqual(arguments["SOURCE_DATE_EPOCH"], "1700000000")
        self.assertEqual(arguments["QOS_GENERATION"], "42")
        self.assertEqual(arguments["QOS_WITH_BROWSER"], "0")
        self.assertEqual(arguments["FEX_PAYLOAD_REF"], self.env["FEX_PAYLOAD_REF"])
        self.assertEqual(arguments["WINDOWS_GUEST_REF"], self.env["WINDOWS_GUEST_REF"])

    def test_arm_provider_requires_both_immutable_inputs_before_build(self):
        for key in ("FEX_PAYLOAD_REF", "WINDOWS_GUEST_REF"):
            for value in ("", "registry.invalid/qos/provider:mutable"):
                with self.subTest(key=key, value=value):
                    self.assert_rejected_before_build(
                        self.run_builder(**{key: value}), "Windows provider inputs are incomplete")

    def test_explicit_unpublished_refs_survive_loading_the_snapshot_pins(self):
        kernel = "localhost/qos-kernel:lenovo-83ed-fixture"
        rpms = "localhost/qos-rpms:arm64-fixture"
        arguments = self.build_arguments(self.run_builder(KERNEL_RPMS_REF=kernel, RPMS_REF=rpms))
        self.assertEqual(arguments["KERNEL_RPMS_REF"], kernel)
        self.assertEqual(arguments["RPMS_REF"], rpms)

    def test_lenovo_profile_is_forwarded_with_a_native_builder_and_manifest(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        arguments = self.build_arguments(self.run_builder(QOS_HARDWARE_PROFILE="lenovo-83ed", QOS_TARGET_IMGREF=TARGET_REF))
        self.assertEqual(arguments["QOS_HARDWARE_PROFILE"], "lenovo-83ed")
        self.assertEqual(arguments["QOS_TARGET_IMGREF"], TARGET_REF)

    def test_hardware_profile_requires_explicit_trusted_update_reference(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        for value in ("", "registry.invalid/qos:fixture", "code.podesta.ai/qos/qos:",
                      "code.podesta.ai/qos/qos@sha256:bad", TARGET_REF + "\n"):
            with self.subTest(value=value):
                result = self.run_builder(QOS_HARDWARE_PROFILE="lenovo-83ed", QOS_TARGET_IMGREF=value)
                self.assert_rejected_before_build(result, "QOS_TARGET_IMGREF")

    def test_unknown_profile_fails_before_build(self):
        result = self.run_builder(QOS_HARDWARE_PROFILE="unknown-board")
        self.assert_rejected_before_build(result, "unsupported hardware profile: unknown-board")

    def test_registered_profiles_are_not_limited_to_one_board_or_architecture(self):
        for architecture in ("x86_64", "aarch64"):
            with self.subTest(architecture=architecture):
                self.write("os/hardware/profiles/other-board.json", json.dumps({
                    "schema_version": 1, "id": "other-board", "architecture": architecture,
                    "devicetree": None, "firmware": {},
                }))
                result = self.run_builder(QOS_HARDWARE_PROFILE="other-board", QOS_TEST_ARCH=architecture,
                                          QOS_TARGET_IMGREF=TARGET_REF)
                self.assertEqual(self.build_arguments(result)["QOS_HARDWARE_PROFILE"], "other-board")
                result = self.run_builder(QOS_HARDWARE_PROFILE="other-board", QOS_TEST_ARCH=architecture, QOS_SKIP_BUILD="1")
                self.assert_rejected_before_build(result, "QOS_SKIP_BUILD cannot apply a hardware profile")

    def test_lenovo_on_x86_builder_fails_even_when_manifest_exists(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        result = self.run_builder(QOS_HARDWARE_PROFILE="lenovo-83ed", QOS_TEST_ARCH="x86_64")
        self.assert_rejected_before_build(result, "lenovo-83ed requires a native aarch64 builder")

    def test_lenovo_requires_a_nonempty_manifest_before_build(self):
        for content in (None, ""):
            with self.subTest(manifest="missing" if content is None else "empty"):
                if content is not None:
                    self.write("os/hardware/firmware/lenovo-83ed/manifest.json", content)
                result = self.run_builder(QOS_HARDWARE_PROFILE="lenovo-83ed")
                self.assert_rejected_before_build(result, "stage the lenovo-83ed firmware manifest first")

    def test_lenovo_profile_cannot_be_ignored_by_skipping_the_build(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        result = self.run_builder(QOS_HARDWARE_PROFILE="lenovo-83ed", QOS_SKIP_BUILD="1")
        self.assert_rejected_before_build(result, "QOS_SKIP_BUILD cannot apply a hardware profile")

    def test_installer_layers_on_the_exact_machine_specific_base(self):
        self.script = self.write(
            "os/dev/build-custom-installer.sh",
            (REPO / "os/dev/build-custom-installer.sh").read_text(),
        )
        self.write("os/Containerfile.installer", (REPO / "os/Containerfile.installer").read_text())
        for path in (
            "os/qos-installer", "os/dev/uki-cmdline.py",
            "os/etc/greetd/config.toml", "os/etc/greetd/config.installer.toml",
            "os/usr/share/qos/fixture",
        ):
            self.write(path, "fixture only\n")
        disk_log = self.root / "disk-step"
        probe_log = self.root / "base-probe"
        self.env.update(QOS_TEST_DISK_LOG=str(disk_log), QOS_TEST_PROBE_LOG=str(probe_log))
        self.write("os/dev/disk-image.sh", """\
#!/bin/sh
printf 'unexpected disk step\\n' > "$QOS_TEST_DISK_LOG"
exit 72
""", executable=True)
        self.write("bin/podman", f"""\
#!/bin/sh
if [ "$#" = 3 ] && [ "$1" = image ] && [ "$2" = exists ]; then
    printf '%s\\0' "$@" > "$QOS_TEST_PROBE_LOG"
    exit 0
fi
printf '%s\\0' "$@" > "$QOS_TEST_PODMAN_LOG"
[ "$1" = build ] || exit 72
exit {BUILD_INTERCEPTED}
""", executable=True)
        base = "localhost/qos:lenovo-83ed-reviewed"
        arguments = self.build_arguments(self.run_builder("--no-cargo", QOS_IMAGE=base))
        self.assertEqual(arguments["QOS_BASE_IMAGE"], base)
        self.assertEqual(probe_log.read_bytes().decode().split("\0")[:-1], ["image", "exists", base])
        self.assertFalse(disk_log.exists(), "the intercepted build continued into disk imaging")

    def test_installer_containerfile_binds_from_to_the_base_argument(self):
        instructions = [line.strip() for line in (REPO / "os/Containerfile.installer").read_text().splitlines()
                        if line.strip() and not line.lstrip().startswith("#")]
        self.assertEqual(instructions[0], "ARG QOS_BASE_IMAGE=localhost/qos:latest")
        self.assertEqual(instructions[1], "FROM ${QOS_BASE_IMAGE}")


if __name__ == "__main__":
    unittest.main()
