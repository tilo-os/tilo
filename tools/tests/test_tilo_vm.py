import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "tilo_vm.py"
SPEC = importlib.util.spec_from_file_location("tilo_vm", MODULE_PATH)
tilo_vm = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(tilo_vm)


class TiloVmTests(unittest.TestCase):
    def test_launcher_arguments_validate_and_redact(self):
        parsed = tilo_vm.parse_launcher_arguments(
            ["DebugBoot", "CpuCount=8", "Password=do-not-record"]
        )
        self.assertEqual(
            tilo_vm.redacted_arguments(parsed),
            {"DebugBoot": True, "CpuCount": "8", "Password": "<redacted>"},
        )
        with self.assertRaises(ValueError):
            tilo_vm.parse_launcher_arguments(["bad-name=value"])

    def test_run_directories_are_unique_and_receipts_are_create_once(self):
        with tempfile.TemporaryDirectory() as temporary:
            runs = Path(temporary)
            run_id, run_dir = tilo_vm.new_run_dir(runs, "quick-test")
            self.assertEqual(run_id, "quick-test")
            tilo_vm.write_new(run_dir / "run.json", "first\n")
            with self.assertRaises(FileExistsError):
                tilo_vm.write_new(run_dir / "run.json", "second\n")
            with self.assertRaises(FileExistsError):
                tilo_vm.new_run_dir(runs, "quick-test")

    def test_launcher_command_is_an_argument_vector(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            launcher = base / "launch.ps1"
            launcher.write_text("", encoding="utf-8")
            with mock.patch.object(tilo_vm, "powershell", return_value="pwsh"):
                command = tilo_vm.launcher_command(
                    launcher,
                    base / "run",
                    41001,
                    41002,
                    [("DiskPath", "a path with spaces.raw"), ("DebugBoot", None)],
                    check_only=True,
                    state_parameter="StateDirectory",
                    qmp_parameter="QmpPort",
                    serial_parameter="SerialPort",
                )
            self.assertEqual(command[0], "pwsh")
            self.assertIn("a path with spaces.raw", command)
            self.assertEqual(command[-1], "-CheckOnly")
            self.assertNotIn("&", command)

    def test_serial_tail_does_not_read_the_whole_log(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "serial.log"
            log.write_bytes(b"old output" + b"new output")
            self.assertEqual(tilo_vm.read_tail(log, len(b"new output")), b"new output")

    def test_preflight_writes_normalized_receipt_without_touching_disk(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            launcher = base / "fake.ps1"
            launcher.write_text("fake", encoding="utf-8")
            disk = base / "disk.raw"
            disk.write_bytes(b"EFI PART fixture")
            runs = base / "runs"
            args = tilo_vm.parser().parse_args(
                [
                    "--runs-dir",
                    str(runs),
                    "preflight",
                    "--launcher",
                    str(launcher),
                    "--disk",
                    str(disk),
                    "--run-id",
                    "receipt-test",
                ]
            )
            completed = mock.Mock(returncode=0, stdout="{}\n", stderr="")
            with mock.patch.object(tilo_vm, "run_launcher", return_value=completed):
                self.assertEqual(tilo_vm.preflight_or_start(args, start=False), 0)
            receipt = json.loads((runs / "receipt-test" / "run.json").read_text())
            self.assertEqual(receipt["schema"], "tilo.vm-run/v1")
            self.assertEqual(receipt["kind"], "preflight")
            self.assertEqual(receipt["inputs"]["disk"]["bytes"], len(b"EFI PART fixture"))
            self.assertEqual(disk.read_bytes(), b"EFI PART fixture")

    def test_status_uses_receipt_port(self):
        with tempfile.TemporaryDirectory() as temporary:
            run_dir = Path(temporary) / "one"
            run_dir.mkdir()
            (run_dir / "run.json").write_text(
                json.dumps({"run_id": "one", "ports": {"qmp": 4555}, "pid": 123}),
                encoding="utf-8",
            )
            args = mock.Mock(run="one", runs_dir=Path(temporary))
            fake_qmp = mock.MagicMock()
            fake_qmp.__enter__.return_value.command.return_value = {"status": "running"}
            with mock.patch.object(tilo_vm, "QMP", return_value=fake_qmp) as qmp_type, mock.patch.object(
                tilo_vm, "process_status", return_value="running"
            ):
                self.assertEqual(tilo_vm.command_status(args), 0)
            qmp_type.assert_called_once_with(4555)


if __name__ == "__main__":
    unittest.main()
