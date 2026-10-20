"""No VM, firmware writes, enrollment, or physical devices are used."""
import importlib.util
import json
from pathlib import Path
import tempfile
import shutil
import subprocess
import unittest
from restart_fixture import restart_fixture

SPEC = importlib.util.spec_from_file_location(
    "evidence", Path(__file__).parents[1] / "dev/secureboot-evidence.py")
evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(evidence)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.before = dict(schema=1, nonce="current-run", phase="before",
                           boot_id="00000000-0000-4000-8000-000000000001",
                           SecureBoot=0, SetupMode=1)
        self.after = dict(self.before, phase="after", SecureBoot=1, SetupMode=0,
                          boot_id="00000000-0000-4000-8000-000000000002")

    def test_enforcement_evidence(self):
        with self.subTest(phase="exact_enforcement_and_reboot"):
            evidence.validate(self.before, self.after, "current-run", "after")
            evidence.validate(self.before, dict(self.before, phase="refused"),
                              "current-run", "refused")
        restart_fixture(self)
        with self.subTest(phase="reads_exact_raw_efi_value_not_attribute_bytes"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                boot = root / "boot-id"
                boot.write_text(self.after["boot_id"])
                for name, value in (("SecureBoot", 1), ("SetupMode", 0)):
                    (root / f"{name}-{evidence.GUID}").write_bytes(b"\x07\0\0\0" + bytes([value]))
                report = evidence.collect("current-run", "after", root, boot)
                evidence.validate(self.before, report, "current-run", "after")
                path = root / f"SecureBoot-{evidence.GUID}"
                for raw in (b"", b"\x01", b"\0" * 4, b"\0" * 6, b"\0" * 4 + b"\x02"):
                    path.write_bytes(raw)
                    with self.assertRaises(ValueError):
                        evidence.collect("current-run", "after", root, boot)
                path.unlink()
                with self.assertRaises(FileNotFoundError):
                    evidence.collect("current-run", "after", root, boot)
        restart_fixture(self)
        with self.subTest(phase="refusal_must_leave_setup_mode_on_same_boot"):
            with self.assertRaises(ValueError):
                evidence.validate(self.before, dict(self.after, phase="refused"),
                                  "current-run", "refused")
        restart_fixture(self)
        with self.subTest(phase="actual_guest_runner_and_host_pipeline_fail_closed"):
            if not (shutil.which("bash")):
                self.skipTest("requires Bash")
            script = (Path(__file__).parents[1] / "dev/test-secureboot.sh").read_text()
            runner = script[script.index("runscript() {"):script.index("wait_ssh() {")]
            with tempfile.TemporaryDirectory() as directory:
                # Run the actual function with a local SSH transport double. No guest,
                # sudo invocation, fixed /tmp paths, or firmware operations occur.
                runner = runner.replace("/tmp/step.sh", f"{directory}/step.sh")
                common = 'set -euo pipefail\n' + f'OUT="{directory}"\n' + '''
GUEST_PW=test
GUEST=unused
SSHOPTS=()
sudo() { read -r password; shift 3; "$@"; }
export -f sudo
sshpass() { bash -c "${!#}"; }
'''+runner
                for body, expected in (("false; echo should-not-succeed", 1),
                                       ("printf success", 0)):
                    result = subprocess.run(["bash", "-c", common +
                        f"runscript regression '{body}' | tee /dev/null\necho host-success\n"],
                        capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stderr)
                    self.assertEqual("host-success" in result.stdout, expected == 0)
                    self.assertNotIn("should-not-succeed", result.stdout)

    def test_evidence_rejections(self):
        with self.subTest(phase="disabled_missing_stale_contradictory_or_wrong_run_rejected"):
            cases = [dict(self.after, SecureBoot=0), dict(self.after, SetupMode=1),
                     dict(self.after, SecureBoot=True),
                     dict(self.after, boot_id=self.before["boot_id"]),
                     dict(self.after, nonce="old-run"), dict(self.after, phase="before"),
                     dict(self.after, log="SecureBoot enabled; lockdown armed"),
                     {k: v for k, v in self.after.items() if k != "SetupMode"},
                     "SecureBoot disabled; lockdown armed"]
            for report in cases:
                with self.subTest(report=report), self.assertRaises(ValueError):
                    evidence.validate(self.before, report, "current-run", "after")
        restart_fixture(self)
        with self.subTest(phase="existing_enrolled_store_rejected"):
            with self.assertRaises(ValueError):
                evidence.validate(dict(self.before, SetupMode=0), self.after,
                                  "current-run", "after")
        restart_fixture(self)
        with self.subTest(phase="duplicate_contradictory_fields_rejected"):
            with self.assertRaises(ValueError):
                json.loads('{"SecureBoot":0,"SecureBoot":1}',
                           object_pairs_hook=evidence.unique_object)


if __name__ == "__main__":
    unittest.main()
