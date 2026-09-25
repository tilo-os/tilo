"""tilo-codec-pack end to end, against a real signed repository.

Builds tiny RPMs shaped like the pack's real packages, signs them with
throwaway OpenPGP keys, publishes them as local repositories with
createrepo_c, and drives the helper's own install/refresh/remove code (real
dnf5, rpmkeys and rpm2archive) into a temporary root. Only what cannot exist
off a Tilo machine is replaced: the pinned fingerprints point at the test keys,
the base's package list is given, and connectivity is "full".

Needs rpmbuild, rpmsign, createrepo_c, dnf5, gpg, rpmkeys and rpm2archive; it
skips otherwise. os/tests/codec-pack-e2e.sh runs it in the development
container. Scratch space lives under ~/.cache/codec-pack-e2e (or
$TILO_CODEC_PACK_E2E_DIR), never /tmp.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_codec_pack import pack  # noqa: E402  (the helper, loaded once)

TOOLS = ("rpmbuild", "rpmsign", "createrepo_c", "dnf5", "gpg", "rpmkeys", "rpm2archive")
MISSING = [tool for tool in TOOLS if shutil.which(tool) is None]

# (name, version, repository, signing key, files, symlinks, ld.so.conf.d text)
PACKAGES = {
    "gstreamer1-plugin-openh264": ("1.26.5", "fedora", "good", {
        "usr/lib64/gstreamer-1.0/libgstopenh264.so": "plugin",
        "etc/openh264-plugin.conf": "never part of an extension",
    }, {}, None),
    "openh264": ("2.5.0", "fedora-cisco-openh264", "good", {
        "usr/lib64/libopenh264.so.7": "cisco",
    }, {}, None),
    "libavcodec-freeworld": ("7.1.1", "rpmfusion-free", "good", {
        "usr/lib64/libavcodec-freeworld/libavcodec.so.61": "freeworld libavcodec",
    }, {}, "/usr/lib64/libavcodec-freeworld\n"),
    "mesa-va-drivers-freeworld": ("25.1.4", "rpmfusion-free", "good", {
        "usr/lib64/mesa-freeworld/libgallium-25.1.4.so": "freeworld gallium",
    }, {
        "usr/lib64/dri-freeworld/radeonsi_drv_video.so": "../mesa-freeworld/libgallium-25.1.4.so",
    }, "/usr/lib64/mesa-freeworld\n"),
    # Claims to be RPM Fusion's but is signed by another key.
    "gstreamer1-plugins-ugly": ("1.26.5", "rpmfusion-free", "rogue", {
        "usr/lib64/gstreamer-1.0/libgstugly.so": "impostor",
    }, {}, None),
}
REPOSITORIES = ("rpmfusion-free", "fedora", "fedora-cisco-openh264")


def run(*argv, **kwargs):
    result = subprocess.run(argv, capture_output=True, text=True, **kwargs)
    if result.returncode != 0:
        raise AssertionError(f"{' '.join(map(str, argv))} failed:\n{result.stdout}\n{result.stderr}")
    return result.stdout


def scratch_root() -> Path:
    base = Path(os.environ.get("TILO_CODEC_PACK_E2E_DIR", Path.home() / ".cache/codec-pack-e2e"))
    base.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="run-", dir=base))


class Keys:
    """Two throwaway signing keys in a private GnuPG home."""

    def __init__(self, home: Path):
        self.home = home
        home.mkdir(mode=0o700)
        self.fingerprints = {}
        self.armored = {}
        for name in ("good", "rogue"):
            uid = f"Tilo codec-pack test {name} <{name}@codec-pack.test>"
            run("gpg", "--homedir", str(home), "--batch", "--pinentry-mode", "loopback", "--passphrase", "",
                "--quick-generate-key", uid, "ed25519", "sign", "never")
            listing = run("gpg", "--homedir", str(home), "--with-colons", "--fingerprint", uid)
            fpr = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
            self.fingerprints[name] = fpr.lower()
            self.armored[name] = run("gpg", "--homedir", str(home), "--armor", "--export", fpr)

    def uid(self, name):
        return self.fingerprints[name]


def build_rpm(top: Path, name: str, version: str, files: dict, links: dict, ld_conf: str | None) -> Path:
    install = []
    listed = []
    for path, content in files.items():
        install.append(f"mkdir -p %{{buildroot}}/{Path(path).parent}")
        install.append(f"printf '%s' '{content}' > %{{buildroot}}/{path}")
        listed.append(f"/{path}")
    for path, target in links.items():
        install.append(f"mkdir -p %{{buildroot}}/{Path(path).parent}")
        install.append(f"ln -s {target} %{{buildroot}}/{path}")
        listed.append(f"/{path}")
    if ld_conf is not None:
        install.append("mkdir -p %{buildroot}/etc/ld.so.conf.d")
        install.append(f"printf '%s' '{ld_conf}' > %{{buildroot}}/etc/ld.so.conf.d/{name}.conf")
        listed.append(f"/etc/ld.so.conf.d/{name}.conf")
    spec = top / f"{name}.spec"
    spec.write_text(textwrap.dedent(f"""\
        %global debug_package %{{nil}}
        %global __os_install_post %{{nil}}
        %global _build_id_links none
        Name: {name}
        Version: {version}
        Release: 1.fc44
        Summary: codec-pack end-to-end fixture
        License: MIT
        AutoReqProv: no

        %description
        Test fixture shaped like {name}.

        %install
        """) + "\n".join(install) + "\n\n%files\n" + "\n".join(listed) + "\n")
    run("rpmbuild", "--define", f"_topdir {top}", "-bb", str(spec))
    return next((top / "RPMS").rglob(f"{name}-{version}-*.rpm"))


@unittest.skipIf(MISSING, f"needs {', '.join(MISSING)} (run os/tests/codec-pack-e2e.sh)")
class EndToEnd(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = scratch_root()
        cls.keys = Keys(cls.scratch / "gnupg")
        top = cls.scratch / "rpmbuild"
        top.mkdir()
        cls.rpms = {}
        for name, (version, repo, key, files, links, ld_conf) in PACKAGES.items():
            path = build_rpm(top, name, version, files, links, ld_conf)
            run("rpmsign", "--define", f"_gpg_path {cls.keys.home}",
                "--define", f"_gpg_name {cls.keys.uid(key)}", "--addsign", str(path))
            cls.rpms[name] = (path, repo)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.scratch, ignore_errors=True)

    def publish(self, root: Path, names) -> Path:
        """Local repositories holding `names`, with the pack's repo ids."""
        repos = root / "repos"
        for repo in REPOSITORIES:
            (repos / repo).mkdir(parents=True)
        for name in names:
            path, repo = self.rpms[name]
            shutil.copy2(path, repos / repo / path.name)
        for repo in REPOSITORIES:
            run("createrepo_c", "--quiet", str(repos / repo))
        return repos

    def machine(self, names, base_mesa="25.1.4"):
        """A temporary root laid out like a Tilo image with the pack's data."""
        root = Path(tempfile.mkdtemp(prefix="root-", dir=self.scratch))
        layout = pack.Layout(root)
        repos = self.publish(root, names)
        (layout.data / "repos.d").mkdir(parents=True)
        (layout.data / "repos.d/tilo-codec-pack.repo").write_text("".join(
            f"[{repo}]\nname={repo}\nbaseurl=file://{repos / repo}\nenabled=1\ngpgcheck=1\n"
            f"gpgkey=file://{layout.data / 'keys' / 'RPM-GPG-KEY-rpmfusion-free-fedora-2020'}\n\n"
            for repo in REPOSITORIES))
        (layout.data / "keys").mkdir()
        (layout.data / "keys/RPM-GPG-KEY-rpmfusion-free-fedora-2020").write_text(self.keys.armored["good"])
        (layout.data / "keys/RPM-GPG-KEY-rpmfusion-nonfree-fedora-2020").write_text(self.keys.armored["rogue"])
        layout.fedora_keys.mkdir(parents=True)
        (layout.fedora_keys / "RPM-GPG-KEY-fedora-44-primary").write_text(self.keys.armored["good"])
        (root / "usr/lib").mkdir(parents=True, exist_ok=True)
        layout.os_release.write_text('ID=fedora\nVERSION_ID=44\n')
        base = pack.Base("fedora", "44", "x86_64", {
            "gstreamer1": pack.Nevra("gstreamer1", 0, "1.26.5", "1.fc44", "x86_64", "base"),
            "libavcodec-free": pack.Nevra("libavcodec-free", 0, "7.1.1", "1.fc44", "x86_64", "base"),
            "mesa-dri-drivers": pack.Nevra("mesa-dri-drivers", 0, base_mesa, "1.fc44", "x86_64", "base"),
        })
        good, rogue = self.keys.fingerprints["good"], self.keys.fingerprints["rogue"]
        statuses = []
        real_write = pack.write_status

        def recording_write(target, **changes):
            status = real_write(target, **changes)
            statuses.append(json.loads(json.dumps(status)))
            return status

        patches = [
            mock.patch.object(pack, "RPMFUSION_FREE_FPR", good),
            mock.patch.object(pack, "RPMFUSION_NONFREE_FPR", rogue),
            mock.patch.object(pack, "FEDORA_PRIMARY_FPR", {"44": good}),
            mock.patch.object(pack, "probe_base", lambda _layout: base),
            mock.patch.object(pack, "connectivity", lambda: "full"),
            mock.patch.object(pack, "relabel", lambda _tree: None),
            mock.patch.object(pack, "write_status", recording_write),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        layout.state.mkdir(parents=True)
        layout.desired.write_text("owner opted in\n")
        return layout, statuses

    def status(self, layout):
        return json.loads(layout.status.read_text())

    def test_install_builds_both_extensions_then_remove_restores_everything(self):
        layout, statuses = self.machine(
            ["gstreamer1-plugin-openh264", "openh264", "libavcodec-freeworld", "mesa-va-drivers-freeworld"])
        pack.refresh(layout, explicit=True)

        seen = [s["progress"]["label"] for s in statuses if s.get("progress")]
        self.assertEqual(list(dict.fromkeys(seen)), list(pack.PROGRESS_STEPS))
        self.assertTrue(all(s["state"] == "installing" for s in statuses if s.get("progress")))
        final = self.status(layout)
        self.assertEqual((final["state"], final["mesa"], final["desired"], final["last_error"]),
                         ("installed", "enabled", True, ""))
        self.assertNotIn("progress", final)
        self.assertGreaterEqual(final["generation"], 1)
        self.assertEqual(sorted(final["packages"][pack.CODECS]), [
            "gstreamer1-plugin-openh264-1.26.5-1.fc44.x86_64",
            "libavcodec-freeworld-7.1.1-1.fc44.x86_64",
            "openh264-2.5.0-1.fc44.x86_64",
        ])
        self.assertIn("gstreamer1-plugins-bad-freeworld", final["waiting"])

        codecs = layout.extensions / pack.CODECS
        self.assertEqual(pack.extension_release_path(codecs, pack.CODECS).read_text(),
                         "ID=fedora\nVERSION_ID=44\nARCHITECTURE=x86-64\nSYSEXT_SCOPE=system\n"
                         "TILO_CODEC_PACK=tilo-codecs\n")
        self.assertEqual((codecs / "usr/lib64/gstreamer-1.0/libgstopenh264.so").read_text(), "plugin")
        self.assertFalse((codecs / "etc").exists(), "an extension carries only /usr")
        # The freeworld libavcodec moved to the base's own path, where it shadows it.
        self.assertEqual((codecs / "usr/lib64/libavcodec.so.61").read_text(), "freeworld libavcodec")
        self.assertTrue((codecs / "usr/lib64/libavcodec-freeworld/libavcodec.so.61").is_symlink())
        manifest = json.loads((codecs / "usr/share/tilo-codec-pack/tilo-codecs.json").read_text())
        self.assertEqual({(p["nvra"].split("-")[0], p["repo"]) for p in manifest["packages"]}, {
            ("gstreamer1", "fedora"), ("openh264", "fedora-cisco-openh264"), ("libavcodec", "rpmfusion-free")})
        self.assertEqual({p["fingerprint"] for p in manifest["packages"]}, {self.keys.fingerprints["good"]})
        self.assertEqual(manifest["relocated"], ["/usr/lib64/libavcodec.so.61"])

        mesa = layout.extensions / pack.MESA
        release = pack.parse_os_release(pack.extension_release_path(mesa, pack.MESA).read_text())
        self.assertEqual((release["TILO_MESA_VERSION"], release["SYSEXT_SCOPE"]), ("25.1.4", "system"))
        self.assertEqual((mesa / "usr/lib64/libgallium-25.1.4.so").read_text(), "freeworld gallium")
        # The VA driver still resolves to the (moved) gallium library.
        self.assertEqual((mesa / "usr/lib64/dri-freeworld/radeonsi_drv_video.so").read_text(), "freeworld gallium")

        self.listed_by_systemd(layout)

        pack.remove(layout)
        final = self.status(layout)
        self.assertEqual((final["state"], final["desired"], final["mesa"], final["packages"]),
                         ("not-installed", False, "off", {}))
        self.assertEqual(sorted(p.name for p in layout.extensions.iterdir()), [])
        self.assertFalse(layout.cache.exists())
        self.assertFalse(layout.desired.exists())

    def test_mesa_waits_for_a_build_of_the_base_mesa(self):
        layout, _ = self.machine(
            ["gstreamer1-plugin-openh264", "openh264", "mesa-va-drivers-freeworld"], base_mesa="25.2.0")
        pack.refresh(layout, explicit=True)
        final = self.status(layout)
        self.assertEqual((final["state"], final["mesa"]), ("installed", "waiting"))
        self.assertIn(pack.WAITING_MESSAGE, final["message"])
        self.assertIn("RPM Fusion offers 25.1.4", final["message"])
        self.assertTrue((layout.extensions / pack.CODECS).is_dir())
        self.assertFalse((layout.extensions / pack.MESA).exists())

    def test_a_package_signed_by_another_key_is_rejected(self):
        layout, _ = self.machine(["gstreamer1-plugin-openh264", "openh264", "gstreamer1-plugins-ugly"])
        with self.assertRaises(pack.PackError) as caught:
            pack.refresh(layout, explicit=True)
        self.assertIn("gstreamer1-plugins-ugly", str(caught.exception))
        self.assertIn("not signed by that repository's key", str(caught.exception))
        final = self.status(layout)
        self.assertEqual((final["state"], final["desired"]), ("failed", True))
        self.assertIn("not signed by that repository's key", final["last_error"])
        self.assertFalse((layout.extensions / pack.CODECS).exists(), "nothing unverified is merged")

    def listed_by_systemd(self, layout) -> None:
        """systemd-sysext's own discovery of the directory extensions.

        `list` runs rootless against the temporary root; merging needs root and
        is not attempted. Skipped where systemd-sysext is not installed.
        """
        sysext = shutil.which("systemd-sysext")
        if sysext is None:
            return
        listing = run(sysext, f"--root={layout.root}", "list")
        for name in pack.EXTENSIONS:
            self.assertRegex(listing, rf"(?m)^{name}\s+directory\s", listing)


if __name__ == "__main__":
    unittest.main()
