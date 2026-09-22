import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "tilo_install_disk.py"
SPEC = importlib.util.spec_from_file_location("tilo_install_disk", MODULE_PATH)
tilo_install_disk = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(tilo_install_disk)


class TiloInstallDiskTests(unittest.TestCase):
    def fixture(self, base: Path):
        script = base / "disk-image.sh"
        script.write_text("#!/bin/bash\n", encoding="utf-8")
        seed = base / "seed"
        seed.write_text("alice:$6$salt$" + "a" * 86 + "\n", encoding="utf-8")
        digest = "sha256:" + "a" * 64
        return script, seed, digest

    def test_dry_run_is_rootless_and_does_not_create_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            script, seed, digest = self.fixture(base)
            output = base / "new-output"
            receipt = base / "receipt.json"
            args = tilo_install_disk.parser().parse_args([
                "--image", f"code.example/tilo@{digest}",
                "--mirror", f"127.0.0.1:5000/tilo@{digest}",
                "--output", str(output),
                "--firstboot-seed", str(seed),
                "--disk-image", str(script),
                "--receipt", str(receipt),
                "--dry-run",
            ])
            with mock.patch.object(tilo_install_disk.subprocess, "run") as execute:
                self.assertEqual(tilo_install_disk.run(args), 0)
            execute.assert_not_called()
            self.assertFalse(output.exists())
            record = json.loads(receipt.read_text(encoding="utf-8"))
            self.assertEqual(record["status"], "preflight-passed")
            self.assertEqual(record["inputs"]["image_digest"], digest)
            self.assertNotIn("$6$salt", json.dumps(record))

    def test_refuses_digest_mismatch_and_existing_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            script, seed, digest = self.fixture(base)
            common = [
                "--image", f"code.example/tilo@{digest}",
                "--output", str(base / "output"),
                "--firstboot-seed", str(seed),
                "--disk-image", str(script),
                "--dry-run",
            ]
            mismatch = tilo_install_disk.parser().parse_args(
                ["--mirror", "127.0.0.1:5000/tilo@sha256:" + "b" * 64, *common]
            )
            with self.assertRaisesRegex(ValueError, "different digests"):
                tilo_install_disk.validate(mismatch)
            (base / "output").mkdir()
            matching = tilo_install_disk.parser().parse_args(
                ["--mirror", f"127.0.0.1:5000/tilo@{digest}", *common]
            )
            with self.assertRaisesRegex(ValueError, "must not already exist"):
                tilo_install_disk.validate(matching)

    def test_real_run_requires_create_once_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            script, seed, digest = self.fixture(base)
            output = base / "output"
            receipt = base / "receipt.json"
            args = tilo_install_disk.parser().parse_args([
                "--image", f"code.example/tilo@{digest}",
                "--mirror", f"127.0.0.1:5000/tilo@{digest}",
                "--output", str(output),
                "--firstboot-seed", str(seed),
                "--disk-image", str(script),
                "--receipt", str(receipt),
            ])
            completed = mock.Mock(returncode=0)
            with mock.patch.object(tilo_install_disk.subprocess, "run", return_value=completed):
                self.assertEqual(tilo_install_disk.run(args), 0)
            self.assertEqual(json.loads(receipt.read_text())["status"], "complete")
            with self.assertRaises(FileExistsError):
                tilo_install_disk.run(args)


if __name__ == "__main__":
    unittest.main()
