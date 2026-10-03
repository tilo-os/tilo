"""os/usr/libexec/tilo-extension without network, rpm tooling or root.

Artifacts here are small JSON stand-ins for RPMs (`rpm_header` and
`extract_rpm` read them); the network is a dictionary of URLs; the signature
check is replaced by a keyed digest so that "signed by the trusted key",
"signed by another key" and "bytes changed after signing" stay distinct. The
run against real RPMs, rpm2archive and a real P-256 signature is
test_tilo_extension_e2e.py.
"""

import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import re
import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
HELPER = REPO / "os/usr/libexec/tilo-extension"
CATALOGUE = REPO / "os/usr/share/tilo/system-extensions/sysext.d"


def load():
    loader = importlib.machinery.SourceFileLoader("tilo_extension_under_test", str(HELPER))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


ext = load()

BASE_PACKAGES = [
    "gstreamer1 0:1.26.5-1.fc44.x86_64",
    "libavcodec-free 0:7.1.1-3.fc44.x86_64",
    "mesa-dri-drivers 0:25.2.4-1.fc44.x86_64",
    "glibc 0:2.42-4.fc44.x86_64",
    "gpg-pubkey 0:d651ff2e-5dadbbc1.(none)",
]
TRUSTED = b"tilo release key"
ROGUE = b"somebody else"
RPMFUSION = "https://download1.rpmfusion.org/free/fedora/updates/44/x86_64"


def sign(key: bytes, data: bytes) -> bytes:
    return hashlib.sha256(key + data).hexdigest().encode()


def rpm(name, version="1.0", files=None):
    """A stand-in RPM: its header and payload as JSON."""
    nvra = f"{name}-{version}-1.fc44.x86_64"
    return nvra, json.dumps({"name": name, "nvra": nvra, "files": files or {
        f"usr/lib64/{name}.so": f"{name} {version}"}}).encode()


class Machine:
    """A Tilo root in a temporary directory with a fake network."""

    def __init__(self, test: unittest.TestCase):
        self.test = test
        self.tmp = tempfile.TemporaryDirectory()
        test.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.layout = ext.Layout(self.root)
        (self.root / "usr/lib").mkdir(parents=True)
        (self.root / "usr/lib/os-release").write_text('ID=fedora\nVERSION_ID="44"\n')
        (self.root / "usr/share/tilo").mkdir(parents=True)
        (self.root / "usr/share/tilo/extensions.pub").write_text("trusted\n")
        catalogue = self.layout.catalogue_dir
        catalogue.mkdir(parents=True)
        (catalogue / "media-codecs.json").write_text((CATALOGUE / "media-codecs.json").read_text())
        self.packages = list(BASE_PACKAGES)
        self.web: dict[str, bytes] = {}
        self.requests: list[tuple[str, int]] = []
        self.online = "full"
        self.merges = 0
        self.revision = 1
        patches = [
            mock.patch.object(ext, "rpm_query_all", lambda layout: list(self.packages)),
            mock.patch.object(ext, "http_open", self.http_open),
            mock.patch.object(ext, "verify_signature", self.verify),
            mock.patch.object(ext, "connectivity", lambda: self.online),
            mock.patch.object(ext, "rpm_header", self.rpm_header),
            mock.patch.object(ext, "extract_rpm", self.extract_rpm),
            mock.patch.object(ext, "sysext_refresh", self.merge),
            mock.patch.object(ext, "sysext_masked", lambda: True),
            mock.patch.object(ext.os, "uname", lambda: os.uname_result(("Linux", "h", "7", "v", "x86_64"))),
        ]
        for patch in patches:
            patch.start()
            test.addCleanup(patch.stop)

    # -- fakes -------------------------------------------------------------

    def http_open(self, url, offset=0):
        self.requests.append((url, offset))
        if url not in self.web:
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
        data = self.web[url]
        response = io.BytesIO(data[offset:])
        response.status = 206 if offset else 200
        return response

    def verify(self, layout, data, signature, workdir):
        if signature != sign(TRUSTED, data):
            raise ext.PackError("the manifest's signature is not valid for this image; nothing was changed")

    @staticmethod
    def rpm_header(path):
        value = json.loads(Path(path).read_bytes())
        return {"name": value["name"], "nvra": value["nvra"]}

    @staticmethod
    def extract_rpm(path, tree, ld_conf):
        for name, text in json.loads(Path(path).read_bytes())["files"].items():
            target = tree / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)

    def merge(self, layout):
        self.merges += 1

    # -- publishing --------------------------------------------------------

    @property
    def fingerprint(self):
        return ext.base_packages_sha256(self.packages)

    def manifest_url(self, fingerprint=None):
        return ("https://code.tilo.so/api/packages/tilo/generic/tilo-extension-media-codecs/"
                f"x86_64-{fingerprint or self.fingerprint}/manifest.json")

    def artifact(self, payload, url=None, **overrides):
        nvra, data = payload
        url = url or f"{RPMFUSION}/{nvra[0]}/{nvra}.rpm"
        self.web[url] = data
        entry = {"package": nvra, "urls": [url], "size": len(data),
                 "sha256": hashlib.sha256(data).hexdigest(), "publisher": "RPM Fusion",
                 "license": "GPL-2.0-or-later"}
        entry.update(overrides)
        return entry

    def manifest(self, codecs, mesa=None, *, fingerprint=None, key=TRUSTED, revision=None, **base):
        parts = [{"name": "tilo-codecs", "format": "rpm", "state": "available",
                  "requires_base": {"gstreamer1": "0:1.26.5-1.fc44", "libavcodec-free": "0:7.1.1-3.fc44"},
                  "artifacts": codecs}]
        if mesa is None:
            parts.append({"name": "tilo-codecs-mesa", "format": "rpm", "state": "waiting",
                          "message": "Hardware H.264/HEVC is waiting for a matching Mesa build.",
                          "requires_base": {"mesa-dri-drivers": "0:25.2.4-1.fc44"}, "artifacts": []})
        else:
            parts.append({"name": "tilo-codecs-mesa", "format": "rpm", "state": "available",
                          "requires_base": {"mesa-dri-drivers": "0:25.2.4-1.fc44"}, "artifacts": mesa})
        if revision is None:
            revision = self.revision
            self.revision += 1
        document = {
            "type": ext.MANIFEST_TYPE, "schema": 1, "id": "media-codecs", "revision": revision,
            "created": "2026-10-03T12:00:00Z",
            "base": {"os_id": "fedora", "version_id": "44", "architecture": "x86_64",
                     "packages_sha256": fingerprint or self.fingerprint,
                     "image": "registry.tilo.so/tilo/tilo@sha256:" + "a" * 64, **base},
            "parts": parts,
        }
        return self.publish(json.dumps(document, indent=2).encode(), key=key, fingerprint=fingerprint)

    def publish(self, data, *, key=TRUSTED, fingerprint=None):
        url = self.manifest_url(fingerprint)
        self.web[url] = data
        self.web[url + ".sig"] = sign(key, data)
        return data

    # -- driving -----------------------------------------------------------

    def run(self, verb, ident="media-codecs"):
        with mock.patch.object(ext, "Layout", lambda: self.layout), \
                mock.patch.object(ext.os, "geteuid", lambda: 0):
            return ext.main([f"{verb}:{ident}" if verb != "activate" else "activate"])

    def status(self):
        return json.loads(self.layout.status("media-codecs").read_text())

    def part(self, name="tilo-codecs"):
        return self.layout.extensions / name

    def release(self, name="tilo-codecs"):
        return ext.parse_os_release(
            (self.part(name) / f"usr/lib/extension-release.d/extension-release.{name}").read_text())


class Arguments(unittest.TestCase):
    def test_accepts_verb_and_catalogue_id(self):
        self.assertEqual(ext.parse_command(["install:media-codecs"]), ("install", "media-codecs"))
        self.assertEqual(ext.parse_command(["refresh:all"]), ("refresh", "all"))
        self.assertEqual(ext.parse_command(["activate"]), ("activate", ""))

    def test_rejects_everything_else(self):
        for argv in ([], ["install"], ["install", "media-codecs"], ["Install:media-codecs"],
                     ["install:../x"], ["install:Media"], ["install:all"], ["remove:all"],
                     ["upgrade:media-codecs"], ["install:media-codecs;rm"], ["activate:x"], ["guard"]):
            with self.assertRaises(ext.PackError, msg=argv):
                ext.parse_command(argv)

    def test_polkit_rule_matches_the_helper_and_the_unit(self):
        rule = (REPO / "os/etc/polkit-1/rules.d/49-tilo-extension.rules").read_text()
        verbs = re.search(r"tilo-extension@\(([a-z|]+)\)", rule).group(1).split("|")
        self.assertEqual(set(verbs), {"install", "remove", "refresh"})
        self.assertTrue(set(verbs) <= set(ext.VERBS))
        unit = (REPO / "os/usr/lib/systemd/system/tilo-extension@.service").read_text()
        self.assertIn("ExecStart=/usr/libexec/tilo-extension %i\n", unit)

    def test_unknown_id_is_refused(self):
        machine = Machine(self)
        self.assertEqual(machine.run("install", "nvidia"), 1)
        self.assertFalse(machine.layout.desired("nvidia").exists())


class Fingerprint(unittest.TestCase):
    def test_order_and_gpg_keys_do_not_matter(self):
        shuffled = list(reversed(BASE_PACKAGES))[1:] + ["gpg-pubkey 0:aaaa-bbbb.(none)"]
        self.assertEqual(ext.base_packages_sha256(BASE_PACKAGES), ext.base_packages_sha256(shuffled))

    def test_any_package_change_changes_it(self):
        changed = [line.replace("25.2.4-1", "25.2.4-2") for line in BASE_PACKAGES]
        self.assertNotEqual(ext.base_packages_sha256(BASE_PACKAGES), ext.base_packages_sha256(changed))

    def test_definition_is_pinned(self):
        lines = ["b 0:1-1.x86_64\n", "a 1:2-3.noarch\n"]
        expected = hashlib.sha256(b"tilo.base-packages.v1\na 1:2-3.noarch\nb 0:1-1.x86_64\n").hexdigest()
        self.assertEqual(ext.base_packages_sha256(lines), expected)


class Catalogue(unittest.TestCase):
    def test_shipped_entries_parse(self):
        ids = sorted(p.stem for p in CATALOGUE.glob("*.json"))
        self.assertIn("media-codecs", ids)
        for ident in ids:
            entry = ext.Catalogue.parse((CATALOGUE / f"{ident}.json").read_text(), ident)
            self.assertTrue(entry.manifest.startswith("https://code.tilo.so/"))
            self.assertNotIn("code.tilo.so", entry.hosts, "Tilo must not serve extension payloads")

    def test_entry_must_name_itself_and_pin_the_base(self):
        good = json.loads((CATALOGUE / "media-codecs.json").read_text())
        for change in ({"id": "other"}, {"manifest": "http://code.tilo.so/{arch}-{base}"},
                       {"manifest": "https://code.tilo.so/{arch}/manifest.json"},
                       {"hosts": ["evil host"]}, {"parts": ["Bad Part"]}, {"run": "sh"}):
            with self.assertRaises(ext.PackError, msg=change):
                ext.Catalogue.parse(json.dumps({**good, **change}), "media-codecs")


class Install(unittest.TestCase):
    def test_installs_verified_parts_and_reports_status(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld", "7.1.1")),
                          machine.artifact(rpm("x265-libs", "4.1"))])
        self.assertEqual(machine.run("install"), 0)
        self.assertEqual((machine.part() / "usr/lib64/libavcodec-freeworld.so").read_text(),
                         "libavcodec-freeworld 7.1.1")
        release = machine.release()
        self.assertEqual((release["ID"], release["VERSION_ID"], release["ARCHITECTURE"]),
                         ("fedora", "44", "x86-64"))
        self.assertEqual(release["TILO_BASE_PACKAGES"], machine.fingerprint)
        status = machine.status()
        self.assertEqual(status["state"], "installed")
        self.assertTrue(status["desired"])
        self.assertEqual(status["parts"], {"tilo-codecs": "enabled", "tilo-codecs-mesa": "waiting"})
        self.assertIn("waiting for a matching Mesa build", status["message"])
        self.assertEqual(status["generation"], 1)
        self.assertEqual(machine.merges, 1)
        self.assertFalse(machine.part("tilo-codecs-mesa").exists())
        objects = machine.layout.objects("media-codecs")
        self.assertEqual(len(list(objects.iterdir())), 2, "verified downloads are kept for activation")
        self.assertFalse((machine.layout.cache("media-codecs") / "partial").exists())
        record = json.loads((machine.part() / "usr/share/tilo-extension/tilo-codecs.json").read_text())
        self.assertEqual(record["packages"], ["libavcodec-freeworld", "x265-libs"])
        self.assertEqual(record["requires_base"]["gstreamer1"], "0:1.26.5-1.fc44")

    def test_refresh_without_changes_neither_downloads_nor_remerges(self):
        machine = Machine(self)
        codecs = [machine.artifact(rpm("libavcodec-freeworld"))]
        machine.manifest(codecs)
        self.assertEqual(machine.run("install"), 0)
        machine.requests.clear()
        machine.manifest(codecs)
        self.assertEqual(machine.run("refresh"), 0)
        self.assertEqual([url for url, _ in machine.requests if url.endswith(".rpm")], [])
        self.assertEqual(machine.merges, 1)
        self.assertEqual(machine.status()["revision"], 2)

    def test_refresh_without_opt_in_does_nothing(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))])
        self.assertEqual(machine.run("refresh"), 0)
        self.assertEqual(machine.requests, [])
        self.assertEqual(machine.status()["state"], "not-installed")

    def test_mesa_part_arrives_when_published(self):
        machine = Machine(self)
        codecs = [machine.artifact(rpm("libavcodec-freeworld"))]
        machine.manifest(codecs)
        machine.run("install")
        machine.manifest(codecs, [machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))])
        self.assertEqual(machine.run("refresh"), 0)
        self.assertTrue(machine.part("tilo-codecs-mesa").is_dir())
        self.assertEqual(machine.status()["parts"]["tilo-codecs-mesa"], "enabled")
        self.assertEqual(machine.status()["message"], "")


class Refusals(unittest.TestCase):
    """Every mismatch refuses the whole transaction and changes nothing."""

    def assert_refused(self, machine, fragment):
        self.assertEqual(machine.run("install"), 1)
        status = machine.status()
        self.assertEqual(status["state"], "failed")
        self.assertIn(fragment, status["last_error"])
        self.assertFalse(machine.part().exists())
        self.assertEqual(machine.merges, 0)

    def test_manifest_signed_by_another_key(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))], key=ROGUE)
        self.assert_refused(machine, "signature is not valid")

    def test_manifest_changed_after_signing(self):
        machine = Machine(self)
        data = machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))])
        url = machine.manifest_url()
        machine.web[url] = data.replace(b'"size": ', b'"size": 1')
        self.assert_refused(machine, "signature is not valid")

    def test_artifact_bytes_do_not_match_the_pin(self):
        machine = Machine(self)
        entry = machine.artifact(rpm("libavcodec-freeworld"))
        machine.manifest([entry])
        evil = json.dumps({"name": "libavcodec-freeworld", "nvra": entry["package"],
                           "files": {"usr/lib64/libavcodec.so": "evil"}}).encode()
        machine.web[entry["urls"][0]] = evil[:entry["size"]].ljust(entry["size"], b" ")
        self.assert_refused(machine, "does not match the signed manifest")

    def test_artifact_larger_than_the_pin(self):
        machine = Machine(self)
        entry = machine.artifact(rpm("libavcodec-freeworld"))
        machine.manifest([entry])
        machine.web[entry["urls"][0]] += b"more"
        self.assert_refused(machine, "larger than the signed manifest")

    def test_manifest_for_another_base(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))])
        other = ext.base_packages_sha256(BASE_PACKAGES + ["extra 0:1-1.x86_64"])
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))], fingerprint=other)
        machine.web[machine.manifest_url()] = machine.web[machine.manifest_url(other)]
        machine.web[machine.manifest_url() + ".sig"] = machine.web[machine.manifest_url(other) + ".sig"]
        self.assert_refused(machine, "for another base")

    def test_manifest_for_another_release(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))], version_id="45")
        self.assert_refused(machine, "for another base")

    def test_artifact_from_a_host_the_image_does_not_allow(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"),
                                           url="https://code.tilo.so/mirror/libavcodec.rpm")])
        self.assert_refused(machine, "not allowed for this extension")

    def test_artifact_over_plain_http(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"),
                                           url="http://download1.rpmfusion.org/x.rpm")])
        self.assert_refused(machine, "not plain HTTPS")

    def test_part_the_image_does_not_know(self):
        machine = Machine(self)
        data = json.loads(machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))]))
        data["parts"][0]["name"] = "tilo-anything"
        machine.publish(json.dumps(data).encode())
        self.assert_refused(machine, "part this image does not allow")

    def test_unknown_fields_such_as_commands(self):
        machine = Machine(self)
        data = json.loads(machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))]))
        data["parts"][0]["post_install"] = "rm -rf /"
        machine.publish(json.dumps(data).encode())
        self.assert_refused(machine, "unknown fields")

    def test_package_that_replaces_a_base_package(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("glibc", "2.99"))])
        self.assert_refused(machine, "would replace a package of this system")

    def test_file_that_is_not_the_named_package(self):
        machine = Machine(self)
        entry = machine.artifact(rpm("x265-libs"))
        entry["package"] = "libavcodec-freeworld-1.0-1.fc44.x86_64"
        machine.manifest([entry])
        self.assert_refused(machine, "the file is x265-libs")

    def test_older_revision_than_already_accepted(self):
        machine = Machine(self)
        codecs = [machine.artifact(rpm("libavcodec-freeworld"))]
        machine.manifest(codecs, revision=5)
        self.assertEqual(machine.run("install"), 0)
        machine.manifest(codecs, revision=4)
        self.assertEqual(machine.run("refresh"), 1)
        status = machine.status()
        self.assertEqual(status["state"], "installed", "the working extension stays")
        self.assertIn("refusing a downgrade", status["last_error"])
        self.assertTrue(machine.part().is_dir())

    def test_failed_refresh_keeps_the_working_extension(self):
        machine = Machine(self)
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))])
        machine.run("install")
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld", "2.0"))], key=ROGUE)
        self.assertEqual(machine.run("refresh"), 1)
        self.assertEqual(machine.status()["state"], "installed")
        self.assertEqual((machine.part() / "usr/lib64/libavcodec-freeworld.so").read_text(),
                         "libavcodec-freeworld 1.0")


class Downloads(unittest.TestCase):
    def test_resumes_a_partial_download_by_offset(self):
        machine = Machine(self)
        entry = machine.artifact(rpm("libavcodec-freeworld"))
        machine.manifest([entry])
        partial = machine.layout.cache("media-codecs") / "partial"
        partial.mkdir(parents=True)
        data = machine.web[entry["urls"][0]]
        (partial / entry["sha256"]).write_bytes(data[:10])
        self.assertEqual(machine.run("install"), 0)
        self.assertIn((entry["urls"][0], 10), machine.requests)

    def test_falls_back_to_the_next_url(self):
        machine = Machine(self)
        nvra, data = rpm("libavcodec-freeworld")
        entry = machine.artifact((nvra, data))
        entry["urls"].insert(0, "https://download1.rpmfusion.org/gone.rpm")
        machine.manifest([entry])
        self.assertEqual(machine.run("install"), 0)

    def test_unpublished_base_is_reported_and_retried_later(self):
        machine = Machine(self)
        self.assertEqual(machine.run("install"), 1)
        status = machine.status()
        self.assertEqual(status["state"], "failed")
        self.assertTrue(status["desired"], "the choice is kept for the daily retry")
        self.assertIn("Not available for this version of Tilo yet", status["last_error"])

    def test_offline_opt_in_waits_without_an_error(self):
        machine = Machine(self)
        machine.online = "none"
        self.assertEqual(machine.run("install"), 0)
        status = machine.status()
        self.assertEqual(status["state"], "waiting-for-network")
        self.assertEqual(status["last_error"], "")
        self.assertEqual(machine.requests, [])


class Lifecycle(unittest.TestCase):
    def install(self, machine, mesa=True):
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))],
                         [machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))] if mesa else None)
        self.assertEqual(machine.run("install"), 0)

    def test_remove_deletes_everything_and_unmerges(self):
        machine = Machine(self)
        self.install(machine)
        self.assertEqual(machine.run("remove"), 0)
        self.assertFalse(machine.part().exists())
        self.assertFalse(machine.part("tilo-codecs-mesa").exists())
        self.assertFalse(machine.layout.desired("media-codecs").exists())
        self.assertFalse(machine.layout.objects("media-codecs").exists())
        self.assertFalse(machine.layout.bases("media-codecs").exists())
        status = machine.status()
        self.assertEqual(status["state"], "not-installed")
        self.assertEqual(status["parts"], {"tilo-codecs": "off", "tilo-codecs-mesa": "off"})
        self.assertEqual(machine.merges, 2)

    def test_activation_rebuilds_parts_from_verified_bytes(self):
        machine = Machine(self)
        self.install(machine)
        library = machine.part() / "usr/lib64/libavcodec-freeworld.so"
        library.write_text("replaced behind the helper's back")
        (machine.part() / "usr/lib64/extra.so").write_text("added behind its back")
        self.assertEqual(machine.run("activate"), 0)
        self.assertEqual(library.read_text(), "libavcodec-freeworld 1.0")
        self.assertFalse((machine.part() / "usr/lib64/extra.so").exists())

    def test_rebuilding_keeps_the_times_media_processes_compare(self):
        machine = Machine(self)
        self.install(machine)
        release = machine.part() / "usr/lib/extension-release.d/extension-release.tilo-codecs"
        before = release.stat().st_mtime
        machine.run("activate")
        self.assertEqual(release.stat().st_mtime, before)

    def test_activation_refuses_a_tampered_object(self):
        machine = Machine(self)
        self.install(machine, mesa=False)
        for path in machine.layout.objects("media-codecs").iterdir():
            path.write_bytes(path.read_bytes().replace(b"1.0", b"6.6"))
        machine.run("activate")
        self.assertFalse(machine.part().exists())
        self.assertEqual(machine.status()["parts"]["tilo-codecs"], "waiting")

    def test_activation_refuses_a_tampered_kept_manifest(self):
        machine = Machine(self)
        self.install(machine)
        kept = next(machine.layout.bases("media-codecs").iterdir()) / "manifest.json"
        kept.write_bytes(kept.read_bytes().replace(b'"license": "GPL', b'"license": "MIT GPL'))
        machine.run("activate")
        self.assertFalse(machine.part().exists())
        self.assertFalse(machine.part("tilo-codecs-mesa").exists())

    def test_activation_merges_nothing_beside_a_foreign_extension(self):
        machine = Machine(self)
        self.install(machine)
        foreign = machine.layout.extensions / "evil"
        (foreign / "usr/lib/extension-release.d").mkdir(parents=True)
        self.assertEqual(machine.run("activate"), 1)
        self.assertFalse(machine.part().exists())
        self.assertIn("unexpected system extensions", machine.status()["last_error"])

    def test_development_image_merges_its_own_layers_too(self):
        machine = Machine(self)
        self.install(machine)
        (machine.layout.extensions / "tilo-vm-layer/usr").mkdir(parents=True)
        with mock.patch.object(ext, "sysext_masked", lambda: False):
            self.assertEqual(machine.run("activate"), 0)
        self.assertTrue(machine.part().is_dir())

    def test_parts_stay_across_an_update_that_keeps_their_base_packages(self):
        machine = Machine(self)
        self.install(machine)
        machine.packages.append("new-app 0:1-1.fc44.x86_64")
        machine.run("activate")
        self.assertTrue(machine.part().is_dir())
        self.assertTrue(machine.part("tilo-codecs-mesa").is_dir())

    def test_a_part_whose_base_package_changed_waits_and_returns_on_rollback(self):
        machine = Machine(self)
        self.install(machine)
        booted = list(machine.packages)
        machine.packages = [p.replace("mesa-dri-drivers 0:25.2.4", "mesa-dri-drivers 0:25.3.0")
                            for p in booted]
        machine.run("activate")
        self.assertTrue(machine.part().is_dir(), "the software codecs still fit")
        self.assertFalse(machine.part("tilo-codecs-mesa").exists())
        self.assertEqual(machine.status()["parts"]["tilo-codecs-mesa"], "waiting")
        # Rolling back to the deployment it was built for brings it back, offline.
        machine.packages = booted
        machine.run("activate")
        self.assertTrue(machine.part("tilo-codecs-mesa").is_dir())
        self.assertEqual(machine.status()["parts"]["tilo-codecs-mesa"], "enabled")

    def test_a_part_whose_package_the_base_now_ships_waits(self):
        machine = Machine(self)
        self.install(machine, mesa=False)
        machine.packages.append("libavcodec-freeworld 0:1.0-1.fc44.x86_64")
        machine.run("activate")
        self.assertFalse(machine.part().exists())

    def test_nothing_is_merged_on_another_os_release(self):
        machine = Machine(self)
        self.install(machine)
        (machine.root / "usr/lib/os-release").write_text("ID=fedora\nVERSION_ID=45\n")
        machine.run("activate")
        self.assertFalse(machine.part().exists())
        self.assertFalse(machine.part("tilo-codecs-mesa").exists())

    def test_parts_nothing_vouches_for_are_removed(self):
        machine = Machine(self)
        stray = machine.part()
        (stray / "usr/lib/extension-release.d").mkdir(parents=True)
        machine.run("activate")
        self.assertFalse(stray.exists())

    def test_refresh_after_an_update_rebuilds_for_the_new_base(self):
        machine = Machine(self)
        self.install(machine)
        machine.packages = [p.replace("gstreamer1 0:1.26.5", "gstreamer1 0:1.26.6") for p in machine.packages]
        machine.run("activate")
        self.assertFalse(machine.part().exists())
        data = json.loads(machine.manifest([machine.artifact(rpm("libavcodec-freeworld", "1.1"))],
                                          [machine.artifact(rpm("mesa-va-drivers-freeworld", "25.2.4"))]))
        data["parts"][0]["requires_base"]["gstreamer1"] = "0:1.26.6-1.fc44"
        machine.publish(json.dumps(data).encode())
        self.assertEqual(machine.run("refresh"), 0)
        self.assertEqual(machine.release()["TILO_BASE_PACKAGES"], machine.fingerprint)
        self.assertEqual(machine.status()["parts"], {"tilo-codecs": "enabled", "tilo-codecs-mesa": "enabled"})
        self.assertEqual(len(list(machine.layout.bases("media-codecs").iterdir())), 2,
                         "the previous base's manifest stays for a rollback")

    def test_installer_seed_is_adopted_once(self):
        machine = Machine(self)
        seed = machine.layout.seed("media-codecs")
        seed.parent.mkdir(parents=True)
        seed.write_text("owner opted in during installation\n")
        machine.manifest([machine.artifact(rpm("libavcodec-freeworld"))])
        self.assertEqual(machine.run("refresh", "all"), 0)
        self.assertEqual(machine.status()["state"], "installed")
        machine.run("remove")
        self.assertEqual(machine.run("refresh", "all"), 0)
        self.assertEqual(machine.status()["state"], "not-installed", "a removal sticks")


class Shipped(unittest.TestCase):
    def test_activation_unit_merges_in_the_host_namespace(self):
        unit = (REPO / "os/usr/lib/systemd/system/tilo-extension-activate.service").read_text()
        self.assertIn("ExecStart=/usr/libexec/tilo-extension activate\n", unit)
        settings = [line.split("=", 1)[0] for line in unit.splitlines() if "=" in line and not line.startswith("#")]
        for namespaced in ("ProtectSystem", "PrivateTmp", "ProtectHome", "PrivateDevices", "ReadWritePaths",
                           "PrivateMounts", "ProtectKernelModules", "TemporaryFileSystem"):
            self.assertNotIn(namespaced, settings, f"{namespaced} would hide the merge from the host")
        helper = HELPER.read_text()
        self.assertIn('run(["systemctl", "start", "tilo-extension-activate.service"])', helper)

    def test_manifests_verify_only_against_the_extension_key(self):
        self.assertEqual(str(ext.Layout().trust_key), "/usr/share/tilo/extensions.pub")
        shipped = (REPO / "keys/extensions.pub").read_text()
        self.assertNotEqual(shipped, (REPO / "keys/cosign.pub").read_text())
        self.assertIn("COPY keys/extensions.pub /usr/share/tilo/extensions.pub", (REPO / "os/Containerfile").read_text())
        self.assertNotIn("cosign.pub", HELPER.read_text())

    def test_sealed_image_still_masks_the_boot_time_merge(self):
        containerfile = (REPO / "os/Containerfile").read_text()
        self.assertIn("systemctl mask systemd-sysext.service systemd-confext.service systemd-sysext.socket",
                      containerfile)
        self.assertNotIn("enable systemd-sysext", HELPER.read_text())

    def test_timer_and_update_hook_refresh_every_extension(self):
        timer = (REPO / "os/usr/lib/systemd/system/tilo-extension-refresh.timer").read_text()
        self.assertIn("Unit=tilo-extension@refresh:all.service", timer)
        hook = (REPO / "os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/"
                "40-tilo-extension.conf").read_text()
        self.assertIn("tilo-extension@refresh:all.service", hook)

    def test_dispatcher_starts_refresh_for_a_pending_opt_in(self):
        script = (REPO / "os/usr/lib/NetworkManager/dispatcher.d/90-tilo-extension").read_text()
        self.assertIn("tilo-extension@refresh:all.service", script)
        self.assertIn("SEED_DIR=/etc/tilo/extensions/opt-in", script)
        layout = ext.Layout()
        self.assertEqual(str(layout.seed("x").parent), "/etc/tilo/extensions/opt-in")
        self.assertEqual(str(layout.state_root), "/var/lib/tilo/extensions")

    def test_installer_seeds_the_path_the_helper_adopts(self):
        script = (REPO / "os/usr/libexec/tilo-install").read_text()
        self.assertIn('SEED_DIR="$DEPLOY_ETC/tilo/extensions/opt-in"', script)
        self.assertIn('/usr/share/tilo/system-extensions/sysext.d/${2}.json', script)


if __name__ == "__main__":
    unittest.main()
