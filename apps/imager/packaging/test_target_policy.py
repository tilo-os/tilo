"""The target policy producer must export only the signed, exact target bundle."""
import base64
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "fixtures/target-policy"
AUTHORITY = FIXTURE / "authority.pem"
ENCODED_FIXTURES = {
    "tilo-target.efi": FIXTURE / "encoded/tilo-target.efi.hex",
    "predictions/generation-7.json.uki.sig":
        FIXTURE / "encoded/predictions/generation-7.json.uki.sig.hex",
}
TAG = "imager-v1.2.3"
REVISION = "a" * 40
SPEC = importlib.util.spec_from_file_location("export_target_policy", HERE / "export-target-policy.py")
producer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(producer)
CATALOG_SPEC = importlib.util.spec_from_file_location("release_catalog_consumer", HERE / "release-catalog.py")
catalog = importlib.util.module_from_spec(CATALOG_SPEC)
CATALOG_SPEC.loader.exec_module(catalog)


def decode_hex_fixture(path):
    try:
        return bytes.fromhex(path.read_text(encoding="ascii"))
    except (UnicodeDecodeError, ValueError) as error:
        raise AssertionError(f"{path} is not checked-in hexadecimal") from error


def fixture_files():
    return {
        "tilo-target.efi": decode_hex_fixture(ENCODED_FIXTURES["tilo-target.efi"]),
        "predictions/generation-7.json": (FIXTURE / "predictions/generation-7.json").read_bytes(),
        "predictions/generation-7.json.uki.sig": decode_hex_fixture(
            ENCODED_FIXTURES["predictions/generation-7.json.uki.sig"]),
    }


class TargetPolicyProducerTests(unittest.TestCase):
    def setUp(self):
        scratch = os.environ.get("TMPDIR", "/var/tmp")
        self.temp = tempfile.TemporaryDirectory(prefix="tilo-target-policy-test-", dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.decoded = self.root / "decoded-fixture"
        for name, data in fixture_files().items():
            path = self.decoded / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

    def bundle(self):
        destination = self.root / "bundle"
        if destination.exists():
            shutil.rmtree(destination)
        # The decoded fixture tree has exactly the three signed bundle files;
        # the checked-in hexadecimal sources stay outside the bundle shape.
        shutil.copytree(self.decoded, destination)
        return destination

    def test_signed_fixture_archive_is_exact_deterministic_and_catalog_parses_it(self):
        result, files = producer.read_archive(self.archive(), AUTHORITY, arch="x86_64")
        self.assertEqual(result["generation"], 7)
        self.assertEqual(result["target_uki_sha256"], producer.sha256(files["tilo-target.efi"]))
        self.assertEqual(set(files), producer.expected_names(7))
        with tarfile.open(self.archive(), "r:") as archive:
            members = archive.getmembers()
        self.assertEqual([member.name for member in members], sorted(producer.expected_names(7)))
        for member in members:
            self.assertTrue(member.isfile())
            self.assertEqual((member.mtime, member.uid, member.gid, member.mode), (0, 0, 0, 0o644))
            self.assertEqual((member.uname, member.gname), ("", ""))

        second = self.root / "second.tar"
        producer.write_archive(second, files)
        self.assertEqual(second.read_bytes(), self.archive().read_bytes())

        fragment = producer.fragment_for("x86_64", TAG, "tilo", 7, result["target_uki_sha256"], self.archive())
        # The shipped consumer's parser is the authority on the fragment shape.
        catalog.target_policy(fragment, TAG)
        producer.validate_fragment(fragment, TAG, "tilo")
        self.assertEqual(fragment["archive_sha256"], producer.sha256(self.archive().read_bytes()))
        self.assertEqual(fragment["archive_size"], self.archive().stat().st_size)
        self.assertNotIn("token", json.dumps(fragment).lower())
        self.assertNotIn(str(self.root), json.dumps(fragment))

    def test_verify_command_accepts_the_exported_bundle_and_rejects_changed_bytes(self):
        output = self.root / "export"
        output.mkdir()
        archive = output / "tilo-target-policy-x86_64.tar"
        producer.write_archive(archive, fixture_files())
        fragment = producer.fragment_for("x86_64", TAG, "tilo", 7,
                                         producer.sha256(fixture_files()["tilo-target.efi"]), archive)
        (output / "target-policy-catalog-x86_64.json").write_text(json.dumps(fragment), encoding="utf-8")
        command = [sys.executable, str(HERE / "export-target-policy.py"), "verify",
                   "--bundle", str(output), "--release-id", TAG, "--authority", str(AUTHORITY)]
        self.assertEqual(subprocess.run(command, capture_output=True, text=True).returncode, 0)
        archive.write_bytes(archive.read_bytes() + b"changed")
        rejected = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("do not match", rejected.stderr)

    def test_wrong_architecture_and_generation_are_rejected(self):
        bundle = self.bundle()
        with self.assertRaisesRegex(ValueError, "not a aarch64"):
            producer.verify_bundle_directory(bundle, AUTHORITY, arch="aarch64")
        with self.assertRaisesRegex(ValueError, "generation 8"):
            producer.verify_bundle_directory(bundle, AUTHORITY, generation=8)

    def test_extra_entries_are_rejected(self):
        for relative in ("extra", "predictions/extra", "predictions/generation-8.json"):
            with self.subTest(relative=relative):
                bundle = self.bundle()
                path = bundle / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("unexpected", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "missing or extra|exactly one"):
                    producer.verify_bundle_directory(bundle, AUTHORITY, arch="x86_64")
                shutil.rmtree(bundle)

    def test_changed_target_or_prediction_bytes_break_the_release_authority_signature(self):
        bundle = self.bundle()
        target = bundle / "tilo-target.efi"
        target.write_bytes(target.read_bytes() + b"tamper")
        with self.assertRaisesRegex(ValueError, "pinned release authority"):
            producer.verify_bundle_directory(bundle, AUTHORITY, arch="x86_64")

        shutil.rmtree(bundle)
        bundle = self.bundle()
        prediction = bundle / "predictions/generation-7.json"
        document = json.loads(prediction.read_text(encoding="utf-8"))
        document["approved_policy"] = "A" * 43 + "="
        prediction.write_text(json.dumps(document, separators=(",", ":")), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "pinned release authority"):
            producer.verify_bundle_directory(bundle, AUTHORITY, arch="x86_64")

    def test_published_provenance_must_bind_the_exact_digest_source_and_generation(self):
        digest = "d" * 64
        image = "registry.tilo.so/tilo/tilo@sha256:" + digest
        statement = {
            "predicateType": "https://slsa.dev/provenance/v1",
            "subject": [{"digest": {"sha256": digest}}],
            "predicate": {"buildDefinition": {
                "buildType": "https://tilo.sh/buildtypes/bootc-os/v1",
                "internalParameters": {"expectedReleaseDigest": "sha256:" + digest},
                "externalParameters": {"generation": 7},
                "resolvedDependencies": [
                    {"uri": "git+ssh://code.tilo.so/tilo/tilo@" + REVISION}],
            }},
        }
        row = {"payload": base64.b64encode(json.dumps(statement).encode()).decode()}
        result = types.SimpleNamespace(returncode=0, stdout=json.dumps([row]), stderr="")
        with patch.object(producer.subprocess, "run", return_value=result):
            producer.verify_image_provenance(image, REVISION, 7)
            with self.assertRaisesRegex(ValueError, "does not bind"):
                producer.verify_image_provenance(image, "b" * 40, 7)
            with self.assertRaisesRegex(ValueError, "does not bind"):
                producer.verify_image_provenance(image, REVISION, 8)
        statement["predicate"]["buildDefinition"]["internalParameters"]["expectedReleaseDigest"] = "sha256:" + "e" * 64
        row = {"payload": base64.b64encode(json.dumps(statement).encode()).decode()}
        result = types.SimpleNamespace(returncode=0, stdout=json.dumps([row]), stderr="")
        with patch.object(producer.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(ValueError, "does not bind"):
                producer.verify_image_provenance(image, REVISION, 7)

    def test_source_and_architecture_labels_are_bound_to_the_committed_source(self):
        config = json.loads((FIXTURE / "image-config-x86_64.json").read_text(encoding="utf-8"))
        self.assertEqual(producer.check_image_config(config, "x86_64", REVISION), REVISION)
        with self.assertRaisesRegex(ValueError, "differs from the committed source"):
            producer.check_image_config(config, "x86_64", "b" * 40)
        with self.assertRaisesRegex(ValueError, "not Linux aarch64"):
            producer.check_image_config(config, "aarch64", REVISION)
        missing = copy.deepcopy(config)
        del missing["config"]["Labels"]["org.opencontainers.image.revision"]
        with self.assertRaisesRegex(ValueError, "no full source revision label"):
            producer.check_image_config(missing, "x86_64", REVISION)

    def test_diagnostic_export_never_writes_a_qualified_fragment(self):
        output = self.root / "diagnostic"
        output.mkdir()
        stale_fragment = output / "target-policy-catalog-x86_64.json"
        stale_archive = output / "tilo-target-policy-x86_64.tar"
        stale_fragment.write_text("stale", encoding="utf-8")
        stale_archive.write_bytes(b"stale")
        files = fixture_files()
        with patch.object(producer, "read_bundle", return_value={
                "generation": 7,
                "target_uki_sha256": producer.sha256(files["tilo-target.efi"]),
                "files": files}), contextlib.redirect_stdout(io.StringIO()):
            producer.export(types.SimpleNamespace(
                image="registry.tilo.so/tilo/tilo@sha256:" + "c" * 64,
                arch="x86_64", release_id=TAG, expected_source=REVISION,
                output=output, owner="tilo", authority=AUTHORITY, diagnostic=True))
        self.assertTrue((output / "tilo-target-policy-x86_64.diagnostic.tar").is_file())
        self.assertFalse(stale_fragment.exists())
        self.assertFalse(stale_archive.exists())
        self.assertEqual(list(output.glob("target-policy-catalog-*.json")), [])

    def test_fragment_rejects_foreign_origin_release_and_unknown_fields(self):
        archive = self.archive()
        result = producer.read_archive(archive, AUTHORITY, arch="x86_64")[0]
        fragment = producer.fragment_for("x86_64", TAG, "tilo", result["generation"],
                                        result["target_uki_sha256"], archive)
        for field, value in (("url", fragment["url"].replace("code.tilo.so", "example.invalid")),
                             ("release_id", "imager-v9.9.9"),
                             ("arch", "riscv64"),
                             ("archive_size", 0),
                             ("target_uki_sha256", "not-a-digest")):
            with self.subTest(field=field):
                changed = copy.deepcopy(fragment)
                changed[field] = value
                with self.assertRaises(ValueError):
                    producer.validate_fragment(changed, TAG, "tilo")
        changed = copy.deepcopy(fragment)
        changed["token"] = "must-not-travel"
        with self.assertRaisesRegex(ValueError, "missing or unknown"):
            producer.validate_fragment(changed, TAG, "tilo")

    def archive(self):
        path = self.root / "fixture.tar"
        if not path.exists():
            producer.write_archive(path, fixture_files())
        return path

    def test_archive_shape_rejects_extra_or_non_file_entries(self):
        files = fixture_files()
        path = self.root / "extra.tar"
        producer.write_archive(path, files | {"predictions/extra": b"unexpected"})
        with self.assertRaisesRegex(ValueError, "exactly three"):
            producer.read_archive(path, AUTHORITY, arch="x86_64")

        path = self.root / "link.tar"
        with tarfile.open(path, "w", format=tarfile.USTAR_FORMAT) as archive:
            for name, data in files.items():
                if name.endswith(".uki.sig"):
                    link = tarfile.TarInfo(name)
                    link.type = tarfile.SYMTYPE
                    link.linkname = "generation-7.json"
                    archive.addfile(link)
                    continue
                member = tarfile.TarInfo(name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        with self.assertRaisesRegex(ValueError, "non-file"):
            producer.read_archive(path, AUTHORITY, arch="x86_64")


if __name__ == "__main__":
    unittest.main()
