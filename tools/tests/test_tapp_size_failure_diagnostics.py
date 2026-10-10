"""Real failure collector and workflow retention contract; no compilation."""
from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "retain_tapp_size_failure", ROOT / "tools/ci/retain_tapp_size_failure.py")
assert SPEC and SPEC.loader
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)
BUILDER = "registry.tilo.so/tilo/tilo-builder@sha256:" + "b" * 64


def elf(arch="aarch64"):
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    header[18:20] = collector.MACHINES[arch].to_bytes(2, "little")
    return bytes(header)


class TappSizeFailureDiagnostics(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@invalid")
        self.git("config", "commit.gpgsign", "false")
        for name in ("Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"):
            self.write(name, b"fixture\n")
        self.write(".gitignore", b"target/\ndist/app-release/\n")
        self.write("tools/perf/tapp-size-budget/budget.toml", b'''
architectures = ["aarch64", "x86_64"]
headroom = 0
[apps."so.tilo.fixture"]
aarch64 = 100
x86_64 = 100
''')
        self.write("apps/fixture/tilo-app.toml", b'''
[package]
id = "so.tilo.fixture"
[entrypoint]
bin = "bin/fixture"
''')
        self.git("add", ".")
        self.git("commit", "-qm", "fixture source")
        self.source = self.git("rev-parse", "HEAD")
        self.write("target/thin-app/fixture", elf())
        self.write("target/thin-app/libruntime_dylib.so", elf())
        self.write("dist/app-release/work/payload-fixture/bin/fixture", elf())
        self.write("dist/app-release/work/publisher.json", b"PRIVATE IDENTITY MUST NOT UPLOAD")
        self.tapp = b"s" * 101
        self.write("dist/app-release/tapp/fixture-aarch64.tapp", self.tapp)
        self.index = {"source": self.source, "architecture": "aarch64", "apps": [
            {"name": "fixture", "id": "so.tilo.fixture", "binary": "fixture",
             "file": "fixture-aarch64.tapp", "sha256": hashlib.sha256(self.tapp).hexdigest()}]}
        self.save_index()

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def save_index(self):
        self.write("dist/app-release/index.json", json.dumps(self.index).encode())

    def retain(self, **overrides):
        args = dict(arch="aarch64", event_source=self.source, base_source="",
                    builder=BUILDER, run_id="113769", attempt="1")
        args.update(overrides)
        return collector.retain(self.root, **args)

    def test_failed_gate_retains_exact_elf_and_existing_sidecars_but_no_secrets_or_packages(self):
        self.write("target/thin-app/fixture.map", b"existing linker map\n")
        self.write("target/thin-app/fixture.debug", elf())
        budget = collector.ratchet.load(self.root)
        self.assertEqual(collector.ratchet.measure(self.root / "dist/app-release", budget)[0], 1)
        manifest = self.retain()
        output = self.root / "target/ci/tapp-size-failure"
        self.assertEqual(set(p.name for p in output.iterdir()), {
            "fixture", "libruntime_dylib.so", "fixture.map", "fixture.debug", "manifest.json"})
        self.assertEqual(manifest["event_source"], self.source)
        self.assertEqual(manifest["tested_source"], self.source)
        self.assertEqual(manifest["builder_image"], BUILDER)
        self.assertEqual(manifest["target"], "aarch64-unknown-linux-gnu")
        self.assertTrue(manifest["complete_required_binaries"])
        for entry in manifest["files"]:
            payload = (output / entry["path"]).read_bytes()
            self.assertEqual(entry["sha256"], hashlib.sha256(payload).hexdigest())
            self.assertEqual(entry["bytes"], len(payload))
            self.assertNotIn(b"PRIVATE IDENTITY", payload)
        # Diagnostic collection cannot turn the original size failure green.
        self.assertEqual(collector.ratchet.measure(self.root / "dist/app-release", budget)[0], 1)
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.retain()

    def test_success_path_and_wrong_identity_never_emit_artifacts(self):
        for bad in ({"builder": "mutable:latest"}, {"event_source": "a" * 40},
                    {"run_id": "0"}, {"attempt": "0"}, {"arch": "arm64"}):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                self.retain(**bad)
        self.write("dist/app-release/tapp/fixture-aarch64.tapp", b"s" * 100)
        self.index["apps"][0]["sha256"] = hashlib.sha256(b"s" * 100).hexdigest()
        self.save_index()
        with self.assertRaisesRegex(ValueError, "no oversized"):
            self.retain()
        self.assertFalse((self.root / "target/ci/tapp-size-failure").exists())

    def test_index_digest_catalog_target_payload_and_symlink_negatives(self):
        original = json.loads(json.dumps(self.index))
        for field, value in (("source", "c" * 40), ("architecture", "x86_64")):
            self.index[field] = value
            self.save_index()
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.retain()
            self.index = json.loads(json.dumps(original))
        for field, value in (("sha256", "c" * 64), ("binary", "publisher.json"),
                             ("file", "../work/publisher.json"), ("name", "../work")):
            self.index["apps"][0][field] = value
            self.save_index()
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.retain()
            self.index = json.loads(json.dumps(original))
        self.save_index()
        binary = self.root / "target/thin-app/fixture"
        self.write("target/thin-app/fixture", elf("x86_64"))
        with self.assertRaisesRegex(ValueError, "ELF target mismatch"):
            self.retain()
        self.write("target/thin-app/fixture", elf() + b"changed")
        with self.assertRaisesRegex(ValueError, "signed payload staging"):
            self.retain()
        binary.unlink()
        binary.symlink_to(self.root / "dist/app-release/work/publisher.json")
        with self.assertRaisesRegex(ValueError, "symlinked"):
            self.retain()
        self.assertFalse((self.root / "target/ci/tapp-size-failure/manifest.json").exists())

    def test_existing_sidecar_symlink_cannot_export_identity_files(self):
        sidecar = self.root / "target/thin-app/fixture.map"
        sidecar.symlink_to(self.root / "dist/app-release/work/publisher.json")
        manifest = self.retain()
        self.assertTrue(manifest["complete_required_binaries"])
        self.assertEqual(manifest["omitted"][0]["input"], "target/thin-app/fixture.map")
        self.assertIn("symlinked", manifest["omitted"][0]["reason"])
        self.assertFalse((self.root / "target/ci/tapp-size-failure/fixture.map").exists())

    def test_bounded_storage_reports_optional_omissions_without_rebuilding(self):
        self.write("target/thin-app/fixture.map", b"map")
        with patch.object(collector, "MAX_TOTAL", 70):
            manifest = self.retain()
        self.assertLessEqual(manifest["total_bytes"], 70)
        self.assertTrue(manifest["complete_required_binaries"])
        self.assertEqual(manifest["omitted"][0]["input"], "target/thin-app/libruntime_dylib.so")
        self.assertFalse((self.root / "target/ci/tapp-size-failure/fixture.sym").exists())

    def test_up_to_date_pr_with_noop_merge_keeps_exact_head_and_selected_base(self):
        manifest = self.retain(base_source=self.source)
        self.assertEqual(manifest["tested_source"], self.source)
        self.assertEqual(manifest["base_source"], self.source)

    def test_pr_identity_records_actual_merge_and_rejects_another_base_or_dirty_tree(self):
        self.git("checkout", "-qb", "base")
        self.write("base.txt", b"base")
        self.git("add", "base.txt")
        self.git("commit", "-qm", "base")
        base = self.git("rev-parse", "HEAD")
        self.git("checkout", "-qb", "pr", self.source)
        self.write("pr.txt", b"pr")
        self.git("add", "pr.txt")
        self.git("commit", "-qm", "pr")
        self.source = self.git("rev-parse", "HEAD")
        self.git("merge", "--no-ff", "-qm", "tested merge", base)
        self.index["source"] = self.source
        self.save_index()
        with self.assertRaisesRegex(ValueError, "expected PR merge"):
            self.retain(base_source="d" * 40)
        self.write("Cargo.lock", b"dirty tracked source")
        with self.assertRaises(subprocess.CalledProcessError):
            self.retain(base_source=base)
        self.write("Cargo.lock", b"fixture\n")
        manifest = self.retain(base_source=base)
        self.assertEqual(manifest["tested_source"], self.git("rev-parse", "HEAD"))
        self.assertNotEqual(manifest["tested_source"], self.source)
        self.assertEqual(manifest["base_source"], base)

    def test_workflows_preserve_gate_failure_and_scope_failure_only_arm_uploads(self):
        for filename, job, builder, base in (
            ("check.yml", "tapps", "needs.builder.outputs.image", True),
            ("release-apps.yml", "build", "needs.plan.outputs.builder_image", False),
        ):
            with self.subTest(workflow=filename):
                workflow = yaml.safe_load((ROOT / ".forgejo/workflows" / filename).read_text())
                steps = workflow["jobs"][job]["steps"]
                gate = next(s for s in steps if s.get("id") == "tapp-size")
                self.assertNotIn("continue-on-error", gate)
                self.assertNotIn("if", gate)
                self.assertEqual(gate["run"].strip(), 'python3 tools/perf/tapp_size_ratchet.py measure dist/app-release --arch "${{ matrix.arch }}" --summary "$GITHUB_STEP_SUMMARY"')
                retention = next(s for s in steps if s.get("id") == "tapp-size-evidence")
                upload = next(s for s in steps if s.get("name") == "Save failed ARM TAPP size evidence")
                for step in (retention, upload):
                    self.assertIn("failure()", step["if"])
                    self.assertIn("steps.tapp-size.outcome == 'failure'", step["if"])
                    self.assertIn("matrix.arch == 'aarch64'", step["if"])
                self.assertIn("steps.tapp-size-evidence.outcome == 'success'", upload["if"])
                self.assertEqual(retention["env"], {"DIAGNOSTIC_BUILDER_IMAGE": "${{ " + builder + " }}"})
                self.assertIn('--event-source "$GITHUB_SHA"', retention["run"])
                self.assertEqual('--base-source "${{ needs.select.outputs.main }}"' in retention["run"], base)
                self.assertEqual(upload["with"]["path"], "target/ci/tapp-size-failure/")
                self.assertEqual(upload["with"]["retention-days"], 3)
                self.assertEqual(upload["with"]["if-no-files-found"], "error")
                self.assertRegex(upload["uses"], r"^actions/upload-artifact@[0-9a-f]{40}$")
                self.assertLess(steps.index(gate), steps.index(retention))
                self.assertLess(steps.index(retention), steps.index(upload))


if __name__ == "__main__":
    unittest.main()
