"""Exact same-run SDK tool and qualification regressions; no Cargo or network."""

import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("sdk_tool_artifact", ROOT / "tools/ci/sdk_tool_artifact.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)


class SdkToolArtifact(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / "target/sdk-tool-artifact-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "Cargo.lock").write_text("locked fixture")
        binary = bytearray(64)
        binary[:6], binary[18:20] = b"\x7fELF\x02\x01", b"\x3e\x00"
        self.binary = self.repo / "target/debug/tilo-app"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(binary)
        self.cgroup = self.repo / "cgroup"
        self.cgroup.mkdir()
        for name, value in {"cpu.max": "200000 100000", "memory.max": str(5 << 30),
                            "memory.peak": str(4 << 30)}.items():
            (self.cgroup / name).write_text(value)
        self.env = patch.dict(os.environ, tool.ENVIRONMENT)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.git = patch.object(tool, "git", side_effect=lambda repo, *args:
                                "" if args[0] == "status" else "a" * 40 if args[-1] == "HEAD" else "b" * 40)
        self.git.start()
        self.addCleanup(self.git.stop)
        self.supplied = dict(builder_image="registry.tilo.so/tilo/tilo-builder@sha256:" + "c" * 64,
                             run_id=1234, run_attempt=1, toolchain="rustc fixture")
        self.expected = tool.identity(self.repo, self.supplied)
        self.directory = self.repo / "artifact"
        self.digest = tool.create(self.repo, self.directory, self.expected, self.cgroup)
        self.manifest = self.directory / "manifest.json"

    def install(self, expected=None, digest=None):
        tool.install(self.repo, self.directory, expected or self.expected, digest or self.digest)

    def rewrite(self, change):
        value = json.loads(self.manifest.read_bytes())
        change(value)
        self.manifest.write_text(json.dumps(value))
        return tool.sha(self.manifest)

    def test_exact_roundtrip_installs_the_verified_executable(self):
        original = self.binary.read_bytes()
        self.binary.unlink()
        self.install()
        self.assertEqual(self.binary.read_bytes(), original)
        self.assertEqual(self.binary.stat().st_mode & 0o777, 0o755)

    def test_cross_source_tree_builder_run_attempt_and_toolchain_rejected(self):
        for key, wrong in dict(source_commit="d" * 40, source_tree="e" * 40,
                               builder_image=self.supplied["builder_image"].replace("c", "d"),
                               run_id=1235, run_attempt=2, toolchain="different compiler").items():
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "identity"):
                self.install(expected=dict(self.expected, **{key: wrong}))

    def test_receipt_hash_is_authenticated_outside_download(self):
        self.manifest.write_text(self.manifest.read_text() + " ")
        with self.assertRaisesRegex(ValueError, "authenticated"):
            self.install()

    def test_tampered_truncated_and_missing_product_fail_without_fallback(self):
        product = self.directory / "tilo-app"
        original = product.read_bytes()
        for content in (original + b"tamper", original[:12]):
            product.write_bytes(content)
            with self.subTest(size=len(content)), self.assertRaises(ValueError):
                self.install()
        product.unlink()
        with self.assertRaises(ValueError):
            self.install()

    def test_wrong_native_architecture_and_size_fail(self):
        product = self.directory / "tilo-app"
        data = bytearray(product.read_bytes())
        data[18:20] = b"\xb7\x00"
        product.write_bytes(data)
        digest = self.rewrite(lambda r: r["product"].update(sha256=tool.sha(product)))
        with self.assertRaisesRegex(ValueError, "x86_64"):
            self.install(digest=digest)

    def test_unknown_fields_and_layout_escape_fail(self):
        digest = self.rewrite(lambda r: r.update(untrusted=True))
        with self.assertRaises(ValueError):
            self.install(digest=digest)
        digest = self.rewrite(lambda r: (r.pop("untrusted"), r["product"].update(path="../tilo-app")))
        with self.assertRaisesRegex(ValueError, "layout"):
            self.install(digest=digest)

    def test_symlink_product_and_destination_fail(self):
        product = self.directory / "tilo-app"
        product.unlink()
        product.symlink_to(self.binary)
        with self.assertRaises(ValueError):
            self.install()
        product.unlink()
        product.write_bytes(self.binary.read_bytes())
        self.binary.unlink()
        self.binary.symlink_to(product)
        with self.assertRaisesRegex(ValueError, "destination"):
            self.install()

    def test_quality_limits_and_85_percent_memory_cutoff_are_enforced(self):
        for name, value in {"cpu.max": "600000 100000", "memory.max": str(22 << 30),
                            "memory.peak": str((5 << 30) * 85 // 100 + 1)}.items():
            path = self.cgroup / name
            old = path.read_text()
            path.write_text(value)
            with self.subTest(name=name), self.assertRaises(ValueError):
                tool.resources(self.cgroup)
            path.write_text(old)
        (self.cgroup / "memory.peak").unlink()
        with self.assertRaises(FileNotFoundError):
            tool.resources(self.cgroup)

    def test_source_and_build_environment_cannot_drift(self):
        with patch.object(tool, "git", return_value=" M src.rs"), self.assertRaises(ValueError):
            tool.identity(self.repo, self.supplied)
        with patch.dict(os.environ, CARGO_PROFILE_DEV_DEBUG="1"), self.assertRaises(ValueError):
            tool.identity(self.repo, self.supplied)
        self.assertNotIn("CARGO_TARGET_DIR", tool.ENVIRONMENT)

    def test_workflow_builds_once_and_both_consumers_bind_all_identities(self):
        source = (ROOT / ".forgejo/workflows/app-sdk.yml").read_text()
        self.assertEqual(source.count('cargo build --locked -p tilo-app-cli --bin tilo-app\n'), 1)
        producer = source.split("\n  web-tool:\n", 1)[1].split("\n  linux:\n", 1)[0]
        self.assertIn("runs-on: runner-quality", producer)
        self.assertIn('CARGO_BUILD_JOBS: "2"', producer)
        self.assertIn("SDK shared native tool build", producer)
        for name in ("web", "web-size"):
            # Extract by mapping indentation, rather than truncating on steps.
            body = re.split(r"\n  [\w-]+:\n", source.split(f"\n  {name}:\n", 1)[1], maxsplit=1)[0]
            self.assertIn("needs: [builder, policy, web-tool]", body)
            self.assertIn("needs.web-tool.result == 'success'", body)
            self.assertIn("name: app-sdk-native-tool-${{ github.run_id }}-${{ github.run_attempt }}", body)
            self.assertIn("RECEIPT_SHA256: ${{ needs.web-tool.outputs.receipt-sha256 }}", body)
            for flag in ("--builder-image", "--run-id", "--run-attempt", "--toolchain", "--receipt-sha256"):
                self.assertIn(flag, body)
            self.assertNotIn("continue-on-error", body)
            self.assertNotIn("cargo build --locked -p tilo-app-cli --bin tilo-app", body)


if __name__ == "__main__":
    unittest.main()
