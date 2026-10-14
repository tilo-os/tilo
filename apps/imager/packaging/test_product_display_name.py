"""Check shipped display metadata without renaming stable installation identities."""
import configparser
import hashlib
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

PACKAGING = Path(__file__).resolve().parent


def inno_metadata():
    """Read sections and expand the string defines used by display metadata."""
    source = (PACKAGING / "windows/tilo-imager.iss").read_text()
    defines = dict(re.findall(r'^\s*#define (\w+) "([^"]*)"$', source, re.MULTILINE))
    sections = {}
    section = None
    for line in source.splitlines():
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            sections[section] = []
        elif section and line and not line.startswith(";"):
            sections[section].append(re.sub(
                r"\{#(\w+)\}", lambda match: defines.get(match[1], match[0]), line))
    return defines, sections


class ProductDisplayNameTests(unittest.TestCase):
    def test_app_manifest_and_linux_launcher_names(self):
        with (PACKAGING.parent / "tilo-app.toml").open("rb") as stream:
            manifest = tomllib.load(stream)
        self.assertEqual(manifest["package"]["name"], "Tilo Installer")
        for locale, name in manifest["package"]["names"].items():
            with self.subTest(locale=locale):
                self.assertEqual(name, "Tilo Installer")
        self.assertEqual(manifest["package"]["id"], "so.tilo.imager")
        self.assertEqual(manifest["entrypoint"]["bin"], "bin/imager")
        desktop = configparser.ConfigParser(interpolation=None)
        desktop.read(PACKAGING / "linux/tilo-imager.desktop")
        self.assertEqual(desktop["Desktop Entry"]["Name"], "Tilo Installer")

    def test_macos_names_preserve_bundle_and_executable_identity(self):
        with (PACKAGING / "macos/Info.plist").open("rb") as stream:
            info = plistlib.load(stream)
        for key in ("CFBundleDisplayName", "CFBundleName"):
            self.assertEqual(info[key], "Tilo Installer")
        self.assertEqual(info["CFBundleIdentifier"], "so.tilo.imager")
        self.assertEqual(info["CFBundleExecutable"], "tilo-imager")

    def test_windows_names_preserve_upgrade_identity_and_launcher(self):
        defines, sections = inno_metadata()
        setup = dict(line.split("=", 1) for line in sections["Setup"])
        self.assertEqual(setup["AppName"], "Tilo Installer")
        self.assertEqual(setup["AppPublisher"], "Tilo Project")
        self.assertEqual(setup["AppId"], "{{A47B49F8-4B63-4CE8-886D-3C124393028C}")
        self.assertEqual(setup["DefaultDirName"], r"{localappdata}\Programs\Tilo Imager")
        self.assertEqual(setup["PrivilegesRequired"], "lowest")
        self.assertEqual(defines["MyAppExeName"], "tilo-imager.exe")
        icons = sections["Icons"]
        self.assertEqual(len(icons), 2)
        for location, line in zip(("autoprograms", "autodesktop"), icons):
            self.assertIn(f'Name: "{{{location}}}\\Tilo Installer";', line)
            self.assertIn(r'IconFilename: "{app}\tilo-imager.exe"', line)
        self.assertIn("Tasks: desktopicon", icons[1])
        self.assertIn('Flags: unchecked', sections["Tasks"][0])
        self.assertEqual(len(sections["Run"]), 1)
        self.assertIn('Description: "Launch Tilo Installer"', sections["Run"][0])
        for line in icons + sections["Run"]:
            self.assertIn(r'Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"', line)
            self.assertIn(r'-File ""{app}\Start-Tilo-Imager.ps1""', line)

    def test_windows_upgrade_removes_only_exact_legacy_shortcuts(self):
        _, sections = inno_metadata()
        self.assertEqual(sections["InstallDelete"], [
            r'Type: files; Name: "{autoprograms}\Tilo Imager.lnk"',
            r'Type: files; Name: "{autodesktop}\Tilo Imager.lnk"',
        ])
        self.assertNotIn("UninstallDelete", sections)

    def test_generated_winget_display_name_preserves_package_and_publisher(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR", "/var/tmp")) as temporary:
            root = Path(temporary)
            for arch in ("x86_64", "aarch64"):
                installer = root / f"tilo-imager-setup-windows-{arch}.exe"
                installer.write_bytes(arch.encode())
                digest = hashlib.sha256(installer.read_bytes()).hexdigest()
                installer.with_suffix(".exe.sha256").write_text(f"{digest}  {installer.name}\n")
            with patch.dict(os.environ, {
                "TILO_RELEASE_SERVER": "https://github.com",
                "TILO_RELEASE_REPOSITORY": "tilo-os/releases",
            }):
                subprocess.run([
                    sys.executable, str(PACKAGING / "generate-winget.py"), str(root),
                    "--version", "1.2.3", "--base-url",
                    "https://github.com/tilo-os/releases/releases/download/imager-v1.2.3",
                ], check=True, capture_output=True, text=True)
            documents = list((root / "winget").glob("*.yaml"))
            self.assertEqual(len(documents), 3)
            for document in documents:
                self.assertIn("PackageIdentifier: Tilo.Imager\n", document.read_text())
            locale = (root / "winget/Tilo.Imager.locale.en-US.yaml").read_text()
            self.assertIn("PackageName: Tilo Installer\n", locale)
            self.assertIn("Publisher: Tilo Project\n", locale)


if __name__ == "__main__":
    unittest.main()
