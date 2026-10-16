"""Cleanup regressions exercise production functions without creating a VM."""
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest import mock

DEV = Path(__file__).resolve().parents[1] / "dev"
spec = importlib.util.spec_from_file_location("nearby_identity", DEV / "nearby-vm-identity.py")
identity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(identity)


class OwnedIdentityTest(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.proc = self.root / "proc"
        (self.proc / "self").mkdir(parents=True)
        (self.proc / "self" / "stat").write_text("observer namespace exists")
        self.pidfile = self.root / "runtime" / "qemu.pid"
        self.pidfile.parent.mkdir()
        self.pidfile.write_text("321\n")
        self.pid = self.proc / "321"
        self.pid.mkdir()
        self.write_stat(80)
        (self.pid / "cmdline").write_bytes(
            b"/usr/bin/qemu-system-x86_64\0-pidfile\0" + bytes(str(self.pidfile), "utf8") + b"\0"
        )
        (self.pid / "exe").symlink_to("/usr/bin/qemu-system-x86_64")
        self.patch = mock.patch.object(identity, "PROC_ROOT", self.proc)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def write_stat(self, start, state="S"):
        fields = [state] + ["0"] * 18 + [str(start)]
        (self.pid / "stat").write_text("321 (qemu fixture) " + " ".join(fields))

    def test_capture_owned_identity_and_verify_before_stop(self):
        captured = identity.snapshot(self.pidfile)
        self.assertEqual((captured["pid"], captured["start_time"]), (321, 80))
        self.assertEqual(identity.before_stop(captured)["state"], "verified-before-stop")

    def test_pid_file_deletion_does_not_hide_live_owned_qemu(self):
        captured = identity.snapshot(self.pidfile)
        self.pidfile.unlink()
        with mock.patch.object(identity.time, "monotonic", side_effect=range(20)), mock.patch.object(identity.time, "sleep"):
            with self.assertRaisesRegex(ValueError, "still alive"):
                identity.after_stop(captured)

    def test_normal_exit_is_verified_by_original_pid(self):
        captured = identity.snapshot(self.pidfile)
        (self.pid / "stat").unlink()
        self.pidfile.unlink()
        self.assertEqual(identity.after_stop(captured)["state"], "owned-process-exited")

    def test_reused_pid_is_refused_before_normal_stop(self):
        captured = identity.snapshot(self.pidfile)
        self.write_stat(81)
        with self.assertRaisesRegex(ValueError, "reused"):
            identity.before_stop(captured)

    def test_reused_pid_after_stop_is_not_the_owned_process(self):
        captured = identity.snapshot(self.pidfile)
        self.write_stat(81)
        self.assertEqual(identity.after_stop(captured)["state"], "owned-process-exited")

    def test_changed_pid_file_refuses_stop_even_after_original_exit(self):
        captured = identity.snapshot(self.pidfile)
        (self.pid / "stat").unlink()
        self.pidfile.write_text("999\n")
        with self.assertRaisesRegex(ValueError, "pid file changed"):
            identity.before_stop(captured)

    def test_foreign_runtime_is_not_owned(self):
        (self.pid / "cmdline").write_bytes(b"qemu-system-x86_64\0-pidfile\0/another/runtime/qemu.pid\0")
        with self.assertRaisesRegex(ValueError, "another runtime"):
            identity.snapshot(self.pidfile)

    def test_unobservable_process_namespace_fails(self):
        captured = identity.snapshot(self.pidfile)
        (self.proc / "self" / "stat").unlink()
        with self.assertRaises(FileNotFoundError):
            identity.after_stop(captured)

    def test_permission_error_is_not_mistaken_for_exit(self):
        captured = identity.snapshot(self.pidfile)
        original = Path.read_text
        def read(path, *args, **kwargs):
            if path == self.pid / "stat":
                raise PermissionError("fixture denies observation")
            return original(path, *args, **kwargs)
        with mock.patch.object(Path, "read_text", read):
            with self.assertRaises(PermissionError):
                identity.after_stop(captured)


def function(source, name):
    match = re.search(r"(?ms)^" + name + r"\(\) \{\n.*?^\}", source)
    if match is None:
        raise AssertionError("missing production function " + name)
    return match.group()


class CleanupTrapTest(unittest.TestCase):
    def run_cleanup(self, gate=0, stop_a=0, after_a=0, before_a=0, source=None):
        source = source or (DEV / "test-nearby-pairing.sh").read_text()
        definitions = []
        for name in ("stop_owned_vm", "cleanup"):
            if name == "cleanup" or name + "() {" in source:
                definitions.append(function(source, name))
        with tempfile.TemporaryDirectory() as work:
            out = Path(work)
            (out / "verdict.txt").write_text("PASS\n" if gate == 0 else "FAIL\n")
            runner = '''set -euo pipefail
out=$1; run_a=owned-a; run_b=owned-b; dev_dir=unused
collect() { printf 'collect %s\\n' "$1" >> "$out/calls.txt"; }
note() { printf '%s\\n' "$*" >> "$out/notes.txt"; }
backend() {
  local label=$1; shift
  printf '%s %s\\n' "$label" "$*" >> "$out/calls.txt"
  if [ "$label" = a ]; then
    case "$1:${4:-}" in
      stop:) return STOP_A;;
      tool:before-stop) return BEFORE_A;;
      tool:after-stop) return AFTER_A;;
    esac
  fi
  printf '{}\\n'
}
vm_a() { backend a "$@"; }
vm_b() { backend b "$@"; }
'''.replace("STOP_A", str(stop_a)).replace("AFTER_A", str(after_a)).replace("BEFORE_A", str(before_a))
            runner += "\n".join(definitions) + "\ntrap cleanup EXIT\n(exit " + str(gate) + ")\n"
            result = subprocess.run(["bash", "-c", runner, "fixture", str(out)], capture_output=True, text=True, timeout=5)
            files = {p.name: p.read_text() for p in out.iterdir() if p.is_file()}
            return result.returncode, files

    def test_normal_cleanup_passes_and_retains_original_evidence(self):
        rc, files = self.run_cleanup()
        self.assertEqual(rc, 0)
        self.assertEqual(files["cleanup-exit-status.txt"], "0\n")
        self.assertEqual(files["gate-verdict.txt"], "PASS\n")
        self.assertEqual(files["verdict.txt"], "PASS\n")

    def test_failed_stop_cannot_report_gate_pass(self):
        rc, files = self.run_cleanup(stop_a=73)
        self.assertEqual(rc, 1)
        self.assertEqual(files["gate-exit-status.txt"], "0\n")
        self.assertEqual(files["gate-verdict.txt"], "PASS\n")
        self.assertEqual(files["verdict.txt"], "FAIL\n")
        self.assertIn("stop=73 verify=0", files["a-cleanup-status.txt"])
        self.assertIn("b stop", files["calls.txt"])

    def test_successful_stop_command_with_live_process_still_fails(self):
        rc, files = self.run_cleanup(after_a=1)
        self.assertEqual(rc, 1)
        self.assertIn("stop=0 verify=1", files["a-cleanup-status.txt"])
        self.assertEqual(files["cleanup-exit-status.txt"], "1\n")

    def test_identity_refusal_prevents_stop_but_still_cleans_other_vm(self):
        rc, files = self.run_cleanup(before_a=1)
        self.assertEqual(rc, 1)
        self.assertNotIn("a stop", files["calls.txt"])
        self.assertIn("b stop", files["calls.txt"])

    def test_existing_gate_failure_status_is_preserved(self):
        rc, files = self.run_cleanup(gate=42, stop_a=73)
        self.assertEqual(rc, 42)
        self.assertEqual(files["gate-exit-status.txt"], "42\n")
        self.assertEqual(files["exit-status.txt"], "42\n")


class BootSnapshotTest(unittest.TestCase):
    def run_boot(self, boot_rc, snapshot_rc):
        source = (DEV / "test-nearby-pairing.sh").read_text()
        with tempfile.TemporaryDirectory() as work:
            out = Path(work)
            runner = '''set -euo pipefail
out=$1; display=2d; dev_dir=unused
vm_a() {
  printf '%s\\n' "$*" >> "$out/calls.txt"
  if [ "$1" = boot ]; then return BOOT_RC; fi
  printf '{}\\n'; return SNAPSHOT_RC
}
'''.replace("BOOT_RC", str(boot_rc)).replace("SNAPSHOT_RC", str(snapshot_rc))
            runner += function(source, "boot_owned_vm") + '\nboot_owned_vm a owned-a vm_a\n'
            result = subprocess.run(["bash", "-c", runner, "fixture", str(out)], capture_output=True, text=True, timeout=5)
            return result.returncode, {p.name: p.read_text() for p in out.iterdir()}

    def test_partial_boot_failure_still_captures_identity(self):
        rc, files = self.run_boot(42, 0)
        self.assertEqual(rc, 42)
        self.assertEqual(files["a-boot-exit-status.txt"], "42\n")
        self.assertIn("snapshot owned-a/qemu.pid", files["calls.txt"])
        self.assertEqual(files["a-qemu-identity.json"], "{}\n")

    def test_successful_boot_without_observable_identity_fails(self):
        rc, files = self.run_boot(0, 1)
        self.assertEqual(rc, 1)
        self.assertEqual(files["a-boot-exit-status.txt"], "0\n")

    def test_failed_boot_keeps_its_status_when_identity_capture_also_fails(self):
        rc, _ = self.run_boot(42, 1)
        self.assertEqual(rc, 42)


if __name__ == "__main__":
    unittest.main()
