"""Exercise the installed-purpose publication in the production backend."""
import pathlib
import subprocess
import tempfile
import unittest

BACKEND = pathlib.Path(__file__).parents[1] / "usr/libexec/tilo-install"


class InstallPurposeTests(unittest.TestCase):
    def test_same_os_purpose_is_published_for_power_defaults(self):
        source = BACKEND.read_text()
        start = source.index("# Same OS for either purpose.")
        stop = source.index("# --- 3b.", start)
        block = source[start:stop]
        for purpose in ("computer", "server"):
            with self.subTest(purpose=purpose), tempfile.TemporaryDirectory() as root:
                etc = pathlib.Path(root) / "etc"
                (etc / "tilo").mkdir(parents=True)
                subprocess.run(
                    ["bash", "-eu", "-c",
                     'label_installed_tilo_path() { :; }\n' + block],
                    env={"DEPLOY_ETC": str(etc), "PURPOSE_VAL": purpose},
                    check=True,
                )
                marker = etc / "tilo/install-purpose"
                self.assertEqual(marker.read_text(), purpose + "\n")
                self.assertEqual(marker.stat().st_mode & 0o777, 0o644)
                self.assertEqual(list(etc.iterdir()), [etc / "tilo"])

    def test_backend_rejects_unknown_purpose_before_disk_changes(self):
        source = BACKEND.read_text()
        start = source.index('case "$PURPOSE_VAL" in')
        stop = source.index("esac", start) + len("esac")
        result = subprocess.run(
            ["bash", "-c", 'die() { exit 91; }\n' + source[start:stop]],
            env={"PURPOSE_VAL": "different-os"},
            check=False,
        )
        self.assertEqual(result.returncode, 91)


if __name__ == "__main__":
    unittest.main()
