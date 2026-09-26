#!/usr/bin/env python3
"""Exercise release ordering and rejection paths without registry writes."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "promotion", Path(__file__).resolve().parents[1] / "dev/promote-native-index.py")
promotion = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(promotion)


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class PromotionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.revision = "a" * 40
        self.children = {"x86_64": "sha256:" + "1" * 64, "aarch64": "sha256:" + "2" * 64}
        for arch, digest in self.children.items():
            (self.directory / f"{arch}.json").write_text(json.dumps({
                "architecture": arch, "digest": digest, "sourceRevision": self.revision}))
        self.index = {"schemaVersion": 2, "manifests": [
            {"digest": digest, "platform": {"os": "linux", "architecture": promotion.ARCHITECTURES[arch]}}
            for arch, digest in self.children.items()]}
        self.raw = json.dumps(self.index).encode() + b"\n"
        self.digest = "sha256:" + hashlib.sha256(self.raw).hexdigest()
        self.calls = []
        self.bad_arch = False
        self.bad_revision = False
        self.bad_signature = False

    def command(self, *args):
        self.calls.append(args)
        if args[:2] == ("cosign", "verify") and self.bad_signature:
            raise subprocess.CalledProcessError(1, args)
        if args[:2] == ("skopeo", "inspect"):
            digest = args[-1].split("@")[-1]
            arch = next(arch for arch, value in self.children.items() if value == digest)
            return json.dumps({"Architecture": "unknown" if self.bad_arch else promotion.ARCHITECTURES[arch],
                               "Os": "linux", "Digest": digest,
                               "Labels": {"org.opencontainers.image.revision": "b" * 40 if self.bad_revision else self.revision}})
        if args[:3] == ("podman", "manifest", "push"):
            Path(args[args.index("--digestfile") + 1]).write_text(self.digest)
        return ""

    def promote(self):
        with patch.object(promotion, "run", side_effect=self.command), \
                patch.object(promotion.subprocess, "check_output", return_value=self.raw), \
                patch.object(promotion.subprocess, "run"):
            return promotion.promote("registry.tilo.so/tilo/rpms", ["latest", self.revision],
                                     self.revision, self.directory, Path("keys/cosign.pub"), "env://COSIGN_PRIVATE_KEY")

    def test_promotion_order_and_candidate_checks(self):
        with self.subTest(phase="signed_complete_index_precedes_release_tags"):
            self.assertTrue(self.promote().endswith("@" + self.digest))
            sign = next(i for i, call in enumerate(self.calls) if call[:2] == ("cosign", "sign"))
            copies = [(i, call) for i, call in enumerate(self.calls) if call[:2] == ("skopeo", "copy")]
            self.assertEqual(len(copies), 2)
            for i, call in copies:
                self.assertGreater(i, sign)
                self.assertIn("--all", call)
                self.assertIn("--preserve-digests", call)
        restart_fixture(self)
        with self.subTest(phase="candidate_digest_mismatch_never_promotes_tags"):
            self.raw += b" "
            with self.assertRaisesRegex(ValueError, "unexpected index content"):
                self.promote()
            self.assertFalse(any(call[:2] == ("skopeo", "copy") for call in self.calls))
        restart_fixture(self)
        with self.subTest(phase="candidate_missing_platform_never_signed"):
            self.index["manifests"].pop()
            self.raw = json.dumps(self.index).encode()
            self.digest = "sha256:" + hashlib.sha256(self.raw).hexdigest()
            with self.assertRaises(ValueError):
                self.promote()
            self.assertFalse(any(call[:2] == ("cosign", "sign") for call in self.calls))

    def test_rejects_invalid_sources(self):
        with self.subTest(phase="wrong_architecture_source_or_signature_never_creates_candidate"):
            for flag in ("bad_arch", "bad_revision", "bad_signature"):
                with self.subTest(flag=flag):
                    setattr(self, flag, True)
                    self.calls.clear()
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        self.promote()
                    self.assertFalse(any(call[0] == "podman" for call in self.calls))
                    setattr(self, flag, False)
        restart_fixture(self)
        with self.subTest(phase="missing_child_rejected_before_remote_access"):
            (self.directory / "aarch64.json").unlink()
            with self.assertRaises(ValueError):
                self.promote()
            self.assertEqual(self.calls, [])
        restart_fixture(self)
        with self.subTest(phase="malformed_indexes_rejected"):
            for document in ([], {}, {"schemaVersion": 2, "manifests": [None]},
                             {"schemaVersion": 2, "manifests": [{"platform": []}]}):
                with self.subTest(document=document), self.assertRaises(ValueError):
                    promotion.verify_index(document, self.children)
        restart_fixture(self)
        with self.subTest(phase="foreign_revision_descriptor_rejected"):
            with self.assertRaises(ValueError):
                promotion.load_children(self.directory, "b" * 40)


if __name__ == "__main__":
    unittest.main()
