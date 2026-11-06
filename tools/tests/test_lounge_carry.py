"""The Lounge's release image: the gated build publish-site published for a commit.

`lounge-carry.sh` reads the registry anonymously, and the fake skopeo here records what it was
asked and serves the tags it holds, so nothing reaches a registry.
"""
from __future__ import annotations

import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PUBLISH = ROOT / "tools/ci/publish"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
ANOTHER = "fedcba9876543210fedcba9876543210fedcba98"
DIGEST = "sha256:" + "ab" * 32

# `skopeo inspect` answers the two formats the script asks for, and `skopeo copy` writes the
# archive the registry would serve: one image whose index names the digest it was asked for.
FAKE_SKOPEO = r'''#!/usr/bin/env python3
import io, json, os, sys, tarfile
log = os.environ["FAKE_LOG"]
state = json.load(open(os.environ["FAKE_SKOPEO_STATE"]))
with open(log, "a") as f:
    f.write("skopeo " + " ".join(sys.argv[1:]) + "\n")
args = [a for a in sys.argv[1:] if not a.startswith("--")]
if args[0] == "inspect":
    if args[-1] not in state:
        sys.exit("no such image " + args[-1])
    fmt = sys.argv[sys.argv.index("--format") + 1]
    print(state[args[-1]]["digest"] if fmt == "{{.Digest}}" else state[args[-1]]["revision"])
elif args[0] == "copy":
    if args[-2] not in state:
        sys.exit("no such image " + args[-2])
    digest = state[args[-2]]["digest"]
    index = json.dumps({"schemaVersion": 2, "manifests": [{"digest": digest, "size": 1}]},
                       separators=(",", ":")).encode()
    with tarfile.open(args[-1][len("oci-archive:"):], "w") as tar:
        info = tarfile.TarInfo("index.json")
        info.size = len(index)
        tar.addfile(info, io.BytesIO(index))
else:
    sys.exit("unexpected skopeo " + " ".join(sys.argv[1:]))
'''


class LoungeCarry(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.tmp)]))
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        (bin_dir / "skopeo").write_text(FAKE_SKOPEO)
        (bin_dir / "skopeo").chmod(0o755)
        (self.tmp / "scratch").mkdir()
        self.log = self.tmp / "log"
        self.state = self.tmp / "state.json"
        self.out = self.tmp / "out"
        self.env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "TMPDIR": str(self.tmp / "scratch"),
                    "FAKE_LOG": str(self.log), "FAKE_SKOPEO_STATE": str(self.state),
                    "T_COMMIT": COMMIT}

    def serve(self, tag: str, revision: str, digest: str = DIGEST) -> None:
        state = json.loads(self.state.read_text()) if self.state.exists() else {}
        served = {"digest": digest, "revision": revision}
        state[f"docker://registry.tilo.so/tilo/lounge:{tag}"] = served
        state[f"docker://registry.tilo.so/tilo/lounge@{digest}"] = served
        self.state.write_text(json.dumps(state))

    def carry(self) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", str(PUBLISH / "lounge-carry.sh"), str(self.out)],
                              env=self.env, capture_output=True, text=True)

    def test_the_gated_build_of_the_commit_is_carried_by_digest(self):
        self.serve(f"dev-{COMMIT}", COMMIT)
        result = self.carry()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.out / "digest").read_text(), DIGEST + "\n")
        self.assertIn(f"docker://registry.tilo.so/tilo/lounge@{DIGEST}", self.log.read_text())
        self.assertNotIn("login", self.log.read_text())
        self.assertIn(f"carried {DIGEST}", result.stdout)

    def test_a_commit_publish_site_has_not_published_is_named(self):
        result = self.carry()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"no publish-site run has published {COMMIT}", result.stderr)
        self.assertFalse((self.out / "digest").exists())

    def test_an_image_built_from_another_commit_is_refused(self):
        self.serve(f"dev-{COMMIT}", ANOTHER)
        result = self.carry()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"was built from {ANOTHER}, not {COMMIT}", result.stderr)
        self.assertFalse((self.out / "digest").exists())


if __name__ == "__main__":
    unittest.main()
