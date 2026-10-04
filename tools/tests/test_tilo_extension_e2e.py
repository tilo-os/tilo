"""tilo-extension end to end, with real RPMs and a real P-256 signature.

Builds tiny RPMs shaped like the media-codecs packages, publishes a manifest
pinning them, signs it with a throwaway P-256 key (openssl, as cosign's KMS
signer would) and drives the helper's install/remove into a temporary root:
real rpm, rpm2archive and `tilo-crypto verify-p256-sha256`. Only what cannot
exist off a Tilo machine is replaced: the network is a dictionary of URLs, the
base's package list is given, connectivity is "full", and merging is left to
`systemd-sysext list` (rootless) rather than done.

Needs rpmbuild, rpm, rpm2archive, openssl and a tilo-crypto binary
($TILO_CRYPTO, target/{debug,release}/tilo-crypto or PATH); skips otherwise.
Scratch space lives under ~/.cache/tilo-extension-e2e (or
$TILO_EXTENSION_E2E_DIR), never /tmp.
"""

import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_tilo_extension import BASE_PACKAGES, REPO, ext  # noqa: E402  (the helper, loaded once)


def find_tilo_crypto() -> str | None:
    candidates = [os.environ.get("TILO_CRYPTO", "")]
    candidates += [str(REPO / "target" / profile / "tilo-crypto") for profile in ("debug", "release")]
    candidates.append(shutil.which("tilo-crypto") or "")
    return next((c for c in candidates if c and os.access(c, os.X_OK)), None)


TILO_CRYPTO = find_tilo_crypto()
TOOLS = ("rpmbuild", "rpm", "rpm2archive", "openssl")
MISSING = [tool for tool in TOOLS if shutil.which(tool) is None] + ([] if TILO_CRYPTO else ["tilo-crypto"])
RPMFUSION = "https://download1.rpmfusion.org/free/fedora/updates/44/x86_64"

# (name, version, files, symlinks, ld.so.conf.d text)
PACKAGES = {
    "libavcodec-freeworld": ("7.1.1", {
        "usr/lib64/libavcodec-freeworld/libavcodec.so.61": "freeworld libavcodec",
    }, {}, "/usr/lib64/libavcodec-freeworld\n"),
    "gstreamer1-plugin-openh264": ("1.26.5", {
        "usr/lib64/gstreamer-1.0/libgstopenh264.so": "plugin",
        "etc/openh264-plugin.conf": "never part of an extension",
    }, {}, None),
    "mesa-va-drivers-freeworld": ("25.2.4", {
        "usr/lib64/mesa-freeworld/libgallium-25.2.4.so": "freeworld gallium",
    }, {
        "usr/lib64/dri-freeworld/radeonsi_drv_video.so": "../mesa-freeworld/libgallium-25.2.4.so",
    }, "/usr/lib64/mesa-freeworld\n"),
}


def run(*argv, **kwargs):
    result = subprocess.run(argv, capture_output=True, text=True, **kwargs)
    if result.returncode != 0:
        raise AssertionError(f"{' '.join(map(str, argv))} failed:\n{result.stdout}\n{result.stderr}")
    return result.stdout


def scratch_root() -> Path:
    base = Path(os.environ.get("TILO_EXTENSION_E2E_DIR", Path.home() / ".cache/tilo-extension-e2e"))
    base.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="run-", dir=base))


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
        Summary: tilo-extension end-to-end fixture
        License: MIT
        BuildArch: x86_64
        AutoReqProv: no

        %description
        Test fixture shaped like {name}.

        %install
        """) + "\n".join(install) + "\n\n%files\n" + "\n".join(listed) + "\n")
    run("rpmbuild", "--define", f"_topdir {top}", "--target", "x86_64", "-bb", str(spec))
    return next((top / "RPMS").rglob(f"{name}-{version}-*.rpm"))


@unittest.skipIf(MISSING, f"needs {', '.join(MISSING)}")
class EndToEnd(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = scratch_root()
        top = cls.scratch / "rpmbuild"
        top.mkdir()
        cls.rpms = {name: build_rpm(top, name, *spec) for name, spec in PACKAGES.items()}
        cls.key = cls.scratch / "release.key"
        cls.public = cls.scratch / "release.pub"
        run("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(cls.key))
        run("openssl", "ec", "-in", str(cls.key), "-pubout", "-out", str(cls.public))
        cls.rogue = cls.scratch / "rogue.key"
        run("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(cls.rogue))

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.scratch, ignore_errors=True)

    def sign(self, data: bytes, key: Path) -> bytes:
        """What `cosign sign-blob --output-signature` writes: base64 DER."""
        message = self.root / "to-sign"
        message.write_bytes(data)
        der = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(key), str(message)],
                             capture_output=True, check=True).stdout
        return subprocess.run(["openssl", "base64", "-A"], input=der, capture_output=True, check=True).stdout

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="root-", dir=self.scratch))
        self.layout = ext.Layout(self.root)
        (self.root / "usr/lib").mkdir(parents=True)
        (self.root / "usr/lib/os-release").write_text("ID=fedora\nVERSION_ID=44\n")
        (self.root / "usr/share/tilo").mkdir(parents=True)
        shutil.copy2(self.public, self.layout.trust_key)
        self.layout.catalogue_dir.mkdir(parents=True)
        shutil.copy2(REPO / "os/usr/share/tilo/system-extensions/sysext.d/media-codecs.json",
                     self.layout.catalogue_dir / "media-codecs.json")
        self.web = {}
        patches = [
            mock.patch.object(ext, "rpm_query_all", lambda layout: list(BASE_PACKAGES)),
            mock.patch.object(ext, "http_open", self.http_open),
            mock.patch.object(ext, "tilo_crypto", lambda: TILO_CRYPTO),
            mock.patch.object(ext, "connectivity", lambda: "full"),
            mock.patch.object(ext, "relabel", lambda tree: None),
            mock.patch.object(ext, "sysext_refresh", lambda layout: None),
            mock.patch.object(ext, "sysext_masked", lambda: True),
            mock.patch.object(ext, "Layout", lambda: self.layout),
            mock.patch.object(ext.os, "geteuid", lambda: 0),
            mock.patch.object(ext.os, "uname", lambda: os.uname_result(("Linux", "h", "7", "v", "x86_64"))),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def http_open(self, url, offset=0):
        if url not in self.web:
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
        response = io.BytesIO(self.web[url][offset:])
        response.status = 206 if offset else 200
        return response

    def publish(self, *, key=None, tamper=None):
        fingerprint = ext.base_packages_sha256(BASE_PACKAGES)
        parts = {"tilo-codecs": ["libavcodec-freeworld", "gstreamer1-plugin-openh264"],
                 "tilo-codecs-mesa": ["mesa-va-drivers-freeworld"]}
        manifest = {
            "type": ext.MANIFEST_TYPE, "schema": 1, "id": "media-codecs", "revision": 20261003120000,
            "created": "2026-10-03T12:00:00Z",
            "base": {"os_id": "fedora", "version_id": "44", "architecture": "x86_64",
                     "packages_sha256": fingerprint},
            "parts": [],
        }
        for part, names in parts.items():
            artifacts = []
            for name in names:
                data = self.rpms[name].read_bytes()
                url = f"{RPMFUSION}/{name[0]}/{self.rpms[name].name}"
                self.web[url] = data
                artifacts.append({"package": self.rpms[name].name.removesuffix(".rpm"), "urls": [url],
                                  "size": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                                  "publisher": "RPM Fusion", "license": "MIT"})
            manifest["parts"].append({"name": part, "format": "rpm", "state": "available",
                                      "requires_base": {}, "artifacts": artifacts})
        data = json.dumps(manifest, indent=2).encode()
        url = (f"https://code.tilo.so/api/packages/tilo/generic/tilo-extension-media-codecs/"
               f"x86_64-{fingerprint}/manifest.json")
        self.web[url] = data
        self.web[url + ".sig"] = self.sign(data, key or self.key)
        if tamper:
            tamper(self.web, url, manifest)
        return manifest

    def install(self):
        code = ext.main(["install:media-codecs"])
        return code, json.loads(self.layout.status("media-codecs").read_text())

    def test_install_assembles_signed_artifacts_and_remove_restores_everything(self):
        self.publish()
        code, status = self.install()
        self.assertEqual(code, 0, status)
        self.assertEqual((status["state"], status["parts"]),
                         ("installed", {"tilo-codecs": "enabled", "tilo-codecs-mesa": "enabled"}))
        codecs = self.layout.extensions / "tilo-codecs"
        self.assertEqual((codecs / "usr/lib64/gstreamer-1.0/libgstopenh264.so").read_text(), "plugin")
        self.assertFalse((codecs / "etc").exists(), "an extension carries only /usr")
        self.assertEqual((codecs / "usr/lib64/libavcodec.so.61").read_text(), "freeworld libavcodec")
        mesa = self.layout.extensions / "tilo-codecs-mesa"
        self.assertEqual((mesa / "usr/lib64/dri-freeworld/radeonsi_drv_video.so").read_text(),
                         "freeworld gallium")
        sysext = shutil.which("systemd-sysext")
        if sysext:
            listing = run(sysext, f"--root={self.root}", "list")
            for name in ("tilo-codecs", "tilo-codecs-mesa"):
                self.assertRegex(listing, rf"(?m)^{name}\s+directory\s", listing)
        # Whatever is written to /var later is replaced by what the signed
        # manifest pins, the next time the parts are activated.
        (codecs / "usr/lib64/libavcodec.so.61").write_text("hijacked")
        self.assertEqual(ext.main(["activate"]), 0)
        self.assertEqual((codecs / "usr/lib64/libavcodec.so.61").read_text(), "freeworld libavcodec")
        self.assertEqual(ext.main(["remove:media-codecs"]), 0)
        status = json.loads(self.layout.status("media-codecs").read_text())
        self.assertEqual(status["state"], "not-installed")
        self.assertEqual(list(self.layout.extensions.iterdir()), [])
        self.assertFalse(self.layout.objects("media-codecs").exists())

    def refused(self, fragment, **publish):
        self.publish(**publish)
        code, status = self.install()
        self.assertEqual(code, 1)
        self.assertIn(fragment, status["last_error"])
        self.assertEqual(status["state"], "failed")
        self.assertFalse((self.layout.extensions / "tilo-codecs").exists())

    def test_a_manifest_signed_by_another_key_is_refused(self):
        self.refused("signature is not valid", key=self.rogue)

    def test_a_manifest_changed_after_signing_is_refused(self):
        def flip(web, url, manifest):
            web[url] = web[url].replace(b'"license": "MIT"', b'"license": "MIU"', 1)
        self.refused("signature is not valid", tamper=flip)

    def test_a_replaced_artifact_is_refused(self):
        def swap(web, url, manifest):
            artifact = manifest["parts"][0]["artifacts"][0]
            original = web[artifact["urls"][0]]
            web[artifact["urls"][0]] = original[:-1] + bytes([original[-1] ^ 1])
        self.refused("does not match the signed manifest", tamper=swap)


if __name__ == "__main__":
    unittest.main()
