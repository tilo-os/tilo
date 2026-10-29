"""The generic publishing chain: the OCI scripts' channel rules.

The scripts run against stand-ins for skopeo and cosign that record what they were asked, in
the dry-run mode (TILO_PUBLISH_DRY_RUN), so nothing reaches a registry or KMS.
"""
from __future__ import annotations

import base64
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
COMMIT = "0123456789abcdef0123456789abcdef01234567"


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
    if src in state:
        state[dst] = state[src]
    elif src.startswith("oci:") and os.path.isfile(src[4:] + "/index.json"):
        # A layout is read where it is: what it holds is kept, as a registry would.
        os.makedirs(state_path + ".held", exist_ok=True)
        held = state_path + ".held/" + os.urandom(8).hex()
        with tarfile.open(held, "w") as tar:
            for name in ["index.json"] + ["blobs/sha256/" + b for b in os.listdir(src[4:] + "/blobs/sha256")]:
                tar.add(src[4:] + "/" + name, name)
        state[dst] = held
    else:
        sys.exit("skopeo: cannot copy from " + src)
    json.dump(state, open(state_path, "w"))
elif args[0] == "inspect":
    if args[-1] not in state:
        sys.exit("skopeo: no such image " + args[-1])
    archive = state[args[-1]]
    with tarfile.open(archive) as tar:
        digest = json.load(tar.extractfile("index.json"))["manifests"][0]["digest"]
        manifest = tar.extractfile("blobs/sha256/" + digest[7:]).read()
        if "--format" in sys.argv:
            # Only the label lookup the scripts make: {{index .Labels "NAME"}}.
            name = sys.argv[sys.argv.index("--format") + 1].split('"')[1]
            config = json.load(tar.extractfile("blobs/sha256/" + json.loads(manifest)["config"]["digest"][7:]))
            print(config.get("config", {}).get("Labels", {}).get(name, "<no value>"))
        else:
            sys.stdout.buffer.write(manifest)
'''
FAKE_COSIGN = '''#!/bin/sh
echo "cosign $*" >> "$FAKE_LOG"
case $1 in
  generate-key-pair) : > cosign.key; echo pub > cosign.pub ;;
  sign-blob) prev=; for a; do [ "$prev" = --output-signature ] && echo sig > "$a"; prev=$a; done ;;
  attest-blob) python3 - "$@" <<'PY'
import base64, json, sys
args = sys.argv[1:]
value = lambda flag: args[args.index(flag) + 1]
kind = value("--type")
statement = {"_type": "https://in-toto.io/Statement/v0.1",
             "predicateType": "https://slsa.dev/provenance/v1" if kind == "slsaprovenance1" else kind,
             "subject": [{"name": args[-1].rsplit("/", 1)[-1], "digest": {"sha256": value("--hash")}}],
             "predicate": json.load(open(value("--predicate")))}
payload = base64.b64encode(json.dumps(statement, separators=(",", ":")).encode()).decode()
print(json.dumps({"payloadType": "application/vnd.in-toto+json", "payload": payload,
                  "signatures": [{"keyid": "", "sig": "c2ln"}]}, separators=(",", ":")))
PY
  ;;
esac
'''

# The forge's compare API: what FAKE_COMPARE holds, as `curl -fsS` prints it.
FAKE_CURL = """#!/bin/sh
echo "curl $*" >> "$FAKE_LOG"
printf '%s' "$FAKE_COMPARE"
"""


class ScriptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.tmp)]))
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("skopeo", FAKE_SKOPEO), ("cosign", FAKE_COSIGN), ("curl", FAKE_CURL)):
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
    def archive(path: Path, config: bytes = b'{"architecture":"amd64","os":"linux"}') -> str:
        blobs = {}

        def blob(data: bytes) -> str:
            digest = "sha256:" + hashlib.sha256(data).hexdigest()
            blobs[digest] = data
            return digest

        config = blob(config)
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

    def test_an_archive_is_unpacked_once_and_pushed_as_a_layout(self):
        result = self.publish("dev:dev-{commit}", ref="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        pushes = [line.split()[-2] for line in self.log.read_text().splitlines() if line.startswith("skopeo copy")]
        self.assertEqual(len(pushes), 1)
        self.assertTrue(pushes[0].startswith("oci:"), pushes)
        self.assertFalse(Path(pushes[0][4:]).exists(), "the unpacked layout outlives the push")
        self.assertEqual(self.receipt()["digest"], self.digest)

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

    def test_a_derived_release_publishes_its_own_archive_with_the_receipt_in_out(self):
        release = self.tmp / "release"
        release.mkdir()
        derived = self.archive(release / "image.oci.tar", b'{"architecture":"amd64","os":"linux","derived":1}')
        self.assertNotEqual(derived, self.digest)
        (release / "digest").write_text(derived + "\n")
        (release / "receipt-extra.json").write_text('{"replaces":"r.example/img@sha256:1","carriedVersion":"abc"}\n')
        self.env["TILO_RELEASE_OUT"] = str(release)
        # Main publishes the build, unsigned, whatever the release directory holds.
        result = self.publish("dev:dev-{commit}", "release:sha-{commit}", ref="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["digest"], self.digest)
        # Stable publishes the derived image with its own signature, not the build's, and records
        # what the step added.
        self.signed()
        result = self.publish("dev:dev-{commit}", "release:sha-{commit}", ref="stable")
        self.assertIn("signed only", result.stderr)
        (release / "payload.json").write_text((self.out / "payload.json").read_text().replace(self.digest, derived))
        (release / "signature").write_text("sig")
        result = self.publish("release:sha-{commit}", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = self.receipt()
        self.assertEqual((receipt["digest"], receipt["image"]), (derived, f"r.example/img@{derived}"))
        self.assertEqual((receipt["replaces"], receipt["carriedVersion"]), ("r.example/img@sha256:1", "abc"))
        self.assertFalse((release / "receipt.json").exists())
        (release / "receipt-extra.json").write_text('{"a": 1}\n{"b": 2}\n')
        result = self.publish("release:sha-{commit}", ref="stable")
        self.assertIn("one JSON object on one line", result.stderr)
        (release / "image.oci.tar").unlink()
        result = self.publish("release:sha-{commit}", ref="stable")
        self.assertIn("did not run", result.stderr)

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
        # A branch publishes only where a check publishes for branches, and only signed.
        result = self.publish("dev:x", ref="feature")
        self.assertIn("no tags for the branch channel", result.stderr)
        result = self.publish("x", ref="feature", channel="branch")
        self.assertIn("a branch build is published signed only", result.stderr)

    def test_version_tags_come_from_release_tags(self):
        self.signed()
        result = self.publish("release:{version}", ref="tag:v1.2.3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], ["1.2.3"])

    def test_final_tags_move_only_for_a_final_version(self):
        self.signed()
        result = self.publish("release:{version}", "release:final:latest", ref="tag:v1.2.3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], ["1.2.3", "latest"])
        result = self.publish("release:{version}", "release:final:latest", ref="tag:v1.2.4-rc.1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], ["1.2.4-rc.1"])

    def test_rust_tags_name_the_pinned_toolchain(self):
        self.signed()
        channel = re.search(r'^channel\s*=\s*"([^"]+)"', (ROOT / "rust-toolchain.toml").read_text(), re.M)[1]
        result = self.publish("release:{commit}", "release:{rust}", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], [COMMIT, channel])

    def provenance(self, digest: str | None = None) -> None:
        (self.out / "provenance.json").write_text(json.dumps({
            "buildDefinition": {"internalParameters": {"expectedReleaseDigest": digest or self.digest},
                                "resolvedDependencies": [{"uri": "git+ssh://code.tilo.so/tilo/tilo@{commit}"}]},
            "runDetails": {"builder": {"id": "t"}}}))

    def test_provenance_is_attested_for_the_build_and_attached(self):
        self.provenance()
        for script in ("oci-sign.sh", "oci-attest.sh"):
            result = self.run_script(script, "r.example/img", str(self.out), ref="stable")
            self.assertEqual(result.returncode, 0, result.stderr)
        envelope = json.loads((self.out / "attestation.json").read_text())
        statement = json.loads(base64.b64decode(envelope["payload"]))
        self.assertEqual(statement["subject"][0]["digest"], {"sha256": self.digest[7:]})
        self.assertIn(f"tilo@{COMMIT}", json.dumps(statement["predicate"]))
        self.assertNotIn("awskms", self.log.read_text())
        result = self.publish("release:sha-{commit}", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cosign verify-blob-attestation", self.log.read_text())

    def test_another_predicate_is_attested_beside_the_provenance(self):
        self.provenance()
        (self.out / "freshness.json").write_text(json.dumps({"digest": self.digest, "generation": 3}))
        for args in (("oci-sign.sh",), ("oci-attest.sh",),
                     ("oci-attest.sh", "freshness.json", "https://tilo.sh/security/release-freshness/v1")):
            result = self.run_script(args[0], "r.example/img", str(self.out), *args[1:], ref="stable")
            self.assertEqual(result.returncode, 0, result.stderr)
        envelope = json.loads((self.out / "freshness.attestation.json").read_text())
        statement = json.loads(base64.b64decode(envelope["payload"]))
        self.assertEqual(statement["predicateType"], "https://tilo.sh/security/release-freshness/v1")
        self.assertEqual(statement["subject"][0]["digest"], {"sha256": self.digest[7:]})
        (self.out / "generation").write_text("3\n")
        result = self.publish("release:once:gen{generation}", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], ["gen3"])
        self.assertIn("--type https://tilo.sh/security/release-freshness/v1", self.log.read_text())

    def test_development_provenance_is_attested_on_main_only(self):
        self.provenance()
        env = {"TILO_DEV_SIGNING": "1"}
        for script in ("oci-sign.sh", "oci-attest.sh"):
            result = subprocess.run(["bash", str(PUBLISH / script), "r.example/img", str(self.out)],
                                    env={**self.env, **env, "T_REF": "main"}, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.out / "attestation.json").exists())
        self.assertFalse((self.out / "attestation.bundle.json").exists())
        result = self.publish("dev:{commit}", ref="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cosign verify-blob-attestation", self.log.read_text())
        result = subprocess.run(["bash", str(PUBLISH / "oci-attest.sh"), "r.example/img", str(self.out)],
                                env={**self.env, **env, "T_REF": "stable"}, capture_output=True, text=True)
        self.assertIn("attests main's candidates only", result.stderr)

    def test_provenance_of_another_image_is_refused(self):
        self.provenance("sha256:" + "1" * 64)
        self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="stable")
        result = self.run_script("oci-attest.sh", "r.example/img", str(self.out), ref="stable")
        self.assertIn("not for " + self.digest, result.stderr)
        result = self.run_script("oci-attest.sh", "r.example/img", str(self.out), ref="main")
        self.assertIn("only releases are attested", result.stderr)

    def test_once_tags_never_move(self):
        self.signed()
        (self.out / "fingerprint").write_text("ab" * 32 + "\n")
        result = self.publish("release:once:set-{fingerprint}", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], ["set-" + "ab" * 32])
        result = self.publish("release:once:set-{fingerprint}", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("is published already", result.stdout)
        self.assertTrue(self.receipt()["existing"])
        self.assertEqual(self.receipt()["digest"], self.digest)

    def sign_for(self, out: Path, repository: str = "r.example/img") -> str:
        """What oci-sign.sh leaves in OUT for its archive."""
        digest = (out / "digest").read_text().strip()
        (out / "payload.json").write_text(
            '{"critical":{"identity":{"docker-reference":"' + repository + '"},"image":'
            '{"docker-manifest-digest":"' + digest + '"},'
            '"type":"cosign container image signature"},"optional":null}')
        (out / "signature").write_text("sig")
        return digest

    def build_into(self, out: Path, arch: str) -> None:
        out.mkdir(parents=True, exist_ok=True)
        digest = self.archive(out / "image.oci.tar", f'{{"architecture":"{arch}","os":"linux"}}'.encode())
        (out / "digest").write_text(digest + "\n")

    def test_a_kernel_rebuilt_on_the_same_commit_keeps_the_published_digest_pinned(self):
        kernel = self.tmp / "kernel"
        published = {}
        # The first stable run's x86_64 build stays where the stand-in registry reads it.
        for arch, out in (("x86_64", self.tmp / "first/x86_64"), ("aarch64", kernel / "aarch64")):
            self.build_into(out, arch)
            published[arch] = self.sign_for(out)
            result = self.run_script("oci-publish.sh", "release", "r.example/img", str(out),
                                     f"once:sha-{{commit}}-{arch}", ref="stable")
            self.assertEqual(result.returncode, 0, result.stderr)
        # A second stable run on the commit rebuilt x86_64 into another image.
        self.build_into(kernel / "x86_64", "amd64")
        rebuilt = self.sign_for(kernel / "x86_64")
        self.assertNotEqual(rebuilt, published["x86_64"])
        result = self.run_script("oci-publish.sh", "release", "r.example/img", str(kernel / "x86_64"),
                                 "once:sha-{commit}-x86_64", ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("is published already", result.stdout)
        result = self.run_script("kernel.sh", "pins", str(kernel), ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        pins = (kernel / "pins.env").read_text()
        self.assertIn(f"KERNEL_RPMS_X86_64_REF=registry.tilo.so/tilo/tilo-kernel@{published['x86_64']}\n", pins)
        self.assertIn(f"KERNEL_RPMS_AARCH64_REF=registry.tilo.so/tilo/tilo-kernel@{published['aarch64']}\n", pins)
        self.assertNotIn(rebuilt, pins)

    def env_tools(self, mode: str, *args: str) -> subprocess.CompletedProcess:
        return self.run_script("env-tools-release.sh", mode, *args, ref="stable")

    def test_the_env_tools_catalog_publishes_once_under_the_plan_s_tag(self):
        plan = self.tmp / "plan"
        plan.mkdir()
        tag = "set-" + "a" * 64 + "-" + "b" * 16
        (plan / "tag").write_text(tag + "\n")
        self.sign_for(self.out, "registry.tilo.so/tilo/env-tools")
        result = self.env_tools("release", str(plan), str(self.out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], [tag])
        self.assertTrue(self.receipt()["signed"])
        result = self.env_tools("release", str(plan), str(self.out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.receipt()["existing"])
        (plan / "tag").write_text("sha-" + COMMIT + "\n")
        result = self.env_tools("release", str(plan), str(self.out))
        self.assertIn("not a catalog tag", result.stderr)

    def test_an_env_tools_catalog_published_already_is_neither_sealed_signed_nor_pushed(self):
        plan = self.tmp / "plan"
        plan.mkdir()
        (plan / "published").write_text("set-" + "a" * 64 + "-" + "b" * 16 + "\n")
        missing = str(self.tmp / "missing")
        for mode, args in (("seal", (str(plan), missing, missing, missing)),
                           ("image", (missing, missing, str(plan))),
                           ("sign", (str(plan), missing)),
                           ("release", (str(plan), missing))):
            with self.subTest(mode=mode):
                result = self.env_tools(mode, *args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("is published already", result.stdout)
        self.assertFalse(self.log.exists())

    def test_env_tools_are_sealed_only_with_the_publisher_identity(self):
        plan = self.tmp / "plan"
        (plan / "runtime").mkdir(parents=True)
        result = self.env_tools("seal", str(plan), str(self.tmp), str(self.tmp), str(self.tmp / "sealed"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("TAPP_PUBLISHER_IDENTITY is required", result.stderr)

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

    def test_development_signing_is_for_main_and_branches_only(self):
        self.env["TILO_DEV_SIGNING"] = "1"
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="stable")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signs development candidates only", result.stderr)
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="feature")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.publish("feature-x", ref="feature", channel="branch")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.receipt()["signed"])
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.publish("{commit}", ref="main", channel="dev")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.receipt()["signed"])

    def test_the_dry_run_signs_with_a_throwaway_key(self):
        result = self.run_script("oci-sign.sh", "r.example/img", str(self.out), ref="stable")
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text()
        self.assertIn("cosign generate-key-pair", log)
        self.assertNotIn("awskms", log)
        self.assertEqual(json.loads((self.out / "payload.json").read_text())["critical"]["image"],
                         {"docker-manifest-digest": self.digest})

    def board(self, revision: str = COMMIT, board: str = "lenovo-83ed") -> subprocess.CompletedProcess:
        """os-board-publish.sh on what `os-image.sh board` and its signing leave in OUT: the image as
        an OCI layout."""
        if (self.out / "image.oci.tar").exists():
            with tarfile.open(self.out / "image.oci.tar") as tar:
                tar.extractall(self.out / "oci", filter="data")
            (self.out / "image.oci.tar").unlink()
        (self.out / "payload.json").write_text(
            '{"critical":{"identity":{"docker-reference":"registry.tilo.so/tilo/tilo-vm"},"image":'
            '{"docker-manifest-digest":"' + self.digest + '"},'
            '"type":"cosign container image signature"},"optional":null}')
        (self.out / "signature").write_text("sig")
        (self.out / "board").write_text(board + "\n")
        (self.out / "revision").write_text(revision + "\n")
        (self.out / "disk").mkdir(exist_ok=True)
        (self.out / "disk/disk.qcow2").write_bytes(b"flashable disk")
        return self.run_script("os-board-publish.sh", "lenovo-83ed", str(self.out), ref="main")

    def moving_tag_names(self, revision: str) -> None:
        """tilo-vm:lenovo-83ed-ci holds an image of REVISION."""
        held = self.tmp / "held.oci.tar"
        self.archive(held, json.dumps({"architecture": "arm64", "os": "linux", "config": {
            "Labels": {"org.opencontainers.image.revision": revision}}}).encode())
        state = json.loads((self.tmp / "state.json").read_text()) if (self.tmp / "state.json").exists() else {}
        state[f"oci:{self.tmp}/registry/registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci"] = str(held)
        (self.tmp / "state.json").write_text(json.dumps(state))

    def test_a_board_image_takes_its_commit_tag_and_the_moving_tag(self):
        result = self.board()
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = self.receipt()
        self.assertEqual(receipt["repository"], "registry.tilo.so/tilo/tilo-vm")
        self.assertEqual(receipt["tags"], [f"main-{COMMIT[:7]}-lenovo-83ed", "lenovo-83ed-ci"])
        self.assertEqual((receipt["digest"], receipt["signed"]), (self.digest, True))
        # Its flashable disk, named for the board, with the commit it is of.
        disks = self.tmp / "registry/packages/aarch64-lenovo-83ed"
        self.assertEqual((disks / "tilo-vm-lenovo-83ed.qcow2").read_bytes(), b"flashable disk")
        self.assertTrue((disks / "tilo-vm-lenovo-83ed.qcow2.sha256").read_text().endswith("  tilo-vm-lenovo-83ed.qcow2\n"))
        self.assertEqual((disks / "revision").read_text(), COMMIT + "\n")

    def test_the_board_moving_tag_moves_only_forward_along_main(self):
        older = "f" * 40
        self.moving_tag_names(older)
        # The tag names an ancestor: the forge finds no commit of it this one lacks.
        self.env["FAKE_COMPARE"] = '{"total_commits":0,"commits":[]}'
        result = self.board()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("lenovo-83ed-ci", self.receipt()["tags"])
        self.assertIn(f"/compare/{COMMIT}...{older}", self.log.read_text())
        # A newer build took it meanwhile: this one keeps only its own tag.
        self.moving_tag_names(older)
        self.env["FAKE_COMPARE"] = '{"total_commits":3,"commits":[]}'
        result = self.board()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.receipt()["tags"], [f"main-{COMMIT[:7]}-lenovo-83ed"])
        self.assertIn("not an ancestor", result.stderr)
        # Nor when the forge can't tell.
        self.env["FAKE_COMPARE"] = ""
        result = self.board()
        self.assertEqual(self.receipt()["tags"], [f"main-{COMMIT[:7]}-lenovo-83ed"])
        self.assertIn("could not be compared", result.stderr)

    def test_a_board_image_is_published_only_signed(self):
        self.board()
        (self.out / "signature").unlink()
        result = self.run_script("os-board-publish.sh", "lenovo-83ed", str(self.out), ref="main")
        self.assertIn("holds no signature", result.stderr)

    def test_a_board_publication_refuses_another_build_and_branch(self):
        result = self.board(board="orangepi-3b-v2-1")
        self.assertIn("not lenovo-83ed", result.stderr)
        result = self.board(revision="f" * 40)
        self.assertIn("the run is for", result.stderr)
        (self.out / "board").write_text("lenovo-83ed\n")
        (self.out / "revision").write_text(COMMIT + "\n")
        result = self.run_script("os-board-publish.sh", "lenovo-83ed", str(self.out), ref="stable")
        self.assertIn("from main only", result.stderr)
        self.assertFalse((self.out / "receipt.json").exists())


if __name__ == "__main__":
    unittest.main()
