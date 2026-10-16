"""The generic publishing chain: products.toml's generator and the OCI scripts' channel rules.

The scripts run against stand-ins for skopeo and cosign that record what they were asked, in
the dry-run mode (TILO_PUBLISH_DRY_RUN), so nothing reaches a registry or KMS.
"""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
PUBLISH = ROOT / "tools/ci/publish"
sys.path.insert(0, str(ROOT / "tools/ci"))
SPEC = importlib.util.spec_from_file_location("ci_products", ROOT / "tools/ci/products.py")
assert SPEC and SPEC.loader
products = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(products)

COMMIT = "0123456789abcdef0123456789abcdef01234567"
DEFAULTS = {"buildah": "buildah@sha256:1", "skopeo": "skopeo@sha256:2", "signing_env": {"K": "v"}}


def checks(*product: dict) -> dict[str, dict]:
    text = products.render({"defaults": DEFAULTS, "product": list(product)}, "f")
    return {c["name"]: c for c in tomllib.loads(text)["check"]}


class GeneratorTests(unittest.TestCase):
    def test_one_chain_publishes_development_and_release_tags(self):
        c = checks({"family": "f", "name": "svc", "repository": "r.example/svc",
                    "dev": ["dev-{commit}"], "release": ["sha-{commit}"],
                    "build": {"containerfile": "x/Containerfile", "inputs": ["x/**"]}})
        self.assertEqual(sorted(c), ["image-svc", "publish-svc", "sign-svc"])
        build = c["image-svc"]
        self.assertEqual(build["requires"], ["arch=x86_64"])
        self.assertIn("oci-build.sh $TMPDIR/empty x/Containerfile /target/publish/svc", build["run"])
        self.assertIn("x/**", build["inputs"])
        self.assertFalse(build["default"])
        self.assertNotIn("id_token", build)
        self.assertEqual(build["isolation"], "vm")
        sign = c["sign-svc"]
        self.assertEqual(sign["on"], ["stable"])
        self.assertEqual(sign["needs"], ["image-svc"])
        self.assertEqual(sign["requires"], products.CREDENTIAL_REQUIRES)
        self.assertNotIn("isolation", sign)
        publish = c["publish-svc"]
        self.assertEqual(publish["on"], ["main", "stable"])
        self.assertEqual(publish["needs"], ["image-svc"])
        self.assertEqual(publish["optional_needs"], ["sign-svc"])
        self.assertIn("oci-publish.sh any r.example/svc /target/publish/svc 'dev:dev-{commit}' "
                      "'release:sha-{commit}'", publish["run"])
        self.assertEqual(publish["id_token"], ["registry.tilo.so"])
        self.assertNotIn("isolation", publish)

    def test_development_only_products_are_never_signed(self):
        c = checks({"family": "f", "name": "tool", "repository": "r/tool", "dev": ["sha-{commit}"],
                    "anonymous": True, "build": {"run": "build {out}"}})
        self.assertEqual(sorted(c), ["image-tool", "publish-tool"])
        self.assertEqual(c["publish-tool"]["on"], ["main"])
        self.assertNotIn("optional_needs", c["publish-tool"])
        self.assertIn("oci-anonymous.sh r/tool /target/publish/tool", c["publish-tool"]["run"])

    def test_dispatched_products_publish_development_tags_only_when_asked(self):
        c = checks({"family": "f", "name": "big", "repository": "r/big", "dev": ["{commit}"],
                    "release": ["sha-{commit}"], "dispatch": True, "build": {"run": "build {out}"}})
        self.assertEqual(c["publish-big"]["on"], ["dispatch", "stable"])
        self.assertEqual(c["sign-big"]["on"], ["stable"])
        c = checks({"family": "f", "name": "dev", "repository": "r/dev", "dev": ["{commit}"],
                    "dispatch": True, "build": {"run": "build {out}"}})
        self.assertEqual(c["publish-dev"]["on"], ["dispatch"])

    def test_release_only_products_need_their_signature(self):
        c = checks({"family": "f", "name": "rel", "repository": "r/rel", "release": ["{version}"],
                    "release_on": ["tag:v*"], "tlog": True, "gates": ["rel-e2e"],
                    "build": {"run": "build {out}"}})
        self.assertEqual(c["publish-rel"]["on"], ["tag:v*"])
        self.assertEqual(c["publish-rel"]["needs"], ["image-rel", "rel-e2e", "sign-rel"])
        self.assertEqual(c["sign-rel"]["needs"], ["image-rel", "rel-e2e"])
        self.assertEqual(c["sign-rel"]["env"]["TILO_SIGN_TLOG"], "1")
        self.assertIn("/target/publish/rel/bundle.json", c["sign-rel"]["outputs"])

    def test_architectures_build_natively_and_join_in_an_index(self):
        c = checks({"family": "f", "name": "m", "repository": "r/m", "dev": ["sha-{commit}"],
                    "arches": ["x86_64", "aarch64"], "index": True,
                    "build": {"run": "build {arch} {platform} {out}", "test": ["test {out}/image.oci.tar"]}})
        self.assertEqual(c["image-m-aarch64"]["requires"], ["arch=aarch64"])
        self.assertEqual(c["image-m-aarch64"]["run"],
                         "build aarch64 linux/arm64 /target/publish/m/aarch64\n"
                         "test /target/publish/m/aarch64/image.oci.tar\n")
        self.assertEqual(c["image-m"]["needs"], ["image-m-x86_64", "image-m-aarch64"])
        self.assertEqual(c["image-m"]["requires"], ["arch=any"])
        self.assertEqual(c["publish-m"]["needs"], ["image-m"])
        self.assertIn("/target/publish/m/index 'sha-{commit}'", c["publish-m"]["run"])

    def test_per_architecture_publications_without_an_index(self):
        c = checks({"family": "f", "name": "p", "repository": "r/p", "release": ["sha-{commit}-{arch}"],
                    "arches": ["x86_64", "aarch64"], "build": {"run": "b {arch} {out}"}})
        run = c["publish-p"]["run"]
        self.assertIn("/target/publish/p/x86_64 'sha-{commit}-x86_64'", run)
        self.assertIn("/target/publish/p/aarch64 'sha-{commit}-aarch64'", run)
        self.assertEqual(c["sign-p"]["run"].count("oci-sign.sh"), 2)

    def test_products_without_tags_are_only_built(self):
        c = checks({"family": "f", "name": "b", "repository": "r/b", "build": {"run": "b {out}"}})
        self.assertEqual(sorted(c), ["image-b"])

    def test_mistakes_are_refused(self):
        with self.assertRaisesRegex(products.ProductError, "unknown keys"):
            checks({"family": "f", "name": "x", "repository": "r", "dev": ["t"], "tag": "y", "build": {"run": "b"}})
        with self.assertRaisesRegex(products.ProductError, "without tags"):
            checks({"family": "f", "name": "x", "repository": "r", "anonymous": True, "build": {"run": "b"}})
        with self.assertRaisesRegex(products.ProductError, "may not set"):
            checks({"family": "f", "name": "x", "repository": "r", "dev": ["t"], "build": {"run": "b", "on": ["main"]}})
        with self.assertRaisesRegex(products.ProductError, "index joins"):
            checks({"family": "f", "name": "x", "repository": "r", "dev": ["t"], "index": True, "build": {"run": "b"}})

    def test_matrices_write_each_check_per_variant(self):
        text = products.render({"defaults": DEFAULTS, "matrix": [{
            "family": "f", "about": "per arch",
            "each": [{"arch": "x86_64", "node": "arch=x86_64"}, {"arch": "aarch64", "node": "arch=arm64"}],
            "fields": {"image": "img", "default": False},
            "check": [
                {"name": "build-{arch}", "run": "b {arch} {commit}", "requires": ["{node}"]},
                {"name": "wine-{arch}", "run": "w", "needs": ["build-{arch}"], "only": ["x86_64"]},
                {"name": "pack", "run": "p", "needs": ["build-x86_64", "build-aarch64"], "default": True},
            ]}]}, "f")
        self.assertTrue(text.startswith("# per arch\n[[check]]"))
        c = {x["name"]: x for x in tomllib.loads(text)["check"]}
        self.assertEqual(list(c), ["build-x86_64", "wine-x86_64", "build-aarch64", "pack"])
        self.assertEqual(c["build-aarch64"]["requires"], ["arch=arm64"])
        self.assertEqual(c["build-aarch64"]["run"], "b aarch64 {commit}")
        self.assertEqual(c["build-aarch64"]["image"], "img")
        self.assertFalse(c["build-aarch64"]["default"])
        self.assertTrue(c["pack"]["default"])

    def test_the_manifest_renders(self):
        manifest = products.load()
        names = set()
        for family in products.families(manifest):
            for c in tomllib.loads(products.render(manifest, family))["check"]:
                self.assertNotIn(c["name"], names)
                names.add(c["name"])
                if "id_token" in c:
                    self.assertEqual(c["requires"], products.CREDENTIAL_REQUIRES, c["name"])


def publish_scripts(script: str, seen: set[str]) -> set[str]:
    """The tools/ci/publish scripts SCRIPT runs or sources, itself included."""
    if script in seen:
        return seen
    seen.add(script)
    text = (ROOT / script).read_text(encoding="utf-8")
    for name in re.findall(r'tools/ci/publish/([\w.-]+\.(?:sh|py))', text):
        publish_scripts(f"tools/ci/publish/{name}", seen)
    for name in re.findall(r'\$\(dirname -- "\$\{BASH_SOURCE\[0\]\}"\)/([\w.-]+\.(?:sh|py))', text):
        publish_scripts(f"tools/ci/publish/{name}", seen)
    return seen


class InputsTests(unittest.TestCase):
    def test_checks_read_the_publishing_scripts_they_run(self):
        """A change to a publishing script reruns exactly the checks that run it."""
        for c in tomllib.loads((ROOT / "t.toml").read_text(encoding="utf-8"))["check"]:
            inputs = c.get("inputs", [])
            if "tools/ci/publish/**" in inputs:
                continue
            seen: set[str] = set()
            for name in re.findall(r'tools/ci/publish/[\w.-]+\.(?:sh|py)', c.get("run", "")):
                publish_scripts(name, seen)
            with self.subTest(check=c["name"]):
                self.assertEqual(sorted(seen - set(inputs)), [])


FAKE_SKOPEO = r'''#!/usr/bin/env python3
import json, os, sys, tarfile
log = os.environ["FAKE_LOG"]
with open(log, "a") as f:
    f.write("skopeo " + " ".join(sys.argv[1:]) + "\n")
state_path = os.environ["FAKE_STATE"]
state = json.load(open(state_path)) if os.path.exists(state_path) else {}
args = [a for a in sys.argv[1:] if not a.startswith("--")]
if args[0] == "copy":
    src, dst = args[-2], args[-1]
    state[dst] = src[len("oci-archive:"):] if src.startswith("oci-archive:") else state[src]
    json.dump(state, open(state_path, "w"))
elif args[0] == "inspect":
    archive = state[args[-1]]
    with tarfile.open(archive) as tar:
        digest = json.load(tar.extractfile("index.json"))["manifests"][0]["digest"]
        sys.stdout.buffer.write(tar.extractfile("blobs/sha256/" + digest[7:]).read())
'''
FAKE_COSIGN = '''#!/bin/sh
echo "cosign $*" >> "$FAKE_LOG"
case $1 in
  generate-key-pair) : > cosign.key; echo pub > cosign.pub ;;
  sign-blob) prev=; for a; do [ "$prev" = --output-signature ] && echo sig > "$a"; prev=$a; done ;;
esac
'''


class ScriptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.tmp)]))
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("skopeo", FAKE_SKOPEO), ("cosign", FAKE_COSIGN)):
            (bin_dir / name).write_text(body)
            (bin_dir / name).chmod(0o755)
        self.out = self.tmp / "out"
        self.out.mkdir()
        self.digest = self.archive(self.out / "image.oci.tar")
        (self.out / "digest").write_text(self.digest + "\n")
        self.log = self.tmp / "log"
        self.env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "TMPDIR": str(self.tmp / "scratch"),
                    "FAKE_LOG": str(self.log), "FAKE_STATE": str(self.tmp / "state.json"),
                    "TILO_PUBLISH_DRY_RUN": str(self.tmp / "registry"), "T_COMMIT": COMMIT}
        (self.tmp / "scratch").mkdir()

    @staticmethod
    def archive(path: Path) -> str:
        blobs = {}

        def blob(data: bytes) -> str:
            digest = "sha256:" + hashlib.sha256(data).hexdigest()
            blobs[digest] = data
            return digest

        config = blob(b'{"architecture":"amd64","os":"linux"}')
        manifest = json.dumps({"schemaVersion": 2, "config": {"digest": config, "size": 1}, "layers": []},
                              separators=(",", ":")).encode()
        digest = blob(manifest)
        index = json.dumps({"schemaVersion": 2, "manifests": [{"digest": digest, "size": len(manifest)}]},
                           separators=(",", ":")).encode()
        with tarfile.open(path, "w") as tar:
            for name, data in [("index.json", index)] + [(f"blobs/sha256/{d[7:]}", b) for d, b in blobs.items()]:
                info = tarfile.TarInfo(name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        return digest

    def run_script(self, script: str, *args: str, ref: str) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", str(PUBLISH / script), *args], env={**self.env, "T_REF": ref},
                              capture_output=True, text=True)

    def publish(self, *tags: str, ref: str, channel: str = "any") -> subprocess.CompletedProcess:
        return self.run_script("oci-publish.sh", channel, "r.example/img", str(self.out), *tags, ref=ref)

    def signed(self) -> None:
        """What oci-sign.sh leaves: the simple-signing payload for this image, and a signature."""
        payload = ('{"critical":{"identity":{"docker-reference":"r.example/img"},"image":'
                   '{"docker-manifest-digest":"' + self.digest + '"},'
                   '"type":"cosign container image signature"},"optional":null}')
        (self.out / "payload.json").write_text(payload)
        (self.out / "signature").write_text("sig")

    def receipt(self) -> dict:
        return json.loads((self.out / "receipt.json").read_text())

    def test_main_pushes_only_its_development_tags_unsigned(self):
        result = self.publish("dev:dev-{commit}", "release:sha-{commit}", ref="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = self.receipt()
        self.assertEqual(receipt["tags"], [f"dev-{COMMIT}"])
        self.assertEqual(receipt["digest"], self.digest)
        self.assertEqual(receipt["reference"], "main")
        self.assertFalse(receipt["signed"])
        self.assertNotIn("login", self.log.read_text())

    def test_a_release_needs_its_signature(self):
        result = self.publish("dev:dev-{commit}", "release:sha-{commit}", ref="stable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signed only", result.stderr)
        self.assertFalse((self.out / "receipt.json").exists())

    def test_stable_pushes_the_signed_release_tags(self):
        self.signed()
        result = self.publish("dev:dev-{commit}", "release:sha-{commit}", "release:latest-stable", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], [f"sha-{COMMIT}", "latest-stable"])
        self.assertTrue(self.receipt()["signed"])
        self.assertIn("cosign verify-blob", self.log.read_text())

    def test_a_signature_over_another_image_is_refused(self):
        (self.out / "payload.json").write_text('{"critical":{"identity":{"docker-reference":"r.example/other"}}}')
        (self.out / "signature").write_text("sig")
        result = self.publish("release:sha-{commit}", ref="stable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not over this repository", result.stderr)

    def test_channels_must_match_the_run(self):
        result = self.publish("sha-{commit}", ref="stable", channel="dev")
        self.assertIn("publishes to dev", result.stderr)
        result = self.publish("dev:sha-{commit}", ref="main", channel="dev")
        self.assertIn("name no channel", result.stderr)
        result = self.publish("sha-{commit}", ref="main")
        self.assertIn("names its channel", result.stderr)
        result = self.publish("release:sha-{commit}", ref="main")
        self.assertIn("no tags for the dev channel", result.stderr)
        result = self.publish("dev:x", ref="feature")
        self.assertIn("no channel for feature", result.stderr)

    def test_version_tags_come_from_release_tags(self):
        self.signed()
        result = self.publish("release:{version}", ref="tag:v1.2.3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], ["1.2.3"])

    def test_an_archive_that_is_not_the_build_is_refused(self):
        (self.out / "digest").write_text("sha256:" + "0" * 64 + "\n")
        result = self.publish("dev:x", ref="main")
        self.assertIn("not the build", result.stderr)
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="stable")
        self.assertIn("the build recorded", result.stderr)

    def test_signing_is_for_releases_only(self):
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="main")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("only releases are signed", result.stderr)

    def test_the_dry_run_signs_with_a_throwaway_key(self):
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text()
        self.assertIn("cosign generate-key-pair", log)
        self.assertNotIn("awskms", log)
        self.assertEqual(json.loads((self.out / "payload.json").read_text())["critical"]["image"],
                         {"docker-manifest-digest": self.digest})


if __name__ == "__main__":
    unittest.main()
