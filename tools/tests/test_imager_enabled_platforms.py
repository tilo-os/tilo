"""Check enabled release lanes and execute the real artifact gate without builds."""
import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest

import re
import textwrap

ROOT = Path(__file__).resolve().parents[2]


def section(text, name, indent):
    """Extract one mapping at the known indentation; fail on missing/ambiguous keys."""
    matches = list(re.finditer(rf"(?m)^{' ' * indent}{re.escape(name)}:\s*\n", text))
    if len(matches) != 1:
        raise AssertionError(f"expected one {name} mapping, got {len(matches)}")
    lines = text[matches[0].end():].splitlines()
    result = []
    for line in lines:
        if line.strip() and not line.lstrip().startswith("#") and len(line) - len(line.lstrip()) <= indent:
            break
        result.append(line)
    return "\n".join(result)


def step_script(text, step_id):
    # Both tested steps use block scalars. Refuse a layout change we cannot read.
    marker = f"        id: {step_id}\n"
    if text.count(marker) != 1:
        raise AssertionError(f"expected one step {step_id}")
    block = text.split(marker)[1].split("\n      - ")[0]
    lines = block.split("        run: |\n", 1)[1].splitlines()
    script = []
    for line in lines:
        if line.strip() and not line.startswith("          "):
            break
        script.append(line)
    return textwrap.dedent("\n".join(script)).rstrip() + "\n"


class EnabledImagerPlatforms(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / ".forgejo/workflows/release-imager.yml").read_text()
        self.winget_monitor = (ROOT / ".forgejo/workflows/release-imager-winget.yml").read_text()

    def test_release_gate_requires_every_enabled_platform_artifact(self):
        with self.subTest(phase='full native and tags require both macos architectures'):
            self.doCleanups()
            self.setUp()
            macos = section(self.workflow, "macos", 2)
            self.assertIn(
                "    if: github.event.inputs.installer_only != 'true' && (startsWith(github.ref, 'refs/tags/imager-v') || github.event.inputs.full_native == 'true')",
                macos,
            )
            for arch in ("x86_64", "aarch64"):
                self.assertIn(f"runner: imager-macos-{arch}", macos)
            publish = section(self.workflow, "publish", 2)
            self.assertIn("    needs: [linux, windows, macos]", publish)
            install = section(self.workflow, "homebrew-install-test", 2)
            self.assertNotIn("package_kind: cask", install)
            for arch in ("x86_64", "aarch64"):
                self.assertIn(f"runner: imager-linux-{arch}", install)
            native_casks = section(self.workflow, "homebrew-macos-install-test", 2)
            self.assertIn("    if: startsWith(github.ref, 'refs/tags/imager-v')", native_casks)
            for arch in ("x86_64", "aarch64"):
                self.assertIn(f"runner: imager-macos-{arch}", native_casks)
            homebrew = section(self.workflow, "homebrew", 2)
            self.assertIn('TILO_IMAGER_MACOS_ENABLED: "1"', homebrew)
            site = section(self.workflow, "site-downloads", 2)
            self.assertIn("    needs: [homebrew-install-test, homebrew-macos-install-test, winget]", site)
        with self.subTest(phase='real release gate accepts linux windows and rejects missing or corrupt'):
            self.doCleanups()
            self.setUp()
            script = step_script(self.workflow, "enabled-assets")
            with tempfile.TemporaryDirectory() as temp:
                assets = Path(temp) / "release-assets"
                assets.mkdir()
                for arch in ("x86_64", "aarch64"):
                    names = [f"tilo-installer-linux-{arch}.img.zst"]
                    names += [f"tilo-imager-linux-{arch}.{ext}" for ext in ("AppImage", "tar.gz", "flatpak")]
                    names += [f"tilo-imager-windows-{arch}.{ext}" for ext in ("exe", "zip")]
                    names += [f"tilo-imager-setup-windows-{arch}.exe"]
                    names += [f"tilo-imager-macos-{arch}.{ext}" for ext in ("dmg", "zip")]
                    for name in names:
                        (assets / name).write_bytes(b"fixture")
                        digest = hashlib.sha256(b"fixture").hexdigest()
                        (assets / (name + ".sha256")).write_text(f"{digest}  {name}\n")
                        # This gate checks signature presence; real signature verification
                        # belongs to verify-linux-release.sh and its existing tests.
                        (assets / (name + ".asc")).write_text("signature fixture")
                for name in ("Tilo.Imager.installer.yaml", "Tilo.Imager.yaml", "Tilo.Imager.locale.en-US.yaml"):
                    (assets / name).write_text("manifest fixture")
                def run():
                    return subprocess.run(["bash", "-c", script], cwd=temp, capture_output=True, text=True)
                good = run()
                self.assertEqual(0, good.returncode, good.stderr)
                required = assets / "tilo-imager-windows-aarch64.zip"
                required.unlink()
                missing = run()
                self.assertNotEqual(0, missing.returncode)
                self.assertIn("missing release artifact", missing.stderr)
                required.write_bytes(b"corrupt")
                corrupt = run()
                self.assertNotEqual(0, corrupt.returncode)
                self.assertIn("invalid release digest", corrupt.stderr)
        with self.subTest(phase='release gate explicitly requires every macos artifact'):
            self.doCleanups()
            self.setUp()
            script = step_script(self.workflow, "enabled-assets")
            for arch in ("x86_64", "aarch64"):
                for ext in ("dmg", "zip"):
                    self.assertIn(f"tilo-imager-macos-{arch}.{ext}", script)

    def test_signing_and_publication_are_oidc_broker_only_and_manual_runs_rehearse(self):
        with self.subTest(phase='release signing and publication are oidc broker only'):
            self.doCleanups()
            self.setUp()
            combined = self.workflow + "\n" + self.winget_monitor
            retired_interfaces = (
                "IMAGER_ALLOW_STATIC_SIGNING_BOOTSTRAP",
                "IMAGER_LINUX_SIGNING_KEY_B64",
                "IMAGER_LINUX_SIGNING_KEY_ID",
                "IMAGER_WINDOWS_CERT_PFX_B64",
                "IMAGER_WINDOWS_CERT_PASSWORD",
                "IMAGER_WINDOWS_SIGNER",
                "IMAGER_MAC_CERT_P12_B64",
                "IMAGER_MAC_CERT_PASSWORD",
                "IMAGER_MAC_SIGNING_IDENTITY",
                "IMAGER_APPLE_ID",
                "IMAGER_APPLE_TEAM_ID",
                "IMAGER_APPLE_APP_PASSWORD",
                "secrets.HOMEBREW_TAP_TOKEN",
                "secrets.WINGET_GITHUB_TOKEN",
                "secrets.SITE_RELEASE_TOKEN",
            )
            for interface in retired_interfaces:
                with self.subTest(interface=interface):
                    self.assertNotIn(interface, combined)

            signing_brokers = {
                "linux": "IMAGER_LINUX_CREDENTIAL_BROKER",
                "windows": "IMAGER_WINDOWS_CREDENTIAL_BROKER",
                "macos": "IMAGER_MACOS_CREDENTIAL_BROKER",
            }
            for job, variable in signing_brokers.items():
                with self.subTest(job=job):
                    native = section(self.workflow, job, 2)
                    self.assertIn(f"vars.{variable}", native)
                    self.assertIn("enable-openid-connect: true", native)
                    self.assertIn("release signing is OIDC-broker-only", native)
                    self.assertNotIn("secrets.", native)

            publication_brokers = {
                "publish": ("IMAGER_RELEASE_CREDENTIAL_BROKER", "release"),
                "homebrew": ("IMAGER_HOMEBREW_CREDENTIAL_BROKER", "homebrew"),
                "winget": ("IMAGER_WINGET_CREDENTIAL_BROKER", "winget"),
                "site-downloads": ("IMAGER_SITE_CREDENTIAL_BROKER", "site"),
            }
            for job, (variable, mode) in publication_brokers.items():
                with self.subTest(job=job):
                    publication = section(self.workflow, job, 2)
                    self.assertIn(f"vars.{variable}", publication)
                    self.assertIn("enable-openid-connect: true", publication)
                    self.assertNotIn("secrets.", publication)
                    self.assertRegex(
                        publication,
                        rf"acquire-release-token\.sh\s+\\?\s*{mode}\s+\"\$CREDENTIAL_BROKER\"",
                    )

            self.assertIn("vars.IMAGER_SITE_CREDENTIAL_BROKER != ''", self.winget_monitor)
            self.assertIn("enable-openid-connect: true", self.winget_monitor)
            self.assertNotIn("secrets.", self.winget_monitor)
            self.assertRegex(
                self.winget_monitor,
                r'acquire-release-token\.sh\s+\\?\s*site\s+"\$CREDENTIAL_BROKER"',
            )
        with self.subTest(phase='manual full native dispatch remains a non publishing rehearsal'):
            self.doCleanups()
            self.setUp()
            self.assertIn("full_native:", self.workflow)
            for job in ("windows", "macos"):
                native = section(self.workflow, job, 2)
                self.assertIn("github.event.inputs.full_native == 'true'", native)
            for job in ("publish", "homebrew", "winget", "site-downloads"):
                publication = section(self.workflow, job, 2)
                self.assertIn("if: startsWith(github.ref, 'refs/tags/imager-v')", publication)
                self.assertNotIn("full_native", publication)


if __name__ == "__main__":
    unittest.main()
