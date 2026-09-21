"""Exercise metadata and manifest rejection without pulling a multi-GB snapshot."""

import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "dev/verify-rpm-snapshot-buildkit.py"
SPEC = importlib.util.spec_from_file_location("snapshot_buildkit", SCRIPT)
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)
REF = "code.example/tilo/rpms@sha256:" + "a" * 64


class BuildkitSnapshotTests(unittest.TestCase):
    def check(self, metadata, manifest="example-package\n", host="x86_64", ref=REF,
              inspect_error=False, build_error=False, raw_metadata=None, manifest_kind="file"):
        commands = []

        def read(command, **kwargs):
            commands.append(command)
            if command[:3] == ["docker", "buildx", "imagetools"]:
                self.assertEqual(command[4], ref)
                if inspect_error:
                    raise subprocess.CalledProcessError(1, command)
                return json.dumps(metadata) if raw_metadata is None else raw_metadata
            self.assertTrue(command[1].endswith("derive-rpm-package-set.py"))
            return "example-package\n"

        def build(command, **kwargs):
            commands.append(command)
            self.assertEqual(command[:3], ["docker", "buildx", "build"])
            self.assertEqual(command[command.index("--builder") + 1], "relay-builder")
            self.assertIn("RPMS_REF=" + ref, command)
            dockerfile = (Path(command[-1]) / "Dockerfile").read_text()
            self.assertIn("FROM ${RPMS_REF} AS rpms", dockerfile)
            self.assertIn("COPY --from=rpms /repo/tilo-direct-package-specs.txt /actual", dockerfile)
            self.assertNotIn("RUN ", dockerfile)
            if build_error:
                raise subprocess.CalledProcessError(1, command)
            output = Path(command[command.index("--output") + 1].split("dest=", 1)[1])
            output.mkdir()
            if manifest_kind == "directory":
                (output / "actual").mkdir()
            elif manifest_kind == "symlink":
                target = output / "elsewhere"
                target.write_text("example-package\n")
                (output / "actual").symlink_to(target)
            elif manifest is not None:
                (output / "actual").write_text(manifest)

        self.commands = commands
        with patch.object(VERIFY.platform, "machine", return_value=host), \
             patch.object(VERIFY.subprocess, "check_output", side_effect=read), \
             patch.object(VERIFY.subprocess, "run", side_effect=build):
            VERIFY.verify(ref, "relay-builder")

    def test_native_single_and_multiarch_configs(self):
        for host, arch in (("x86_64", "amd64"), ("aarch64", "arm64")):
            image = {"architecture": arch, "os": "linux"}
            for metadata in (image, {f"linux/{arch}": image, "unknown/unknown": {}}):
                with self.subTest(host=host, metadata=metadata):
                    self.check(metadata, host=host)
                    self.assertIn(f"linux/{arch}", self.commands[-1])

    def test_missing_or_wrong_metadata_never_downloads_layers(self):
        invalid = [None, {}, "", {"architecture": "arm64", "os": "linux"},
                   {"architecture": "amd64"}, {"architecture": "amd64", "os": "windows"},
                   {"linux/amd64": {"architecture": "arm64", "os": "linux"}}]
        for metadata in invalid:
            with self.subTest(metadata=metadata), self.assertRaises(ValueError):
                self.check(metadata)
            self.assertEqual(len(self.commands), 1)

    def test_registry_metadata_failure_is_fatal(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.check({}, inspect_error=True)
        self.assertEqual(len(self.commands), 1)

    def test_invalid_registry_json_is_fatal_before_layer_download(self):
        with self.assertRaises(ValueError):
            self.check({}, raw_metadata="not json")
        self.assertEqual(len(self.commands), 1)

    def test_ambiguous_arm_variant_is_rejected_before_layer_download(self):
        image = {"architecture": "arm64", "os": "linux", "variant": "v8"}
        with self.assertRaises(ValueError):
            self.check({"linux/arm64": image, "linux/arm64/v8": image}, host="aarch64")
        self.assertEqual(len(self.commands), 1)

    def test_manifest_must_be_a_regular_file(self):
        for kind in ("directory", "symlink"):
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, "no regular package manifest"):
                self.check({"architecture": "amd64", "os": "linux"}, manifest_kind=kind)

    def test_unpinned_snapshot_is_rejected_before_registry_access(self):
        with self.assertRaises(ValueError):
            self.check({}, ref="code.example/tilo/rpms:latest")
        self.assertEqual(self.commands, [])

    def test_unknown_host_is_rejected(self):
        with self.assertRaises(ValueError):
            self.check({}, host="riscv64")
        self.assertEqual(self.commands, [])

    def test_missing_and_stale_package_manifest_are_fatal(self):
        for manifest in (None, "old-package\n"):
            with self.subTest(manifest=manifest), self.assertRaises(ValueError):
                self.check({"architecture": "amd64", "os": "linux"}, manifest=manifest)

    def test_buildkit_export_failure_is_fatal(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.check({"architecture": "amd64", "os": "linux"}, build_error=True)

    def test_workflow_uses_same_builder_for_verification_and_relay(self):
        workflow = (SCRIPT.parents[2] / ".forgejo/workflows/tilo-relay.yaml").read_text()
        self.assertLess(workflow.index("id: buildx"), workflow.index("run: python3 os/dev/verify-rpm-snapshot-buildkit.py"))
        self.assertIn("SNAPSHOT_BUILDER: ${{ steps.buildx.outputs.name }}", workflow)
        self.assertIn("builder: ${{ steps.buildx.outputs.name }}", workflow)
        self.assertNotIn('verify-rpm-snapshot-package-set.sh docker "$RPMS_REF"', workflow)


if __name__ == "__main__":
    unittest.main()
