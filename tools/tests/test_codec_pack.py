"""Pure logic of os/usr/libexec/tilo-codec-pack: no network, dnf or root."""

import importlib.machinery
import importlib.util
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parents[2]
HELPER = REPO / "os/usr/libexec/tilo-codec-pack"
KEYS = REPO / "os/usr/share/tilo/codec-pack/keys"


def load():
    loader = importlib.machinery.SourceFileLoader("tilo_codec_pack_under_test", str(HELPER))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


pack = load()


def nevra(name, version, release="1.fc44", epoch=0, repo="rpmfusion-free"):
    return pack.Nevra(name, epoch, version, release, "x86_64", repo)


class ArgumentValidation(unittest.TestCase):
    def test_accepts_exactly_one_known_command(self):
        for command in ("install", "remove", "refresh", "guard", "status"):
            self.assertEqual(pack.parse_command([command]), command)

    def test_rejects_everything_else(self):
        for argv in ([], ["install", "remove"], ["Install"], ["install;rm"], ["../install"],
                     ["--help"], [""], ["install "]):
            with self.assertRaises(pack.PackError, msg=argv):
                pack.parse_command(argv)

    def test_main_exits_2_on_bad_argument_without_touching_anything(self):
        self.assertEqual(pack.main(["upgrade"]), 2)

    def test_polkit_rule_and_helper_agree_on_the_unit_verbs(self):
        rule = (REPO / "os/etc/polkit-1/rules.d/49-tilo-codec-pack.rules").read_text()
        verbs = re.search(r"tilo-codec-pack@\(([a-z|]+)\)", rule).group(1).split("|")
        self.assertEqual(set(verbs), {"install", "remove", "refresh"})
        self.assertTrue(set(verbs) <= set(pack.COMMANDS))


class VersionComparison(unittest.TestCase):
    def test_rpmvercmp(self):
        cases = [
            ("1.0", "1.0", 0), ("1.0", "1.1", -1), ("1.10", "1.9", 1), ("26.2.3", "26.0.3", 1),
            ("1.0~rc1", "1.0", -1), ("1.0^git1", "1.0", 1), ("1.0^git1", "1.0.1", -1),
            ("1.0a", "1.0", 1), ("2.0", "2.0.0", -1), ("fc44", "fc44", 0), ("1a", "1b", -1),
        ]
        for left, right, expected in cases:
            self.assertEqual(pack.rpmvercmp(left, right), expected, (left, right))
            self.assertEqual(pack.rpmvercmp(right, left), -expected, (right, left))


class ComponentChoice(unittest.TestCase):
    GST = [nevra("gstreamer1-plugins-ugly", "1.28.1", epoch=1),
           nevra("gstreamer1-plugins-ugly", "1.28.7", epoch=1, repo="rpmfusion-free-updates")]

    def test_exact_version_wins_even_when_newer_exists(self):
        choice = pack.choose_candidate("compatible", "1.28.1", self.GST)
        self.assertEqual(choice.version, "1.28.1")

    def test_compatible_falls_back_to_newest_older_same_minor(self):
        choice = pack.choose_candidate("compatible", "1.28.4", self.GST)
        self.assertEqual(choice.version, "1.28.1")

    def test_compatible_never_picks_newer_than_base(self):
        self.assertIsNone(pack.choose_candidate("compatible", "1.28.0", self.GST))

    def test_compatible_never_crosses_minor(self):
        self.assertIsNone(pack.choose_candidate("compatible", "1.30.0", self.GST))

    def test_newest_ignores_base(self):
        self.assertEqual(pack.choose_candidate("newest", None, self.GST).version, "1.28.7")

    def test_following_rules_need_a_base_version(self):
        self.assertIsNone(pack.choose_candidate("compatible", None, self.GST))

    def test_highest_release_of_the_exact_version(self):
        candidates = [nevra("x", "1.0", "1.fc44"), nevra("x", "1.0", "3.fc44"), nevra("x", "1.0", "2.fc44")]
        self.assertEqual(pack.choose_candidate("exact", "1.0", candidates).release, "3.fc44")


class MesaLockstep(unittest.TestCase):
    AVAILABLE = [nevra("mesa-va-drivers-freeworld", "26.0.3"),
                 nevra("mesa-va-drivers-freeworld", "26.2.3", repo="rpmfusion-free-updates")]

    def test_enable_on_exact_version_despite_different_release(self):
        base = nevra("mesa-dri-drivers", "26.0.3", "4.fc44", repo="base")
        decision = pack.decide_mesa(base, self.AVAILABLE)
        self.assertEqual(decision.action, "enable")
        self.assertEqual(decision.candidate.spec, "mesa-va-drivers-freeworld-26.0.3-1.fc44.x86_64")

    def test_enable_latest_update(self):
        base = nevra("mesa-dri-drivers", "26.2.3", repo="base")
        self.assertEqual(pack.decide_mesa(base, self.AVAILABLE).candidate.version, "26.2.3")

    def test_wait_when_base_is_between_published_builds(self):
        base = nevra("mesa-dri-drivers", "26.1.4", repo="base")
        decision = pack.decide_mesa(base, self.AVAILABLE)
        self.assertEqual(decision.action, "wait")
        self.assertIsNone(decision.candidate)
        self.assertIn("waiting for a matching Mesa build from RPM Fusion", decision.message)
        self.assertIn("26.1.4", decision.message)

    def test_wait_when_base_is_newer_than_rpm_fusion(self):
        base = nevra("mesa-dri-drivers", "26.2.4", repo="base")
        self.assertEqual(pack.decide_mesa(base, self.AVAILABLE).action, "wait")

    def test_wait_when_nothing_is_published(self):
        base = nevra("mesa-dri-drivers", "26.2.3", repo="base")
        self.assertEqual(pack.decide_mesa(base, []).action, "wait")

    def test_unsupported_without_base_mesa(self):
        self.assertEqual(pack.decide_mesa(None, self.AVAILABLE).action, "unsupported")


class BootGuard(unittest.TestCase):
    def test_keep_matching(self):
        self.assertEqual(pack.guard_decision("26.2.3", "26.2.3", []), ("keep", "26.2.3"))

    def test_park_after_base_mesa_changed(self):
        self.assertEqual(pack.guard_decision("26.0.3", "26.2.3", []), ("park", "26.0.3"))

    def test_park_when_base_mesa_is_unknown(self):
        self.assertEqual(pack.guard_decision("26.0.3", None, []), ("park", "26.0.3"))

    def test_restore_parked_build_after_rollback(self):
        self.assertEqual(pack.guard_decision(None, "26.0.3", ["26.0.3", "25.1.0"]), ("restore", "26.0.3"))

    def test_nothing_to_do(self):
        self.assertEqual(pack.guard_decision(None, "26.2.3", ["26.0.3"]), ("none", None))

    def test_guard_parks_and_restores_on_disk(self):
        with tempfile.TemporaryDirectory() as tmp:
            layout = pack.Layout(Path(tmp))
            tree = layout.extensions / pack.MESA
            release = pack.extension_release_path(tree, pack.MESA)
            release.parent.mkdir(parents=True)
            release.write_text(pack.extension_release("fedora", "44", "x86-64", {"TILO_MESA_VERSION": "26.0.3"}))
            pack.park(layout, pack.MESA, "26.0.3")
            self.assertFalse(tree.exists())
            self.assertEqual(pack.parked_versions(layout, pack.MESA), ["26.0.3"])


class ExtensionRelease(unittest.TestCase):
    def test_contents(self):
        text = pack.extension_release("fedora", "44", "x86-64", {
            "TILO_MESA_VERSION": "26.2.3", "TILO_CODEC_PACK": "tilo-codecs-mesa"})
        self.assertEqual(text, "ID=fedora\nVERSION_ID=44\nARCHITECTURE=x86-64\nSYSEXT_SCOPE=system\n"
                               "TILO_CODEC_PACK=tilo-codecs-mesa\nTILO_MESA_VERSION=26.2.3\n")
        self.assertEqual(pack.parse_os_release(text)["TILO_MESA_VERSION"], "26.2.3")

    def test_rejects_injection(self):
        with self.assertRaises(pack.PackError):
            pack.extension_release("fedora", "44\nID=evil", "x86-64", {})
        with self.assertRaises(pack.PackError):
            pack.extension_release("fedora", "44", "x86-64", {"ID": "evil"})

    def test_architecture_names_are_systemds(self):
        self.assertEqual(pack.ARCHITECTURES, {"x86_64": "x86-64", "aarch64": "arm64"})


class StatusTransitions(unittest.TestCase):
    def test_install_path(self):
        state = pack.next_state("not-installed", "install-started")
        self.assertEqual(state, "installing")
        self.assertEqual(pack.next_state(state, "succeeded"), "installed")

    def test_failed_first_install(self):
        self.assertEqual(pack.next_state("installing", "failed", installed=False), "failed")

    def test_failed_refresh_keeps_working_pack(self):
        state = pack.next_state("installed", "refresh-started", installed=True)
        self.assertEqual(state, "installed")
        self.assertEqual(pack.next_state(state, "failed", installed=True), "installed")

    def test_failed_reinstall_keeps_working_pack(self):
        self.assertEqual(pack.next_state("installing", "failed", installed=True), "installed")

    def test_remove(self):
        self.assertEqual(pack.next_state("installed", "remove-started"), "removing")
        self.assertEqual(pack.next_state("removing", "removed"), "not-installed")
        self.assertEqual(pack.next_state("removing", "failed", installed=True), "failed")

    def test_unknown_event(self):
        with self.assertRaises(ValueError):
            pack.next_state("installed", "reboot")

    def test_status_file_is_world_readable_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            layout = pack.Layout(Path(tmp))
            pack.write_status(layout, state="installed", mesa="waiting", generation=3)
            self.assertEqual(layout.status.stat().st_mode & 0o777, 0o644)
            status = json.loads(layout.status.read_text())
            self.assertEqual((status["schema"], status["state"], status["mesa"], status["generation"]),
                             (1, "installed", "waiting", 3))
            self.assertFalse(status["desired"])


class Signatures(unittest.TestCase):
    def test_vendored_keys_match_pinned_fingerprints(self):
        self.assertEqual(pack.openpgp_fingerprint(
            (KEYS / "RPM-GPG-KEY-rpmfusion-free-fedora-2020").read_text()), pack.RPMFUSION_FREE_FPR)
        self.assertEqual(pack.openpgp_fingerprint(
            (KEYS / "RPM-GPG-KEY-rpmfusion-nonfree-fedora-2020").read_text()), pack.RPMFUSION_NONFREE_FPR)

    def test_checksig_parsing(self):
        good = ("/x.rpm:\n"
                "    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: "
                "e9a491a3de247814e7e067eae06f8ecdd651ff2e: OK\n"
                "    Header SHA256 digest: OK\n    Payload SHA256 digest: OK\n")
        self.assertEqual(pack.parse_checksig(good), (True, {pack.RPMFUSION_FREE_FPR}))
        unsigned = "/x.rpm:\n    Header SHA256 digest: OK\n    Payload SHA256 digest: OK\n"
        self.assertFalse(pack.parse_checksig(unsigned)[0])
        bad = good.replace("Payload SHA256 digest: OK", "Payload SHA256 digest: BAD")
        self.assertFalse(pack.parse_checksig(bad)[0])

    def test_each_repository_has_exactly_its_own_key(self):
        self.assertEqual(pack.expected_fingerprints("rpmfusion-free-updates", "44"), {pack.RPMFUSION_FREE_FPR})
        self.assertEqual(pack.expected_fingerprints("rpmfusion-nonfree", "44"), {pack.RPMFUSION_NONFREE_FPR})
        self.assertEqual(pack.expected_fingerprints("fedora-cisco-openh264", "44"),
                         {pack.FEDORA_PRIMARY_FPR["44"]})
        self.assertEqual(pack.expected_fingerprints("fedora-cisco-openh264", "99"), set())
        self.assertEqual(pack.expected_fingerprints("", "44"), set())
        self.assertEqual(pack.expected_fingerprints("terra", "44"), set())

    def test_repo_file_names_only_known_repositories(self):
        text = (REPO / "os/usr/share/tilo/codec-pack/repos.d/tilo-codec-pack.repo").read_text()
        for repo in re.findall(r"^\[([^\]]+)\]", text, re.MULTILINE):
            self.assertTrue(pack.expected_fingerprints(repo, "44"), repo)
        self.assertEqual(text.count("includepkgs=intel-media-driver"), 2)


class TreeRules(unittest.TestCase):
    def test_only_usr_survives(self):
        self.assertEqual(pack.safe_member("./usr/lib64/libx264.so.165"), PurePosixPath("usr/lib64/libx264.so.165"))
        self.assertIsNone(pack.safe_member("./etc/ld.so.conf.d/x.conf"))
        self.assertIsNone(pack.safe_member("./var/lib/x"))
        self.assertIsNone(pack.safe_member("./usr/lib/.build-id/45/abc"))

    def test_escape_is_rejected(self):
        for name in ("./usr/../etc/shadow", "../usr/lib64/x", "./../../usr/x"):
            with self.assertRaises(pack.PackError, msg=name):
                pack.safe_member(name)

    def test_writes_through_an_earlier_symlink_are_caught(self):
        with tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp) / "tree"
            (tree / "usr").mkdir(parents=True)
            (tree / "usr/lib64").symlink_to("/etc")
            self.assertFalse(pack.inside(tree, tree / "usr/lib64/passwd"))
            self.assertTrue(pack.inside(tree, tree / "usr/share/x"))

    def test_relocation_dirs(self):
        self.assertEqual(pack.relocation_dirs("/usr/lib64/dri-freeworld\n# c\n\n/opt/x\n/usr/lib64/a/b\n"),
                         [PurePosixPath("/usr/lib64/dri-freeworld")])

    def test_relocation_moves_only_libraries(self):
        with tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp)
            private = tree / "usr/lib64/dri-freeworld"
            private.mkdir(parents=True)
            (private / "libgallium-26.2.3.so").write_text("lib")
            (private / "radeonsi_drv_video.so").symlink_to("libgallium-26.2.3.so")
            (private / "notes.txt").write_text("not a library")
            moved = pack.relocate_private_libraries(tree, ["/usr/lib64/dri-freeworld\n"])
            self.assertEqual(moved, ["/usr/lib64/libgallium-26.2.3.so"])
            self.assertEqual((tree / "usr/lib64/libgallium-26.2.3.so").read_text(), "lib")
            # The driver symlink still resolves, now through a link to the moved library.
            self.assertTrue((private / "radeonsi_drv_video.so").is_symlink())
            self.assertEqual((private / "radeonsi_drv_video.so").read_text(), "lib")
            self.assertEqual(Path(private / "libgallium-26.2.3.so").readlink(), Path("../libgallium-26.2.3.so"))
            self.assertTrue((private / "notes.txt").is_file())


class Units(unittest.TestCase):
    def test_guard_runs_offline_before_sysext(self):
        unit = (REPO / "os/usr/lib/systemd/system/tilo-codec-pack-guard.service").read_text()
        self.assertIn("Before=systemd-sysext.service", unit)
        self.assertIn("PrivateNetwork=yes", unit)
        self.assertIn("ExecStart=/usr/libexec/tilo-codec-pack guard", unit)

    def test_template_passes_only_the_instance(self):
        unit = (REPO / "os/usr/lib/systemd/system/tilo-codec-pack@.service").read_text()
        self.assertIn("ExecStart=/usr/libexec/tilo-codec-pack %i\n", unit)


if __name__ == "__main__":
    unittest.main()
