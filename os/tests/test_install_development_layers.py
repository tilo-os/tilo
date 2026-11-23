"""Exercise the installed-system handover of a development target's prebuilt layers."""
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
BACKEND = ROOT / "os/usr/libexec/tilo-install"
STAGING = ROOT / "crates/os/imager-backend/src/staging/layout.rs"

STUBS = """\
die() { printf 'die: %s\\n' "$*" >&2; exit 91; }
step() { printf 'step: %s\\n' "$*" >> "$TRACE"; }
log() { printf 'log: %s\\n' "$*" >> "$TRACE"; }
find_installed_var() { printf '%s\\n' "$TARGET_VAR"; }
chcon() { printf '%s\\n' "$*" >> "$CHCON"; }
"""


def block() -> str:
    source = BACKEND.read_text()
    start = source.index('if [ -d "$DEVELOPMENT_LAYERS" ]')
    return source[start:source.index('VM_INSTALL_PROOF=""', start)]


class DevelopmentLayers(unittest.TestCase):
    def test_the_staged_directory_is_the_one_the_installer_reads(self):
        self.assertIn(
            "DEVELOPMENT_LAYERS=/etc/tilo/development-layers", BACKEND.read_text()
        )
        self.assertIn(
            'pub const STAGED_DEVELOPMENT_LAYERS_DIR: &str = "/etc/tilo/development-layers";',
            STAGING.read_text(),
        )

    def run_block(self, work: pathlib.Path, offline_target: int) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", "-eu", "-c", STUBS + block()],
            env={
                **os.environ,
                "DEVELOPMENT_LAYERS": str(work / "etc/tilo/development-layers"),
                "MNT": str(work / "installed"),
                "TARGET_VAR": str(work / "installed/var"),
                "OFFLINE_TARGET": str(offline_target),
                "TRACE": str(work / "trace"),
                "CHCON": str(work / "chcon"),
            },
            text=True,
            capture_output=True,
        )

    def layers(self, work: pathlib.Path) -> pathlib.Path:
        layers = work / "etc/tilo/development-layers"
        layers.mkdir(parents=True)
        (layers / "components.layer").write_bytes(b"components layer")
        (layers / "payload.layer").write_bytes(b"payload layer")
        return layers

    def test_a_development_target_gets_every_layer(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary).resolve()
            layers = self.layers(work)
            (work / "installed/var").mkdir(parents=True)
            result = self.run_block(work, offline_target=2)
            self.assertEqual(result.returncode, 0, result.stderr)
            incoming = work / "installed/var/lib/tilo-vm/incoming"
            self.assertEqual(sorted(p.name for p in incoming.iterdir()),
                             ["components.layer", "payload.layer"])
            for layer in layers.iterdir():
                self.assertEqual((incoming / layer.name).read_bytes(), layer.read_bytes())
                self.assertEqual(stat.S_IMODE((incoming / layer.name).stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(incoming.stat().st_mode), 0o700)
            labels = (work / "chcon").read_text().splitlines()
            for written in (incoming, incoming / "components.layer", incoming / "payload.layer"):
                self.assertTrue(
                    any("--reference=/var/lib" in line and str(written) in line
                        for line in labels),
                    f"{written} takes the installed /var label: {labels}",
                )
            log = (work / "trace").read_text()
            self.assertIn("handed over components.layer", log)
            self.assertIn("handed over payload.layer", log)

    def test_a_release_target_refuses_unsigned_layers(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary).resolve()
            self.layers(work)
            (work / "installed/var").mkdir(parents=True)
            for offline_target in (0, 1):
                with self.subTest(offline_target=offline_target):
                    result = self.run_block(work, offline_target=offline_target)
                    self.assertEqual(result.returncode, 91, result.stderr)
                    self.assertIn("release targets never take unsigned layers",
                                  result.stderr)
                    self.assertFalse((work / "installed/var/lib").exists())

    def test_no_layers_leave_the_target_alone(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary).resolve()
            (work / "installed/var").mkdir(parents=True)
            for path in (work / "etc/tilo/development-layers",
                         work / "etc/tilo/development-layers/nested"):
                path.mkdir(parents=True, exist_ok=True)
            (work / "etc/tilo/development-layers/nested/ignored").write_bytes(b"ignored")
            result = self.run_block(work, offline_target=2)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((work / "installed/var/lib").exists())


if __name__ == "__main__":
    unittest.main()
