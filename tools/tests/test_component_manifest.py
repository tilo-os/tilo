import contextlib
import importlib.util
import io
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


check = load("check_component_manifest", REPO / "tools/policy/check_component_manifest.py")
installer = load("install_components_under_test", REPO / "os/scripts/install-components.py")


class ComponentManifestPolicyTests(unittest.TestCase):
    def fixture(self) -> Path:
        """A repository view whose os/ inputs can be edited without touching the checkout."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for shared in ("Cargo.toml", "apps", "crates", "examples"):
            if (REPO / shared).exists():
                (root / shared).symlink_to(REPO / shared)
        (root / "os/scripts").mkdir(parents=True)
        (root / "os/usr").symlink_to(REPO / "os/usr")
        (root / "os/dev").symlink_to(REPO / "os/dev")
        (root / "os/kernel").symlink_to(REPO / "os/kernel")
        for name in ("os/Containerfile", "os/components.toml", "os/scripts/install-components.py",
                     "os/scripts/build-thin-apps.sh"):
            shutil.copy(REPO / name, root / name)
        return root

    def edit(self, path: Path, old: str, new: str) -> None:
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def test_component_manifest_is_the_only_source_of_builder_copies_and_layers(self):
        with self.subTest(phase='repository passes'):
            self.assertEqual(check.audit(REPO), [])
        with self.subTest(phase='hand written builder copy names line and fix'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "COPY --from=component-files /image/windows/ /\n",
                      "COPY --from=component-files /image/windows/ /\n"
                      "COPY --from=builder /out/tilo-example /usr/libexec/tilo-example\n")
            line = next(n for n, text in enumerate((root / "os/Containerfile").read_text().splitlines(), 1)
                        if "tilo-example" in text)
            errors = check.audit(root)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn(f"os/Containerfile:{line}:", errors[0])
            self.assertIn("outside os/components.toml", errors[0])
            self.assertIn("why:", errors[0])
            self.assertIn('{ from = "tilo-example", to = "/usr/libexec/tilo-example", mode = "0755" }', errors[0])
            self.assertIn("re-run: python3 tools/policy/check_component_manifest.py", errors[0])
        with self.subTest(phase='unknown cargo bin fails'):
            root = self.fixture()
            self.edit(root / "os/components.toml", 'bins = ["tilo-crypto"]', 'bins = ["tilo-cryptoo"]')
            errors = "\n".join(check.audit(root))
            self.assertIn("ships cargo bin 'tilo-cryptoo'", errors)
            self.assertIn("os/components.toml:", errors)
        with self.subTest(phase='builder bin missing from manifest fails'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "--bin tilo-projector ", "--bin tilo-projector --bin tilo-new ")
            self.assertIn("compiles --bin tilo-new", "\n".join(check.audit(root)))
        with self.subTest(phase='boot component needs existing scenario'):
            root = self.fixture()
            manifest = root / "os/components.toml"
            self.edit(manifest, 'scenario = "os/dev/vm.sh"', 'scenario = "os/dev/missing.sh"')
            self.assertIn("names scenario 'os/dev/missing.sh'", "\n".join(check.audit(root)))
            self.edit(manifest, 'scenario = "os/dev/missing.sh"\n', "")
            self.assertIn("needs `scenario`", "\n".join(check.audit(root)))
        with self.subTest(phase='swappable boot component needs its build script'):
            root = self.fixture()
            manifest = root / "os/components.toml"
            self.edit(manifest, 'dev_build = "os/kernel/dev-build.sh"',
                      'dev_build = "os/kernel/missing.sh"')
            self.assertIn("names dev_build 'os/kernel/missing.sh'", "\n".join(check.audit(root)))
            self.edit(manifest, 'dev_build = "os/kernel/missing.sh"\n', "")
            self.assertIn("needs `dev_build`", "\n".join(check.audit(root)))
            self.edit(manifest, 'dev_swap = "reboot"\n', 'dev_swap = "hot"\n')
            self.assertIn("`dev_swap` must be one of reboot", "\n".join(check.audit(root)))
        with self.subTest(phase='unapplied layer fails'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "COPY --from=component-files /image/windows/ /\n", "")
            self.assertIn("layer 'windows'", "\n".join(check.audit(root)))


class InstallerTests(unittest.TestCase):
    def build_outputs(self, root: Path, *, browser: bool) -> Path:
        out = root / "out"
        (out / "optional").mkdir(parents=True)
        manifest = installer.load_manifest(REPO / "os/components.toml")
        for entry in manifest.files():
            if entry.source and not entry.source.startswith("/") and entry.source != "browser":
                (out / entry.source).parent.mkdir(parents=True, exist_ok=True)
                (out / entry.source).write_bytes(b"\x7fELF" + entry.source.encode())
                (out / entry.source).chmod(0o700)
        if browser:
            (out / "optional/browser").write_bytes(b"\x7fELFbrowser")
        return out

    def run_installer(self, *args: str) -> int:
        with contextlib.redirect_stdout(io.StringIO()):
            return installer.main(list(args))

    def test_installer_selects_manifest_layers_and_keeps_dev_and_production_apart(self):
        with self.subTest(phase='image layers match manifest'):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                out = self.build_outputs(root, browser=False)
                image = root / "image"
                status = self.run_installer("--from", str(out), "--from", str(out / "optional"),
                                            "--dest", str(image), "--by-layer", "--all")
                self.assertEqual(status, 0)
                manifest = installer.load_manifest(REPO / "os/components.toml")
                expected = {f"{e.layer}{e.to}" for e in manifest.files()
                            if e.layer != "direct" and e.component != "browser" and not e.dev_only}
                installed = {p.relative_to(image).as_posix()
                             for p in image.rglob("*") if p.is_file() or p.is_symlink()}
                self.assertEqual(installed, expected)
                shell = image / "desktop-binaries/usr/bin/shell"
                self.assertEqual(shell.stat().st_mode & 0o777, 0o755)
                self.assertEqual(os.readlink(image / "desktop-binaries/usr/bin/store"), "app")
                self.assertFalse((image / "desktop-binaries/usr/bin/browser").exists())
        with self.subTest(phase='optional browser installed when built'):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                out = self.build_outputs(root, browser=True)
                status = self.run_installer("--from", str(out), "--from", str(out / "optional"),
                                            "--dest", str(root / "layer"), "browser")
                self.assertEqual(status, 0)
                self.assertTrue((root / "layer/usr/bin/browser").is_file())
        with self.subTest(phase='production selection never includes dev only components'):
            manifest = installer.load_manifest(REPO / "os/components.toml")
            production = {c.name for c in installer.select_components(manifest, [], True)}
            dev = {c.name for c in installer.select_components(manifest, [], False, True)}
            self.assertIn("control", dev)
            self.assertIn("vmd", dev)
            self.assertIn("shell", dev)
            self.assertIn("native-runtime-host", dev)
            self.assertNotIn("control", production)
            self.assertNotIn("vmd", production)
            self.assertIn("shell", production)
            self.assertIn("native-runtime-host", production)
        with self.subTest(phase='dev cargo plan adds dev features'):
            manifest = installer.load_manifest(REPO / "os/components.toml")
            dev = installer.select_components(manifest, [], False, True)
            packages, bins, features = installer.cargo_plan(dev, True)
            self.assertIn("dev-overrides", features)
            self.assertEqual(set(bins), {"shell", "tilo-runtime-mcp", "tilo-control", "tilo-vmd"})
            _, _, production_features = installer.cargo_plan(dev, False)
            self.assertNotIn("dev-overrides", production_features)
        with self.subTest(phase='mixed dev and production layers are rejected'):
            document = {"component": [{
                "name": "mixed", "kind": "userspace", "scope": "session", "packages": ["shell"],
                "files": [
                    {"from": "a", "to": "/usr/bin/a", "mode": "0755", "layer": "dev-tools"},
                    {"from": "b", "to": "/usr/bin/b", "mode": "0755", "layer": "desktop-binaries"},
                ]}]}
            with self.assertRaisesRegex(installer.ManifestError, "mixes dev-only layers"):
                installer.parse_manifest(document, REPO / "os/components.toml")
        with self.subTest(phase='missing required output fails'):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                with contextlib.redirect_stderr(io.StringIO()) as stderr:
                    status = self.run_installer("--from", str(root), "--dest", str(root / "layer"), "shell")
                self.assertEqual(status, 1)
                self.assertIn("build output 'shell' not found", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
