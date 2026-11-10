import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
import urllib.error
from email.message import Message
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
fc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fc)

FONTS = ROOT / "crates/sdk/ui-core/assets/fonts"
ONE_FONT = "NotoSansMath-Regular.ttf"


def pinned():
    """Every pinned font: its file name, size, digest and release-store URL."""
    return json.loads((FONTS / "artifacts.json").read_text())["assets"]


class Answer:
    """One pinned font's response, streamed from a catalog already on this machine."""

    def __init__(self, path, url):
        self.stream = path.open("rb")
        self.url = url

    def geturl(self):
        return self.url

    def read(self, size):
        return self.stream.read(size)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.stream.close()
        return False


def http_error(url, code, retry_after=None):
    headers = Message()
    if retry_after is not None:
        headers["Retry-After"] = str(retry_after)
    return urllib.error.HTTPError(url, code, f"{code} refused", headers, io.BytesIO())


class Store:
    """The release store as a job meets it: it records what was asked for, and answers as a busy
    one does when asked to — a rate limit, a 5xx, a pin it does not have."""

    def __init__(self, catalog, refuse=None, delay=0.0, redirect=None):
        self.catalog = Path(catalog)
        self.refuse = refuse or (lambda row, asked: None)
        self.delay = delay
        self.redirect = redirect
        self.asked = {}

    def open_url(self, url):
        row = next(row for row in pinned() if row["url"] == url)
        asked = self.asked[row["file"]] = self.asked.get(row["file"], 0) + 1
        if self.delay:
            time.sleep(self.delay)
        refusal = self.refuse(row, asked)
        if refusal is not None:
            raise refusal
        return Answer(self.catalog / row["file"], self.redirect or url)


class StoreTest(unittest.TestCase):
    """A store that is busy is waited out; one that has no such file is not."""

    @classmethod
    def setUpClass(cls):
        cls.catalog = Path(fc.prepared_root(FONTS, "fixtures"))

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.cache = Path(directory.name) / "cache"
        # The builder image's own copy would answer before the store this test wants to hear from.
        self.patch(fc, "IMAGE_SEED", Path(directory.name) / "no-image-seed")
        self.entry = next(row for row in pinned() if row["file"] == ONE_FONT)
        self.waits = []
        self.patch(fc, "time", types.SimpleNamespace(sleep=self.waits.append))

    def patch(self, module, name, value):
        patcher = mock.patch.object(module, name, value)
        patcher.start()
        self.addCleanup(patcher.stop)

    def store(self, **kwargs):
        store = Store(self.catalog, **kwargs)
        self.patch(fc, "open_url", store.open_url)
        return store

    def fetch(self):
        path = self.cache / self.entry["sha256"] / self.entry["file"]
        path.parent.mkdir(parents=True, exist_ok=True)
        fc.fetch(self.entry, path)
        return path

    def fetch_noticing(self):
        """Fetch, and the notices the job was given about the store."""
        stderr = io.StringIO()
        with mock.patch.object(sys, "stderr", stderr):
            path = self.fetch()
        return path, stderr.getvalue()

    def test_a_rate_limited_fetch_waits_and_brings_the_pinned_bytes(self):
        store = self.store(refuse=lambda row, asked: http_error(row["url"], 429) if asked <= 2 else None)
        path, notice = self.fetch_noticing()
        self.assertEqual(store.asked[self.entry["file"]], 3)
        fc.verify_artifact(path, self.entry)
        self.assertEqual(len(self.waits), 2)
        self.assertEqual(self.waits, sorted(self.waits), "the waits grow")
        self.assertIn(f"fetching {self.entry['file']}: HTTP Error 429", notice)
        self.assertIn("attempt 1 of 5", notice)
        self.assertIn("again in", notice)

    def test_a_store_that_stays_busy_names_the_font_the_url_and_the_attempts(self):
        self.store(refuse=lambda row, asked: http_error(row["url"], 429))
        stderr = io.StringIO()
        with mock.patch.object(sys, "stderr", stderr), self.assertRaisesRegex(
                ValueError,
                r"could not fetch NotoSansMath-Regular\.ttf from https://\S+ in 5 attempts: "
                r"HTTP Error 429"):
            self.fetch()
        self.assertEqual(len(self.waits), fc.ATTEMPTS - 1)
        self.assertIn("attempt 4 of 5", stderr.getvalue())

    def test_a_pin_the_store_does_not_have_is_not_asked_for_again(self):
        store = self.store(refuse=lambda row, asked: http_error(row["url"], 404))
        with self.assertRaisesRegex(ValueError, r"in 1 attempt: HTTP Error 404"):
            self.fetch()
        self.assertEqual(store.asked[self.entry["file"]], 1)
        self.assertEqual(self.waits, [])

    def test_the_store_says_how_long_it_is_busy(self):
        self.store(refuse=lambda row, asked: http_error(row["url"], 429, retry_after=7)
                   if asked == 1 else None)
        stderr = io.StringIO()
        with mock.patch.object(sys, "stderr", stderr):
            self.fetch()
        self.assertEqual(self.waits, [7.0])
        self.assertIn("again in 7.0s", stderr.getvalue())

    def test_bytes_from_another_address_are_refused(self):
        self.store(redirect="https://elsewhere.invalid/font.ttf")
        with self.assertRaisesRegex(ValueError, "redirect is forbidden"):
            self.fetch()
        self.assertEqual(self.waits, [])

    def test_a_body_that_does_not_match_its_pin_is_not_kept_and_not_asked_again(self):
        real = self.catalog / ONE_FONT
        with tempfile.TemporaryDirectory() as tmp:
            short = Path(tmp) / "short"
            short.write_bytes(real.read_bytes()[:-16])
            asked = []

            def open_url(url):
                asked.append(url)
                return Answer(short, url)
            self.patch(fc, "open_url", open_url)
            with mock.patch.object(sys, "stderr", io.StringIO()), self.assertRaisesRegex(
                    ValueError, r"in 1 attempt: invalid cached font size"):
                self.fetch()
        self.assertEqual(len(asked), 1)
        self.assertFalse((self.cache / self.entry["sha256"] / self.entry["file"]).exists())
        self.assertEqual(list((self.cache / self.entry["sha256"]).iterdir()), [],
                         "no partial font is left behind")

    def test_a_connection_that_drops_is_asked_again(self):
        def open_url(url):
            raise ConnectionResetError("connection reset by peer")
        self.patch(fc, "open_url", open_url)
        with mock.patch.object(sys, "stderr", io.StringIO()), self.assertRaisesRegex(
                ValueError, r"in 5 attempts: connection reset by peer"):
            self.fetch()
        self.assertEqual(len(self.waits), fc.ATTEMPTS - 1)


class CacheLockTest(unittest.TestCase):
    """The jobs sharing a cache take turns: the first fetches the pinned fonts and publishes the
    catalog, and the others verify what it left instead of each fetching 56 MB."""

    @classmethod
    def setUpClass(cls):
        cls.catalog = Path(fc.prepared_root(FONTS, "fixtures"))

    def test_concurrent_preparations_fetch_every_font_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp) / "cache"
            store = Store(self.catalog, delay=0.005)
            patcher = mock.patch.object(fc, "open_url", store.open_url)
            patcher.start()
            self.addCleanup(patcher.stop)
            seed = tempfile.TemporaryDirectory()
            self.addCleanup(seed.cleanup)
            patcher = mock.patch.object(fc, "IMAGE_SEED", Path(seed.name) / "no-image-seed")
            patcher.start()
            self.addCleanup(patcher.stop)
            roots, failures = [], []
            barrier = threading.Barrier(2)

            def prepare():
                barrier.wait()
                try:
                    roots.append(fc.prepared_root(FONTS, "fixtures", cache=cache))
                except Exception as error:  # the other thread's failure is the test's
                    failures.append(error)

            threads = [threading.Thread(target=prepare) for _ in range(2)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(120)
            self.assertEqual(failures, [])
            self.assertEqual(len(roots), 2)
            self.assertEqual(roots[0], roots[1])
            self.assertEqual(fc.verify(roots[0], "fixtures"), (37, 55771564))
            self.assertEqual(set(store.asked), {row["file"] for row in pinned()},
                             "every font came from the store")
            self.assertEqual(max(store.asked.values()), 1, store.asked)

    def test_a_cache_is_usable_again_once_its_lock_is_released(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp) / "cache"
            with fc.locked(cache):
                pass
            with fc.locked(cache):
                # The lock is held on the directory, so it names nothing of its own.
                self.assertEqual(list(cache.iterdir()), [])


class CatalogTest(unittest.TestCase):
    def test_reviewed_payload_is_staged_and_corruption_is_never_substituted(self):
        with self.subTest(phase='reviewed payload and product staging'):
            source = fc.prepared_root(ROOT / "crates/sdk/ui-core/assets/fonts", "fixtures")
            self.assertEqual(fc.verify(source, "fixtures"), (37, 55771564))
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(source, dest, "os")
                self.assertEqual(fc.verify(dest, "os"), (36, 54891768))
                self.assertTrue((dest / "NewCM10-Regular.otf").is_file())
                self.assertFalse((dest / "AdwaitaSans-Regular.ttf").exists())
                self.assertTrue((dest / "LICENSES/Noto-Color-Emoji-OFL.txt").is_file())
                with self.assertRaises(ValueError):
                    fc.verify(dest, "fixtures")
        with self.subTest(phase='the standalone catalog carries only fonts any app can bundle'):
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(source, dest, "product")
                self.assertEqual(fc.verify(dest, "product"), (35, 54174180))
                # New Computer Modern's GPL-licensed Regular cut stays in the
                # Tilo OS; its GUST-licensed cuts ship.
                self.assertFalse((dest / "NewCM10-Regular.otf").exists())
                for cut in ["NewCM10-Book", "NewCM10-BookItalic", "NewCM10-Italic",
                            "NewCM10-Bold", "NewCM10-BoldItalic", "NewCM08-Book"]:
                    self.assertTrue((dest / f"{cut}.otf").is_file(), cut)
                staged = fc.selected(fc.manifest(dest), "product")
                self.assertTrue(all(f["license_spdx"] in fc.STANDALONE_LICENSES for f in staged))
        with self.subTest(phase='an OS catalog linked to the product catalog shares its inodes'):
            with tempfile.TemporaryDirectory() as tmp:
                product, os_fonts, linked = Path(tmp) / "product", Path(tmp) / "os", Path(tmp) / "linked"
                fc.stage(source, product, "product")
                fc.stage(source, os_fonts, "os")
                fc.link(product, linked, "os", extra=os_fonts)
                self.assertEqual(fc.verify(linked, "os"), (36, 54891768))
                face = "NotoSansMath-Regular.ttf"
                self.assertEqual((linked / face).stat().st_ino, (product / face).stat().st_ino)
                # The OS-only face is not in the product catalog: copied, not linked.
                self.assertNotEqual((linked / "NewCM10-Regular.otf").stat().st_ino,
                                    (os_fonts / "NewCM10-Regular.otf").stat().st_ino)
                with self.assertRaisesRegex(ValueError, "missing.*NewCM10-Regular"):
                    fc.link(product, Path(tmp) / "no-extra", "os")
        with self.subTest(phase='the recorded faces travel with every copy of the catalog'):
            with tempfile.TemporaryDirectory() as tmp:
                product, staged, linked = Path(tmp) / "product", Path(tmp) / "staged", Path(tmp) / "linked"
                fc.stage(source, product, "product")
                (product / fc.FACE_INDEX).write_text('{"recorded": true}\n')
                fc.stage(product, staged, "product")
                fc.link(product, linked, "product")
                for copy in (staged, linked):
                    self.assertEqual((copy / fc.FACE_INDEX).read_text(), '{"recorded": true}\n')
                self.assertEqual((linked / fc.FACE_INDEX).stat().st_ino,
                                 (product / fc.FACE_INDEX).stat().st_ino)
        with self.subTest(phase='corruption is not substituted'):
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(source, dest, "product")
                target = dest / "NotoSansMath-Regular.ttf"
                data = bytearray(target.read_bytes())
                data[-1] ^= 1
                target.write_bytes(data)
                with self.assertRaisesRegex(ValueError, "SHA-256.*NotoSansMath"):
                    fc.verify(dest, "product")
                target.unlink()
                with self.assertRaisesRegex(ValueError, "missing.*NotoSansMath"):
                    fc.verify(dest, "product")


class CatalogBuildTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.build = Path(cls.directory.name) / "font-catalog-build"
        environment = dict(os.environ, CARGO_MANIFEST_DIR=str(ROOT / "crates/sdk/font-catalog"))
        subprocess.run(["rustc", "--edition=2024", str(ROOT / "crates/sdk/font-catalog/build.rs"),
                        "-o", str(cls.build)], env=environment, check=True)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def test_missing_python_identifies_the_interpreter_and_preparation_tool(self):
        environment = dict(os.environ, PATH=self.directory.name, OUT_DIR=self.directory.name)
        environment.pop("PYTHON", None)
        default_python = "python" if os.name == "nt" else "python3"
        for executable in [default_python, str(Path(self.directory.name) / "missing-python")]:
            with self.subTest(executable=executable):
                if executable != default_python:
                    environment["PYTHON"] = executable
                result = subprocess.run([str(self.build)], env=environment,
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"could not launch `{executable}`", result.stderr)
                self.assertIn("tools/font-catalog.py", result.stderr)
                self.assertIn("Python 3.11+", result.stderr)
                self.assertIn("dnf install python3", result.stderr)
                self.assertNotIn("cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT=", result.stdout)

    def test_installed_python_prepares_the_real_verified_catalog_offline(self):
        source = ROOT / "crates/sdk/ui-core/assets/fonts"
        fc.prepared_root(source, "fixtures")
        # Cargo's own layout: the catalog is this build's OUT_DIR/fonts, and the
        # crate names it from the profile directory up, so the build directory's
        # path is not compiled into the crate. A shallow OUT_DIR names it from
        # wherever it happens to sit, which is what the build directory must not
        # depend on, so the fixture puts it where cargo does.
        profile = Path(os.path.abspath(self.directory.name)) / "target/debug"
        out = profile / "build/font-catalog-test/out"
        out.mkdir(parents=True)
        named = out.relative_to(profile) / "fonts"
        environment = dict(os.environ, PYTHON=sys.executable, TILO_FONT_ASSETS_OFFLINE="1",
                           OUT_DIR=str(out))
        prefix = "cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT="
        # A second run replaces the first run's catalog, as cargo's rerun of the script does.
        for _ in range(2):
            result = subprocess.run([str(self.build)], env=environment,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            roots = [line[len(prefix):] for line in result.stdout.splitlines() if line.startswith(prefix)]
            self.assertEqual(roots, [named.as_posix()], result.stdout)
            self.assertEqual(fc.verify(profile / named, "fixtures"), (37, 55771564))


if __name__ == "__main__":
    unittest.main()
