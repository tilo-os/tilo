#!/usr/bin/env python3
"""Small file-backed checks for the native Windows direct source boundary."""
import importlib.util
import io
import json
from contextlib import contextmanager, redirect_stdout
import platform
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch
import uuid

DEV = Path(__file__).resolve().parents[1] / "dev"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prep = load("windows_direct_source", DEV / "prepare-windows-direct-source.py")
fixture = load("windows_direct_copy_fixture", Path(__file__).with_name(
    "test_copy_windows_direct_final_install.py"))


class SourceTemplateTests(unittest.TestCase):
    def test_finalize_realized_windows_numbers_move_only_source_winre_slot(self):
        plan, _, final, disk_size = fixture.direct_fixture()
        # Windows' offset-ordered target numbers collide with retained WinRE4.
        for number, row in zip((4, 5, 6), plan['target_partitions'], strict=True):
            row['number'] = number
        original_plan = json.loads(json.dumps(plan))
        with tempfile.TemporaryDirectory() as tmp:
            raw = Path(tmp) / 'source.raw'
            plan_path = Path(tmp) / 'plan.json'
            plan_path.write_text(json.dumps(plan))
            with patch.object(prep.direct, 'GIB', fixture.MIB):
                prep.create_template(SimpleNamespace(plan=plan_path, output=raw))
                # The VM allocated GPT slots5/6/7, retaining synthetic WinRE4.
                self.guided_vm_install(raw, plan, final)
                before = raw.read_bytes()
                with raw.open('rb') as stream:
                    original_guid, _ = prep.direct.read_gpt(stream, disk_size)
                prep.finalize_source(SimpleNamespace(plan=plan_path, source_raw=raw))
            after = raw.read_bytes()
            with raw.open('rb') as stream:
                guid, actual = prep.direct.read_gpt(stream, disk_size)
            self.assertEqual(guid, original_guid)
            by_id = {row['partuuid']: row for row in actual}
            winre = plan['preserve_partitions'][-1]
            self.assertEqual(by_id[winre['partuuid']], {**winre, 'number': 7})
            for row in plan['preserve_partitions'][:-1] + plan['target_partitions']:
                self.assertEqual(by_id[row['partuuid']], prep.direct.normalized([row])[0])
            for row in plan['preserve_partitions'] + plan['target_partitions']:
                start, size = row['offset_bytes'], row['size_bytes']
                self.assertEqual(after[start:start + size], before[start:start + size])
            # GPT occupies only the leading/trailing sectors. Every other byte
            # is unchanged, including unused extents between partitions.
            self.assertEqual(after[34 * 512:-33 * 512], before[34 * 512:-33 * 512])
            self.assertEqual(plan, original_plan)
            self.assertTrue(prep.direct.same_gpt_content(actual,
                plan['preserve_partitions'] + plan['target_partitions']))

    def test_plan_rows_refuses_other_preserved_partition_collisions(self):
        plan, _, _, _ = fixture.direct_fixture()
        plan['target_partitions'][0]['number'] = 3
        with self.assertRaisesRegex(ValueError, 'only preserved WinRE'):
            prep.plan_rows(plan)
        plan['target_partitions'][0]['number'] = 6
        with self.assertRaisesRegex(ValueError, 'duplicate slots'):
            prep.plan_rows(plan)

    def test_reset_reuses_payload_and_refuses_changed_identity(self):
        plan, _, final, disk_size = fixture.direct_fixture()
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            raw = work / 'source.raw'
            plan_path = work / 'plan.json'
            plan_path.write_text(json.dumps(plan))
            with patch.object(prep.direct, 'GIB', fixture.MIB):
                prep.create_template(SimpleNamespace(plan=plan_path, output=raw))
                self.guided_vm_install(raw, plan, final)
            manifest_path = Path(str(raw) + '.manifest.json')
            manifest = json.loads(manifest_path.read_text())
            args = SimpleNamespace(plan=plan_path, manifest=manifest_path, source_raw=raw,
                                   confirm_source_disk_guid=manifest['source_disk_guid'])

            def reset():
                with patch.object(prep.direct, 'GIB', fixture.MIB):
                    prep.reset_template(args)

            @contextmanager
            def fixture_lock(path):
                with path.open('r+b', buffering=0) as stream:
                    yield stream

            before = raw.read_bytes()
            args.confirm_source_disk_guid = str(uuid.uuid4())
            with self.assertRaisesRegex(ValueError, 'reset confirmation'):
                reset()
            self.assertEqual(raw.read_bytes(), before)
            args.confirm_source_disk_guid = manifest['source_disk_guid']
            with patch.object(prep, 'exclusive_source', fixture_lock):
                reset()
            after = raw.read_bytes()
            self.assertEqual(len(after), disk_size)
            for row in plan['preserve_partitions']:
                start, size = row['offset_bytes'], row['size_bytes']
                self.assertEqual(after[start:start + size], before[start:start + size])
            for row in final:
                start, size = row['offset_bytes'], row['size_bytes']
                edge = min(1024 * 1024, size // 2)
                self.assertEqual(after[start:start + edge], bytes(edge))
                self.assertEqual(after[start + size - edge:start + size], bytes(edge))
                self.assertEqual(after[start + edge:start + size - edge],
                                 before[start + edge:start + size - edge])
            with raw.open('rb') as stream:
                identity, actual = prep.direct.read_gpt(stream, disk_size)
            self.assertEqual(identity, manifest['source_disk_guid'])
            self.assertTrue(prep.direct.same_gpt_content(actual, plan['preserve_partitions']))
            # The same manifest must refuse resetting a modified Windows edge.
            with raw.open('r+b') as stream:
                stream.seek(plan['preserve_partitions'][0]['offset_bytes'])
                stream.write(b'X')
            damaged = raw.read_bytes()
            with self.assertRaisesRegex(ValueError, 'edge contents changed'):
                reset()
            self.assertEqual(raw.read_bytes(), damaged)

    @unittest.skipUnless(platform.system() == 'Windows', 'native Windows sharing rules')
    def test_exclusive_source_refuses_open_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            raw = Path(tmp) / 'source.raw'
            raw.write_bytes(b'owned VM source')
            with raw.open('rb'):
                with self.assertRaisesRegex(OSError, 'locked exclusively'):
                    with prep.exclusive_source(raw):
                        self.fail('open source was admitted')
            with prep.exclusive_source(raw) as stream:
                self.assertEqual(stream.read(), b'owned VM source')

    def guided_vm_install(self, raw, plan, final):
        vm_final = [{**row, "partuuid": str(uuid.UUID(int=90 + i))}
                    for i, row in enumerate(final)]
        with raw.open("r+b", buffering=0) as stream:
            disk_guid, _ = prep.direct.read_gpt(stream, raw.stat().st_size)
            prep.write_template(stream, plan, rows=plan["preserve_partitions"] + vm_final,
                                disk_guid=uuid.UUID(disk_guid))
            for row in final:
                stream.seek(row["offset_bytes"])
                stream.write(bytes([row["number"]]) * row["size_bytes"])
        return vm_final

    def test_template_creation_and_finalize(self):
        with self.subTest(phase="template_starts_with_only_synthetic_windows_and_guided_free_extent"):
            plan, rows, final, disk_size = fixture.direct_fixture()
            with tempfile.TemporaryDirectory() as tmp:
                out = Path(tmp) / "source.raw"
                plan_path = Path(tmp) / "plan.json"
                plan_path.write_text(json.dumps(plan))
                with patch.object(prep.direct, "GIB", fixture.MIB):
                    prep.create_template(SimpleNamespace(plan=plan_path, output=out))
                manifest = Path(str(out) + ".manifest.json")
                self.assertTrue(manifest.is_file())
                prep.verify_template(SimpleNamespace(manifest=manifest, source_raw=out))
                self.assertEqual(out.stat().st_size, disk_size)
                with out.open("rb") as raw:
                    _, actual = prep.direct.read_gpt(raw, disk_size)
                    self.assertTrue(prep.direct.same_gpt_content(
                        actual, plan["preserve_partitions"]))
                    for row in plan["preserve_partitions"]:
                        raw.seek(row["offset_bytes"])
                        self.assertEqual(raw.read(64), bytes(64))
                self.assertEqual(len(actual), 4)
        with self.subTest(phase="finalize_rewrites_only_gpt_metadata_after_guided_vm_install"):
            plan, rows, final, disk_size = fixture.direct_fixture()
            with tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                raw = work / "source.raw"
                plan_path = work / "plan.json"
                plan_path.write_text(json.dumps(plan))
                with patch.object(prep.direct, "GIB", fixture.MIB):
                    prep.create_template(SimpleNamespace(plan=plan_path, output=raw))
                    vm_final = self.guided_vm_install(raw, plan, final)
                    output = io.StringIO()
                    with redirect_stdout(output):
                        prep.verify_template(SimpleNamespace(
                            manifest=Path(str(raw) + ".manifest.json"), source_raw=raw))
                    proof = json.loads(output.getvalue())
                    self.assertEqual(proof["final_partuuids"], dict(zip(
                        ("efi", "boot", "root"),
                        (row["partuuid"] for row in vm_final), strict=True)))
                    self.assertEqual(proof["disk_guid"], json.loads(
                        Path(str(raw) + ".manifest.json").read_text())["source_disk_guid"])
                    with raw.open("rb") as stream:
                        before = {row["partuuid"]: prep.direct.hash_range(
                            stream, row["offset_bytes"], row["size_bytes"]) for row in final}
                    prep.finalize_source(SimpleNamespace(plan=plan_path, source_raw=raw))
                with raw.open("rb") as stream:
                    _, actual = prep.direct.read_gpt(stream, disk_size)
                    self.assertTrue(prep.direct.same_gpt_content(actual, rows))
                    after = {row["partuuid"]: prep.direct.hash_range(
                        stream, row["offset_bytes"], row["size_bytes"]) for row in final}
                self.assertEqual(after, before)
                prep.verify_template(SimpleNamespace(
                    manifest=Path(str(raw) + ".manifest.json"), source_raw=raw))
        with self.subTest(phase="finalize_rejects_wrong_vm_geometry_without_rewriting_gpt"):
            plan, _, final, disk_size = fixture.direct_fixture()
            with tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                raw = work / "source.raw"
                plan_path = work / "plan.json"
                plan_path.write_text(json.dumps(plan))
                with patch.object(prep.direct, "GIB", fixture.MIB):
                    prep.create_template(SimpleNamespace(plan=plan_path, output=raw))
                    vm_final = self.guided_vm_install(raw, plan, final)
                    vm_final[-1]["size_bytes"] -= fixture.MIB
                    with raw.open("r+b", buffering=0) as stream:
                        disk_guid, _ = prep.direct.read_gpt(stream, raw.stat().st_size)
                        prep.write_template(stream, plan,
                                            rows=plan["preserve_partitions"] + vm_final,
                                            disk_guid=uuid.UUID(disk_guid))
                    with self.assertRaisesRegex(ValueError, "different final partition layout"):
                        prep.finalize_source(SimpleNamespace(plan=plan_path, source_raw=raw))
                with raw.open("rb") as stream:
                    _, after = prep.direct.read_gpt(stream, disk_size)
                self.assertTrue(prep.direct.same_gpt_content(
                    after, plan["preserve_partitions"] + vm_final))

    def test_verification_and_receipts(self):
        with self.subTest(phase="verify_template_detects_synthetic_windows_byte_change"):
            plan, _, _, _ = fixture.direct_fixture()
            with tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                raw = work / "source.raw"
                plan_path = work / "plan.json"
                plan_path.write_text(json.dumps(plan))
                with patch.object(prep.direct, "GIB", fixture.MIB):
                    prep.create_template(SimpleNamespace(plan=plan_path, output=raw))
                with raw.open("r+b") as stream:
                    stream.seek(plan["preserve_partitions"][0]["offset_bytes"])
                    stream.write(b"X")
                with self.assertRaisesRegex(ValueError, "edge contents changed"):
                    prep.verify_template(SimpleNamespace(
                        manifest=Path(str(raw) + ".manifest.json"), source_raw=raw))
        with self.subTest(phase="receipt_hashes_only_installed_final_extents"):
            plan, rows, final, disk_size = fixture.direct_fixture()
            with tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp)
                raw = work / "source.raw"
                plan_path = work / "plan.json"
                receipt_path = work / "source.json"
                plan_path.write_text(json.dumps(plan))
                with patch.object(prep.direct, "GIB", fixture.MIB):
                    prep.create_template(SimpleNamespace(plan=plan_path, output=raw))
                    self.guided_vm_install(raw, plan, final)
                    prep.finalize_source(SimpleNamespace(plan=plan_path, source_raw=raw))
                    with raw.open("rb") as source:
                        disk_guid, _ = prep.direct.read_gpt(source, disk_size)
                    proof_path = work / "checked.json"
                    proof_path.write_text("{}")
                    args = SimpleNamespace(plan=plan_path, source_raw=raw,
                                           ci_image="registry.example/tilo@sha256:" + "a" * 64,
                                           verify_key=work / "key.pub", output=receipt_path,
                                           ci_index_manifest=work / "index.json",
                                           ci_child_manifest=work / "child.json",
                                           vm_proof_checked=proof_path)
                    checked = {"guest_proof": {"disk_guid": disk_guid,
                               "composefs_digest": "c" * 128,
                               "target_uki_sha256": "d" * 64}}
                    ci_info = {"key_sha256": "b" * 64, "source_revision": "c" * 40,
                               "generation": 7, "statement_sha256": "d" * 64,
                               "invocation_id": "1234",
                               "image_kind": "generic-index", "firmware_candidate": None,
                               "firmware_manifest_sha256": None}
                    with (patch.object(prep, "verified_ci_image", return_value=ci_info),
                          patch.object(prep.provenance, "signed_ci_child",
                                       return_value=("sha256:" + "a" * 64,
                                                     "sha256:" + "e" * 64, [], "generic-index")),
                          patch.object(prep.provenance, "checked_guest_proof", return_value=checked),
                          patch.object(prep.provenance, "installed_uki_sha256", return_value="d" * 64),
                          patch.object(prep.provenance, "installed_loader", return_value=("\\EFI\\BOOT\\BOOTAA64.EFI", "f" * 64))):
                        prep.make_receipt(args)
                receipt = json.loads(receipt_path.read_text())
                self.assertEqual(receipt["ci_image"], args.ci_image)
                self.assertEqual(receipt["ci_verification_key_sha256"], "b" * 64)
                self.assertEqual(receipt["ci_child_digest"], "sha256:" + "e" * 64)
                self.assertEqual(receipt["ci_source_revision"], "c" * 40)
                self.assertEqual(set(receipt["final_sha256"]), set(prep.direct.ROLES))
                self.assertEqual(prep.direct.source_preflight(raw, receipt, rows, final, disk_size),
                                 receipt["final_sha256"])
        with self.subTest(phase="ci_verification_requires_digest_signature_and_attestation"):
            with tempfile.TemporaryDirectory() as tmp:
                key = Path(tmp) / "cosign.pub"
                key.write_bytes(b"test-key")
                image = "registry.example/tilo@sha256:" + "a" * 64
                with self.assertRaisesRegex(ValueError, "digest-pinned"):
                    prep.verified_ci_image("registry.example/tilo:latest", key, [], "generic-index")
                with patch.object(prep.subprocess, "run", return_value=SimpleNamespace(
                        returncode=1, stderr="bad signature")):
                    with self.assertRaisesRegex(ValueError, "verification failed"):
                        prep.verified_ci_image(image, key, [], "generic-index")


if __name__ == "__main__":
    unittest.main()
