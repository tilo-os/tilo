"""Exact-package evidence and exact-source Desktop catalog consumption."""
import hashlib
import importlib.util
import io
import json
import re
from pathlib import Path
import tempfile
import tomllib
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from urllib.request import Request


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("desktop_catalog_release", Path(__file__).with_name("desktop-catalog-release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)
SOURCE = "a" * 40


class DesktopCatalogReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "package.rpm"
        self.package.write_bytes(b"TEST PACKAGE ONLY")
        self.report = self.root / "native-report.md"
        self.report.write_text("TEST FIXTURE ONLY: reviewed native journey report.\n")
        evidence = {"schema": 1, "reference": self.report.name,
                    "package_sha256": hashlib.sha256(self.package.read_bytes()).hexdigest(),
                    **{key: True for key in release.generator.EVIDENCE_CHECKS}}
        self.manifest = {"schema": 1, "tag": "desktop-v0.0.0-fixture", "releases": [{
            "id": "fedora-44-fixture", "version": "0.0.0-fixture", "distro_id": "fedora", "distro_version": "44",
            "architecture": "x86_64", "package_format": "rpm", "url": "https://code.tilo.so/test/package.rpm",
            "artifact": self.package.name, "verified_runtime": evidence}]}
        self.manifest_path = self.root / "review.json"
        self.catalog_path = self.root / "catalog.json"
        self.output = self.root / "verified"
        self.write_inputs()

    def write_inputs(self):
        self.manifest_path.write_text(json.dumps(self.manifest))
        self.catalog_path.write_text(json.dumps(release.generator.build(self.manifest, self.root)))

    def test_verified_bytes_reach_consumer_only_for_the_workflow_source(self):
        release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE)
        copied = self.root / "consumer/desktop-catalog.json"
        release.consume(self.output, SOURCE, copied)
        self.assertEqual(copied.read_bytes(), self.catalog_path.read_bytes())
        copied.write_bytes(b"previous catalog")
        with self.assertRaisesRegex(ValueError, "another workflow source"):
            release.consume(self.output, "b" * 40, copied)
        self.assertEqual(copied.read_bytes(), b"previous catalog")
        (self.output / "desktop-catalog.json").write_bytes(b"modified artifact")
        with self.assertRaisesRegex(ValueError, "differs from its verified receipt"):
            release.consume(self.output, SOURCE, copied)
        self.assertEqual(copied.read_bytes(), b"previous catalog")

    def test_explicit_unreleased_catalog_needs_no_package_or_review(self):
        self.catalog_path.write_text(json.dumps({"schema": 1, "tag": "unreleased", "releases": []}))
        with patch.object(release, "fetch_package", side_effect=AssertionError("empty catalog must not fetch")):
            release.prepare(self.catalog_path, None, self.output, SOURCE, fetch=True)
        release.consume(self.output, SOURCE, self.root / "empty.json")
        self.assertEqual(json.loads((self.root / "empty.json").read_text())["releases"], [])

    def test_nonempty_catalog_requires_exact_packages_and_complete_reviewed_evidence(self):
        with self.assertRaisesRegex(ValueError, "reviewed release manifest"):
            release.prepare(self.catalog_path, None, self.output, SOURCE)
        self.package.write_bytes(b"different package bytes")
        with self.assertRaisesRegex(ValueError, "does not match artifact digest"):
            release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE)
        self.assertFalse(self.output.exists())
        self.package.write_bytes(b"TEST PACKAGE ONLY")
        for key in release.generator.EVIDENCE_CHECKS:
            with self.subTest(key=key):
                self.manifest["releases"][0]["verified_runtime"][key] = False
                self.manifest_path.write_text(json.dumps(self.manifest))
                with self.assertRaisesRegex(ValueError, "complete every native check"):
                    release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE)
                self.assertFalse(self.output.exists())
                self.manifest["releases"][0]["verified_runtime"][key] = True
        self.manifest_path.write_text(json.dumps(self.manifest))
        self.report.unlink()
        with self.assertRaises(OSError):
            release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE)

    def test_preparation_refuses_catalog_drift_and_invalid_source(self):
        catalog = json.loads(self.catalog_path.read_text())
        catalog["releases"][0]["byte_size"] += 1
        self.catalog_path.write_text(json.dumps(catalog))
        with self.assertRaisesRegex(ValueError, "differs from its reviewed"):
            release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE)
        with self.assertRaisesRegex(ValueError, "full workflow source revision"):
            release.prepare(self.catalog_path, self.manifest_path, self.output, "main")
        self.assertFalse(self.output.exists())

    def test_catalog_json_types_and_fields_are_not_coerced(self):
        for field, value in [("schema", True), ("byte_size", float(len(self.package.read_bytes()))),
                             ("extra", "unreviewed")]:
            with self.subTest(field=field):
                catalog = release.generator.build(self.manifest, self.root)
                if field == "schema":
                    catalog[field] = value
                else:
                    catalog["releases"][0][field] = value
                self.catalog_path.write_text(json.dumps(catalog))
                with self.assertRaises(ValueError):
                    release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE)
                self.assertFalse(self.output.exists())

    def test_download_is_bounded_and_digest_verified_before_generation(self):
        expected = json.loads(self.catalog_path.read_text())["releases"][0]
        for data, valid in [(self.package.read_bytes(), True), (b"x" * expected["byte_size"], False),
                            (self.package.read_bytes() + b"oversized", False)]:
            with self.subTest(valid=valid, size=len(data)):
                opener = SimpleNamespace(open=lambda request, timeout: io.BytesIO(data))
                with patch.object(release, "build_opener", return_value=opener):
                    if valid:
                        release.prepare(self.catalog_path, self.manifest_path, self.output, SOURCE, fetch=True)
                    else:
                        with self.assertRaises(ValueError):
                            release.fetch_package(self.manifest["releases"][0], expected, self.root / "download")

    def test_redirects_never_forward_repository_token_to_other_origins(self):
        request = Request("https://code.tilo.so/test/package.rpm", headers={"Authorization": "token TEST_ONLY"})
        handler = release.PackageRedirect()
        redirected = handler.redirect_request(request, None, 302, "redirect", {}, "https://cdn.example.test/package")
        self.assertIsNone(redirected.get_header("Authorization"))
        same_origin = handler.redirect_request(request, None, 302, "redirect", {}, "https://code.tilo.so/attachment")
        self.assertEqual(same_origin.get_header("Authorization"), "token TEST_ONLY")
        for target in ["http://cdn.example.test/package", "https://user:password@cdn.example.test/package"]:
            with self.assertRaises(ValueError):
                handler.redirect_request(request, None, 302, "redirect", {}, target)

    def test_every_native_imager_lane_consumes_the_prepared_exact_source_catalog(self):
        workflow = (ROOT / ".forgejo/workflows/release-imager.yml").read_text()
        catalog = workflow.split("\n  catalog:\n", 1)[1].split("\n  linux:\n", 1)[0]
        self.assertIn("desktop-catalog-release.py prepare", catalog)
        self.assertIn("--review-manifest apps/imager/packaging/desktop-release-manifest.json", catalog)
        self.assertLess(catalog.index("desktop-catalog-release.py prepare"), catalog.index("name: tilo-desktop-catalog"))
        for lane in ("linux", "windows", "macos"):
            block = re.split(r"\n  [a-z][a-z0-9-]*:\n", workflow.split(f"\n  {lane}:\n", 1)[1], maxsplit=1)[0]
            self.assertIn("name: tilo-desktop-catalog", block)
            self.assertIn("desktop-catalog-release.py consume desktop-catalog", block)
            self.assertIn("--copy-to apps/imager/desktop-catalog.json", block)
            self.assertIn("--source ", block)
            self.assertLess(block.index("desktop-catalog-release.py consume"), block.index("package-host."))

    def test_published_desktop_packages_carry_their_login_test_and_its_exact_bytes(self):
        checks = {check["name"]: check
                  for matrix in tomllib.loads((ROOT / "tools/ci/products.toml").read_text())["matrix"]
                  for check in matrix["check"]}
        self.assertIn("host-session-e2e", checks["publish-desktop-payload"]["needs"])
        session = checks["host-session-e2e"]
        self.assertEqual(["desktop-package-amd64"], session["needs"])
        self.assertIn("run.py source --rpms \"$rpms\" --producer desktop-package-amd64", session["run"])
        self.assertLess(session["run"].index("run.py source"), session["run"].index("run.py test"))
        self.assertIn("target/host-session-e2e/**", session["outputs"])
        spec = importlib.util.spec_from_file_location("vm_e2e", ROOT / "dist/session/vm-e2e/run.py")
        harness = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(harness)
        with tempfile.TemporaryDirectory() as temporary:
            rpms = Path(temporary)
            packages = {"tilo-session-1.x86_64.rpm": b"session", "tilo-session-apps-1.x86_64.rpm": b"apps"}
            for name, content in packages.items():
                (rpms / name).write_bytes(content)
            (rpms / "failed-rpms").mkdir()
            (rpms / "failed-rpms/tilo-session-0.x86_64.rpm").write_bytes(b"failed")
            with patch.dict("os.environ", {"T_COMMIT": "a" * 40, "T_REF": "stable"}):
                harness.record_source(rpms, "desktop-package-amd64")
            source = json.loads((rpms / "source.json").read_text())
            self.assertEqual(sorted(packages), source["rpms"])
            self.assertEqual({name: {"byte_size": len(content), "sha256": hashlib.sha256(content).hexdigest()}
                              for name, content in packages.items()}, source["artifacts"])
            self.assertEqual(("a" * 40, "stable", "desktop-package-amd64"),
                             (source["commit"], source["ref"], source["producer"]))
            self.assertEqual("none", source["release_signing_claim"])
            for name in packages:
                (rpms / name).unlink()
            with self.assertRaises(harness.Fail):
                harness.record_source(rpms, "desktop-package-amd64")


if __name__ == "__main__":
    unittest.main()
