"""os/usr/libexec/tilo-codec-pack without network, dnf or root.

The end-to-end run against a real signed repository is test_codec_pack_e2e.py.
"""

import importlib.machinery
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path, PurePosixPath
from unittest import mock

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


class Connectivity(unittest.TestCase):
    def test_only_a_confirmed_lack_of_internet_waits(self):
        for state in ("full", "FULL", "unknown", "", " full\n"):
            self.assertTrue(pack.is_online(state), state)
        for state in ("none", "limited", "portal"):
            self.assertFalse(pack.is_online(state), state)


class FakeDnf:
    """The pieces of Dnf that refresh() uses, with one available build each."""

    offered = {
        "gstreamer1-plugin-openh264": "1.26.5",
        "openh264": "2.5.0",
        "mesa-va-drivers-freeworld": "25.1.4",
    }

    def __init__(self, layout, base):
        self.base = base

    def candidates(self, names):
        return {name: [nevra(name, self.offered[name])] if name in self.offered else [] for name in names}


class Refresh(unittest.TestCase):
    """refresh() with the network, dnf and rpm replaced; the state machine is real."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.layout = pack.Layout(Path(self.tmp.name))
        self.layout.state.mkdir(parents=True)
        self.layout.desired.write_text("owner opted in\n")
        self.online = "full"
        self.fail_download = None
        self.statuses = []
        base = pack.Base("fedora", "44", "x86_64", {
            "gstreamer1": nevra("gstreamer1", "1.26.5", repo="base"),
            "mesa-dri-drivers": nevra("mesa-dri-drivers", "25.1.4", repo="base"),
        })
        real_write = pack.write_status

        def recording_write(layout, **changes):
            status = real_write(layout, **changes)
            self.statuses.append(json.loads(json.dumps(status)))
            return status

        def fake_fetch(layout, base, dnf, keyring, specs, workdir, verifying=None):
            if self.fail_download:
                raise pack.PackError(self.fail_download)
            if verifying is not None:
                pack.report_progress(layout, verifying)
            return [{"nvra": spec, "path": "", "repo": "rpmfusion-free", "license": "x",
                     "vendor": "x", "fingerprint": "f"} for spec in specs]

        def fake_build(layout, base, name, rpms, extra):
            (layout.extensions / name).mkdir(parents=True, exist_ok=True)

        self.patches = [
            mock.patch.object(pack, "connectivity", lambda: self.online),
            mock.patch.object(pack, "write_status", recording_write),
            mock.patch.object(pack, "probe_base", lambda layout: base),
            mock.patch.object(pack, "Dnf", FakeDnf),
            mock.patch.object(pack, "Keyring", lambda layout, base, workdir: object()),
            mock.patch.object(pack, "fetch", fake_fetch),
            mock.patch.object(pack, "build_extension", fake_build),
        ]
        for patch in self.patches:
            patch.start()

    def tearDown(self):
        for patch in reversed(self.patches):
            patch.stop()
        self.tmp.cleanup()

    def status(self):
        return json.loads(self.layout.status.read_text())

    def test_install_reports_every_step_in_order_then_clears_progress(self):
        pack.refresh(self.layout, explicit=True)
        labels = []
        for status in self.statuses:
            progress = status.get("progress")
            if progress:
                self.assertEqual(status["state"], "installing")
                self.assertEqual(progress["steps"], len(pack.PROGRESS_STEPS))
                self.assertEqual(pack.PROGRESS_STEPS[progress["step"] - 1], progress["label"])
                if not labels or labels[-1] != progress["label"]:
                    labels.append(progress["label"])
        self.assertEqual(labels, list(pack.PROGRESS_STEPS))
        final = self.status()
        self.assertEqual((final["state"], final["mesa"], final["desired"]), ("installed", "enabled", True))
        self.assertNotIn("progress", final)
        self.assertEqual(final["last_error"], "")

    def test_offline_opt_in_waits_instead_of_failing(self):
        self.online = "none"
        result = pack.refresh(self.layout, explicit=True)
        self.assertEqual(result["state"], "waiting-for-network")
        final = self.status()
        self.assertEqual((final["state"], final["desired"], final["last_error"]),
                         ("waiting-for-network", True, ""))
        self.assertEqual(final["message"], pack.OFFLINE_MESSAGE)
        self.assertFalse((self.layout.extensions / pack.CODECS).exists())

    def test_the_deferred_install_completes_once_online(self):
        self.online = "limited"
        pack.refresh(self.layout, explicit=True)
        self.assertEqual(self.status()["state"], "waiting-for-network")
        self.online = "full"
        pack.refresh(self.layout, explicit=False)
        final = self.status()
        self.assertEqual((final["state"], final["message"]), ("installed", ""))

    def test_a_working_pack_stays_installed_offline_without_an_error(self):
        (self.layout.extensions / pack.CODECS).mkdir(parents=True)
        pack.write_status(self.layout, state="installed", mesa="enabled", last_error="")
        self.online = "none"
        pack.refresh(self.layout, explicit=False)
        final = self.status()
        self.assertEqual((final["state"], final["last_error"]), ("installed", ""))

    def test_losing_the_connection_mid_download_waits(self):
        self.fail_download = "dnf5 failed: Curl error (6): Couldn't resolve host name"
        calls = iter(["full", "none"])
        with mock.patch.object(pack, "connectivity", lambda: next(calls)):
            result = pack.refresh(self.layout, explicit=True)
        self.assertEqual(result["state"], "waiting-for-network")
        self.assertEqual(self.status()["last_error"], "")

    def test_a_real_failure_while_online_is_reported(self):
        self.fail_download = "x.rpm: signature check failed"
        with self.assertRaises(pack.PackError):
            pack.refresh(self.layout, explicit=True)
        final = self.status()
        self.assertEqual((final["state"], final["last_error"], final["desired"]),
                         ("failed", "x.rpm: signature check failed", True))
        self.assertNotIn("progress", final)

    def test_without_the_opt_in_nothing_happens(self):
        self.layout.desired.unlink()
        self.online = "none"
        self.assertEqual(pack.refresh(self.layout, explicit=False)["state"], "not-installed")

    def test_remove_clears_the_opt_in_and_the_progress(self):
        pack.refresh(self.layout, explicit=True)
        pack.write_status(self.layout, state="installing",
                          progress={"step": 1, "steps": 7, "label": "Checking this device"})
        pack.remove(self.layout)
        final = self.status()
        self.assertEqual((final["state"], final["desired"], final["mesa"]), ("not-installed", False, "off"))
        self.assertNotIn("progress", final)
        for name in pack.EXTENSIONS:
            self.assertFalse((self.layout.extensions / name).exists())


class InstallerSeed(unittest.TestCase):
    def test_adopted_once_and_never_after_a_remove(self):
        with tempfile.TemporaryDirectory() as tmp:
            layout = pack.Layout(Path(tmp))
            self.assertFalse(pack.adopt_installer_seed(layout), "no seed, nothing to adopt")
            layout.seed.parent.mkdir(parents=True)
            layout.seed.write_text("owner opted in during installation\n")
            self.assertTrue(pack.adopt_installer_seed(layout))
            self.assertTrue(layout.desired.exists())
            self.assertEqual(layout.desired.stat().st_mode & 0o777, 0o644)
            self.assertFalse(pack.adopt_installer_seed(layout), "adopted only once")
            pack.remove(layout)
            self.assertFalse(layout.desired.exists())
            self.assertTrue(layout.seed.exists(), "the helper never writes /etc")
            self.assertFalse(pack.adopt_installer_seed(layout), "a remove sticks")
            self.assertFalse(layout.desired.exists())

    def test_installer_flag_writes_the_seed_the_helper_reads(self):
        script = (REPO / "os/usr/libexec/tilo-install").read_text()
        self.assertIn("--codecs) WANT_CODECS=1; shift ;;", script)
        self.assertIn('CODECS_SEED="$DEPLOY_ETC/tilo/codec-pack-opt-in"', script)
        self.assertEqual(pack.INSTALLER_SEED, "/etc/tilo/codec-pack-opt-in")


class StatusFile(unittest.TestCase):
    def test_progress_exists_only_while_installing(self):
        with tempfile.TemporaryDirectory() as tmp:
            layout = pack.Layout(Path(tmp))
            pack.write_status(layout, state="installing")
            pack.report_progress(layout, "Downloading codecs")
            status = json.loads(layout.status.read_text())
            self.assertEqual(status["progress"], {"step": 3, "steps": 7, "label": "Downloading codecs"})
            pack.write_status(layout, state="installed")
            self.assertNotIn("progress", json.loads(layout.status.read_text()))

    def test_every_state_the_desktop_parses(self):
        rust = (REPO / "crates/system/os-actions/src/codec_pack.rs").read_text()
        for state in pack.STATES:
            self.assertIn(f'"{state}"', rust, state)


DISPATCHER = REPO / "os/usr/lib/NetworkManager/dispatcher.d/90-tilo-codec-pack"


class Dispatcher(unittest.TestCase):
    """The NetworkManager hook, with its two paths pointed into a temp tree."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.state = root / "state"
        self.state.mkdir()
        self.seed = root / "seed"
        self.calls = root / "calls"
        bin_dir = root / "bin"
        bin_dir.mkdir()
        stub = bin_dir / "systemctl"
        stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{self.calls}"\n')
        stub.chmod(0o755)
        script = DISPATCHER.read_text()
        self.assertIn("STATE_DIR=/var/lib/tilo/codec-pack\n", script)
        self.assertIn("INSTALLER_SEED=/etc/tilo/codec-pack-opt-in\n", script)
        script = script.replace("STATE_DIR=/var/lib/tilo/codec-pack\n", f"STATE_DIR={self.state}\n")
        script = script.replace("INSTALLER_SEED=/etc/tilo/codec-pack-opt-in\n", f"INSTALLER_SEED={self.seed}\n")
        self.script = root / "dispatcher"
        self.script.write_text(script)
        self.script.chmod(0o755)
        self.env = {"PATH": f"{bin_dir}:/usr/bin:/bin"}

    def tearDown(self):
        self.tmp.cleanup()

    def dispatch(self, action, connectivity=None):
        env = dict(self.env)
        if connectivity is not None:
            env["CONNECTIVITY_STATE"] = connectivity
        subprocess.run([str(self.script), "wlan0", action], env=env, check=True)
        calls = self.calls.read_text().splitlines() if self.calls.exists() else []
        self.calls.unlink(missing_ok=True)
        return calls

    def test_ships_executable(self):
        self.assertTrue(DISPATCHER.stat().st_mode & 0o111)

    def test_starts_the_deferred_install_when_the_internet_appears(self):
        (self.state / "enabled").write_text("x")
        (self.state / "status.json").write_text(json.dumps({"state": "waiting-for-network"}, indent=2))
        expected = ["start --no-block tilo-codec-pack@refresh.service"]
        self.assertEqual(self.dispatch("connectivity-change", "FULL"), expected)
        self.assertEqual(self.dispatch("up"), expected)

    def test_ignores_other_events_and_partial_connectivity(self):
        (self.state / "enabled").write_text("x")
        for action, connectivity in (("connectivity-change", "LIMITED"), ("connectivity-change", "NONE"),
                                     ("down", None), ("dhcp4-change", None), ("pre-up", None)):
            self.assertEqual(self.dispatch(action, connectivity), [], action)

    def test_does_nothing_without_the_opt_in_or_once_installed(self):
        self.assertEqual(self.dispatch("connectivity-change", "FULL"), [])
        (self.state / "enabled").write_text("x")
        (self.state / "status.json").write_text(json.dumps({"state": "installed"}, indent=2))
        self.assertEqual(self.dispatch("connectivity-change", "FULL"), [])

    def test_an_unadopted_installer_seed_counts_as_the_opt_in(self):
        self.seed.write_text("owner opted in during installation\n")
        self.assertEqual(len(self.dispatch("connectivity-change", "FULL")), 1)
        (self.state / "installer-seed-adopted").write_text("x")
        self.assertEqual(self.dispatch("connectivity-change", "FULL"), [], "a remove after adoption sticks")

    def test_the_helper_writes_the_state_line_the_hook_matches(self):
        with tempfile.TemporaryDirectory() as tmp:
            layout = pack.Layout(Path(tmp))
            pack.write_status(layout, state="installed")
            self.assertIn('"state": "installed"', layout.status.read_text())


if __name__ == "__main__":
    unittest.main()
