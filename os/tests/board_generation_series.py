#!/usr/bin/env python3
"""Run the real board allocator against an inert signed registry boundary."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BoardSeries(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)
        self.manifest = b'{"schemaVersion":2,"board":"seed"}\n'
        self.digest = "sha256:" + hashlib.sha256(self.manifest).hexdigest()
        self.seed = "code.tilo.so/tilo/tilo@" + self.digest
        (self.path / "manifest").write_bytes(self.manifest)
        (self.path / "registry.json").write_text(json.dumps({"gen9": self.digest}))
        (self.path / "key").write_text("fixture key")
        self.predicate = {"runDetails": {"builder": {"id": "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1"},
            "byproducts": [{"name": "independent-rebuild", "uri": "urn:tilo:verification:manifest-digest-equal"}]},
            "buildDefinition": {"buildType": "https://tilo.sh/buildtypes/bootc-os/v1",
            "internalParameters": {"expectedReleaseDigest": self.digest},
            "externalParameters": {"generation": 1, "hardwareProfile": "lenovo-83ed",
                "architecture": "aarch64", "firmwareManifestSha256": "b" * 64},
            "resolvedDependencies": [
                {"uri": "git+ssh://code.tilo.so/tilo/tilo@" + "a" * 40},
                {"uri": "code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:" + "c" * 64}],
        }}
        self.write_predicate()
        bin_dir = self.path / "bin"
        bin_dir.mkdir()
        tools = {
            "skopeo": '''#!/usr/bin/env python3
import json, pathlib, sys
root=pathlib.Path.cwd(); registry=root/'registry.json'; rows=json.loads(registry.read_text())
args=sys.argv[1:]
if args[0]=='list-tags': print(json.dumps({'Tags': list(rows)}))
elif args[:2]==['inspect','--raw']:
    tag=args[-1].rsplit(':',1)[1]
    if tag not in rows: print('manifest unknown',file=sys.stderr);sys.exit(1)
    sys.stdout.buffer.write((root/'manifest').read_bytes())
elif args[0]=='copy':
    target=args[-1].rsplit(':',1)[1]; rows[target]=args[-2].rsplit('@',1)[1]
    registry.write_text(json.dumps(rows))
    with (root/'copies').open('a') as out: out.write(target+'\\n')
else: sys.exit(2)
''',
            "cosign": '''#!/usr/bin/env python3
import base64,json,os,pathlib,sys
if os.environ.get('BAD_SIGNATURE')=='1': sys.exit(1)
root=pathlib.Path.cwd()
if sys.argv[1]=='verify-attestation':
    print(json.dumps([{'payload':base64.b64encode((root/'predicate').read_bytes()).decode()}]))
else:
    digest='sha256:'+__import__('hashlib').sha256((root/'manifest').read_bytes()).hexdigest()
    print(json.dumps([{'critical':{'image':{'docker-manifest-digest':digest}}}]))
''',
        }
        for name, script in tools.items():
            tool = bin_dir / name
            tool.write_text(script)
            tool.chmod(0o755)
        self.env = os.environ | {"PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
            "TILO_GENERATION_VERIFY_KEY": str(self.path / "key"),
            "TILO_BOARD_SERIES_SEED": self.seed}

    def write_predicate(self):
        (self.path / "predicate").write_text(json.dumps({"predicate": self.predicate}))

    def run_allocator(self, *args):
        return subprocess.run([str(ROOT / "os/dev/board-generation.sh"), *args], cwd=self.path,
                              env=self.env, capture_output=True, text=True)

    def test_verified_seed_creates_dense_board_history_without_generic_mutation(self):
        result = self.run_allocator()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "2")
        self.assertFalse((self.path / "copies").exists())
        result = self.run_allocator("--retain")
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = json.loads((self.path / "registry.json").read_text())
        self.assertEqual(rows, {"gen9": self.digest, "lenovo-83ed-gen1": self.digest,
                                "lenovo-83ed-latest": self.digest})
        # Existing initialized state needs no continuing genesis input.
        self.env["TILO_BOARD_SERIES_SEED"] = ""
        self.assertEqual(self.run_allocator().stdout.strip(), "2")
        self.assertEqual(self.run_allocator("--retain").returncode, 0)
        self.assertEqual((self.path / "copies").read_text().split(),
                         ["lenovo-83ed-gen1", "lenovo-83ed-latest"])

    def test_pinned_seed_is_a_verified_historical_generation_one(self):
        self.env.pop("TILO_BOARD_SERIES_SEED")
        pinned = (ROOT / "os/publication.env").read_text().split("TILO_LENOVO_SERIES_SEED=")[1].split()[0]
        self.assertRegex(pinned, r"^code\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$")
        # The fixture registry serves another digest than the pinned one, so the provenance's
        # expected digest cannot bind it: the pinned seed is what gets verified.
        self.assertNotEqual(self.run_allocator().returncode, 0)

    def test_missing_or_mutable_seed_refuses_before_publication(self):
        for seed in ["", "code.tilo.so/tilo/tilo:lenovo-83ed-latest"]:
            with self.subTest(seed=seed):
                self.env["TILO_BOARD_SERIES_SEED"] = seed
                self.assertNotEqual(self.run_allocator("--retain").returncode, 0)
                self.assertFalse((self.path / "copies").exists())

    def test_seed_signature_refusal_is_not_a_series_reset(self):
        self.env["BAD_SIGNATURE"] = "1"
        self.assertNotEqual(self.run_allocator("--retain").returncode, 0)
        self.assertFalse((self.path / "copies").exists())

    def test_wrong_board_architecture_generation_or_digest_refuses_before_copy(self):
        external = self.predicate["buildDefinition"]["externalParameters"]
        for key, wrong in [("hardwareProfile", "generic"), ("architecture", "x86_64"),
                           ("generation", 2), ("firmwareManifestSha256", "unverified")]:
            with self.subTest(key=key):
                original = external[key]
                external[key] = wrong
                self.write_predicate()
                self.assertNotEqual(self.run_allocator("--retain").returncode, 0)
                self.assertFalse((self.path / "copies").exists())
                external[key] = original
        self.predicate["buildDefinition"]["internalParameters"]["expectedReleaseDigest"] = "sha256:" + "f" * 64
        self.write_predicate()
        self.assertNotEqual(self.run_allocator().returncode, 0)
        self.assertFalse((self.path / "copies").exists())

    def test_existing_foreign_board_provenance_cannot_allocate_next(self):
        (self.path / "registry.json").write_text(json.dumps({
            "lenovo-83ed-gen1": self.digest, "lenovo-83ed-latest": self.digest}))
        self.predicate["buildDefinition"]["externalParameters"]["hardwareProfile"] = "generic"
        self.write_predicate()
        self.assertNotEqual(self.run_allocator().returncode, 0)
        self.assertFalse((self.path / "copies").exists())


if __name__ == "__main__":
    unittest.main()
