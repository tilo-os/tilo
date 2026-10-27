import contextlib
import hashlib
import importlib.util
import io
import os
import shutil
import sys
import tempfile
import unittest
from unittest import mock
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
        (root / "os/image").mkdir(parents=True)
        (root / "os/usr").symlink_to(REPO / "os/usr")
        (root / "os/dev").symlink_to(REPO / "os/dev")
        (root / "os/kernel").symlink_to(REPO / "os/kernel")
        for name in ("os/Containerfile", "os/components.toml", "os/scripts/install-components.py",
                     "os/scripts/build-thin-apps.sh", "os/image/compile.sh"):
            shutil.copy(REPO / name, root / name)
        return root

    def edit(self, path: Path, old: str, new: str) -> None:
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def test_shell_only_recipe_is_refused_before_deployment(self):
        root = self.fixture()
        entry = '  { from = "@manifest/usr/libexec/tilo-session", to = "/usr/libexec/tilo-session", mode = "0755" },'
        self.edit(root / "os/components.toml", entry + "\n", "")
        self.assertIn("shell must ship its matching mandatory PAM session wrapper", "\n".join(check.audit(root)))

    def test_component_manifest_is_the_only_source_of_builder_copies_and_layers(self):
        with self.subTest(phase='repository passes'):
            self.assertEqual(check.audit(REPO), [])
        with self.subTest(phase='hand written builder copy names line and fix'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "COPY --from=component-files /image/windows/ /\n",
                      "COPY --from=component-files /image/windows/ /\n"
                      "COPY --from=apps-payload /out/tilo-example /usr/libexec/tilo-example\n")
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
        with self.subTest(phase='compiled bin missing from manifest fails'):
            root = self.fixture()
            self.edit(root / "os/image/compile.sh", "--bin tilo-projector ", "--bin tilo-projector --bin tilo-new ")
            self.assertIn("builds --bin tilo-new", "\n".join(check.audit(root)))
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
        with self.subTest(phase='the assembled runtime tree must name each direct tree it installs'):
            root = self.fixture()
            containerfile = root / "os/Containerfile"
            containerfile.write_text(containerfile.read_text().replace(
                "/image/usr/share/tilo/font-catalogs", "/image/elsewhere"))
            self.assertIn("direct file /usr/share/tilo/font-catalogs/ (component 'apps+runtime') is never copied",
                          "\n".join(check.audit(root)))
        with self.subTest(phase='a runtime publisher key travels from the packing stage with its runtime'):
            root = self.fixture()
            self.edit(root / "os/Containerfile",
                      "--mount=type=bind,from=apps-payload,source=/out/runtime-bundle-publisher.hex,",
                      "--mount=type=bind,from=native-builder-payload,source=/out/runtime-bundle-publisher.hex,")
            self.assertIn("direct file /usr/share/tilo/runtime-bundle-publisher.hex (component 'apps+runtime') "
                          "is never copied", "\n".join(check.audit(root)))
        with self.subTest(phase='the assembled runtime tree is copied whole'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "COPY --from=runtime-image-payload /image/usr/ /usr/",
                      "COPY --from=runtime-image-payload /image/usr/lib/ /usr/lib/")
            self.assertIn("copy the assembled tree whole", "\n".join(check.audit(root)))
        with self.subTest(phase='the seeded app store is copied whole from the payload store'):
            root = self.fixture()
            self.edit(root / "os/Containerfile",
                      "COPY --from=app-store /out/usr/share/tilo-apps/ /usr/share/tilo-apps/",
                      "COPY --from=app-store /out/usr/share/tilo-apps/pkgs/ /usr/share/tilo-apps/pkgs/")
            found = "\n".join(check.audit(root))
            self.assertIn("copy the seeded store whole", found)
            self.assertIn("direct file /usr/share/tilo-apps/ (component 'apps+runtime') is never copied", found)
            root = self.fixture()
            self.edit(root / "os/Containerfile",
                      "--mount=type=bind,from=native-builder-payload,source=/usr/share/tilo-apps,"
                      "target=/run/tilo-apps-built,ro",
                      "--mount=type=bind,from=apps-payload,source=/usr/share/tilo-apps,target=/run/tilo-apps-built,ro")
            self.assertIn("copy the seeded store whole", "\n".join(check.audit(root)))
        with self.subTest(phase='unapplied layer fails'):
            root = self.fixture()
            self.edit(root / "os/Containerfile", "COPY --from=component-files /image/windows/ /\n", "")
            self.assertIn("layer 'windows'", "\n".join(check.audit(root)))


class InstallerTests(unittest.TestCase):
    def test_source_shell_layer_carries_verified_fonts_and_refuses_incomplete_catalogs(self):
        catalog = load("shell_font_catalog_under_test", REPO / "tools/font-catalog.py")
        source = REPO / "crates/sdk/ui-core/assets/fonts"
        reviewed = catalog.prepared_root(source, "os", offline=True)
        manifest = installer.load_manifest(REPO / "os/components.toml")
        fonts = next(e for e in manifest.files() if e.component == "shell" and e.tree)
        self.assertEqual(fonts.to, "/usr/share/fonts/tilo/")
        self.assertFalse(fonts.optional)
        for defect in (None, "missing", "tampered", "manifest", "provenance", "offline"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                supplied = root / "fonts"
                catalog.stage(reviewed, supplied, "os")
                face = supplied / catalog.manifest(source)["font"][0]["file"]
                if defect == "missing":
                    face.unlink()
                elif defect == "tampered":
                    data = bytearray(face.read_bytes())
                    data[-1] ^= 1
                    face.write_bytes(data)
                elif defect in ("manifest", "provenance"):
                    name = "manifest.toml" if defect == "manifest" else "PROVENANCE.md"
                    (supplied / name).write_bytes((supplied / name).read_bytes() + b"changed")
                out = self.build_outputs(root)
                layer = root / "layer"
                with mock.patch.dict(os.environ, {
                    "TILO_FONT_CATALOG_ROOT": "" if defect == "offline" else str(supplied),
                    "TILO_FONT_ASSET_CACHE": str(root / "empty-cache"),
                    "TILO_FONT_ASSETS_OFFLINE": "1",
                }), contextlib.redirect_stderr(io.StringIO()) as stderr:
                    status = self.run_installer("--from", str(out), "--dest", str(layer), "shell")
                installed = layer / "usr/share/fonts/tilo"
                if defect is None:
                    self.assertEqual(status, 0, stderr.getvalue())
                    self.assertEqual(catalog.verify(installed, "os"), catalog.verify(reviewed, "os"))
                    for path in supplied.rglob("*"):
                        if path.is_file():
                            self.assertEqual((installed / path.relative_to(supplied)).read_bytes(),
                                             path.read_bytes())
                    self.assertEqual((layer / "usr/bin/shell").read_bytes(), (out / "shell").read_bytes())
                    self.assertEqual((layer / "usr/libexec/tilo-session").read_bytes(),
                                     (REPO / "os/usr/libexec/tilo-session").read_bytes())
                else:
                    self.assertEqual(status, 1)
                    self.assertIn("mandatory shell font catalog", stderr.getvalue())
                    self.assertFalse(installed.exists())

    def test_image_component_stage_carries_the_speech_workers_license(self):
        # Reconstruct checked-in COPY inputs from the actual component-files
        # stage, then execute the production installer against that manifest.
        # A host-checkout install alone cannot observe missing image inputs.
        copies = [pair
                  for instruction in check.instructions(REPO / "os/Containerfile")
                  if instruction.stage == check.INSTALL_STAGE
                  and instruction.words[0].upper() == "COPY"
                  for pair in check.copy_pairs(instruction)[1]
                  if pair[1].startswith("/tilo-components/")]
        license_source = "os/usr/share/licenses/tilo-speech-worker/LICENSE"
        for include_license in (True, False):
            with self.subTest(staged_license=include_license), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                for source, destination in copies:
                    if not include_license and source == license_source:
                        continue
                    target = root / destination.lstrip("/")
                    if destination.endswith("/"):
                        target /= Path(source).name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(REPO / source, target)
                out = self.build_outputs(root)
                layer = root / "image"
                with contextlib.redirect_stderr(io.StringIO()):
                    status = self.run_installer(
                        "--manifest", str(root / "tilo-components/components.toml"),
                        "--from", str(out), "--dest", str(layer), "--by-layer", "model-manager")
                self.assertEqual(status, 0 if include_license else 1)
                if include_license:
                    notice = layer / "desktop-binaries/usr/share/licenses/tilo-speech-worker/LICENSE"
                    self.assertEqual(notice.read_bytes(), (REPO / license_source).read_bytes())
                    self.assertEqual(notice.stat().st_mode & 0o777, 0o644)
                    self.assertTrue((layer / "desktop-binaries/usr/libexec/tilo/tilo-speech-worker").is_file())

    def test_shell_layer_requires_and_installs_its_exact_startup_wrapper(self):
        copies = [pair
                  for instruction in check.instructions(REPO / "os/Containerfile")
                  if instruction.stage == check.INSTALL_STAGE
                  and instruction.words[0].upper() == "COPY"
                  for pair in check.copy_pairs(instruction)[1]
                  if pair[1].startswith("/tilo-components/")]
        for include_wrapper in (True, False):
            with self.subTest(staged_wrapper=include_wrapper), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                for source, destination in copies:
                    if not include_wrapper and source == "os/usr/libexec/tilo-session":
                        continue
                    target = root / destination.lstrip("/")
                    if destination.endswith("/"):
                        target /= Path(source).name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(REPO / source, target)
                out = self.build_outputs(root)
                layer = root / "layer"
                with contextlib.redirect_stderr(io.StringIO()):
                    status = self.run_installer(
                        "--manifest", str(root / "tilo-components/components.toml"),
                        "--from", str(out), "--dest", str(layer), "shell")
                self.assertEqual(status, 0 if include_wrapper else 1)
                if include_wrapper:
                    wrapper = layer / "usr/libexec/tilo-session"
                    self.assertEqual(wrapper.read_bytes(), (REPO / "os/usr/libexec/tilo-session").read_bytes())
                    self.assertEqual(wrapper.stat().st_mode & 0o777, 0o755)
                    self.assertTrue((layer / "usr/bin/shell").is_file())

    def test_user_identity_bootstrap_installs_verified_native_asset_and_helpers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_root = root / "os"
            shutil.copytree(REPO / "os/usr/libexec", manifest_root / "usr/libexec")
            (manifest_root / "dev").mkdir()
            shutil.copyfile(REPO / "os/dev/hardware_profile.py", manifest_root / "dev/hardware_profile.py")
            (manifest_root / "external-assets/cache").mkdir(parents=True)
            header = bytearray(20)
            header[:6] = b"\x7fELF\x02\x01"
            header[18:20] = (62).to_bytes(2, "little")
            payload = bytes(header) + b"controlled native verifier fixture"
            asset = manifest_root / "external-assets/cache/cosign-v2.5.2-linux-amd64"
            asset.write_bytes(payload)
            pins = manifest_root / "external-assets/github-assets.tsv"
            pins.write_text(f"{hashlib.sha256(payload).hexdigest()}\t{asset.name}\thttps://fixture.invalid/pinned\n")
            shutil.copyfile(REPO / "os/components.toml", manifest_root / "components.toml")
            entries = installer.load_manifest(manifest_root / "components.toml").component("user-identity").files
            out = self.build_outputs(root)
            dest = root / "layer"
            with mock.patch.object(installer.platform, "machine", return_value="x86_64"), mock.patch.dict(os.environ, {}, clear=True):
                installer.install(entries, [out], None, dest, False, lambda _: None)
                self.assertEqual((dest / "usr/libexec/tilo-cosign").read_bytes(), payload)
                self.assertEqual((dest / "usr/libexec/tilo-cosign").stat().st_mode & 0o777, 0o755)
                for name in ("tilo-verified-update", "tilo-image-trust-staged"):
                    self.assertEqual((dest / "usr/libexec" / name).read_bytes(), (REPO / "os/usr/libexec" / name).read_bytes())
                # The board-successor verifier is compiled: a build output, not a manifest file.
                successor = dest / "usr/libexec/tilo-board-successor"
                self.assertEqual(successor.read_bytes(), (out / "tilo-board-successor").read_bytes())
                self.assertEqual(successor.stat().st_mode & 0o777, 0o755)
                original_copy = installer.shutil.copy2
                def corrupt_copy(source, destination, **kwargs):
                    result = original_copy(source, destination, **kwargs)
                    if Path(source) == asset:
                        Path(destination).write_bytes(payload + b"changed during copy")
                    return result
                with mock.patch.object(installer.shutil, "copy2", side_effect=corrupt_copy):
                    with self.assertRaisesRegex(installer.ManifestError, "checksum"):
                        installer.install(entries, [out], None, root / "raced", False, lambda _: None)
                self.assertFalse((root / "raced/usr/libexec/tilo-cosign").exists())
                asset.write_bytes(payload + b"corrupt")
                with self.assertRaisesRegex(installer.ManifestError, "checksum"):
                    installer.install(entries, [out], None, root / "refused", False, lambda _: None)
                asset.write_bytes(payload)
                with mock.patch.object(installer.platform, "machine", return_value="aarch64"):
                    with self.assertRaisesRegex(installer.ManifestError, "pin"):
                        installer.native_cosign(manifest_root)
                asset.unlink()
                asset.symlink_to(dest / "usr/libexec/tilo-cosign")
                with self.assertRaisesRegex(installer.ManifestError, "regular"):
                    installer.native_cosign(manifest_root)

    def build_outputs(self, root: Path) -> Path:
        out = root / "out"
        out.mkdir(parents=True)
        manifest = installer.load_manifest(REPO / "os/components.toml")
        for entry in manifest.files():
            if entry.source and not entry.source.startswith(("/", "@manifest/")):
                (out / entry.source).parent.mkdir(parents=True, exist_ok=True)
                (out / entry.source).write_bytes(b"\x7fELF" + entry.source.encode())
                (out / entry.source).chmod(0o700)
        return out

    def run_installer(self, *args: str) -> int:
        with contextlib.redirect_stdout(io.StringIO()):
            return installer.main(list(args))

    def test_ush_swap_installs_the_service_pin_with_the_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            out = self.build_outputs(root)
            (out / "tilo-ush").write_bytes(b"fixture-ush")
            layer = root / "layer"
            self.assertEqual(self.run_installer("--from", str(out), "--dest", str(layer), "ush"), 0)
            self.assertEqual((layer / "usr/libexec/tilo-ush").read_bytes(), b"fixture-ush")
            unit = layer / "usr/lib/systemd/user/tilo-ush.service"
            self.assertEqual(unit.read_bytes(), (REPO / "os/usr/lib/systemd/user/tilo-ush.service").read_bytes())
            self.assertIn("Environment=TILO_USH_BIN=/usr/libexec/tilo-ush", unit.read_text())
            self.assertEqual(unit.stat().st_mode & 0o777, 0o644)

    def test_app_host_swap_installs_the_exact_confined_service_with_the_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            out = self.build_outputs(root)
            (out / "tilo-app-host").write_bytes(b"fixture-app-host")
            layer = root / "layer"
            self.assertEqual(self.run_installer("--from", str(out), "--dest", str(layer), "app-host"), 0)
            self.assertEqual((layer / "usr/libexec/tilo-app-host").read_bytes(), b"fixture-app-host")
            unit = layer / "usr/lib/systemd/user/tilo-app-host.service"
            self.assertEqual(unit.read_bytes(), (REPO / "os/usr/lib/systemd/user/tilo-app-host.service").read_bytes())
            self.assertEqual(unit.stat().st_mode & 0o777, 0o644)
            self.assertIn("ProtectHome=read-only", unit.read_text())
            self.assertIn("ProtectSystem=strict", unit.read_text())
            self.assertIn("%h/.config/tilo-store/registry-checkpoints", unit.read_text())
            self.assertIn("%h/.local/share/flatpak %h/.cache/flatpak", unit.read_text())
            manifest = installer.load_manifest(REPO / "os/components.toml")
            self.assertEqual(manifest.component("app-host").user_units, ("tilo-app-host.service",))

    def test_installer_selects_manifest_layers_and_keeps_dev_and_production_apart(self):
        with self.subTest(phase='image layers match manifest'):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                out = self.build_outputs(root)
                image = root / "image"
                # The pinned Cosign is an external asset the image build is given; its
                # verification has a test of its own.
                cosign = root / "cosign"
                cosign.write_bytes(b"\x7fELF cosign fixture")
                with mock.patch.object(installer, "native_cosign", return_value=cosign):
                    status = self.run_installer("--from", str(out), "--dest", str(image), "--by-layer", "--all")
                self.assertEqual(status, 0)
                manifest = installer.load_manifest(REPO / "os/components.toml")
                expected = {f"{e.layer}{e.to}" for e in manifest.files()
                            if e.layer != "direct" and not e.dev_only}
                installed = {p.relative_to(image).as_posix()
                             for p in image.rglob("*") if p.is_file() or p.is_symlink()}
                self.assertEqual(installed, expected)
                shell = image / "desktop-binaries/usr/bin/shell"
                self.assertEqual(shell.stat().st_mode & 0o777, 0o755)
                self.assertEqual(os.readlink(image / "desktop-binaries/usr/bin/store"), "app")
                self.assertEqual(os.readlink(image / "desktop-binaries/usr/bin/ush"), "../libexec/tilo-ush")
                self.assertFalse((image / "desktop-binaries/usr/bin/browser").exists())
        with self.subTest(phase='production selection never includes dev only components'):
            manifest = installer.load_manifest(REPO / "os/components.toml")
            production = {c.name for c in installer.select_components(manifest, [], True)}
            dev = {c.name for c in installer.select_components(manifest, [], False, True)}
            self.assertIn("vmd", dev)
            self.assertIn("shell", dev)
            self.assertIn("native-runtime-host", dev)
            # The control CLI is a plain client every image ships.
            self.assertIn("control", production)
            self.assertNotIn("control", dev)
            self.assertNotIn("vmd", production)
            self.assertNotIn("test-registry", production)
            self.assertIn("shell", production)
            self.assertIn("native-runtime-host", production)
        with self.subTest(phase='developer CLI installs globally through its VM component'):
            manifest = installer.load_manifest(REPO / "os/components.toml")
            component = manifest.component("vm-cli")
            self.assertEqual(component.scope, "vm")
            self.assertTrue(component.dev_only)
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                out = self.build_outputs(root)
                image = root / "cli-layer"
                status = self.run_installer("--from", str(out), "--dest", str(image),
                                            "vm-cli")
                self.assertEqual(status, 0)
                binary = image / "usr/libexec/tilo/tilo-vm"
                self.assertEqual(binary.read_bytes(), b"\x7fELFtilo-vm")
                self.assertEqual(binary.stat().st_mode & 0o777, 0o755)
                self.assertEqual({p.relative_to(image).as_posix() for p in image.rglob("*")
                                  if p.is_file()}, {"usr/libexec/tilo/tilo-vm"})
                # `tilo device` drives it; the old name is a link to tilo.
                self.assertEqual(os.readlink(image / "usr/bin/tilo-vm"), "tilo")
        with self.subTest(phase='dev cargo plan adds dev features'):
            manifest = installer.load_manifest(REPO / "os/components.toml")
            dev = installer.select_components(manifest, [], False, True)
            packages, bins, features = installer.cargo_plan(dev, True)
            self.assertIn("dev-overrides", features)
            self.assertEqual(set(bins), {"shell", "tilo-runtime-mcp", "tilo-vmd", "tilo-device-admin", "tilo-vm",
                                         "tilo-test-registry"})
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
