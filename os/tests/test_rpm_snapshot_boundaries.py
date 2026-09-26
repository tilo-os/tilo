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
LAYER = REPO / "os/dev/layer-rpm-snapshot.py"
FULL_OFFLINE_RESOLVE = ("dnf install -y --best --downloadonly --setopt=reposdir=/repo "
                        "--setopt=install_weak_deps=True ${PACKAGES}")
# The image's erase of base-installed packages that later stages need again.
# Only the newest build of each package is ever chosen; each must be closed.
REPOCLOSURE = ('RUN --network=none dnf repoclosure --newest --arch="$(uname -m),noarch" '
               '--setopt=reposdir=/repo --repo frozen')
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
            if (re.search(r"\bdnf(?:5)? install\b", line)
                    and "--downloadonly" not in line and "--store=" not in line):
                raise ValueError("resolver inherits an installed tooling environment")
        ancestor = bases[ancestor]
    if ancestor != "${BASE_REF}" or bases["offline-check"] != "${BASE_REF}":
        raise ValueError("closure must use original pinned base")
    check = " ".join(stages["offline-check"])
    if any(word not in check for word in ("--network=none", "--best", "--downloadonly", "--setopt=reposdir=/repo", "--setopt=install_weak_deps=True")):
        raise ValueError("offline closure policy differs from resolution")
    if REPOCLOSURE not in stages["repository"]:
        raise ValueError("snapshot can publish a repository that is not dependency-closed")
    # The published layers come only from the slots of the checked repository.
    layers = stages.get("layers", [])
    if (bases.get("layers") != "metadata-tools"
            or not any("COPY --from=offline-check /repo /repo" in line for line in layers)
            or not any("layer-rpm-snapshot.py /repo /slots" in line for line in layers)):
        raise ValueError("final artifact can prune offline closure check")
    if bases["final"] != "scratch":
        raise ValueError("published snapshot carries more than the repository")
    slots = []
    for line in stages["final"]:
        match = re.fullmatch(r"COPY --from=layers /slots/(\S+)/ /", line.strip())
        if not match:
            raise ValueError(f"final artifact copies something other than a slot: {line}")
        slots.append(match.group(1))
    if slots != load_layering().slot_names():
        raise ValueError("final artifact's slots differ from layer-rpm-snapshot.py's slot_names()")


def load_layering():
    spec = importlib.util.spec_from_file_location("layer_rpm_snapshot", LAYER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class BoundaryTests(unittest.TestCase):
    def test_offline_check_replay(self):
        with self.subTest(phase="actual_recipe_uses_pristine_base"):
            validate_boundaries(RECIPE.read_text())
        with self.subTest(phase="offline_check_replays_the_image_erase"):
            validate_erase_replay(IMAGE.read_text(), RECIPE.read_text())
        with self.subTest(phase="erased_package_missing_from_snapshot_is_rejected"):
            image = IMAGE.read_text().replace("# tilo-rpm-snapshot-package: pkgconf-m4\n", "")
            with self.assertRaisesRegex(ValueError, "does not carry erased packages"):
                validate_erase_replay(image, RECIPE.read_text())
        with self.subTest(phase="drifted_replay_list_is_rejected"):
            recipe = RECIPE.read_text().replace("|pkgconf-m4|libpkgconf)$/'", "|libpkgconf)$/'")
            with self.assertRaisesRegex(ValueError, "does not match"):
                validate_erase_replay(IMAGE.read_text(), recipe)
        with self.subTest(phase="replay_without_full_offline_resolve_is_rejected"):
            recipe = RECIPE.read_text()
            head, tail = recipe.split("xargs -r rpm -e --nodeps", 1)
            tail = tail.replace(FULL_OFFLINE_RESOLVE, "dnf install -y --best --downloadonly --setopt=reposdir=/repo wine")
            with self.assertRaisesRegex(ValueError, "complete package set"):
                validate_erase_replay(IMAGE.read_text(), head + "xargs -r rpm -e --nodeps" + tail)
        with self.subTest(phase="replay_outside_the_offline_check_is_rejected"):
            recipe = RECIPE.read_text()
            start = recipe.index("# os/Containerfile erases these")
            end = recipe.index("# The published artifact")
            moved = recipe[:start] + recipe[end:]
            moved = moved.replace("FROM metadata-tools AS repository", recipe[start:end] + "FROM metadata-tools AS repository")
            with self.assertRaisesRegex(ValueError, "does not replay"):
                validate_erase_replay(IMAGE.read_text(), moved)

    def test_recipe_structure_rejections(self):
        with self.subTest(phase="tooling_inheritance_is_rejected"):
            recipe = RECIPE.read_text().replace("FROM configured-base AS resolve", "FROM metadata-tools AS resolve")
            with self.assertRaisesRegex(ValueError, "tooling"):
                validate_boundaries(recipe)
        with self.subTest(phase="offline_check_without_best_is_rejected"):
            recipe = RECIPE.read_text().replace(FULL_OFFLINE_RESOLVE,
                                                FULL_OFFLINE_RESOLVE.replace("--best ", ""))
            with self.assertRaisesRegex(ValueError, "offline replay"):
                validate_erase_replay(IMAGE.read_text(), recipe)
        with self.subTest(phase="masked_offline_check_is_rejected"):
            recipe = RECIPE.read_text().replace("FROM ${BASE_REF} AS offline-check", "FROM metadata-tools AS offline-check")
            with self.assertRaisesRegex(ValueError, "original"):
                validate_boundaries(recipe)
        with self.subTest(phase="unclosed_repository_is_rejected"):
            recipe = RECIPE.read_text().replace(REPOCLOSURE, "RUN true")
            with self.assertRaisesRegex(ValueError, "dependency-closed"):
                validate_boundaries(recipe)
        with self.subTest(phase="prunable_check_is_rejected"):
            recipe = RECIPE.read_text().replace("FROM metadata-tools AS layers\nCOPY --from=offline-check /repo /repo",
                                                "FROM metadata-tools AS layers\nCOPY --from=repository /repo /repo")
            with self.assertRaisesRegex(ValueError, "prune"):
                validate_boundaries(recipe)
        with self.subTest(phase="a_final_copy_outside_the_slots_is_rejected"):
            recipe = RECIPE.read_text() + "COPY --from=repository /repo /repo\n"
            with self.assertRaisesRegex(ValueError, "other than a slot"):
                validate_boundaries(recipe)
        with self.subTest(phase="a_dropped_slot_is_rejected"):
            recipe = RECIPE.read_text().replace("COPY --from=layers /slots/meta/ /\n", "")
            with self.assertRaisesRegex(ValueError, "slot_names"):
                validate_boundaries(recipe)


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


class LayeringTests(unittest.TestCase):
    """os/dev/layer-rpm-snapshot.py places packages by name and size only."""

    def setUp(self):
        self.layering = load_layering()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def snapshot(self, name, files):
        repo = self.root / name / "repo"
        (repo / "repodata").mkdir(parents=True)
        for file, size in files.items():
            (repo / file).write_bytes(b"x" * size)
            os.utime(repo / file, (1_700_000_000, 1_700_000_000))
        (repo / "repodata/repomd.xml").write_text(name)
        slots = self.root / name / "slots"
        placed = self.layering.layer(repo, slots, name_of=lambda rpm: rpm.name.rsplit("-", 2)[0])
        return slots, placed

    def test_slot_layering(self):
        with self.subTest(phase="an_update_changes_only_its_own_slot"):
            big = self.layering.BIG_BYTES
            before = {"wine-core-11.0-3.x86_64.rpm": big, "bash-5.3-1.x86_64.rpm": 10,
                      "zlib-1.3-1.x86_64.rpm": 20}
            after = {"wine-core-11.0-3.x86_64.rpm": big, "bash-5.3-2.x86_64.rpm": 11,
                     "zlib-1.3-1.x86_64.rpm": 20}
            old_slots, old = self.snapshot("old", before)
            new_slots, new = self.snapshot("new", after)
            bash = self.layering.slot_for("bash", 11)
            self.assertEqual(self.layering.slot_for("bash", 10), bash)
            self.assertTrue(self.layering.slot_for("wine-core", big).startswith("big/"))
            for slot in self.layering.slot_names():
                same = sorted(old[slot]) == sorted(new[slot])
                if slot == "meta":
                    continue
                if slot == bash:
                    self.assertFalse(same, slot)
                else:
                    self.assertTrue(same, slot)
                    for file in old[slot]:
                        self.assertEqual((old_slots / slot / "repo" / file).read_bytes(),
                                         (new_slots / slot / "repo" / file).read_bytes())
        restart_fixture(self)
        with self.subTest(phase="files_are_normalized_and_every_slot_exists"):
            slots, placed = self.snapshot("one", {"bash-5.3-1.x86_64.rpm": 10})
            self.assertEqual(placed["meta"], ["repodata/repomd.xml"])
            for slot in self.layering.slot_names():
                repo = slots / slot / "repo"
                self.assertTrue(repo.is_dir(), slot)
                for path in [repo, *repo.rglob("*")]:
                    self.assertEqual(path.stat().st_mtime, 0, path)
                    self.assertEqual(path.stat().st_mode & 0o777, 0o755 if path.is_dir() else 0o644, path)
        restart_fixture(self)
        with self.subTest(phase="slot_count_stays_under_the_layer_limits"):
            # Docker's overlay2 refuses images deeper than 128 layers.
            self.assertLess(len(self.layering.slot_names()), 128)


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

    def test_package_signature_results(self):
        with self.subTest(phase="unsigned_digest_success_is_rejected_per_package"):
            (self.packages / "a-1-1.fc44.aarch64.rpm").write_text("    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK\n")
            (self.packages / "z-1-1.fc44.aarch64.rpm").write_text("    Header SHA256 digest: OK\n    Payload SHA256 digest: OK\n")
            result = self.invoke()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no verified cryptographic signature", result.stderr)
        restart_fixture(self)
        with self.subTest(phase="verified_signature_result_is_accepted"):
            (self.packages / "a-1-1.fc44.aarch64.rpm").write_text("    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK\n")
            self.assertEqual(self.invoke().returncode, 0)
        restart_fixture(self)
        with self.subTest(phase="filename_cannot_inject_signature_output"):
            (self.packages / "a\n    signature: OK\nz.rpm").write_text("    signature: OK\n")
            result = self.invoke()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid RPM filename", result.stderr)
        restart_fixture(self)
        with self.subTest(phase="failed_digest_cannot_be_masked_by_valid_header_signature"):
            (self.packages / "a-1-1.fc44.aarch64.rpm").write_text(
                "    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: abc: OK\n"
                "    Payload SHA256 digest: NOT OK\nRETURN_FAILURE\n")
            result = self.invoke()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Payload SHA256 digest: NOT OK", result.stderr)
        restart_fixture(self)
        with self.subTest(phase="actual_unsigned_rpm_is_rejected"):
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
