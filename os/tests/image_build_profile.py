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
KERNEL_PIN = "registry.invalid/tilo/kernel@sha256:" + "1" * 64
RPMS_PIN = "registry.invalid/tilo/rpms@sha256:" + "2" * 64
TARGET_REF = "code.podesta.ai/qos/tilo:board-fixture"


class ImageBuildProfileTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="tilo image build test ")
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
TILO_REPO="$TILO_TEST_REPO"
TILO_OS_DIR="$TILO_REPO/os"
TILO_OUTPUT="$TILO_OS_DIR/output"
TILO_IMAGE="${TILO_IMAGE:-localhost/tilo-fixture:latest}"
TILO_DISK="$TILO_OUTPUT/qcow2/disk.qcow2"
TILO_INSTALLER_IMAGE=localhost/tilo-installer:fixture
TILO_INSTALLER_OUTPUT="$TILO_OUTPUT/installer"
TILO_INSTALLER_DISK="$TILO_INSTALLER_OUTPUT/qcow2/disk.qcow2"
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
        self.write("os/base-snapshot.env", "BASE_REF=registry.invalid/tilo/base:fixture\n")
        self.write("os/rpms-snapshot.env", f"RPMS_REF={RPMS_PIN}\n")
        self.write("os/kernel-snapshot.env", f"KERNEL_RPMS_REF={KERNEL_PIN}\n")
        self.write("bin/uname", """\
#!/bin/sh
[ "$#" = 1 ] && [ "$1" = -m ] || exit 72
printf '%s\\n' "$TILO_TEST_ARCH"
""", executable=True)
        self.write("bin/podman", f"""\
#!/bin/sh
printf '%s\\0' "$@" > "$TILO_TEST_PODMAN_LOG"
[ "$1" != --remote=false ] || shift
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
            "TILO_TEST_REPO": str(self.root),
            "TILO_TEST_ARCH": "aarch64",
            "TILO_TEST_PODMAN_LOG": str(self.log),
            "SOURCE_DATE_EPOCH": "1700000000",
            "TILO_GENERATION": "42",
            "TILO_WITH_BROWSER": "0",
            "FEX_PAYLOAD_REF": "registry.invalid/tilo/fex@sha256:" + "3" * 64,
            "WINDOWS_GUEST_REF": "registry.invalid/tilo/guest@sha256:" + "4" * 64,
            # Syntactically valid test-only owner key; nothing is signed or built.
            "TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY": "a" * 64,
        }

    def development_inputs(self):
        # Crypto/parser behavior is exercised with real OpenSSL by the public
        # bundle suite; this stub isolates host argument and disk-stage control.
        self.write("os/dev/development-trust.py", 'import json; print(json.dumps({"identities":{"registry-owner.pub":"' + 'a' * 64 + '"}}))\n')
        public = self.root / "public bundle"
        public.mkdir()
        self.write('bin/id', '#!/bin/sh\nprintf "%s\\n" "${TILO_TEST_UID:-0}"\n', executable=True)
        self.write('.gitignore', 'podman-arguments\nscratch/\nos/output/\n__pycache__/\n')
        subprocess.run(['git', 'init', '-q', str(self.root)], env=self.env, check=True)
        self.commit_fixture()
        return {"TILO_PUBLIC_TRUST_MODE": "development",
                "TILO_PUBLIC_TRUST_BUNDLE": str(public),
                "TILO_PUBLIC_TRUST_MANIFEST_SHA256": "b" * 64,
                "TILO_IMAGE": "localhost/tilo-development:fixture",
                "TILO_PCR_PREDICTION_SIGNER": "/independent/fixture-signer",
                "TILO_INSTALLER_SCRATCH": str(self.root / 'scratch'),
                "TILO_TARGET_IMGREF": "127.0.0.1:5000/tilo-development/os:fixture"}

    def commit_fixture(self):
        subprocess.run(['git', 'add', '-A'], cwd=self.root, env=self.env, check=True)
        subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                        '-c', 'commit.gpgsign=false', 'commit', '-qm', 'Record fixture inputs'],
                       cwd=self.root, env=self.env, check=True)

    def test_development_forwards_pinned_public_context(self):
        result = self.run_builder(**self.development_inputs())
        self.assertEqual(result.returncode, BUILD_INTERCEPTED, result.stderr)
        args = self.log.read_bytes().decode().split("\0")
        self.assertIn("public-trust=" + str(self.root / "public bundle"), args)
        self.assertIn("TILO_PUBLIC_TRUST_MODE=development", args)
        self.assertIn("TILO_PUBLIC_TRUST_MANIFEST_SHA256=" + "b" * 64, args)
        revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=self.root,
                                           env=self.env, text=True).strip()
        self.assertIn('org.opencontainers.image.revision=' + revision, args)

    def development_pipeline_inputs(self):
        inputs = self.development_inputs()
        self.write("bin/podman", '''#!/bin/sh
printf '%s\\0' "$@" >> "$TILO_TEST_PODMAN_LOG"
[ "$1" != --remote=false ] || shift
[ "$1" = build ] || exit 72
while [ "$#" -gt 0 ]; do
  if [ "$1" = --iidfile ]; then shift; printf '%s\\n' "sha256:''' + '1' * 64 + '''" > "$1"; fi
  shift
done
''', executable=True)
        self.write('os/dev/sign-development-rootfs.py', '''import json,os,sys
assert sys.argv[sys.argv.index('--rootfs-id') + 1] == 'sha256:''' + '1' * 64 + ''''
assert sys.argv[sys.argv.index('--source-date-epoch') + 1] == '1700000000'
if os.environ.get('TILO_TEST_SIGNER_FAIL') == 'exit': raise SystemExit(81)
identity = 'mutable:tag' if os.environ.get('TILO_TEST_SIGNER_FAIL') == 'identity' else 'sha256:''' + '2' * 64 + ''''
print(json.dumps({'signed_rootfs_id': identity}))
''')
        self.commit_fixture()
        (self.root / 'scratch').mkdir()
        return inputs

    def test_development_stops_before_disk_imaging(self):
        inputs = self.development_pipeline_inputs()
        result = self.run_builder(**inputs)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Development container staged", result.stderr)
        self.assertNotIn("2/2 disk image", result.stderr)
        arguments = self.log.read_bytes().decode().split('\0')
        self.assertIn('rootfs', arguments)
        self.assertIn('final', arguments)
        self.assertIn('rootfs=container-image://sha256:' + '2' * 64, arguments)
        self.assertIn('--pull=never', arguments)
        self.assertEqual(arguments.count('-t'), 1)

    def test_development_wpe_cache_overrides_only_immutable_dependency_stage(self):
        inputs = self.development_pipeline_inputs()
        self.write('os/dev/prepare-wpe-stage.py', '''import sys
assert sys.argv[sys.argv.index('--epoch') + 1] == '0'
assert '--public-bundle' not in sys.argv
print('sha256:''' + '3' * 64 + '''')
''')
        self.commit_fixture()
        inputs.update(TILO_WITH_BROWSER='1', WPE_BUILDROOT_REF='code.podesta.ai/qos/tilo-wpe-buildroot@sha256:' + '4' * 64,
            TILO_WPE_BUILD_CACHE_REF='code.podesta.ai/qos/tilo-development-wpe-cache-aarch64',
            TILO_WPE_SOURCE_DATE_EPOCH='0')
        result = self.run_builder(**inputs)
        self.assertEqual(result.returncode, 0, result.stderr)
        arguments = self.log.read_bytes().decode().split('\0')
        self.assertEqual(arguments.count('wpe-builder=container-image://sha256:' + '3' * 64), 2)
        self.assertNotIn('--cache-to', arguments)
        self.assertEqual(arguments.count('1700000000'), 2)

    def test_wpe_cache_is_rejected_for_production_or_browserless_build(self):
        result = self.run_builder(TILO_WPE_BUILD_CACHE_REF='code.podesta.ai/qos/tilo-development-wpe-cache-aarch64')
        self.assert_rejected_before_build(result, 'WPE stage caching requires a fresh full-browser development build')

    def test_development_signer_failure_never_enters_sealing(self):
        inputs = self.development_pipeline_inputs()
        for failure in ('exit', 'identity'):
            with self.subTest(failure=failure):
                result = self.run_builder(**(inputs | {'TILO_TEST_SIGNER_FAIL': failure}))
                self.assertNotEqual(result.returncode, 0)
                arguments = self.log.read_bytes().decode().split('\0')
                self.assertEqual(arguments.count('build'), 1)
                self.assertNotIn('final', arguments)
                self.assertNotIn('-t', arguments)
                self.assertNotIn('Development container staged', result.stderr)

    def test_development_requires_signer_before_build(self):
        result = self.run_builder(**(self.development_inputs() | {'TILO_PCR_PREDICTION_SIGNER': ''}))
        self.assertIn('independent EFI/PCR signer', result.stderr)
        self.assertFalse(self.log.exists())

    def test_development_rejects_rootless_or_remote_storage_before_build(self):
        inputs = self.development_inputs()
        for overrides in ({'TILO_TEST_UID': '1000'}, {'CONTAINER_HOST': 'ssh://remote'}):
            result = self.run_builder(**(inputs | overrides))
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.log.exists())

    def test_development_rejects_uncommitted_source_before_build(self):
        inputs = self.development_inputs()
        for path in ('os/dev/_common.sh', 'untracked-source.py'):
            self.write(path, '# uncommitted source\n' + (self.root / path).read_text()
                       if (self.root / path).exists() else '# new source\n')
            result = self.run_builder(**inputs)
            self.assertIn('commit development source changes', result.stderr)
            self.assertFalse(self.log.exists())
            self.commit_fixture()

    def test_development_rejects_skip_build_and_production_alias(self):
        inputs = self.development_inputs()
        for override in ({"TILO_SKIP_BUILD": "1"}, {"TILO_IMAGE": "code.podesta.ai/qos/tilo:latest"}):
            result = self.run_builder(**(inputs | override))
            self.assertNotEqual(result.returncode, BUILD_INTERCEPTED)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.log.exists())

    def test_development_rejects_conflicting_owner(self):
        result = self.run_builder(**(self.development_inputs() | {"TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY": "c" * 64}))
        self.assertIn("registry owner differs", result.stderr)
        self.assertFalse(self.log.exists())

    def test_production_rejects_development_bundle_inputs(self):
        result = self.run_builder(TILO_PUBLIC_TRUST_BUNDLE="unexpected")
        self.assertIn("require TILO_PUBLIC_TRUST_MODE", result.stderr)
        self.assertFalse(self.log.exists())

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
        self.assertEqual(arguments["TILO_HARDWARE_PROFILE"], "generic")
        self.assertEqual(arguments["SOURCE_DATE_EPOCH"], "1700000000")
        self.assertEqual(arguments["TILO_GENERATION"], "42")
        self.assertEqual(arguments["TILO_WITH_BROWSER"], "0")
        self.assertEqual(arguments["FEX_PAYLOAD_REF"], self.env["FEX_PAYLOAD_REF"])
        self.assertEqual(arguments["WINDOWS_GUEST_REF"], self.env["WINDOWS_GUEST_REF"])

    def test_arm_provider_requires_both_immutable_inputs_before_build(self):
        for key in ("FEX_PAYLOAD_REF", "WINDOWS_GUEST_REF"):
            for value in ("", "registry.invalid/tilo/provider:mutable"):
                with self.subTest(key=key, value=value):
                    self.assert_rejected_before_build(
                        self.run_builder(**{key: value}), "Windows provider inputs are incomplete")

    def test_explicit_unpublished_refs_survive_loading_the_snapshot_pins(self):
        kernel = "localhost/tilo-kernel:lenovo-83ed-fixture"
        rpms = "localhost/tilo-rpms:arm64-fixture"
        arguments = self.build_arguments(self.run_builder(KERNEL_RPMS_REF=kernel, RPMS_REF=rpms))
        self.assertEqual(arguments["KERNEL_RPMS_REF"], kernel)
        self.assertEqual(arguments["RPMS_REF"], rpms)

    def test_lenovo_profile_is_forwarded_with_a_native_builder_and_manifest(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        arguments = self.build_arguments(self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_TARGET_IMGREF=TARGET_REF))
        self.assertEqual(arguments["TILO_HARDWARE_PROFILE"], "lenovo-83ed")
        self.assertEqual(arguments["TILO_TARGET_IMGREF"], TARGET_REF)

    def test_hardware_profile_requires_explicit_trusted_update_reference(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        for value in ("", "registry.invalid/tilo:fixture", "code.podesta.ai/qos/tilo:",
                      "code.podesta.ai/qos/tilo@sha256:bad", TARGET_REF + "\n"):
            with self.subTest(value=value):
                result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_TARGET_IMGREF=value)
                self.assert_rejected_before_build(result, "TILO_TARGET_IMGREF")

    def test_unknown_profile_fails_before_build(self):
        result = self.run_builder(TILO_HARDWARE_PROFILE="unknown-board")
        self.assert_rejected_before_build(result, "unsupported hardware profile: unknown-board")

    def test_registered_profiles_are_not_limited_to_one_board_or_architecture(self):
        for architecture in ("x86_64", "aarch64"):
            with self.subTest(architecture=architecture):
                self.write("os/hardware/profiles/other-board.json", json.dumps({
                    "schema_version": 1, "id": "other-board", "architecture": architecture,
                    "devicetree": None, "firmware": {},
                }))
                result = self.run_builder(TILO_HARDWARE_PROFILE="other-board", TILO_TEST_ARCH=architecture,
                                          TILO_TARGET_IMGREF=TARGET_REF)
                self.assertEqual(self.build_arguments(result)["TILO_HARDWARE_PROFILE"], "other-board")
                result = self.run_builder(TILO_HARDWARE_PROFILE="other-board", TILO_TEST_ARCH=architecture, TILO_SKIP_BUILD="1")
                self.assert_rejected_before_build(result, "TILO_SKIP_BUILD cannot apply a hardware profile")

    def test_lenovo_on_x86_builder_fails_even_when_manifest_exists(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_TEST_ARCH="x86_64")
        self.assert_rejected_before_build(result, "lenovo-83ed requires a native aarch64 builder")

    def test_lenovo_requires_a_nonempty_manifest_before_build(self):
        for content in (None, ""):
            with self.subTest(manifest="missing" if content is None else "empty"):
                if content is not None:
                    self.write("os/hardware/firmware/lenovo-83ed/manifest.json", content)
                result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed")
                self.assert_rejected_before_build(result, "stage the lenovo-83ed firmware manifest first")

    def test_lenovo_profile_cannot_be_ignored_by_skipping_the_build(self):
        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
        result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_SKIP_BUILD="1")
        self.assert_rejected_before_build(result, "TILO_SKIP_BUILD cannot apply a hardware profile")

    def test_installer_layers_on_the_exact_machine_specific_base(self):
        self.script = self.write(
            "os/dev/build-custom-installer.sh",
            (REPO / "os/dev/build-custom-installer.sh").read_text(),
        )
        self.write("os/Containerfile.installer", (REPO / "os/Containerfile.installer").read_text())
        for path in (
            "os/tilo-installer", "os/dev/uki-cmdline.py",
            "os/etc/greetd/config.toml", "os/etc/greetd/config.installer.toml",
            "os/usr/share/tilo/fixture",
        ):
            self.write(path, "fixture only\n")
        disk_log = self.root / "disk-step"
        probe_log = self.root / "base-probe"
        self.env.update(TILO_TEST_DISK_LOG=str(disk_log), TILO_TEST_PROBE_LOG=str(probe_log))
        self.write("os/dev/disk-image.sh", """\
#!/bin/sh
printf 'unexpected disk step\\n' > "$TILO_TEST_DISK_LOG"
exit 72
""", executable=True)
        self.write("bin/podman", f"""\
#!/bin/sh
if [ "$#" = 3 ] && [ "$1" = image ] && [ "$2" = exists ]; then
    printf '%s\\0' "$@" > "$TILO_TEST_PROBE_LOG"
    exit 0
fi
printf '%s\\0' "$@" > "$TILO_TEST_PODMAN_LOG"
[ "$1" = build ] || exit 72
exit {BUILD_INTERCEPTED}
""", executable=True)
        base = "localhost/tilo:lenovo-83ed-reviewed"
        arguments = self.build_arguments(self.run_builder("--no-cargo", TILO_IMAGE=base))
        self.assertEqual(arguments["TILO_BASE_IMAGE"], base)
        self.assertEqual(probe_log.read_bytes().decode().split("\0")[:-1], ["image", "exists", base])
        self.assertFalse(disk_log.exists(), "the intercepted build continued into disk imaging")

    def test_installer_containerfile_binds_from_to_the_base_argument(self):
        instructions = [line.strip() for line in (REPO / "os/Containerfile.installer").read_text().splitlines()
                        if line.strip() and not line.lstrip().startswith("#")]
        self.assertEqual(instructions[0], "ARG TILO_BASE_IMAGE=localhost/tilo:latest")
        self.assertEqual(instructions[1], "FROM ${TILO_BASE_IMAGE}")


if __name__ == "__main__":
    unittest.main()
