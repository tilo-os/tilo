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
        self.workflow = (ROOT / ".github/workflows/release-imager.yml").read_text()

    def test_no_event_can_allocate_macos_or_require_it_for_publication(self):
        self.assertIn("    if: ${{ false }}", section(self.workflow, "macos", 2))
        publish = section(self.workflow, "publish", 2)
        self.assertIn("    needs: [linux, windows]", publish)
        self.assertNotIn("macos", publish)
        for job in ("homebrew", "homebrew-install-test"):
            block = section(self.workflow, job, 2)
            self.assertNotIn("imager-macos", block)
        install = section(self.workflow, "homebrew-install-test", 2)
        self.assertNotIn("package_kind: cask", install)
        self.assertIn("runner: ubuntu-24.04", install)
        self.assertIn("runner: tilo-build-arm64", install)
        deferred = section(self.workflow, "homebrew-macos-install-test", 2)
        self.assertIn("    if: ${{ false }}", deferred)
        self.assertIn("runner: macos-15-intel", deferred)
        self.assertIn("runner: macos-15", deferred)
        homebrew = section(self.workflow, "homebrew", 2)
        self.assertIn('TILO_IMAGER_MACOS_ENABLED: "0"', homebrew)

    def test_real_release_gate_accepts_linux_windows_and_rejects_missing_or_corrupt(self):
        script = step_script(self.workflow, "enabled-assets")
        with tempfile.TemporaryDirectory() as temp:
            assets = Path(temp) / "release-assets"
            assets.mkdir()
            for arch in ("x86_64", "aarch64"):
                names = [f"tilo-installer-linux-{arch}.img.zst"]
                names += [f"tilo-imager-linux-{arch}.{ext}" for ext in ("AppImage", "tar.gz", "flatpak")]
                names += [f"tilo-imager-windows-{arch}.{ext}" for ext in ("exe", "zip")]
                names += [f"tilo-imager-setup-windows-{arch}.exe"]
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


if __name__ == "__main__":
    unittest.main()
