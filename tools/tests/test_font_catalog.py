import importlib.util
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
fc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fc)


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
        environment = dict(os.environ, PATH=self.directory.name)
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
        environment = dict(os.environ, PYTHON=sys.executable, CARGO_NET_OFFLINE="true")
        result = subprocess.run([str(self.build)], env=environment,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = "cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT="
        roots = [line[len(prefix):] for line in result.stdout.splitlines() if line.startswith(prefix)]
        self.assertEqual(len(roots), 1, result.stdout)
        self.assertEqual(fc.verify(Path(roots[0]), "fixtures"), (37, 55771564))


if __name__ == "__main__":
    unittest.main()
