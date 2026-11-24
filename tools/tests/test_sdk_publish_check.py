"""Name-availability checks must distinguish absent crates from failed observation."""

import errno
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
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


def answer(document):
    return io.BytesIO(json.dumps(document).encode())


class ForeignNamesTests(unittest.TestCase):
    def test_a_crate_of_another_repository_is_foreign(self):
        document = {"crate": {"name": "tilo-example", "repository": "https://example.com/other"}}
        with patch.object(check.urllib.request, "urlopen", return_value=answer(document)):
            self.assertEqual(check.foreign_names(["tilo-example"]), {"tilo-example"})

    def test_a_crate_this_repository_published_is_ours(self):
        # Once a release published it, the name exists on crates.io: that is no reason to fail.
        document = {"crate": {"name": "tilo-example", "repository": check.REPOSITORY + "/"}}
        with patch.object(check.urllib.request, "urlopen", return_value=answer(document)):
            self.assertEqual(check.foreign_names(["tilo-example"]), set())

    def test_only_not_found_means_available(self):
        error = urllib.error.HTTPError("https://crates.io", 404, "Not Found", {}, None)
        with patch.object(check.urllib.request, "urlopen", side_effect=error):
            self.assertEqual(check.foreign_names(["tilo-example"]), set())

    def test_invalid_success_response_cannot_mark_a_name_available(self):
        for document in ({}, {"errors": [{"detail": "unavailable"}]},
                         {"crate": None}, {"crate": {}}, {"crate": []},
                         {"crate": {"name": ""}}, []):
            with self.subTest(document=document):
                with patch.object(check.urllib.request, "urlopen", return_value=answer(document)):
                    with self.assertRaisesRegex(ValueError, "tilo-example.*cannot determine"):
                        check.foreign_names(["tilo-example"])

    def test_service_and_transport_failures_propagate(self):
        for error in (urllib.error.HTTPError("https://crates.io", 503, "Unavailable", {}, None),
                      urllib.error.URLError("connection refused")):
            with self.subTest(error=error):
                with patch.object(check.urllib.request, "urlopen", side_effect=error):
                    with self.assertRaises(type(error)) as raised:
                        check.foreign_names(["tilo-example"])
                    self.assertIs(raised.exception, error)

    def test_invalid_json_propagates(self):
        with patch.object(check.urllib.request, "urlopen", return_value=io.BytesIO(b"not json")):
            with self.assertRaises(json.JSONDecodeError):
                check.foreign_names(["tilo-example"])


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


class VersionRewriteTests(unittest.TestCase):
    """Every manifest of a release asks for the one version: the package's own, and each
    dependency by path on a published crate, in each way a manifest writes one."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        for crate in ("ui", "app", "private", "fork"):
            (self.root / crate).mkdir()
        self.published = {self.root / "ui", self.root / "fork"}

    def rewrite(self, text, own=True):
        return check.rewrite_manifest(text, self.root / "app", self.published, "1.2.3", own)

    def test_the_package_and_its_dependencies_on_published_crates_take_the_version(self):
        manifest = (
            '[package]\nname = "app"\nversion = "0.1.0"\n\n'
            '[dependencies]\n'
            'ui = { version = "0.1.0", package = "tilo-ui", path = "../ui" }\n'
            'private = { version = "0.1.0", path = "../private" }\n'
            'serde = { version = "1" }\n\n'
            "[target.'cfg(target_arch = \"wasm32\")'.dependencies]\n"
            'ui = { path = "../ui", version = "0.1.0", default-features = false }\n\n'
            '[dependencies.winit]\nversion = "0.30.13"\npackage = "tilo-winit"\npath = "../fork"\n\n'
            '[dev-dependencies.winit]\npath = "../fork"\nversion = "0.30.13"\n\n'
            '[[bin]]\nname = "app"\npath = "src/main.rs"\n'
        )
        self.assertEqual(self.rewrite(manifest), (
            '[package]\nname = "app"\nversion = "1.2.3"\n\n'
            '[dependencies]\n'
            'ui = { version = "=1.2.3", package = "tilo-ui", path = "../ui" }\n'
            'private = { version = "0.1.0", path = "../private" }\n'
            'serde = { version = "1" }\n\n'
            "[target.'cfg(target_arch = \"wasm32\")'.dependencies]\n"
            'ui = { path = "../ui", version = "=1.2.3", default-features = false }\n\n'
            '[dependencies.winit]\nversion = "=1.2.3"\npackage = "tilo-winit"\npath = "../fork"\n\n'
            '[dev-dependencies.winit]\npath = "../fork"\nversion = "=1.2.3"\n\n'
            '[[bin]]\nname = "app"\npath = "src/main.rs"\n'
        ))

    def test_a_crate_that_is_not_published_keeps_its_version(self):
        manifest = '[package]\nname = "app"\nversion = "0.1.0"\n'
        self.assertEqual(self.rewrite(manifest, own=False), manifest)

    def test_a_dependency_without_a_version_is_left_for_the_report(self):
        manifest = '[dependencies]\nui = { path = "../ui" }\n'
        self.assertEqual(self.rewrite(manifest, own=False), manifest)

    def test_workspace_dependencies_take_the_version(self):
        manifest = '[workspace.dependencies]\nui = { version = "0.1.0", path = "ui" }\n'
        rewritten = check.rewrite_manifest(manifest, self.root, self.published, "1.2.3", False)
        self.assertEqual(rewritten, '[workspace.dependencies]\nui = { version = "=1.2.3", path = "ui" }\n')

    def test_a_version_from_the_workspace_is_refused(self):
        with self.assertRaisesRegex(SystemExit, "comes from the workspace"):
            self.rewrite('[package]\nname = "app"\nversion.workspace = true\n')


class ScratchTreeTests(unittest.TestCase):
    def test_packaging_rewrites_a_copy_never_the_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            repo = root / "repo"
            (repo / "vendor/fork").mkdir(parents=True)
            (repo / "target").mkdir()
            (repo / "target/big").write_text("build output")
            original = '[workspace]\nmembers = [\n    "app",\n]\nexclude = [\n    "vendor/fork",\n]\n'
            (repo / "Cargo.toml").write_text(original)
            fork = '[workspace]\n\n[package]\nname = "tilo-fork"\n'
            (repo / "vendor/fork/Cargo.toml").write_text(fork)
            with patch.object(check, "REPO", repo), patch.object(check, "FORKS", ("vendor/fork",)), \
                    patch.dict(check.os.environ, {"TMPDIR": str(root)}):
                work = check.scratch_tree()
                tree = work / "tree"
                check.forks_as_members(tree)
            self.assertFalse((tree / "target").exists(), "build output stays behind")
            self.assertEqual((tree / "Cargo.toml").read_text(),
                             '[workspace]\nmembers = [\n    "vendor/fork",\n    "app",\n]\nexclude = [\n]\n')
            self.assertEqual((tree / "vendor/fork/Cargo.toml").read_text(), '\n[package]\nname = "tilo-fork"\n')
            self.assertEqual((repo / "Cargo.toml").read_text(), original)
            self.assertEqual((repo / "vendor/fork/Cargo.toml").read_text(), fork)


class ReadmeTests(unittest.TestCase):
    def package(self, crate, readme, name="tilo-example"):
        return {"name": name, "description": "d", "license": "MIT", "repository": check.REPOSITORY,
                "dependencies": [], "manifest_path": str(crate / "Cargo.toml"), "readme": readme}

    def test_a_published_crate_needs_a_readme_that_exists(self):
        with tempfile.TemporaryDirectory() as directory:
            crate = Path(directory) / "crates/example"
            crate.mkdir(parents=True)
            self.assertEqual(check.own_blockers(self.package(crate, None), set()), ["no README"])
            self.assertEqual(check.own_blockers(self.package(crate, "README.md"), set()), ["no README"],
                             "a readme the manifest names but the tree lacks")
            (crate / "README.md").write_text("# example\n")
            self.assertEqual(check.own_blockers(self.package(crate, "README.md"), set()), [])
            (Path(directory) / "shared.md").write_text("# shared\n")
            self.assertEqual(check.own_blockers(self.package(crate, "../../shared.md"), set()), [],
                             "a readme outside the crate, which cargo copies into the package")

    def test_a_published_name_says_whose_it_is(self):
        with tempfile.TemporaryDirectory() as directory:
            crate = Path(directory)
            (crate / "README.md").write_text("# example\n")
            for name in ("tilo-example", "tilolang", "tilolang-example"):
                self.assertEqual(check.own_blockers(self.package(crate, "README.md", name), set()), [])
            self.assertEqual(check.own_blockers(self.package(crate, "README.md", "settings-store"), set()),
                             ["name has no tilo- or tilolang prefix"])


class EmbeddedTests(unittest.TestCase):
    def test_a_package_carries_what_it_embeds_from_elsewhere_in_the_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            crate = tree / "crates/example"
            (crate / "src/deep").mkdir(parents=True)
            (tree / "apps/shell").mkdir(parents=True)
            (tree / "apps/shell/app.tl").write_text("app shell\n")
            (crate / "src/own.txt").write_text("own\n")
            code = ('const SHELL: &str = include_str!("../../../../apps/shell/app.tl");\n'
                    'const OWN: &str = include_str!("../own.txt");\n'
                    'const GONE: &[u8] = include_bytes!("../../../../apps/none.bin");\n')
            (crate / "src/deep/lib.rs").write_text(code)
            self.assertEqual(check.carry_embedded(crate, tree), ["apps/shell/app.tl"])
            self.assertEqual((crate / "published-files/apps/shell/app.tl").read_text(), "app shell\n")
            rewritten = (crate / "src/deep/lib.rs").read_text()
            self.assertIn('include_str!("../../published-files/apps/shell/app.tl")', rewritten)
            self.assertIn('include_str!("../own.txt")', rewritten, "a file of its own stays")
            self.assertIn("apps/none.bin", rewritten, "a missing file is left for the build to name")
            self.assertEqual(check.carry_embedded(crate, tree), [], "carried once")

    def test_a_directory_a_concat_path_starts_with_is_carried_whole_under_names_cargo_keeps(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            crate = tree / "crates/example"
            (crate / "src").mkdir(parents=True)
            (tree / "examples/apps/counter/src").mkdir(parents=True)
            (tree / "examples/apps/counter/src/lib.rs").write_text("// counter\n")
            (tree / "examples/apps/counter/Cargo.toml").write_text("[package]\n")
            (tree / ".cargo").mkdir()
            (tree / ".cargo/config.toml").write_text("[build]\n")
            (crate / "src/lib.rs").write_text(
                'const FILES: &str = include_str!(concat!("../../../examples/apps/", "counter", '
                '"/src/lib.rs"));\n'
                'const CONFIG: &str = include_str!("../../../.cargo/config.toml");\n')
            self.assertEqual(check.carry_embedded(crate, tree), [
                ".cargo/config.toml", "examples/apps/counter/Cargo.toml",
                "examples/apps/counter/src/lib.rs"])
            carried = crate / "published-files"
            self.assertEqual((carried / "examples/apps/counter/src/lib.rs").read_text(), "// counter\n")
            # A Cargo.toml would make cargo leave the directory out of the package, and it leaves
            # hidden files out too.
            self.assertTrue((carried / "examples/apps/counter/Cargo.toml.embedded").is_file())
            self.assertFalse((carried / "examples/apps/counter/Cargo.toml").exists())
            rewritten = (crate / "src/lib.rs").read_text()
            self.assertIn('include_str!(concat!("../published-files/examples/apps/", "counter"',
                          rewritten)
            self.assertIn('include_str!("../published-files/dot.cargo/config.toml")', rewritten)
            self.assertEqual(check.carry_embedded(crate, tree), [], "carried once")


class OversizeTests(unittest.TestCase):
    def test_a_package_over_the_limit_names_its_biggest_files(self):
        with tempfile.TemporaryDirectory() as directory:
            crate = Path(directory) / "big-1.0.0.crate"
            with tarfile.open(crate, "w:gz") as archive:
                for name, size in (("big-1.0.0/assets/font.ttf", 3_000_000), ("big-1.0.0/src/lib.rs", 10)):
                    data = os.urandom(size)
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
            with patch.object(check, "MAX_CRATE_BYTES", 1_000_000):
                over = check.too_big({"big": crate})
            self.assertEqual(len(over), 1)
            self.assertIn("big:", over[0])
            self.assertLess(over[0].index("assets/font.ttf"), over[0].index("src/lib.rs"), "biggest first")
            self.assertEqual(check.too_big({"big": crate}), [], "under crates.io's own limit")


if __name__ == "__main__":
    unittest.main()
