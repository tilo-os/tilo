#!/usr/bin/env python3
"""Exercise the production board release plan and promotion with inert tools."""
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

REPO = Path(__file__).resolve().parents[2]
WORKFLOW = (REPO / ".forgejo/workflows/build-image.yml").read_text()
PROFILE = "orangepi-3b-v2-1"
SOURCE = "a" * 40


def step_script(name):
    step = WORKFLOW.split(f"- name: {name}\n", 1)[1]
    block = step.split("        run: |\n", 1)[1]
    lines = []
    for line in block.splitlines():
        if line and not line.startswith("          "):
            break
        lines.append(line)
    return textwrap.dedent("\n".join(lines))


def attestation(predicate):
    encoded = base64.b64encode(json.dumps({"predicate": predicate}).encode()).decode()
    return json.dumps([{"payload": encoded}])


class BoardRelease(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.raw = b'{"schemaVersion":2,"board":"fixture"}\n'
        self.digest = "sha256:" + hashlib.sha256(self.raw).hexdigest()
        profile_dir = self.root / "os/hardware/profiles"
        profile_dir.mkdir(parents=True)
        definition = (REPO / f"os/hardware/profiles/{PROFILE}.json").read_bytes()
        (profile_dir / f"{PROFILE}.json").write_bytes(definition)
        self.definition = hashlib.sha256(definition).hexdigest()
        self.env = os.environ | {
            "HARDWARE_PROFILE": PROFILE, "GITHUB_SHA": SOURCE,
            "GITHUB_REF": "refs/heads/main", "GITHUB_EVENT_NAME": "workflow_dispatch",
            "REQUESTED_RELEASE": "true", "FIRMWARE_CANDIDATE": "",
            "FIRMWARE_MANIFEST_SHA256": "", "FEX_PAYLOAD_CANDIDATE": "",
            "WINDOWS_GUEST_CANDIDATE": "", "GITHUB_OUTPUT": str(self.root / "outputs"),
            "PUBLICATION_CONFIGURED": "false", "PUBLISHED_REVISION": "",
            "INTEGRATION_REVISION": "", "LENOVO_FIRMWARE_REF": "",
        }
        self.provenance = {"buildDefinition": {
            "internalParameters": {"expectedReleaseDigest": self.digest},
            "externalParameters": {"generation": 1, "hardwareProfile": PROFILE,
                                   "architecture": "aarch64", "firmwareManifestSha256": "",
                                   "hardwareDefinitionSha256": self.definition},
            "resolvedDependencies": [{"uri": f"git+ssh://code.tilo.so/tilo/tilo@{SOURCE}"}],
        }}

    def run_step(self, name):
        return subprocess.run(["bash", "-c", step_script(name)], cwd=self.root,
                              env=self.env, capture_output=True, text=True)

    def test_explicit_arm_only_plan(self):
        result = self.run_step("Select validation or native publication lanes")
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = (self.root / "outputs").read_text()
        self.assertIn("publish=true", outputs)
        self.assertIn('"arch":"aarch64","runner":"runner-release-arm64"', outputs)
        self.assertNotIn('"arch":"x86_64"', outputs)
        self.assertIn("boards=[]", outputs)

    def test_plan_refuses_unapproved_dispatch_and_firmware(self):
        for override in [{"REQUESTED_RELEASE": "false"}, {"GITHUB_REF": "refs/heads/task"},
                         {"GITHUB_EVENT_NAME": "workflow_call"},
                         {"FIRMWARE_CANDIDATE": "code.tilo.so/tilo/firmware@sha256:" + "b" * 64}]:
            with self.subTest(override=override):
                saved = self.env.copy()
                self.env.update(override)
                self.assertNotEqual(self.run_step("Select validation or native publication lanes").returncode, 0)
                self.env = saved

    def test_board_does_not_allocate_or_advance_generic_rollback_series(self):
        self.env["GITHUB_ENV"] = str(self.root / "generation-env")
        # There is no next-generation resolver in this fixture: calling it
        # would fail, so a passing board path cannot consume the generic series.
        result = self.run_step("Resolve the image generation from the published series")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "generation-env").read_text(), "TILO_GENERATION=1\n")

    def prepare_promotion(self):
        child = self.root / "release-child"
        child.mkdir()
        (child / "aarch64.json").write_text(json.dumps({"architecture": "aarch64",
            "sourceRevision": SOURCE, "generation": 1, "digest": self.digest}))
        (self.root / "manifest").write_bytes(self.raw)
        provenance_env = self.env | {
            "RELEASE_DIGEST": self.digest, "TILO_GENERATION": "1", "SOURCE_DATE_EPOCH": "1",
            "BASE_REF": "base@sha256:" + "b" * 64, "RPMS_REF": "rpms@sha256:" + "c" * 64,
            "KERNEL_RPMS_REF": "kernel@sha256:" + "d" * 64,
            "TILO_HARDWARE_PROFILE": PROFILE, "TILO_RELEASE_ARCHITECTURE": "aarch64",
        }
        subprocess.run([str(REPO / "os/dev/release-provenance.sh"), str(self.root / "predicate")],
                       env=provenance_env, check=True, capture_output=True)
        self.provenance = json.loads((self.root / "predicate").read_text())
        self.assertEqual(self.provenance["buildDefinition"]["externalParameters"]
                         ["hardwareDefinitionSha256"], self.definition)
        (self.root / "provenance").write_text(attestation(self.provenance))
        (self.root / "freshness").write_text(attestation({"schema": "https://tilo.sh/security/release-freshness/v1",
            "repository": "code.tilo.so/tilo/tilo", "digest": self.digest,
            "sourceRevision": SOURCE, "generation": 1, "issuedAt": 1, "expiresAt": 1000}))
        # These network-facing verifiers are inert here; the actual production
        # jq checks run unchanged, and the test proves their refusal prevents copy.
        dev = self.root / "os/dev"
        dev.mkdir()
        (dev / "require-private-oci-owner.py").write_text(
            "from pathlib import Path\nPath('private-owner-checked').touch()\n"
            "raise SystemExit('public owner requires Lenovo approval')\n")
        helper = dev / "verify-release-sbom.sh"
        helper.write_text('#!/bin/sh\nexit "${SBOM_RESULT:-0}"\n')
        helper.chmod(0o755)
        binaries = self.root / "bin"
        binaries.mkdir()
        stubs = {
            "date": '#!/bin/sh\necho 100\n',
            "cosign": '#!/bin/sh\n[ "${SIGNATURE_RESULT:-0}" = 0 ] || exit 1\n'
                'case "$*" in *release-freshness*) cat freshness ;; *slsaprovenance*) cat provenance ;; esac\n',
            "skopeo": '#!/bin/sh\ncase "$*" in\n'
                '  *"inspect --raw"*)\n'
                '    case "$*" in *"@sha256:"*) cat manifest ;; *)\n'
                '      if [ -e copied ] || [ "${TAG_STATE:-missing}" = same ]; then cat manifest;\n'
                '      elif [ "${TAG_STATE:-missing}" = conflict ]; then echo conflicting;\n'
                '      elif [ "${TAG_STATE:-missing}" = denied ]; then echo unauthorized >&2; exit 1;\n'
                '      else echo "manifest unknown" >&2; exit 1; fi ;; esac ;;\n'
                '  *inspect*) printf \'{"Architecture":"%s"}\\n\' "${IMAGE_ARCH:-arm64}" ;;\n'
                '  *copy*) printf "%s\\n" "$*" > copied ;;\n'
                '  *) exit 1 ;;\nesac\n',
        }
        for name, script in stubs.items():
            p = binaries / name
            p.write_text(script)
            p.chmod(0o755)
        self.env["PATH"] = str(binaries) + os.pathsep + self.env["PATH"]

    def test_promotion_pins_only_board_source(self):
        self.prepare_promotion()
        result = self.run_step("Verify and publish immutable board ARM64 image")
        self.assertEqual(result.returncode, 0, result.stderr)
        copy = (self.root / "copied").read_text()
        self.assertIn(f"@{self.digest}", copy)
        self.assertIn(f":{PROFILE}-{SOURCE}", copy)
        self.assertNotIn(":latest", copy)
        self.assertNotIn(":gen", copy)

    def test_verified_lenovo_successor_publishes_its_own_dense_namespace(self):
        self.prepare_promotion()
        profile = "lenovo-83ed"
        self.env["HARDWARE_PROFILE"] = profile
        self.env["FIRMWARE_CANDIDATE"] = "code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:" + "d" * 64
        self.env["FIRMWARE_MANIFEST_SHA256"] = "e" * 64
        definition = (REPO / f"os/hardware/profiles/{profile}.json").read_bytes()
        (self.root / f"os/hardware/profiles/{profile}.json").write_bytes(definition)
        # The existing private-owner gate is admitted for this fixture only.
        (self.root / "os/dev/require-private-oci-owner.py").write_text("pass\n")
        child = self.root / "release-child/aarch64.json"
        data = json.loads(child.read_text())
        data["generation"] = 2
        child.write_text(json.dumps(data))
        external = self.provenance["buildDefinition"]["externalParameters"]
        external.update(generation=2, hardwareProfile=profile,
                        hardwareDefinitionSha256=hashlib.sha256(definition).hexdigest(),
                        firmwareManifestSha256=self.env["FIRMWARE_MANIFEST_SHA256"])
        self.provenance["buildDefinition"]["resolvedDependencies"].append(
            {"uri": self.env["FIRMWARE_CANDIDATE"]})
        (self.root / "provenance").write_text(attestation(self.provenance))
        (self.root / "freshness").write_text(attestation({"schema": "https://tilo.sh/security/release-freshness/v1",
            "repository": "code.tilo.so/tilo/tilo", "digest": self.digest,
            "sourceRevision": SOURCE, "generation": 2, "issuedAt": 1, "expiresAt": 1000}))
        # Record every real promotion copy rather than only its last command.
        tool = self.root / "bin/skopeo"
        tool.write_text(tool.read_text().replace(' > copied ;;', ' >> copied ;;').replace(
            '[ -e copied ] ||', '{ [ -e copied ] && grep -Fq -- "$last" copied; } ||').replace(
            '#!/bin/sh\n', '#!/bin/sh\nfor arg do last=$arg; done\n'))
        result = self.run_step("Verify and publish immutable board ARM64 image")
        self.assertEqual(result.returncode, 0, result.stderr)
        copies = (self.root / "copied").read_text().splitlines()
        self.assertEqual(len(copies), 3)
        self.assertIn(f":{profile}-{SOURCE}", copies[0])
        self.assertIn(":lenovo-83ed-gen2", copies[1])
        self.assertIn(":lenovo-83ed-latest", copies[2])
        self.assertTrue(all(f"@{self.digest}" in row for row in copies))
        self.assertFalse(any("/tilo:gen2" in row or "/tilo:latest" in row for row in copies))

    def test_public_orangepi_promotion_does_not_use_lenovo_firmware_override(self):
        self.prepare_promotion()
        result = self.run_step("Verify and publish immutable board ARM64 image")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "private-owner-checked").exists())
        self.assertTrue((self.root / "copied").exists())

    def test_lenovo_promotion_still_requires_private_owner_or_explicit_approval(self):
        self.prepare_promotion()
        self.env["HARDWARE_PROFILE"] = "lenovo-83ed"
        result = self.run_step("Verify and publish immutable board ARM64 image")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / "private-owner-checked").exists())
        self.assertFalse((self.root / "copied").exists())

    def test_unknown_profile_cannot_use_public_board_publication(self):
        self.prepare_promotion()
        self.env["HARDWARE_PROFILE"] = "unknown-board"
        self.assertNotEqual(self.run_step("Verify and publish immutable board ARM64 image").returncode, 0)
        self.assertFalse((self.root / "copied").exists())

    def test_same_immutable_tag_is_idempotent(self):
        self.prepare_promotion()
        self.env["TAG_STATE"] = "same"
        result = self.run_step("Verify and publish immutable board ARM64 image")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "copied").exists())

    def test_expired_freshness_prevents_copy(self):
        self.prepare_promotion()
        (self.root / "freshness").write_text(attestation({"schema": "https://tilo.sh/security/release-freshness/v1",
            "repository": "code.tilo.so/tilo/tilo", "digest": self.digest,
            "sourceRevision": SOURCE, "generation": 1, "issuedAt": 1, "expiresAt": 99}))
        self.assertNotEqual(self.run_step("Verify and publish immutable board ARM64 image").returncode, 0)
        self.assertFalse((self.root / "copied").exists())

    def test_promotion_refuses_signature_sbom_architecture_and_tag_faults(self):
        self.prepare_promotion()
        for override in [{"SIGNATURE_RESULT": "1"}, {"SBOM_RESULT": "1"},
                         {"IMAGE_ARCH": "amd64"}, {"TAG_STATE": "conflict"},
                         {"TAG_STATE": "denied"}]:
            with self.subTest(override=override):
                saved = self.env.copy()
                self.env.update(override)
                self.assertNotEqual(self.run_step("Verify and publish immutable board ARM64 image").returncode, 0)
                self.assertFalse((self.root / "copied").exists())
                self.env = saved

    def test_promotion_refuses_profile_source_definition_and_generation_mismatch(self):
        self.prepare_promotion()
        ext = self.provenance["buildDefinition"]["externalParameters"]
        for key, value in [("hardwareProfile", "generic"), ("hardwareDefinitionSha256", "b" * 64),
                           ("generation", 2), ("architecture", "x86_64")]:
            with self.subTest(key=key):
                previous = ext[key]
                ext[key] = value
                (self.root / "provenance").write_text(attestation(self.provenance))
                self.assertNotEqual(self.run_step("Verify and publish immutable board ARM64 image").returncode, 0)
                self.assertFalse((self.root / "copied").exists())
                ext[key] = previous
        self.provenance["buildDefinition"]["resolvedDependencies"] = []
        (self.root / "provenance").write_text(attestation(self.provenance))
        self.assertNotEqual(self.run_step("Verify and publish immutable board ARM64 image").returncode, 0)
        self.assertFalse((self.root / "copied").exists())


if __name__ == "__main__":
    unittest.main()
