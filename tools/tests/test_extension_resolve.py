"""tools/extensions/resolve.py without network, dnf or rpm.

The real run happens in extension-manifests.yml, inside each published image.
"""

import datetime
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import re
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
RESOLVER = REPO / "tools/extensions/resolve.py"
RECIPE = REPO / "tools/extensions/media-codecs/recipe.toml"
DEVICE_HELPER = REPO / "os/usr/libexec/tilo-extension"
CATALOGUE = REPO / "os/usr/share/tilo/system-extensions/sysext.d/media-codecs.json"


def load(path: Path, name: str):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


resolver = load(RESOLVER, "tilo_extension_resolve_under_test")
recipe = resolver.load_recipe(RECIPE)
WHEN = datetime.datetime(2026, 10, 3, 12, 0, 0, tzinfo=datetime.timezone.utc)
IMAGE = "registry.tilo.so/tilo/tilo@sha256:" + "ab" * 32


def nevra(name, version, release="1.fc44", epoch=0, repo="rpmfusion-free", arch="x86_64", location=""):
    return resolver.Nevra(name, epoch, version, release, arch, repo, location)


def base(installed=None, arch="x86_64"):
    packages = {
        "gstreamer1": nevra("gstreamer1", "1.28.7", repo="base", arch=arch),
        "libavcodec-free": nevra("libavcodec-free", "8.1.2", "4.fc44", repo="base", arch=arch),
        "mesa-dri-drivers": nevra("mesa-dri-drivers", "26.2.3", repo="base", arch=arch),
    }
    if installed is not None:
        packages = installed
    return resolver.Base("fedora", "44", arch, packages, "cd" * 32, IMAGE)


def part(name):
    return next(p for p in recipe.parts if p.name == name)


class Fingerprint(unittest.TestCase):
    RPM = ("bash 0:5.3.0-2.fc44.x86_64\n"
           "gpg-pubkey 0:6d9f90a6-6470bd3b.(none)\n"
           "zlib-ng-compat 0:2.2.5-2.fc44.x86_64\n"
           "audit-libs 0:4.1.1-1.fc44.x86_64\n")

    def test_definition(self):
        lines = resolver.base_package_lines(self.RPM)
        self.assertNotIn("gpg-pubkey", "".join(lines))
        expected = hashlib.sha256(
            b"tilo.base-packages.v1\n"
            b"audit-libs 0:4.1.1-1.fc44.x86_64\n"
            b"bash 0:5.3.0-2.fc44.x86_64\n"
            b"zlib-ng-compat 0:2.2.5-2.fc44.x86_64\n").hexdigest()
        self.assertEqual(resolver.base_packages_sha256(lines), expected)

    def test_order_does_not_matter_and_every_field_does(self):
        lines = resolver.base_package_lines(self.RPM)
        value = resolver.base_packages_sha256(lines)
        self.assertEqual(resolver.base_packages_sha256(reversed(lines)), value)
        for changed in ("bash 1:5.3.0-2.fc44.x86_64\n", "bash 0:5.3.1-2.fc44.x86_64\n",
                        "bash 0:5.3.0-3.fc44.x86_64\n", "bash 0:5.3.0-2.fc44.aarch64\n"):
            other = [changed if line.startswith("bash ") else line for line in lines]
            self.assertNotEqual(resolver.base_packages_sha256(other), value, changed)

    def test_sorted_bytewise_not_by_locale(self):
        lines = ["Zeta 0:1-1.x86_64\n", "alpha 0:1-1.x86_64\n", "_x 0:1-1.x86_64\n"]
        expected = hashlib.sha256(b"tilo.base-packages.v1\n" + b"".join(
            sorted(line.encode() for line in lines))).hexdigest()
        self.assertEqual(resolver.base_packages_sha256(lines), expected)
        self.assertEqual(sorted(line.encode() for line in lines)[0], b"Zeta 0:1-1.x86_64\n")

    def test_query_format_is_the_contract(self):
        self.assertEqual(resolver.RPM_FINGERPRINT_QUERY, "%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n")
        readme = (REPO / "os/usr/share/tilo/system-extensions/README.md").read_text()
        self.assertIn("tilo.base-packages.v1\\n", readme)

    @unittest.skipUnless(DEVICE_HELPER.is_file(), "the device helper is not in this tree")
    def test_the_device_computes_the_same_fingerprint(self):
        helper = load(DEVICE_HELPER, "tilo_extension_device_helper_for_fingerprint")
        for lines in (resolver.base_package_lines(self.RPM), [], ["only 0:1-1.noarch\n"]):
            self.assertEqual(helper.base_packages_sha256(lines), resolver.base_packages_sha256(lines))


class VersionComparison(unittest.TestCase):
    def test_rpmvercmp(self):
        cases = [
            ("1.0", "1.0", 0), ("1.0", "1.1", -1), ("1.10", "1.9", 1), ("26.2.3", "26.0.3", 1),
            ("1.0~rc1", "1.0", -1), ("1.0^git1", "1.0", 1), ("1.0^git1", "1.0.1", -1),
            ("1.0a", "1.0", 1), ("2.0", "2.0.0", -1), ("fc44", "fc44", 0), ("1a", "1b", -1),
        ]
        for left, right, expected in cases:
            self.assertEqual(resolver.rpmvercmp(left, right), expected, (left, right))
            self.assertEqual(resolver.rpmvercmp(right, left), -expected, (right, left))

    def test_dnf_query_lines(self):
        line = ("gstreamer1-plugins-ugly 1 1.28.7 1.fc44 x86_64 rpmfusion-free-updates "
                "https://download1.rpmfusion.org/free/fedora/updates/44/x86_64/g/x.rpm")
        parsed = resolver.parse_nevra_line(line)
        self.assertEqual((parsed.epoch, parsed.repo, parsed.spec),
                         (1, "rpmfusion-free-updates", "gstreamer1-plugins-ugly-1:1.28.7-1.fc44.x86_64"))
        self.assertEqual(parsed.nvra, "gstreamer1-plugins-ugly-1.28.7-1.fc44.x86_64")
        self.assertEqual(parsed.epochnum_evr, "1:1.28.7-1.fc44")
        self.assertIsNone(resolver.parse_nevra_line("too few fields"))


class ComponentChoice(unittest.TestCase):
    GST = [nevra("gstreamer1-plugins-ugly", "1.28.1", epoch=1),
           nevra("gstreamer1-plugins-ugly", "1.28.7", epoch=1, repo="rpmfusion-free-updates")]

    def test_exact_version_wins_even_when_newer_exists(self):
        self.assertEqual(resolver.choose_candidate("compatible", "1.28.1", self.GST).version, "1.28.1")

    def test_compatible_falls_back_to_newest_older_same_minor(self):
        self.assertEqual(resolver.choose_candidate("compatible", "1.28.4", self.GST).version, "1.28.1")

    def test_compatible_never_picks_newer_than_base(self):
        self.assertIsNone(resolver.choose_candidate("compatible", "1.28.0", self.GST))

    def test_compatible_never_crosses_minor(self):
        self.assertIsNone(resolver.choose_candidate("compatible", "1.30.0", self.GST))

    def test_newest_ignores_base(self):
        self.assertEqual(resolver.choose_candidate("newest", None, self.GST).version, "1.28.7")

    def test_following_rules_need_a_base_version(self):
        self.assertIsNone(resolver.choose_candidate("compatible", None, self.GST))

    def test_highest_release_of_the_exact_version(self):
        candidates = [nevra("x", "1.0", "1.fc44"), nevra("x", "1.0", "3.fc44"), nevra("x", "1.0", "2.fc44")]
        self.assertEqual(resolver.choose_candidate("exact", "1.0", candidates).release, "3.fc44")


class MesaLockstep(unittest.TestCase):
    """The Mesa part: freeworld drivers built from exactly the base's Mesa version."""

    AVAILABLE = [nevra("mesa-va-drivers-freeworld", "26.0.3"),
                 nevra("mesa-va-drivers-freeworld", "26.2.3", repo="rpmfusion-free-updates")]

    def decide(self, base_mesa, candidates):
        installed = {"mesa-dri-drivers": base_mesa} if base_mesa else {}
        return resolver.decide_part(part("tilo-codecs-mesa"), recipe.components, base(installed),
                                    {"mesa-va-drivers-freeworld": candidates})

    def test_available_on_exact_version_despite_different_release(self):
        decision = self.decide(nevra("mesa-dri-drivers", "26.0.3", "4.fc44", repo="base"), self.AVAILABLE)
        self.assertEqual(decision.state, "available")
        self.assertEqual([n.spec for n in decision.chosen], ["mesa-va-drivers-freeworld-26.0.3-1.fc44.x86_64"])
        self.assertEqual(decision.requires_base, {"mesa-dri-drivers": "0:26.0.3-4.fc44"})
        self.assertEqual(decision.message, "")

    def test_available_latest_update(self):
        decision = self.decide(nevra("mesa-dri-drivers", "26.2.3", repo="base"), self.AVAILABLE)
        self.assertEqual(decision.chosen[0].version, "26.2.3")

    def test_waiting_when_base_is_between_published_builds(self):
        decision = self.decide(nevra("mesa-dri-drivers", "26.1.4", repo="base"), self.AVAILABLE)
        self.assertEqual((decision.state, decision.chosen, decision.requires_base), ("waiting", (), {}))
        self.assertIn("waiting for a matching Mesa build from RPM Fusion", decision.message)
        self.assertIn("mesa-dri-drivers 26.1.4; published: 26.0.3, 26.2.3.", decision.message)

    def test_waiting_when_base_is_newer_than_rpm_fusion(self):
        self.assertEqual(self.decide(nevra("mesa-dri-drivers", "26.2.4", repo="base"), self.AVAILABLE).state,
                         "waiting")

    def test_waiting_when_nothing_is_published(self):
        decision = self.decide(nevra("mesa-dri-drivers", "26.2.3", repo="base"), [])
        self.assertEqual(decision.state, "waiting")
        self.assertIn("published: none.", decision.message)

    def test_unsupported_without_base_mesa(self):
        decision = self.decide(None, self.AVAILABLE)
        self.assertEqual((decision.state, decision.message), ("unsupported", "The base image has no Mesa drivers."))


class CodecsPart(unittest.TestCase):
    OFFERED = {
        "gstreamer1-plugins-bad-freeworld": [nevra("gstreamer1-plugins-bad-freeworld", "1.28.7", epoch=1)],
        "gstreamer1-plugins-ugly": [nevra("gstreamer1-plugins-ugly", "1.28.7", epoch=1)],
        "gstreamer1-plugin-openh264": [nevra("gstreamer1-plugin-openh264", "1.28.7", repo="updates")],
        "openh264": [nevra("openh264", "2.6.0", "3.fc44", repo="fedora-cisco-openh264")],
        "libavcodec-freeworld": [nevra("libavcodec-freeworld", "8.0.1"), nevra("libavcodec-freeworld", "8.1.3")],
        "intel-media-driver": [nevra("intel-media-driver", "26.1.5", repo="rpmfusion-nonfree-updates")],
    }

    def decide(self, the_base, offered=None):
        return resolver.decide_part(part("tilo-codecs"), recipe.components, the_base,
                                    self.OFFERED if offered is None else offered)

    def test_available_with_the_components_still_waiting_named(self):
        decision = self.decide(base())
        self.assertEqual(decision.state, "available")
        self.assertEqual([n.name for n in decision.chosen], [
            "gstreamer1-plugin-openh264", "gstreamer1-plugins-bad-freeworld", "gstreamer1-plugins-ugly",
            "intel-media-driver", "openh264"])
        self.assertEqual(decision.requires_base, {"gstreamer1": "0:1.28.7-1.fc44"})
        self.assertEqual(decision.message, "Not yet available for this release: libavcodec-freeworld needs "
                                           "libavcodec-free 8.1.2; published: 8.0.1, 8.1.3.")

    def test_architecture_limited_components(self):
        arm = base({k: nevra(k, v.version, v.release, repo="base", arch="aarch64")
                    for k, v in base().installed.items()}, arch="aarch64")
        decision = self.decide(arm)
        self.assertNotIn("intel-media-driver", [n.name for n in decision.chosen])

    def test_a_component_the_base_ships_needs_nothing(self):
        installed = dict(base().installed, openh264=nevra("openh264", "2.6.0", "3.fc44", repo="base"))
        decision = self.decide(base(installed))
        self.assertNotIn("openh264", [n.name for n in decision.chosen])

    def test_waiting_when_nothing_matches(self):
        decision = self.decide(base(), {})
        self.assertEqual(decision.state, "waiting")
        self.assertTrue(decision.message.startswith("The codecs are waiting for builds that match this release."))
        self.assertIn("openh264: no build is published.", decision.message)

    def test_requires_base_names_only_followed_packages_of_chosen_components(self):
        offered = dict(self.OFFERED, **{"libavcodec-freeworld": [nevra("libavcodec-freeworld", "8.1.2", "1.fc44")]})
        decision = self.decide(base(), offered)
        self.assertEqual(decision.requires_base, {"gstreamer1": "0:1.28.7-1.fc44",
                                                  "libavcodec-free": "0:8.1.2-4.fc44"})


class Recipe(unittest.TestCase):
    def test_vendored_keys_match_pinned_fingerprints(self):
        for name in ("rpmfusion-free", "rpmfusion-nonfree"):
            key = recipe.keys[name]
            self.assertEqual(resolver.openpgp_fingerprint(key.path(recipe.directory, "44").read_text()),
                             key.pinned("44"), name)

    def test_checksig_parsing(self):
        good = ("/x.rpm:\n"
                "    Header OpenPGP V4 RSA/SHA256 signature, key fingerprint: "
                "e9a491a3de247814e7e067eae06f8ecdd651ff2e: OK\n"
                "    Header SHA256 digest: OK\n    Payload SHA256 digest: OK\n")
        self.assertEqual(resolver.parse_checksig(good), (True, {"e9a491a3de247814e7e067eae06f8ecdd651ff2e"}))
        unsigned = "/x.rpm:\n    Header SHA256 digest: OK\n    Payload SHA256 digest: OK\n"
        self.assertFalse(resolver.parse_checksig(unsigned)[0])
        bad = good.replace("Payload SHA256 digest: OK", "Payload SHA256 digest: BAD")
        self.assertFalse(resolver.parse_checksig(bad)[0])

    def test_each_repository_has_exactly_its_own_key(self):
        free, nonfree = recipe.keys["rpmfusion-free"].pinned("44"), recipe.keys["rpmfusion-nonfree"].pinned("44")
        fedora = recipe.keys["fedora-primary"].pinned("44")
        self.assertEqual(recipe.fingerprints_for("rpmfusion-free-updates", "44"), {free})
        self.assertEqual(recipe.fingerprints_for("rpmfusion-nonfree", "44"), {nonfree})
        self.assertEqual(recipe.fingerprints_for("fedora-cisco-openh264", "44"), {fedora})
        self.assertEqual(recipe.fingerprints_for("fedora-cisco-openh264", "99"), set())
        self.assertEqual(recipe.fingerprints_for("", "44"), set())
        self.assertEqual(recipe.fingerprints_for("terra", "44"), set())

    def test_repo_file_is_canonical_and_matches_the_recipe(self):
        text = recipe.repo_file.read_text()
        self.assertEqual(sorted(resolver.repo_ids(text)), sorted(recipe.repos))
        self.assertNotIn("metalink", text.replace("# ", "").split("[fedora]", 1)[1])
        self.assertEqual(text.count("includepkgs=intel-media-driver"), 2)
        for repo in recipe.repos.values():
            self.assertTrue(repo.baseurl.startswith("https://"), repo.name)
            host = repo.baseurl.split("/")[2]
            self.assertIn(host, {"download1.rpmfusion.org", "download.fedoraproject.org",
                                 "codecs.fedoraproject.org"}, repo.name)

    def test_a_mirror_list_is_refused(self):
        with self.assertRaises(resolver.ResolveError):
            resolver.repo_baseurls("[x]\nmetalink=https://mirrors.example/metalink\n")
        with self.assertRaises(resolver.ResolveError):
            resolver.repo_baseurls("[x]\nbaseurl=http://example/\n")

    def test_parts_and_components_are_consistent(self):
        self.assertEqual([p.name for p in recipe.parts], ["tilo-codecs", "tilo-codecs-mesa"])
        mesa = [c for c in recipe.components if c.part == "tilo-codecs-mesa"]
        self.assertEqual([(c.name, c.rule, c.follows) for c in mesa],
                         [("mesa-va-drivers-freeworld", "exact", "mesa-dri-drivers")])

    @unittest.skipUnless(CATALOGUE.is_file(), "no media-codecs catalogue entry in this tree")
    def test_the_catalogue_entry_names_the_same_parts(self):
        entry = json.loads(CATALOGUE.read_text())
        self.assertEqual(entry["id"], recipe.id)
        self.assertEqual(sorted(entry["parts"]), sorted(p.name for p in recipe.parts))

    @unittest.skipUnless(CATALOGUE.is_file(), "no media-codecs catalogue entry in this tree")
    def test_the_catalogue_allows_every_host_an_artifact_url_can_name(self):
        hosts = set(json.loads(CATALOGUE.read_text())["hosts"])
        for repo in recipe.repos.values():
            for url in (repo.baseurl, *repo.also):
                self.assertIn(url.split("/")[2], hosts, repo.name)


class Urls(unittest.TestCase):
    def test_canonical_url_from_the_repository_location(self):
        offer = nevra("libde265", "1.0.18", location=(
            "https://download1.rpmfusion.org/free/fedora/releases/44/Everything/x86_64/os/"
            "Packages/l/libde265-1.0.18-1.fc44.x86_64.rpm"))
        self.assertEqual(resolver.artifact_urls(recipe, [offer], "44", "x86_64"), [offer.location])

    def test_openh264_also_names_ciscos_own_https_host(self):
        offer = nevra("openh264", "2.6.0", "3.fc44", repo="fedora-cisco-openh264", location=(
            "https://codecs.fedoraproject.org/openh264/44/x86_64/Packages/o/openh264-2.6.0-3.fc44.x86_64.rpm"))
        self.assertEqual(resolver.artifact_urls(recipe, [offer], "44", "x86_64"), [
            offer.location, "https://ciscobinary.openh264.org/openh264-2.6.0-3.fc44.x86_64.rpm"])

    def test_a_mirror_location_is_refused(self):
        offer = nevra("x", "1", repo="updates",
                      location="http://mirror.example/fedora/linux/updates/44/Everything/x86_64/Packages/x/x.rpm")
        with self.assertRaises(resolver.ResolveError):
            resolver.artifact_urls(recipe, [offer], "44", "x86_64")

    def test_an_escaping_location_is_refused(self):
        base_url = "https://download1.rpmfusion.org/free/fedora/releases/44/Everything/x86_64/os/"
        for href in ("../../x.rpm", "a b.rpm", "/x.rpm", "?x=1"):
            with self.assertRaises(resolver.ResolveError, msg=href):
                resolver.artifact_urls(recipe, [nevra("x", "1", location=base_url + href)], "44", "x86_64")

    def test_only_urls_serving_the_exact_bytes_survive(self):
        served = {
            "https://good/a.rpm": (10, "aa" * 32, "https://mirror/a.rpm"),
            "https://stale/a.rpm": (10, "bb" * 32, "https://stale/a.rpm"),
        }
        calls = []

        def fetch(url):
            calls.append(url)
            if url == "https://gone/a.rpm":
                raise urllib.error.URLError("HTTP Error 404: Not Found")
            return served[url]

        log = []
        urls = resolver.live_urls(["https://good/a.rpm", "https://stale/a.rpm", "https://gone/a.rpm"],
                                  10, "aa" * 32, fetch, log.append)
        self.assertEqual(urls, ["https://good/a.rpm"])
        self.assertEqual(calls.count("https://gone/a.rpm"), 3)
        self.assertTrue(any("dropping https://stale/a.rpm" in line for line in log))

    def test_a_redirect_to_plain_http_is_refused(self):
        handler = resolver.HttpsOnly()
        request = urllib.request.Request("https://codecs.fedoraproject.org/x.rpm")
        with self.assertRaises(urllib.error.HTTPError) as refused:
            handler.redirect_request(request, io.BytesIO(), 302, "Found", {},
                                     "http://ciscobinary.openh264.org/x.rpm")
        refused.exception.close()
        followed = handler.redirect_request(request, io.BytesIO(), 302, "Found", {},
                                            "https://ciscobinary.openh264.org/x.rpm")
        self.assertEqual(followed.full_url, "https://ciscobinary.openh264.org/x.rpm")


class FakeDnf:
    """Repository metadata and downloads for two parts, from a scratch directory."""

    def __init__(self, packages):
        # name -> (nevra, payload bytes)
        self.packages = packages
        self.downloads = []
        # specs -> names of the dependencies `download --resolve` adds
        self.dependencies = {}

    def candidates(self, names):
        return {name: [nv for nv, _ in self.packages.get(name, [])] for name in names}

    def download(self, specs, destdir):
        self.downloads.append(list(specs))
        destdir.mkdir(parents=True, exist_ok=True)
        wanted = set(specs)
        for entries in self.packages.values():
            for nv, payload in entries:
                if nv.spec in wanted or nv.name in self.dependencies.get(tuple(specs), ()):
                    (destdir / f"{nv.nvra}.rpm").write_bytes(payload)
        return sorted(destdir.glob("*.rpm"))


class FakeKeyring:
    def __init__(self):
        self.verified = []

    def verify(self, path, repos):
        self.verified.append((path.name, tuple(repos)))
        return "e9a491a3de247814e7e067eae06f8ecdd651ff2e"


def fake_header(path):
    name, version, release, arch = re.fullmatch(r"(.+)-([^-]+)-([^-]+)\.([^.]+)\.rpm", path.name).groups()
    return {"name": name, "epoch": "0", "version": version, "release": release, "arch": arch,
            "license": f"{name} licence", "nvra": f"{name}-{version}-{release}.{arch}"}


RF_UPDATES = "https://download1.rpmfusion.org/free/fedora/updates/44/x86_64/"
FEDORA_UPDATES = "https://download.fedoraproject.org/pub/fedora/linux/updates/44/Everything/x86_64/"


def offer(name, version, repo, base_url, epoch=0):
    nv = nevra(name, version, epoch=epoch, repo=repo)
    filename = f"{nv.nvra}.rpm"
    return resolver.Nevra(nv.name, nv.epoch, nv.version, nv.release, nv.arch, repo,
                          f"{base_url}{name[0]}/{filename}")


class Emission(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        payload = {}

        def package(name, version, repo, base_url, epoch=0):
            nv = offer(name, version, repo, base_url, epoch)
            payload[nv.nvra] = f"payload of {nv.nvra}".encode()
            return nv, payload[nv.nvra]

        self.payload = payload
        self.dnf = FakeDnf({
            "gstreamer1-plugins-ugly": [package("gstreamer1-plugins-ugly", "1.28.7", "rpmfusion-free-updates",
                                                RF_UPDATES, epoch=1)],
            "gstreamer1-plugin-openh264": [package("gstreamer1-plugin-openh264", "1.28.7", "updates",
                                                   FEDORA_UPDATES)],
            "mesa-va-drivers-freeworld": [package("mesa-va-drivers-freeworld", "26.2.3", "rpmfusion-free-updates",
                                                  RF_UPDATES)],
            "x264-libs": [package("x264-libs", "0.165", "rpmfusion-free-updates", RF_UPDATES)],
        })
        # Downloading the codecs part pulls in a dependency the base lacks.
        self.dnf.dependencies = {("gstreamer1-plugin-openh264-1.28.7-1.fc44.x86_64",
                                  "gstreamer1-plugins-ugly-1:1.28.7-1.fc44.x86_64"): ("x264-libs",)}
        self.keyring = FakeKeyring()
        patch = mock.patch.object(resolver, "rpm_header", fake_header)
        patch.start()
        self.addCleanup(patch.stop)

    def fetch(self, url):
        name = url.rsplit("/", 1)[-1].removesuffix(".rpm")
        data = self.payload[name]
        return len(data), hashlib.sha256(data).hexdigest(), url

    def resolve(self, the_base=None, when=WHEN):
        return resolver.resolve(recipe, the_base or base(), self.dnf, self.keyring, Path(self.tmp.name), when,
                                self.fetch, log=lambda _line: None)

    def test_manifest_follows_the_contract(self):
        manifest = self.resolve()
        self.assertEqual(resolver.check_manifest(manifest), [])
        self.assertEqual({k: manifest[k] for k in ("type", "schema", "id", "revision", "created")}, {
            "type": "tilo.system-extension.sysext-manifest", "schema": 1, "id": "media-codecs",
            "revision": 20261003120000, "created": "2026-10-03T12:00:00Z"})
        self.assertEqual(manifest["base"], {"os_id": "fedora", "version_id": "44", "architecture": "x86_64",
                                            "packages_sha256": "cd" * 32, "image": IMAGE})
        codecs, mesa = manifest["parts"]
        self.assertEqual((codecs["name"], codecs["format"], codecs["state"]), ("tilo-codecs", "rpm", "available"))
        self.assertEqual(codecs["requires_base"], {"gstreamer1": "0:1.28.7-1.fc44"})
        self.assertEqual([a["package"] for a in codecs["artifacts"]], [
            "gstreamer1-plugin-openh264-1.28.7-1.fc44.x86_64",
            "gstreamer1-plugins-ugly-1.28.7-1.fc44.x86_64",
            "x264-libs-0.165-1.fc44.x86_64",
        ])
        ugly = codecs["artifacts"][1]
        data = self.payload["gstreamer1-plugins-ugly-1.28.7-1.fc44.x86_64"]
        self.assertEqual(ugly, {
            "package": "gstreamer1-plugins-ugly-1.28.7-1.fc44.x86_64",
            "urls": [RF_UPDATES + "g/gstreamer1-plugins-ugly-1.28.7-1.fc44.x86_64.rpm"],
            "size": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
            "publisher": "RPM Fusion",
            "license": "gstreamer1-plugins-ugly licence",
        })
        self.assertEqual(codecs["artifacts"][0]["publisher"], "Fedora Project")
        self.assertEqual((mesa["name"], mesa["state"], mesa["requires_base"]),
                         ("tilo-codecs-mesa", "available", {"mesa-dri-drivers": "0:26.2.3-1.fc44"}))
        self.assertEqual(self.dnf.downloads, [
            ["gstreamer1-plugin-openh264-1.28.7-1.fc44.x86_64", "gstreamer1-plugins-ugly-1:1.28.7-1.fc44.x86_64"],
            ["mesa-va-drivers-freeworld-26.2.3-1.fc44.x86_64"],
        ])
        self.assertEqual(sorted({repos for _, repos in self.keyring.verified}),
                         [("rpmfusion-free-updates",), ("updates",)])

    def test_output_is_deterministic(self):
        first = resolver.render(self.resolve())
        self.dnf.packages = dict(reversed(list(self.dnf.packages.items())))
        self.assertEqual(resolver.render(self.resolve()), first)
        self.assertTrue(first.endswith(b"}\n"))

    def test_a_waiting_part_has_no_artifacts_and_a_message(self):
        the_base = base(dict(base().installed, **{"mesa-dri-drivers": nevra("mesa-dri-drivers", "26.3.0",
                                                                             repo="base")}))
        mesa = self.resolve(the_base)["parts"][1]
        self.assertEqual((mesa["state"], mesa["artifacts"], mesa["requires_base"]), ("waiting", [], {}))
        self.assertIn("Hardware H.264/HEVC is waiting", mesa["message"])

    def test_a_package_that_would_replace_the_base_is_refused(self):
        the_base = base(dict(base().installed, **{"x264-libs": nevra("x264-libs", "0.164", repo="base")}))
        with self.assertRaisesRegex(resolver.ResolveError, "would replace a base package"):
            self.resolve(the_base)

    def test_a_package_from_an_unknown_repository_is_refused(self):
        nv, data = self.dnf.packages["x264-libs"][0]
        self.dnf.packages["x264-libs"] = [(resolver.Nevra(nv.name, nv.epoch, nv.version, nv.release, nv.arch,
                                                          "terra", nv.location), data)]
        with self.assertRaisesRegex(resolver.ResolveError, "no repository of the recipe"):
            self.resolve()

    def test_a_package_no_url_serves_is_refused(self):
        self.fetch = lambda url: (1, "00" * 32, url)
        with self.assertRaisesRegex(resolver.ResolveError, "no upstream URL serves the verified bytes"):
            self.resolve()


class Publication(unittest.TestCase):
    def manifest(self, **changes):
        manifest = {
            "type": "tilo.system-extension.sysext-manifest", "schema": 1, "id": "media-codecs",
            "revision": 20261003120000, "created": "2026-10-03T12:00:00Z",
            "base": {"os_id": "fedora", "version_id": "44", "architecture": "x86_64",
                     "packages_sha256": "cd" * 32, "image": IMAGE},
            "parts": [{"name": "tilo-codecs", "format": "rpm", "state": "available", "message": "",
                       "requires_base": {"gstreamer1": "0:1.28.7-1.fc44"},
                       "artifacts": [{"package": "x-1-1.fc44.x86_64", "urls": ["https://download1.rpmfusion.org/x"],
                                      "size": 3, "sha256": "ef" * 32, "publisher": "RPM Fusion",
                                      "license": "MIT"}]}],
        }
        manifest.update(changes)
        return manifest

    def test_revision_time_and_image_name_do_not_make_a_new_publication(self):
        old = self.manifest()
        new = self.manifest(revision=20261004120000, created="2026-10-04T12:00:00Z")
        new["base"] = dict(new["base"], image="registry.tilo.so/tilo/tilo-vm@sha256:" + "01" * 32)
        self.assertTrue(resolver.same_publication(old, new))

    def test_any_pinned_change_does(self):
        old = self.manifest()
        for change in (lambda m: m["parts"][0]["artifacts"][0].update(urls=["https://download1.rpmfusion.org/y"]),
                       lambda m: m["parts"][0]["artifacts"][0].update(sha256="aa" * 32),
                       lambda m: m["parts"][0].update(state="waiting"),
                       lambda m: m["base"].update(packages_sha256="00" * 32)):
            new = self.manifest()
            change(new)
            self.assertFalse(resolver.same_publication(old, new))

    def test_check_rejects_what_a_device_would(self):
        self.assertEqual(resolver.check_manifest(self.manifest()), [])
        broken = [
            self.manifest(type="other"),
            self.manifest(revision="20261003120000"),
            self.manifest(parts=[]),
        ]
        bad_base = self.manifest()
        bad_base["base"]["image"] = "registry.tilo.so/tilo/tilo:latest"
        broken.append(bad_base)
        http = self.manifest()
        http["parts"][0]["artifacts"][0]["urls"] = ["http://download1.rpmfusion.org/x"]
        broken.append(http)
        empty = self.manifest()
        empty["parts"][0]["artifacts"] = []
        broken.append(empty)
        for manifest in broken:
            self.assertNotEqual(resolver.check_manifest(manifest), [], manifest)

    def test_command_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            old, new = Path(tmp, "old.json"), Path(tmp, "new.json")
            old.write_bytes(resolver.render(self.manifest()))
            new.write_bytes(resolver.render(self.manifest(revision=20261004000000)))
            run = lambda *args: subprocess.run([sys.executable, str(RESOLVER), *args],  # noqa: E731
                                               capture_output=True, text=True)
            checked = run("check", str(new))
            self.assertEqual((checked.returncode, checked.stdout),
                             (0, f"media-codecs x86_64 {'cd' * 32} 20261004000000\n"))
            self.assertEqual(run("same", str(old), str(new)).returncode, 0)
            changed = self.manifest()
            changed["parts"][0]["state"] = "waiting"
            new.write_bytes(resolver.render(changed))
            self.assertEqual(run("same", str(old), str(new)).returncode, 1)
            self.assertEqual(run("check", str(new)).returncode, 1)


if __name__ == "__main__":
    unittest.main()
