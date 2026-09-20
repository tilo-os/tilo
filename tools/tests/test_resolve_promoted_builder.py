import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("promoted", ROOT / "tools/ci/resolve_promoted_builder.py")
promoted = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(promoted)


class PromotedBuilderTests(unittest.TestCase):
    def setUp(self):
        self.children = {"x86_64": "sha256:" + "1" * 64, "aarch64": "sha256:" + "2" * 64}
        self.document = {"schemaVersion": 2, "manifests": [
            {"digest": digest, "platform": {"os": "linux", "architecture": promoted.promotion.ARCHITECTURES[arch]}}
            for arch, digest in self.children.items()]}
        self.raw = json.dumps(self.document, separators=(",", ":")).encode()
        self.index = promoted.REPOSITORY + "@sha256:" + hashlib.sha256(self.raw).hexdigest()
        self.calls = []

    def fetch(self, token):
        self.assertEqual(token, "token")
        return [{"name": "manifest.json", "sha256": self.index.rsplit(":", 1)[1], "Size": len(self.raw)}]

    def command(self, *args):
        self.calls.append(args)
        if args[:5] == ("docker", "buildx", "imagetools", "inspect", "--raw"):
            return self.raw
        if args[:2] == ("cosign", "verify"):
            return b"verified"
        raise AssertionError(args)

    def test_signed_index_and_both_signed_children_become_exact_matrix(self):
        matrix = promoted.resolve("token", self.command, self.fetch)
        self.assertEqual({item["arch"]: item["image"] for item in matrix["include"]},
                         {arch: promoted.REPOSITORY + "@" + digest for arch, digest in self.children.items()})
        verified = [call[-1] for call in self.calls if call[:2] == ("cosign", "verify")]
        self.assertEqual(set(verified), {self.index, *(promoted.REPOSITORY + "@" + value for value in self.children.values())})
        self.assertFalse(any(":" + tag in image for image in verified for tag in ("latest", "main")))

    def test_digest_mismatch_fails_before_child_trust(self):
        self.raw += b" "
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            promoted.resolve("token", self.command, self.fetch)

    def test_missing_extra_duplicate_or_wrong_platform_fails(self):
        mutations = [
            self.document["manifests"][:1],
            self.document["manifests"] + [{"digest": "sha256:" + "3" * 64, "platform": {"os": "linux", "architecture": "s390x"}}],
            [self.document["manifests"][0], self.document["manifests"][0]],
            [{**self.document["manifests"][0], "platform": {"os": "windows", "architecture": "amd64"}}, self.document["manifests"][1]],
        ]
        for manifests in mutations:
            with self.subTest(manifests=manifests):
                self.document = {"schemaVersion": 2, "manifests": manifests}
                self.raw = json.dumps(self.document, separators=(",", ":")).encode()
                self.index = promoted.REPOSITORY + "@sha256:" + hashlib.sha256(self.raw).hexdigest()
                with self.assertRaises(ValueError):
                    promoted.resolve("token", self.command, self.fetch)

    def test_signature_failure_is_fatal(self):
        def command(*args):
            if args[:2] == ("cosign", "verify"):
                raise subprocess.CalledProcessError(1, args)
            return self.command(*args)
        with self.assertRaises(subprocess.CalledProcessError):
            promoted.resolve("token", command, self.fetch)


if __name__ == "__main__":
    unittest.main()
