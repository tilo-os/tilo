#!/usr/bin/env python3
"""Exercise the image builder's profile gates and build arguments without a build."""

import os
import json
import platform
import subprocess
import tempfile
import unittest
from pathlib import Path
from restart_fixture import restart_fixture


REPO = Path(__file__).resolve().parents[2]
BUILD_INTERCEPTED = 73
KERNEL_PIN = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "1" * 64
RPMS_PIN = "registry.tilo.so/tilo/rpms@sha256:" + "2" * 64
TARGET_REF = "code.tilo.so/tilo/tilo:board-fixture"


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
        self.write("os/dev/raw-rootfs-receipt.py", (REPO / "os/dev/raw-rootfs-receipt.py").read_text())
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
warn() { printf 'WARN: %s\\n' "$*" >&2; }
warn_slow_podman_storage() { :; }
cargo_build_jobs() { echo 4; }
TILO_PODMAN_BUILD_FLAGS=()
""")
        # These external checks are unrelated to forwarding inputs. A failing
        # podman stub ensures execution never reaches any disk-image operation.
        for path in (
            "os/dev/cache-github-assets.sh",
            "os/dev/derive-rpm-package-set.py",
            "os/dev/verify-rpm-snapshot-package-set.sh",
            "os/dev/verify-kernel-provenance.sh",
            "os/tests/containerfile_sources.sh",
            "os/tests/app_store.sh",
        ):
            self.write(path, "#!/bin/bash\nexit 0\n", executable=True)
        self.write("os/base-snapshot.env", "BASE_REF=registry.invalid/tilo/base:fixture\n")
        self.write(
            "os/dev/resolve-rpm-snapshot.sh",
            f"#!/bin/bash\nprintf '%s\\n' '{RPMS_PIN}'\n",
            executable=True,
        )
        self.write(
            "os/kernel-snapshot.env",
            f"KERNEL_RPMS_X86_64_REF={KERNEL_PIN}\n"
            f"KERNEL_RPMS_AARCH64_REF={KERNEL_PIN}\n",
        )
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
        self.write("bin/cosign", "#!/bin/sh\nexit 0\n", executable=True)
        self.write("bin/skopeo", "#!/bin/sh\nprintf '%s\\n' '{}'\n", executable=True)
        self.write("bin/jq", """\
#!/bin/sh
# Drain the producer before returning fixture output. The production script
# enables pipefail, so exiting early can otherwise SIGPIPE the tiny skopeo or
# printf producer and make an otherwise valid inspection fail nondeterministically.
cat >/dev/null
case "$*" in
  *sort_by*) printf '%s\\n' '[{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"arm64"}]' ;;
  *Architecture*)
    case "$TILO_TEST_ARCH" in
      x86_64) printf '%s\\n' amd64 ;;
      aarch64) printf '%s\\n' arm64 ;;
      *) exit 1 ;;
    esac
    ;;
esac
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
        # Crypto/parser behavior is exercised by the Rust-native public-bundle
        # suite; this stub isolates host argument and disk-stage control.
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

    def test_development_preconditions_before_build(self):
        with self.subTest(phase="development_forwards_pinned_public_context"):
            result = self.run_builder(**self.development_inputs())
            self.assertEqual(result.returncode, BUILD_INTERCEPTED, result.stderr)
            args = self.log.read_bytes().decode().split("\0")
            self.assertIn("public-trust=" + str(self.root / "public bundle"), args)
            self.assertIn("TILO_PUBLIC_TRUST_MODE=development", args)
            self.assertIn("TILO_PUBLIC_TRUST_MANIFEST_SHA256=" + "b" * 64, args)
            revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=self.root,
                                               env=self.env, text=True).strip()
            self.assertIn('org.opencontainers.image.revision=' + revision, args)
        restart_fixture(self)
        with self.subTest(phase="development_stops_before_disk_imaging"):
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
        restart_fixture(self)
        with self.subTest(phase="development_signer_failure_never_enters_sealing"):
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
        restart_fixture(self)
        with self.subTest(phase="development_requires_signer_before_build"):
            result = self.run_builder(**(self.development_inputs() | {'TILO_PCR_PREDICTION_SIGNER': ''}))
            self.assertIn('independent EFI/PCR signer', result.stderr)
            self.assertFalse(self.log.exists())
        restart_fixture(self)
        with self.subTest(phase="development_rejects_rootless_or_remote_storage_before_build"):
            inputs = self.development_inputs()
            for overrides in ({'TILO_TEST_UID': '1000'}, {'CONTAINER_HOST': 'ssh://remote'}):
                result = self.run_builder(**(inputs | overrides))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.log.exists())
        restart_fixture(self)
        with self.subTest(phase="development_rejects_uncommitted_source_before_build"):
            inputs = self.development_inputs()
            for path in ('os/dev/_common.sh', 'untracked-source.py'):
                self.write(path, '# uncommitted source\n' + (self.root / path).read_text()
                           if (self.root / path).exists() else '# new source\n')
                result = self.run_builder(**inputs)
                self.assertIn('commit development source changes', result.stderr)
                self.assertFalse(self.log.exists())
                self.commit_fixture()
        restart_fixture(self)
        with self.subTest(phase="development_rejects_skip_build_and_production_alias"):
            inputs = self.development_inputs()
            for override in ({"TILO_SKIP_BUILD": "1"}, {"TILO_IMAGE": "registry.tilo.so/tilo/tilo:latest"}):
                result = self.run_builder(**(inputs | override))
                self.assertNotEqual(result.returncode, BUILD_INTERCEPTED)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.log.exists())
        restart_fixture(self)
        with self.subTest(phase="development_rejects_conflicting_owner"):
            result = self.run_builder(**(self.development_inputs() | {"TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY": "c" * 64}))
            self.assertIn("registry owner differs", result.stderr)
            self.assertFalse(self.log.exists())
        restart_fixture(self)
        with self.subTest(phase="production_rejects_development_bundle_inputs"):
            result = self.run_builder(TILO_PUBLIC_TRUST_BUNDLE="unexpected")
            self.assertIn("require TILO_PUBLIC_TRUST_MODE", result.stderr)
            self.assertFalse(self.log.exists())

    def test_disk_only_skips_build_asset_downloads(self):
        marker = self.root / "asset-cache-invoked"
        self.write("os/dev/cache-github-assets.sh", f"#!/bin/sh\ntouch '{marker}'\n", executable=True)
        result = self.run_builder(TILO_SKIP_BUILD="1")
        self.assertFalse(marker.exists(), result.stderr)
        self.assertIn("TILO_SKIP_BUILD=1", result.stderr)

        result = self.run_builder()
        self.assertEqual(result.returncode, BUILD_INTERCEPTED, result.stderr)
        self.assertTrue(marker.exists(), "a fresh build still needs its pinned assets")

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

    def test_raw_rootfs_handoff(self):
        with self.subTest(phase="raw_rootfs_handoff_is_explicit_exclusive_and_precedes_signing"):
            inputs = self.development_pipeline_inputs()
            with tempfile.TemporaryDirectory() as external:
                output = Path(external) / 'raw-rootfs.json'
                inputs.update(TILO_RAW_ROOTFS_RECEIPT=str(output), TILO_TEST_ARCH=platform.machine(),
                              TILO_TEST_SIGNER_FAIL='exit')
                result = self.run_builder(**inputs)
                self.assertEqual(result.returncode, 81, result.stderr)
                receipt = json.loads(output.read_text())
                self.assertEqual(receipt['raw_rootfs_id'], 'sha256:' + '1' * 64)
                self.assertEqual(receipt['source_date_epoch'], 1700000000)
                self.assertEqual(receipt['generation'], 42)
                self.assertEqual(receipt['target_update_ref'], inputs['TILO_TARGET_IMGREF'])
                self.assertEqual(receipt['public_manifest_sha256'], 'b' * 64)
                self.assertFalse(receipt['installable'])
                original = output.read_bytes()
                result = self.run_builder(**inputs)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output.read_bytes(), original)
        restart_fixture(self)
        with self.subTest(phase="raw_handoff_is_rejected_in_production"):
            result = self.run_builder(TILO_RAW_ROOTFS_RECEIPT='/evidence/raw-rootfs.json')
            self.assert_rejected_before_build(result, 'raw rootfs handoff is development-only')
        restart_fixture(self)
        with self.subTest(phase="raw_only_stops_after_handoff_without_invoking_signer_or_final_build"):
            inputs = self.development_pipeline_inputs()
            with tempfile.TemporaryDirectory() as external:
                output = Path(external) / 'raw.json'
                inputs.update(TILO_RAW_ROOTFS_ONLY='1', TILO_RAW_ROOTFS_RECEIPT=str(output),
                              TILO_TEST_ARCH=platform.machine(), TILO_TEST_SIGNER_FAIL='exit')
                result = self.run_builder(**inputs)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(output.read_text())['raw_rootfs_id'], 'sha256:' + '1' * 64)
                args = self.log.read_bytes().decode().split('\0')
                self.assertEqual(args.count('build'), 1)
                self.assertIn('rootfs', args)
                self.assertNotIn('final', args)
                self.assertNotIn('-t', args)
                self.assertIn('Unsealed and not installable', result.stderr)
                self.assertNotIn('Development container staged', result.stderr)
                self.assertNotIn('2/2 disk image', result.stderr)
        restart_fixture(self)
        with self.subTest(phase="raw_only_rejects_production_missing_receipt_skip_and_malformed_flags"):
            result = self.run_builder(TILO_RAW_ROOTFS_ONLY='1')
            self.assert_rejected_before_build(result, 'raw-only builds are development-only')
            inputs = self.development_inputs()
            for overrides, message in (
                ({'TILO_RAW_ROOTFS_ONLY': '1'}, 'require TILO_RAW_ROOTFS_RECEIPT'),
                ({'TILO_RAW_ROOTFS_ONLY': '1', 'TILO_SKIP_BUILD': '1'}, 'require a fresh source build'),
                ({'TILO_RAW_ROOTFS_ONLY': 'yes'}, 'must be 0 or 1'),
                ({'TILO_RAW_ROOTFS_ONLY': '01'}, 'must be 0 or 1'),
            ):
                with self.subTest(overrides=overrides):
                    self.assert_rejected_before_build(self.run_builder(**(inputs | overrides)), message)

    def write(self, relative_path, content, executable=False):
        path = self.root / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")
        if executable:
            path.chmod(0o755)
        return path

    def stage_alongside_bootc_inputs(self):
        for path in (
            "os/Containerfile.bootc-alongside-helper",
            "os/external-assets/bootc/0001-probe-missing-partition-metadata.patch",
            "os/external-assets/bootc-install-reseal/0002-select-mounted-esp.patch",
        ):
            self.write(path, (REPO / path).read_text())

    def run_builder(self, *arguments, **overrides):
        self.log.unlink(missing_ok=True)
        return subprocess.run(
            ["/bin/bash", str(self.script), *arguments],
            cwd=self.root,
            env=self.env | overrides,
            text=True,
            capture_output=True,
            # The builder runs against stubs in well under a second; the bound
            # only catches a hang, so a loaded runner must not trip it.
            timeout=120,
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

    def test_windows_provider_pins(self):
        with self.subTest(phase="default_pins_and_generic_profile_reach_the_first_build"):
            arguments = self.build_arguments(self.run_builder())
            self.assertEqual(arguments["KERNEL_RPMS_REF"], KERNEL_PIN)
            self.assertEqual(arguments["RPMS_REF"], RPMS_PIN)
            self.assertEqual(arguments["TILO_HARDWARE_PROFILE"], "generic")
            self.assertEqual(arguments["SOURCE_DATE_EPOCH"], "1700000000")
            self.assertEqual(arguments["TILO_GENERATION"], "42")
            self.assertEqual(arguments["TILO_WITH_BROWSER"], "0")
            self.assertEqual(arguments["FEX_PAYLOAD_REF"], self.env["FEX_PAYLOAD_REF"])
            self.assertEqual(arguments["WINDOWS_GUEST_REF"], self.env["WINDOWS_GUEST_REF"])
            self.assertEqual(arguments["TILO_WITH_WINDOWS"], "1")
        restart_fixture(self)
        with self.subTest(phase="arm_without_either_pin_builds_without_windows"):
            result = self.run_builder(FEX_PAYLOAD_REF="", WINDOWS_GUEST_REF="", TILO_WITH_WINDOWS="1")
            arguments = self.build_arguments(result)
            self.assertEqual(arguments["TILO_WITH_WINDOWS"], "0")
            self.assertEqual(arguments["FEX_PAYLOAD_REF"], "scratch")
            self.assertEqual(arguments["WINDOWS_GUEST_REF"], "scratch")
            self.assertIn("built without Windows apps", result.stderr)
        restart_fixture(self)
        with self.subTest(phase="x86_always_builds_with_windows"):
            arguments = self.build_arguments(self.run_builder(
                TILO_TEST_ARCH="x86_64", FEX_PAYLOAD_REF="", WINDOWS_GUEST_REF="", TILO_WITH_WINDOWS="0"))
            self.assertEqual(arguments["TILO_WITH_WINDOWS"], "1")
        restart_fixture(self)
        with self.subTest(phase="arm_provider_requires_both_immutable_inputs_before_build"):
            for key in ("FEX_PAYLOAD_REF", "WINDOWS_GUEST_REF"):
                for value in ("", "registry.invalid/tilo/provider:mutable"):
                    with self.subTest(key=key, value=value):
                        self.assert_rejected_before_build(
                            self.run_builder(**{key: value}), "Windows provider inputs are incomplete")
        restart_fixture(self)
        with self.subTest(phase="explicit_unpublished_refs_survive_loading_the_snapshot_pins"):
            kernel = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "5" * 64
            rpms = "registry.tilo.so/tilo/rpms@sha256:" + "6" * 64
            arguments = self.build_arguments(self.run_builder(KERNEL_RPMS_REF=kernel, RPMS_REF=rpms))
            self.assertEqual(arguments["KERNEL_RPMS_REF"], kernel)
            self.assertEqual(arguments["RPMS_REF"], rpms)

    def test_hardware_profile_forwarding(self):
        with self.subTest(phase="lenovo_profile_is_forwarded_with_a_native_builder_and_manifest"):
            self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
            arguments = self.build_arguments(self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_TARGET_IMGREF=TARGET_REF))
            self.assertEqual(arguments["TILO_HARDWARE_PROFILE"], "lenovo-83ed")
            self.assertEqual(arguments["TILO_TARGET_IMGREF"], TARGET_REF)
        restart_fixture(self)
        with self.subTest(phase="hardware_profile_requires_explicit_trusted_update_reference"):
            self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
            for value in ("", "registry.invalid/tilo:fixture", "registry.tilo.so/tilo/tilo:",
                          "registry.tilo.so/tilo/tilo@sha256:bad", TARGET_REF + "\n"):
                with self.subTest(value=value):
                    result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_TARGET_IMGREF=value)
                    self.assert_rejected_before_build(result, "TILO_TARGET_IMGREF")
        restart_fixture(self)
        with self.subTest(phase="unknown_profile_fails_before_build"):
            result = self.run_builder(TILO_HARDWARE_PROFILE="unknown-board")
            self.assert_rejected_before_build(result, "unsupported hardware profile: unknown-board")
        restart_fixture(self)
        with self.subTest(phase="registered_profiles_are_not_limited_to_one_board_or_architecture"):
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
        restart_fixture(self)
        with self.subTest(phase="lenovo_on_x86_builder_fails_even_when_manifest_exists"):
            self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
            result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_TEST_ARCH="x86_64")
            self.assert_rejected_before_build(result, "lenovo-83ed requires a native aarch64 builder")
        restart_fixture(self)
        with self.subTest(phase="lenovo_requires_a_nonempty_manifest_before_build"):
            for content in (None, ""):
                with self.subTest(manifest="missing" if content is None else "empty"):
                    if content is not None:
                        self.write("os/hardware/firmware/lenovo-83ed/manifest.json", content)
                    result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed")
                    self.assert_rejected_before_build(result, "stage the lenovo-83ed firmware manifest first")
        restart_fixture(self)
        with self.subTest(phase="lenovo_profile_cannot_be_ignored_by_skipping_the_build"):
            self.write("os/hardware/firmware/lenovo-83ed/manifest.json", "{}\n")
            result = self.run_builder(TILO_HARDWARE_PROFILE="lenovo-83ed", TILO_SKIP_BUILD="1")
            self.assert_rejected_before_build(result, "TILO_SKIP_BUILD cannot apply a hardware profile")

    def test_installer_layers_on_machine_base(self):
        with self.subTest(phase="installer_layers_on_the_exact_machine_specific_base"):
            self.script = self.write(
                "os/dev/build-custom-installer.sh",
                (REPO / "os/dev/build-custom-installer.sh").read_text(),
            )
            self.stage_alongside_bootc_inputs()
            self.write("os/Containerfile.installer", (REPO / "os/Containerfile.installer").read_text())
            for path in (
                "os/tilo-installer", "os/etc/greetd/config.toml", "os/etc/greetd/config.installer.toml",
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
    if [ "$3" = "$TILO_IMAGE" ]; then
        printf '%s\\0' "$@" > "$TILO_TEST_PROBE_LOG"
    fi
    exit 0
fi
if [ "$1" = create ]; then printf '%s\\n' helper-fixture; exit 0; fi
if [ "$1" = cp ]; then printf '%s\\n' 'fixture helper' > "$3"; exit 0; fi
if [ "$1" = rm ]; then exit 0; fi
printf '%s\\0' "$@" > "$TILO_TEST_PODMAN_LOG"
[ "$1" = build ] || exit 72
exit {BUILD_INTERCEPTED}
""", executable=True)
            base = "localhost/tilo:lenovo-83ed-reviewed"
            arguments = self.build_arguments(self.run_builder("--no-cargo", TILO_IMAGE=base))
            self.assertEqual(arguments["TILO_BASE_IMAGE"], base)
            self.assertEqual(probe_log.read_bytes().decode().split("\0")[:-1], ["image", "exists", base])
            self.assertFalse(disk_log.exists(), "the intercepted build continued into disk imaging")
        restart_fixture(self)
        with self.subTest(phase="installer_reuses_base_binary_and_stops_before_disk_or_signing"):
            self.script = self.write("os/dev/build-custom-installer.sh",
                                     (REPO / "os/dev/build-custom-installer.sh").read_text())
            self.stage_alongside_bootc_inputs()
            for path in ("os/etc/greetd/config.toml", "os/etc/greetd/config.installer.toml"):
                self.write(path, "fixture only\n")
            # Both stale binary sources must be ignored in image reuse mode.
            self.write("os/tilo-installer", "stale host binary\n")
            self.write("os/usr/bin/tilo-installer", "stale overlay binary\n")
            self.write("os/usr/share/tilo/preserved", "overlay fixture\n")
            self.write("os/dev/disk-image.sh", "#!/bin/sh\nexit 97\n", executable=True)
            self.write("bin/podman", """\
#!/bin/bash
if [ "$1" = image ] && [ "$2" = exists ]; then exit 0; fi
if [ "$1" = create ]; then printf '%s\\n' helper-fixture; exit 0; fi
if [ "$1" = cp ]; then printf '%s\\n' 'fixture helper' > "$3"; exit 0; fi
if [ "$1" = rm ]; then exit 0; fi
[ "$1" = build ] || exit 72
printf '%s\\0' "$@" > "$TILO_TEST_PODMAN_LOG"
context="${@: -1}"
[ ! -e "$context/rootfs/usr/bin/tilo-installer" ] || exit 74
[ -f "$context/rootfs/usr/share/tilo/preserved" ] || exit 75
exit 0
""", executable=True)
            base = "registry.invalid/tilo/lenovo@sha256:" + "a" * 64
            result = self.run_builder("--reuse-image-installer", "--image-only", TILO_IMAGE=base)
            self.assertEqual(result.returncode, 0, result.stderr)
            arguments = self.log.read_bytes().decode().split("\0")[:-1]
            self.assertIn("TILO_BASE_IMAGE=" + base, arguments)
            self.assertIn("require sealing and signing", result.stderr)
            self.assertFalse((self.os_dir / "output/installer/qcow2").exists())
            for alternative in ("--stub-ui", "--no-cargo"):
                result = self.run_builder("--reuse-image-installer", alternative, "--image-only")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("choose only one installer binary source", result.stderr)
        restart_fixture(self)
        with self.subTest(phase="compact_installer_root_is_opt_in_and_capacity_bounded"):
            self.script = self.write("os/dev/build-custom-installer.sh",
                                     (REPO / "os/dev/build-custom-installer.sh").read_text())
            self.stage_alongside_bootc_inputs()
            for path in ("os/usr/share/tilo/fixture", "os/etc/greetd/config.toml",
                         "os/etc/greetd/config.installer.toml"):
                self.write(path, "fixture only\n")
            disk_args = self.root / "disk-arguments"
            self.write("os/dev/disk-image.sh", """\
#!/bin/sh
printf '%s\\0' "$@" > "$TILO_TEST_DISK_ARGS"
exit 97
""", executable=True)
            self.write("bin/podman", """\
#!/bin/sh
case "$1 $2" in
  'image exists') exit 0 ;;
  'image inspect') printf '%s\\n' "$TILO_TEST_IMAGE_BYTES" ;;
  'build '*) exit 0 ;;
  'create '*) printf '%s\\n' helper-fixture ;;
  'cp '*) printf '%s\\n' 'fixture helper' > "$3" ;;
  'rm '*) exit 0 ;;
  *) exit 72 ;;
esac
""", executable=True)
            output = self.os_dir / "output/installer"
            sentinel = output / "qcow2/existing.qcow2"
            gib = 1024**3
            mib = 1024**2
            for compact, image_bytes, expected_fs, expected_root in (
                (False, 10 * gib, "btrfs", "40G"),
                (False, 17 * gib + 800 * mib, "btrfs", "54G"),
                (True, 10 * gib, "ext4", "40G"),
                (True, 17 * gib + 800 * mib, "ext4", "40G"),
            ):
                with self.subTest(compact=compact, image_bytes=image_bytes):
                    sentinel.parent.mkdir(parents=True, exist_ok=True)
                    sentinel.write_text("existing image")
                    disk_args.unlink(missing_ok=True)
                    args = ["--reuse-image-installer"] + (["--compact-ext4"] if compact else [])
                    result = self.run_builder(*args, TILO_TEST_IMAGE_BYTES=str(image_bytes),
                                              TILO_TEST_DISK_ARGS=str(disk_args))
                    self.assertNotEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(disk_args.exists(), result.stderr)
                    arguments = disk_args.read_bytes().decode().split("\0")[:-1]
                    self.assertEqual(arguments[arguments.index("--root-min") + 1], expected_root)
                    self.assertEqual(arguments[arguments.index("--rootfs") + 1], expected_fs)
                    if compact:
                        self.assertEqual(arguments[arguments.index("--root-max") + 1], "40G")
                    else:
                        self.assertNotIn("--root-max", arguments)
                    self.assertFalse(sentinel.exists())
            sentinel.parent.mkdir(parents=True, exist_ok=True)
            sentinel.write_text("preserve this image")
            disk_args.unlink()
            result = self.run_builder("--reuse-image-installer", "--compact-ext4",
                                      TILO_TEST_IMAGE_BYTES=str(19 * gib),
                                      TILO_TEST_DISK_ARGS=str(disk_args))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("2x OCI image bytes plus 4 GiB slack", result.stderr)
            self.assertEqual(sentinel.read_text(), "preserve this image")
            self.assertFalse(disk_args.exists())
            result = self.run_builder("--reuse-image-installer", "--compact-ext4", "--image-only")
            self.assertIn("requires disk creation", result.stderr)
        restart_fixture(self)
        with self.subTest(phase="disk_image_rootfs_limit_reaches_repart_definition"):
            self.script = self.write("os/dev/disk-image.sh", (REPO / "os/dev/disk-image.sh").read_text())
            root_definition = self.root / "root-definition"
            self.write("bin/podman", """\
#!/bin/sh
case "$1 $2" in
  'image exists') exit 0 ;;
  'run --rm') exit 1 ;;
  *) exit 72 ;;
esac
""", executable=True)
            self.write("bin/sudo", """\
#!/bin/sh
[ "$1" = -n ] && shift
case "$1 $2" in
  'podman image') exit 0 ;;
  'systemd-repart '*)
    for argument in "$@"; do
      case "$argument" in
        --definitions=*) cp "${argument#--definitions=}/02-root.conf" "$TILO_TEST_ROOT_DEF" ;;
      esac
    done
    exit 97 ;;
  *) exit 0 ;;
esac
""", executable=True)
            # The repart stub exits before an image is made; truncate is also
            # stubbed so this test never reserves even a sparse multi-GiB file.
            self.write("bin/truncate", "#!/bin/sh\nexit 0\n", executable=True)
            result = self.run_builder("fixture", str(self.root / "disk"), "--no-composefs",
                                      "--no-default-kargs", "--root-min", "2G",
                                      "--root-max", "3G", "--rootfs", "ext4",
                                      TILO_TEST_ROOT_DEF=str(root_definition))
            self.assertNotEqual(result.returncode, 0)
            definition = root_definition.read_text()
            self.assertIn("Format=ext4\n", definition)
            self.assertIn("SizeMinBytes=2G\n", definition)
            self.assertIn("SizeMaxBytes=3G\n", definition)
            root_definition.unlink()
            result = self.run_builder("fixture", str(self.root / "disk"), "--no-composefs",
                                      "--root-min", "3G", "--root-max", "2G",
                                      TILO_TEST_ROOT_DEF=str(root_definition))
            self.assertIn("smaller than --root-min", result.stderr)
            self.assertFalse(root_definition.exists())
        restart_fixture(self)
        with self.subTest(phase="installer_containerfile_binds_from_to_the_base_argument"):
            instructions = [line.strip() for line in (REPO / "os/Containerfile.installer").read_text().splitlines()
                            if line.strip() and not line.lstrip().startswith("#")]
            self.assertEqual(instructions[0], "ARG TILO_BASE_IMAGE=localhost/tilo:latest")
            self.assertEqual(instructions[1], "FROM ${TILO_BASE_IMAGE}")


if __name__ == "__main__":
    unittest.main()
