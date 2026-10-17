"""Name-availability checks must distinguish absent crates from failed observation."""

import errno
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import patch
import urllib.error


SCRIPT = Path(__file__).resolve().parents[1] / "sdk/publish_check.py"
SPEC = importlib.util.spec_from_file_location("sdk_publish_check_under_test", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(check)
REGISTRY_SPEC = importlib.util.spec_from_file_location(
    "sdk_local_registry_under_test", SCRIPT.with_name("local_registry.py")
)
registry = importlib.util.module_from_spec(REGISTRY_SPEC)
assert REGISTRY_SPEC.loader is not None
REGISTRY_SPEC.loader.exec_module(registry)


class TakenNamesTests(unittest.TestCase):
    def test_registered_crate_is_taken(self):
        response = io.BytesIO(json.dumps({"crate": {"name": "tilo-example"}}).encode())
        with patch.object(check.urllib.request, "urlopen", return_value=response):
            self.assertEqual(check.taken_names(["tilo-example"]), {"tilo-example"})

    def test_only_not_found_means_available(self):
        error = urllib.error.HTTPError("https://crates.io", 404, "Not Found", {}, None)
        with patch.object(check.urllib.request, "urlopen", side_effect=error):
            self.assertEqual(check.taken_names(["tilo-example"]), set())

    def test_invalid_success_response_cannot_mark_a_name_available(self):
        for document in ({}, {"errors": [{"detail": "unavailable"}]},
                         {"crate": None}, {"crate": {}}, {"crate": []},
                         {"crate": {"name": ""}}, []):
            with self.subTest(document=document):
                response = io.BytesIO(json.dumps(document).encode())
                with patch.object(check.urllib.request, "urlopen", return_value=response):
                    with self.assertRaisesRegex(ValueError, "tilo-example.*cannot determine"):
                        check.taken_names(["tilo-example"])

    def test_service_and_transport_failures_propagate(self):
        for error in (urllib.error.HTTPError("https://crates.io", 503, "Unavailable", {}, None),
                      urllib.error.URLError("connection refused")):
            with self.subTest(error=error):
                with patch.object(check.urllib.request, "urlopen", side_effect=error):
                    with self.assertRaises(type(error)) as raised:
                        check.taken_names(["tilo-example"])
                    self.assertIs(raised.exception, error)

    def test_invalid_json_propagates(self):
        with patch.object(check.urllib.request, "urlopen", return_value=io.BytesIO(b"not json")):
            with self.assertRaises(json.JSONDecodeError):
                check.taken_names(["tilo-example"])


class LocalRegistryChecksumTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.out = self.root / "out"
        self.out.mkdir()
        self.cache = self.root / "registry/cache/index.crates.io-test"
        self.cache.mkdir(parents=True)
        self.filename = "example-1.0.0.crate"
        self.data = b"valid crate bytes"
        self.checksum = hashlib.sha256(self.data).hexdigest()
        patcher = patch.object(registry, "CARGO_HOME", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)

    def place(self):
        registry.place_crate("example", "1.0.0", self.checksum, self.out)

    def test_corrupt_cache_cannot_enter_the_registry(self):
        (self.cache / self.filename).write_bytes(b"corrupt")
        with patch.object(registry, "fetch") as fetch:
            with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                self.place()
            fetch.assert_not_called()
        self.assertFalse((self.out / self.filename).exists())

    def test_existing_output_is_checked_before_reuse(self):
        target = self.out / self.filename
        target.write_bytes(b"partial download")
        with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
            self.place()
        target.write_bytes(self.data)
        with patch.object(registry, "fetch") as fetch:
            self.place()
            fetch.assert_not_called()

    def test_valid_cache_preserves_link_and_cross_device_copy(self):
        source = self.cache / self.filename
        source.write_bytes(self.data)
        target = self.out / self.filename
        self.place()
        self.assertTrue(source.samefile(target))
        target.unlink()
        with patch.object(registry.os, "link", side_effect=OSError(errno.EXDEV, "cross-device")):
            self.place()
        self.assertEqual(target.read_bytes(), self.data)
        self.assertFalse(source.samefile(target))

    def test_target_created_during_link_is_checked(self):
        (self.cache / self.filename).write_bytes(self.data)
        for data in (self.data, b"corrupt"):
            with self.subTest(data=data):
                target = self.out / self.filename
                target.unlink(missing_ok=True)

                def competing_link(source, destination):
                    destination.write_bytes(data)
                    raise FileExistsError(destination)

                with patch.object(registry.os, "link", side_effect=competing_link):
                    if data == self.data:
                        self.place()
                    else:
                        with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                            self.place()

    def test_download_checksum_is_still_required(self):
        with patch.object(registry, "fetch", return_value=b"corrupt"):
            with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                self.place()
        self.assertFalse((self.out / self.filename).exists())
        with patch.object(registry, "fetch", return_value=self.data):
            self.place()
        self.assertEqual((self.out / self.filename).read_bytes(), self.data)


class RegistryRebuildTests(unittest.TestCase):
    def test_failed_rebuild_preserves_working_registry_and_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            out, packages = root / "out", root / "packages"
            out.mkdir()
            packages.mkdir()
            marker = out / "working.crate"
            marker.write_bytes(b"known good")
            argv = ["registry", str(out), "--packages", str(packages)]
            with patch.object(sys, "argv", argv):
                with self.assertRaisesRegex(SystemExit, "no .crate"):
                    registry.main()
            self.assertEqual(marker.read_bytes(), b"known good")
            crate = packages / "example-1.0.0.crate"
            crate.write_bytes(b"broken archive")
            with patch.object(sys, "argv", argv), patch.object(
                    registry, "packaged_entry", side_effect=ValueError("bad package")):
                with self.assertRaisesRegex(ValueError, "bad package"):
                    registry.main()
            self.assertEqual(marker.read_bytes(), b"known good")
            self.assertEqual(list(root.glob(".tilo-registry-*")), [])
            with patch.object(sys, "argv", ["registry", str(packages), "--packages", str(packages)]):
                with self.assertRaisesRegex(SystemExit, "must not contain"):
                    registry.main()
            self.assertEqual(crate.read_bytes(), b"broken archive")

    def test_successful_rebuild_replaces_old_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            out, packages = root / "out", root / "packages"
            out.mkdir()
            (out / "obsolete").touch()
            packages.mkdir()
            (packages / "example-1.0.0.crate").write_bytes(b"package")
            (root / "Cargo.lock").write_text("package = []")
            (root / "Cargo.toml").write_text("")
            with patch.object(sys, "argv", ["registry", str(out), "--packages", str(packages)]), \
                    patch.object(registry, "REPO", root), \
                    patch.object(registry, "packaged_entry", return_value={"name": "example", "vers": "1.0.0"}), \
                    patch.object(registry, "probe_lock", return_value={"package": []}):
                rename = Path.rename
                def fail_publication(path, destination):
                    if path.parent.name == "new" and path.name == "index":
                        raise OSError("publication failed")
                    return rename(path, destination)
                with patch.object(Path, "rename", fail_publication):
                    with self.assertRaisesRegex(OSError, "publication failed"):
                        registry.main()
                self.assertEqual(
                    sorted(p.name for p in out.iterdir()), ["obsolete"], "publication failure must roll back"
                )
                self.assertEqual(list(root.glob(".tilo-registry-*")), [])
                self.assertEqual(registry.main(), 0)
            self.assertFalse((out / "obsolete").exists())
            self.assertEqual((out / "example-1.0.0.crate").read_bytes(), b"package")
            self.assertTrue((out / "index/ex/am/example").is_file())
            self.assertEqual(sorted(p.name for p in out.iterdir()), ["example-1.0.0.crate", "index"])

    def test_rebuild_never_renames_a_directory_it_did_not_make(self):
        """On an overlay, a directory from a lower layer can't be renamed (EXDEV):
        a registry an earlier build left is replaced without renaming it or its
        directories."""
        with tempfile.TemporaryDirectory() as directory:
            # Resolved, as the registry's path is: the paths renamed are compared.
            root = Path(directory).resolve()
            out, packages = root / "out", root / "packages"
            (out / "index/ol/d-").mkdir(parents=True)
            (out / "index/ol/d-/old-crate").write_text("{}")
            (out / "old-crate-1.0.0.crate").write_bytes(b"old")
            packages.mkdir()
            (packages / "example-1.0.0.crate").write_bytes(b"package")
            (root / "Cargo.lock").write_text("package = []")
            (root / "Cargo.toml").write_text("")
            lower = {out, *out.rglob("*")}
            rename = Path.rename
            def overlay_rename(path, destination):
                if path in lower and path.is_dir():
                    raise OSError(errno.EXDEV, "Invalid cross-device link", str(path))
                return rename(path, destination)
            with patch.object(sys, "argv", ["registry", str(out), "--packages", str(packages)]), \
                    patch.object(registry, "REPO", root), \
                    patch.object(registry, "packaged_entry", return_value={"name": "example", "vers": "1.0.0"}), \
                    patch.object(registry, "probe_lock", return_value={"package": []}), \
                    patch.object(Path, "rename", overlay_rename):
                self.assertEqual(registry.main(), 0)
            self.assertEqual(sorted(p.name for p in out.iterdir()), ["example-1.0.0.crate", "index"])
            self.assertTrue((out / "index/ex/am/example").is_file())
            self.assertFalse((out / "index/ol").exists())
            self.assertEqual(list(root.glob(".tilo-registry-*")), [])


class ForkManifestRestorationTests(unittest.TestCase):
    def test_partial_manifest_write_is_restored(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, lock = root / "Cargo.toml", root / "Cargo.lock"
            original = '[workspace]\nmembers = [\n]\nexclude = [\n    "vendor/fork",\n]\n'
            manifest.write_text(original)
            lock.write_text("locked")
            write = Path.write_text
            failed = False

            def interrupted(path, text, *args, **kwargs):
                nonlocal failed
                if path == manifest and not failed:
                    failed = True
                    write(path, "partial")
                    raise OSError("disk write interrupted")
                return write(path, text, *args, **kwargs)

            with patch.object(check, "REPO", root), patch.object(check, "FORKS", ("vendor/fork",)), \
                    patch.object(Path, "write_text", interrupted):
                with self.assertRaisesRegex(OSError, "interrupted"):
                    with check.forks_as_members():
                        self.fail("must not enter after a failed write")
            self.assertEqual(manifest.read_text(), original)
            self.assertEqual(lock.read_text(), "locked")


if __name__ == "__main__":
    unittest.main()
