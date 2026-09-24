#!/usr/bin/env python3
"""Guard pristine-base resolution and signature-result acceptance boundaries."""
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
RECIPE = REPO / "os/Containerfile.rpms"
IMAGE = REPO / "os/Containerfile"
VERIFY = REPO / "os/dev/verify-rpm-snapshot-signatures.sh"
DERIVE = REPO / "os/dev/derive-rpm-package-set.py"
FULL_OFFLINE_RESOLVE = ("dnf install -y --downloadonly --setopt=reposdir=/repo "
                        "--setopt=install_weak_deps=True ${PACKAGES}")
# The image's erase of base-installed packages that later stages need again.
ERASE = re.compile(r"awk '/\^\(([^)]*)\)\$/'\s*\|\s*xargs -r rpm -e --nodeps")


def joined(text):
    return re.sub(r"\\\s*\n", " ", text)


def snapshot_packages(image_text, arch):
    spec = importlib.util.spec_from_file_location("derive_rpm_package_set", DERIVE)
    derive = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(derive)
    with tempfile.TemporaryDirectory() as temp:
        image = Path(temp) / "Containerfile"
        image.write_text(image_text)
        found = set(derive.packages(image, arch))
    for source in derive.SOURCES:
        if source != "os/Containerfile":
            found |= set(derive.packages(REPO / source, arch))
    return found


def validate_erase_replay(image_text, recipe_text):
    """The snapshot must reinstall, offline, whatever the image erases and needs again."""
    image_erases = ERASE.findall(joined(image_text))
    if len(image_erases) != 1:
        raise ValueError("expected exactly one erase-and-reinstall step in os/Containerfile")
    erased = set(image_erases[0].split("|"))
    offline = joined(recipe_text).split("AS offline-check", 1)[-1].split("\nFROM ", 1)[0]
    replays = [line for line in offline.split("\nRUN ") if ERASE.search(line)]
    if len(replays) != 1:
        raise ValueError("offline check does not replay the image's erase")
    replay = replays[0]
    if set(ERASE.search(replay).group(1).split("|")) != erased:
        raise ValueError("offline replay does not match the image's erase list")
    if "--network=none" not in replay or FULL_OFFLINE_RESOLVE not in replay.split("rpm -e --nodeps", 1)[1]:
        raise ValueError("offline replay must re-resolve the complete package set without network")
    for arch in ("x86_64", "aarch64"):
        missing = erased - snapshot_packages(image_text, arch)
        if missing:
            raise ValueError(f"snapshot does not carry erased packages on {arch}: {sorted(missing)}")


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

    def test_offline_check_replays_the_image_erase(self):
        validate_erase_replay(IMAGE.read_text(), RECIPE.read_text())

    def test_erased_package_missing_from_snapshot_is_rejected(self):
        image = IMAGE.read_text().replace("# tilo-rpm-snapshot-package: pkgconf-m4\n", "")
        with self.assertRaisesRegex(ValueError, "does not carry erased packages"):
            validate_erase_replay(image, RECIPE.read_text())

    def test_drifted_replay_list_is_rejected(self):
        recipe = RECIPE.read_text().replace("|pkgconf-m4|libpkgconf)$/'", "|libpkgconf)$/'")
        with self.assertRaisesRegex(ValueError, "does not match"):
            validate_erase_replay(IMAGE.read_text(), recipe)

    def test_replay_without_full_offline_resolve_is_rejected(self):
        recipe = RECIPE.read_text()
        head, tail = recipe.split("xargs -r rpm -e --nodeps", 1)
        tail = tail.replace(FULL_OFFLINE_RESOLVE, "dnf install -y --downloadonly --setopt=reposdir=/repo wine")
        with self.assertRaisesRegex(ValueError, "complete package set"):
            validate_erase_replay(IMAGE.read_text(), head + "xargs -r rpm -e --nodeps" + tail)

    def test_replay_outside_the_offline_check_is_rejected(self):
        recipe = RECIPE.read_text()
        start = recipe.index("# os/Containerfile erases these")
        end = recipe.index("# The published artifact")
        moved = recipe[:start] + recipe[end:]
        moved = moved.replace("FROM metadata-tools AS repository", recipe[start:end] + "FROM metadata-tools AS repository")
        with self.assertRaisesRegex(ValueError, "does not replay"):
            validate_erase_replay(IMAGE.read_text(), moved)

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
