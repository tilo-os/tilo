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
        self.document = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json", "manifests": [
            {"digest": digest, "platform": {"os": "linux", "architecture": promoted.promotion.ARCHITECTURES[arch]}}
            for arch, digest in self.children.items()]}
        self.raw = json.dumps(self.document, separators=(",", ":")).encode()
        self.index = promoted.REPOSITORY + "@sha256:" + hashlib.sha256(self.raw).hexdigest()
        self.calls = []

    def fetch(self):
        return self.raw

    def command(self, *args):
        self.calls.append(args)
        if args[:5] == ("docker", "buildx", "imagetools", "inspect", "--raw"):
            return self.raw
        if args[:2] == ("cosign", "verify"):
            return b"verified"
        raise AssertionError(args)

    def test_only_an_exactly_signed_index_with_both_children_becomes_the_matrix(self):
        with self.subTest(phase='signed index and both signed children become exact matrix'):
            self.doCleanups()
            self.setUp()
            matrix = promoted.resolve(self.command, self.fetch)
            self.assertEqual({item["arch"]: item["image"] for item in matrix["include"]},
                             {arch: promoted.REPOSITORY + "@" + digest for arch, digest in self.children.items()})
            verified = [call[-1] for call in self.calls if call[:2] == ("cosign", "verify")]
            self.assertEqual(set(verified), {self.index, *(promoted.REPOSITORY + "@" + value for value in self.children.values())})
            self.assertFalse(any(":" + tag in image for image in verified for tag in ("latest", "main")))
        with self.subTest(phase='digest mismatch fails before child trust'):
            self.doCleanups()
            self.setUp()
            def command(*args):
                if args[:5] == ("docker", "buildx", "imagetools", "inspect", "--raw"):
                    return self.raw + b" "
                return self.command(*args)
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                promoted.resolve(command, self.fetch)
        with self.subTest(phase='missing extra duplicate or wrong platform fails'):
            self.doCleanups()
            self.setUp()
            mutations = [
                self.document["manifests"][:1],
                self.document["manifests"] + [{"digest": "sha256:" + "3" * 64, "platform": {"os": "linux", "architecture": "s390x"}}],
                [self.document["manifests"][0], self.document["manifests"][0]],
                [{**self.document["manifests"][0], "platform": {"os": "windows", "architecture": "amd64"}}, self.document["manifests"][1]],
            ]
            for manifests in mutations:
                with self.subTest(manifests=manifests):
                    self.document = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json", "manifests": manifests}
                    self.raw = json.dumps(self.document, separators=(",", ":")).encode()
                    self.index = promoted.REPOSITORY + "@sha256:" + hashlib.sha256(self.raw).hexdigest()
                    with self.assertRaises(ValueError):
                        promoted.resolve(self.command, self.fetch)
        with self.subTest(phase='signature failure is fatal'):
            self.doCleanups()
            self.setUp()
            def command(*args):
                if args[:2] == ("cosign", "verify"):
                    raise subprocess.CalledProcessError(1, args)
                return self.command(*args)
            with self.assertRaises(subprocess.CalledProcessError):
                promoted.resolve(command, self.fetch)


if __name__ == "__main__":
    unittest.main()
