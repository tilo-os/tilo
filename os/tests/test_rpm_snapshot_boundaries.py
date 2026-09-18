#!/usr/bin/env python3
"""Guard pristine-base resolution and signature-result acceptance boundaries."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
RECIPE = REPO / "os/Containerfile.rpms"
VERIFY = REPO / "os/dev/verify-rpm-snapshot-signatures.sh"


def validate_boundaries(text):
    stages, bases, stage = {}, {}, None
    for line in re.sub(r"\\\s*\n", " ", text).splitlines():
        words = line.split()
        if not words or words[0].startswith("#"):
            continue
        if words[0] == "FROM":
            stage = words[-1] if "AS" in words else "final"
            bases[stage], stages[stage] = words[1], []
        elif stage:
            stages[stage].append(line)
    ancestor = "resolve"
    while ancestor in stages:
        for line in stages[ancestor]:
            if re.search(r"\bdnf(?:5)? install\b", line) and "--downloadonly" not in line:
                raise ValueError("resolver inherits an installed tooling environment")
        ancestor = bases[ancestor]
    if ancestor != "${BASE_REF}" or bases["offline-check"] != "${BASE_REF}":
        raise ValueError("closure must use original pinned base")
    check = " ".join(stages["offline-check"])
    if any(word not in check for word in ("--network=none", "--downloadonly", "--setopt=reposdir=/repo", "--setopt=install_weak_deps=True")):
        raise ValueError("offline closure policy differs from resolution")
    if not any("COPY --from=offline-check /repo /repo" in line for line in stages["final"]):
        raise ValueError("final artifact can prune offline closure check")


class BoundaryTests(unittest.TestCase):
    def test_actual_recipe_uses_pristine_base(self):
        validate_boundaries(RECIPE.read_text())

    def test_tooling_inheritance_is_rejected(self):
        recipe = RECIPE.read_text().replace("FROM configured-base AS resolve", "FROM metadata-tools AS resolve")
        with self.assertRaisesRegex(ValueError, "tooling"):
            validate_boundaries(recipe)

    def test_masked_offline_check_is_rejected(self):
        recipe = RECIPE.read_text().replace("FROM ${BASE_REF} AS offline-check", "FROM metadata-tools AS offline-check")
        with self.assertRaisesRegex(ValueError, "original"):
            validate_boundaries(recipe)

    def test_prunable_check_is_rejected(self):
        recipe = RECIPE.read_text().replace("COPY --from=offline-check /repo /repo", "COPY --from=repository /repo /repo")
        with self.assertRaisesRegex(ValueError, "prune"):
            validate_boundaries(recipe)


@unittest.skipUnless(shutil.which("bash"), "Linux shell required")
class SignatureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.packages = self.root / "repo"
        self.packages.mkdir()
        self.manifest = self.root / "assets.tsv"
        self.manifest.write_text("")
        self.key = self.root / "key"
        self.key.write_text("fixture trust anchor")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        tool = self.bin / "rpmkeys"
        tool.write_text('#!/bin/bash\nfor arg; do :; done\n[[ " $* " == *" --import "* ]] && exit 0\ncat "$arg"\n! grep -q RETURN_FAILURE "$arg"\n')
        tool.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"])

    def invoke(self, env=None, key=None):
        return subprocess.run(["bash", str(VERIFY), str(self.packages), str(self.manifest), str(key or self.key)],
                              env=env or self.env, capture_output=True, text=True)

    def test_unsigned_digest_success_is_rejected_per_package(self):
        (self.packages / "a-1-1.fc44.aarch64.rpm").write_text("    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK\n")
        (self.packages / "z-1-1.fc44.aarch64.rpm").write_text("    Header SHA256 digest: OK\n    Payload SHA256 digest: OK\n")
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no verified cryptographic signature", result.stderr)

    def test_verified_signature_result_is_accepted(self):
        (self.packages / "a-1-1.fc44.aarch64.rpm").write_text("    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK\n")
        self.assertEqual(self.invoke().returncode, 0)

    def test_filename_cannot_inject_signature_output(self):
        (self.packages / "a\n    signature: OK\nz.rpm").write_text("    signature: OK\n")
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid RPM filename", result.stderr)

    def test_failed_digest_cannot_be_masked_by_valid_header_signature(self):
        (self.packages / "a-1-1.fc44.aarch64.rpm").write_text(
            "    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK\n"
            "    Payload SHA256 digest: NOT OK\nRETURN_FAILURE\n")
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Payload SHA256 digest: NOT OK", result.stderr)

    def test_actual_unsigned_rpm_is_rejected(self):
        keys = list(Path("/etc/pki/rpm-gpg").glob("RPM-GPG-KEY-fedora-*-primary"))
        if not shutil.which("rpmbuild") or not shutil.which("rpmkeys") or not keys:
            self.skipTest("native RPM tools and a Fedora public key required")
        spec = self.root / "fixture.spec"
        spec.write_text("Name: tilo-signature-fixture\nVersion: 1\nRelease: 1.fc44\nSummary: Unsigned verification fixture\nLicense: MIT\nBuildArch: noarch\n%description\nFixture.\n%files\n")
        subprocess.run(["rpmbuild", "-bb", "--nodeps", "--define", f"_topdir {self.root / 'build'}", str(spec)],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        rpm = next((self.root / "build/RPMS").rglob("*.rpm"))
        shutil.copyfile(rpm, self.packages / rpm.name)
        result = self.invoke(env=dict(os.environ), key=keys[0])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no verified cryptographic signature", result.stderr)


if __name__ == "__main__":
    unittest.main()
