"""tilo-esp-write applies only its caller's allowed ESP changes, atomically,
and never follows a symlink; tilo-deployment's plans are what it accepts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
HELPER = REPO / "os/usr/libexec/tilo-esp-write"
DEPLOYMENT = REPO / "os/usr/libexec/tilo-deployment"


class EspWrite(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.esp = self.root / "esp"
        (self.esp / "loader/entries").mkdir(parents=True)
        (self.esp / "EFI/Linux").mkdir(parents=True)
        (self.esp / "loader/entries/bootc_fedora-44-1.conf").write_text("title a\n")
        self.plans = self.root / "plans"
        self.plan_dir = self.plans / "boot-entries"
        self.plan_dir.mkdir(parents=True, mode=0o700)
        self.plan_dir.chmod(0o700)

    def env(self, **extra):
        return {"PATH": os.environ["PATH"], "TILO_ESP": str(self.esp),
                "TILO_ESP_PLAN_ROOT": str(self.plans), "TILO_ESP_PLAN_OWNER": str(os.getuid()),
                "TILO_ESP_WRITE_NO_REMOUNT": "1", **extra}

    def stage(self, name, data):
        (self.plan_dir / name).write_bytes(data)
        return hashlib.sha256(data).hexdigest()

    def run_plan(self, ops, caller="boot-entries", **extra):
        (self.plan_dir / "plan.json").write_text(json.dumps({"ops": ops}))
        return subprocess.run([str(HELPER), caller], env=self.env(**extra),
                              capture_output=True, text=True)

    def assertRefused(self, result, text):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(text, result.stderr)

    def test_applies_counters_and_the_repair_entry(self):
        digest = self.stage("repair.efi", b"MZ repair image")
        result = self.run_plan([
            {"op": "mkdir", "path": "EFI/tilo"},
            {"op": "rename", "src": "loader/entries/bootc_fedora-44-1.conf",
             "dst": "loader/entries/bootc_fedora-44-1+3.conf"},
            {"op": "install", "file": "repair.efi", "sha256": digest,
             "dst": "EFI/Linux/tilo-repair.efi"},
            {"op": "remove", "path": "EFI/tilo/repair.efi"},
        ])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1+3.conf").is_file())
        self.assertFalse((self.esp / "loader/entries/bootc_fedora-44-1.conf").exists())
        self.assertEqual((self.esp / "EFI/Linux/tilo-repair.efi").read_bytes(), b"MZ repair image")
        self.assertIn("caller=boot-entries", result.stdout)
        # The plan and its staged files are gone once applied.
        self.assertEqual(list(self.plan_dir.iterdir()), [])

    def test_only_the_callers_operations(self):
        digest = self.stage("x.efi", b"x")
        for op in (
            {"op": "install", "file": "x.efi", "sha256": digest, "dst": "EFI/Linux/bootc/x.efi"},
            {"op": "rename", "src": "loader/entries/bootc_fedora-44-1.conf",
             "dst": "loader/entries/other.conf"},
            {"op": "remove", "path": "loader/entries/bootc_fedora-44-1.conf"},
            {"op": "mkdir", "path": "EFI/other"},
            {"op": "chmod", "path": "EFI/Linux"},
        ):
            with self.subTest(op=op):
                self.stage("x.efi", b"x")
                result = self.run_plan([op])
                self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.run_plan([], caller="someone-else").returncode, 2)

    def test_the_signer_only_replaces_managed_artefacts(self):
        signer = self.plans / "secureboot-sign"
        signer.mkdir(mode=0o700)
        signer.chmod(0o700)
        (self.esp / "EFI/Linux/bootc").mkdir()
        (self.esp / "EFI/Linux/bootc/uki.efi").write_bytes(b"MZ unsigned")

        def sign(dst, data=b"MZ signed"):
            (signer / "signed.efi").write_bytes(data)
            (signer / "plan.json").write_text(json.dumps({"ops": [{
                "op": "install", "file": "signed.efi",
                "sha256": hashlib.sha256(data).hexdigest(), "dst": dst}]}))
            return subprocess.run([str(HELPER), "secureboot-sign"], env=self.env(),
                                  capture_output=True, text=True)

        self.assertEqual(sign("EFI/Linux/bootc/uki.efi").returncode, 0)
        self.assertEqual((self.esp / "EFI/Linux/bootc/uki.efi").read_bytes(), b"MZ signed")
        # A new .efi would be a new boot menu entry: the signer may not add one.
        self.assertRefused(sign("EFI/Linux/bootc/new.efi"), "may only replace")
        self.assertFalse((self.esp / "EFI/Linux/bootc/new.efi").exists())
        # Nor touch loader entries, or anything but an install.
        self.assertRefused(sign("loader/entries/bootc_fedora-44-1.conf"), "may not install")

    def test_traversal_is_refused(self):
        for path in ("../outside", "EFI/../../outside", "/boot/EFI/tilo", "EFI//tilo", "EFI/./tilo"):
            with self.subTest(path=path):
                self.assertRefused(self.run_plan([{"op": "mkdir", "path": path}]), "refused")
        self.assertFalse((self.root / "outside").exists())

    def test_symlinks_on_the_esp_are_refused(self):
        outside = self.root / "outside"
        outside.mkdir()
        (self.esp / "EFI/tilo").symlink_to(outside)
        digest = self.stage("repair.source", b"bootc/x.efi\n")
        result = self.run_plan([{"op": "install", "file": "repair.source", "sha256": digest,
                                 "dst": "EFI/tilo/repair.source"}])
        self.assertRefused(result, "symlink")
        self.assertEqual(list(outside.iterdir()), [])
        # A symlinked destination file is refused too, not written through.
        (self.esp / "EFI/Linux/tilo-repair.efi").symlink_to(outside / "target")
        digest = self.stage("repair.efi", b"MZ")
        result = self.run_plan([{"op": "install", "file": "repair.efi", "sha256": digest,
                                 "dst": "EFI/Linux/tilo-repair.efi"}])
        self.assertRefused(result, "symlink")
        self.assertFalse((outside / "target").exists())

    def test_staged_files_are_verified(self):
        self.stage("repair.efi", b"what was planned")
        wrong = hashlib.sha256(b"something else").hexdigest()
        result = self.run_plan([{"op": "install", "file": "repair.efi", "sha256": wrong,
                                 "dst": "EFI/Linux/tilo-repair.efi"}])
        self.assertRefused(result, "SHA-256")
        (self.root / "elsewhere").write_bytes(b"x")
        (self.plan_dir / "link.efi").symlink_to(self.root / "elsewhere")
        result = self.run_plan([{"op": "install", "file": "link.efi",
                                 "sha256": hashlib.sha256(b"x").hexdigest(),
                                 "dst": "EFI/Linux/tilo-repair.efi"}])
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.esp / "EFI/Linux/tilo-repair.efi").exists())

    def test_the_plan_directory_must_be_the_callers_alone(self):
        self.plan_dir.chmod(0o755)
        self.assertRefused(self.run_plan([]), "0700")

    def test_an_interruption_between_operations_leaves_whole_files(self):
        (self.esp / "EFI/tilo").mkdir()
        (self.esp / "EFI/tilo/repair.source").write_text("old\n")
        new_efi = self.stage("repair.efi", b"MZ new")
        new_src = self.stage("repair.source", b"new\n")
        result = self.run_plan([
            {"op": "install", "file": "repair.efi", "sha256": new_efi, "dst": "EFI/Linux/tilo-repair.efi"},
            {"op": "install", "file": "repair.source", "sha256": new_src, "dst": "EFI/tilo/repair.source"},
        ], TILO_ESP_WRITE_CRASH_AFTER="1")
        self.assertEqual(result.returncode, 99)
        self.assertEqual((self.esp / "EFI/Linux/tilo-repair.efi").read_bytes(), b"MZ new")
        self.assertEqual((self.esp / "EFI/tilo/repair.source").read_text(), "old\n")
        leftovers = [p for p in self.esp.rglob(".tilo-esp-write.*")]
        self.assertEqual(leftovers, [])
        # No partial *.efi in EFI/Linux, where it would be a boot entry.
        self.assertEqual(sorted(p.name for p in (self.esp / "EFI/Linux").iterdir()), ["tilo-repair.efi"])

    def test_tilo_deployment_plans_are_accepted(self):
        text = DEPLOYMENT.read_text()
        helpers = text[text.index("ESP_PLAN_DIR="):text.index("# --- listing")]
        script = helpers + """
die() { echo "$*" >&2; exit 1; }
esp_plan_reset
esp_plan_op rename src loader/entries/bootc_fedora-44-1.conf dst loader/entries/bootc_fedora-44-1+3.conf
printf 'bootc/x.efi\\n' > "$ESP_PLAN_DIR/repair.source"
esp_plan_op mkdir path EFI/tilo
esp_plan_install repair.source EFI/tilo/repair.source
esp_plan_commit
"""
        env = self.env(TILO_ESP_PLAN_DIR=str(self.plan_dir),
                       TILO_ESP_WRITE_CMD=f"{HELPER} boot-entries")
        result = subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1+3.conf").is_file())
        self.assertEqual((self.esp / "EFI/tilo/repair.source").read_text(), "bootc/x.efi\n")
        self.assertNotIn('mv -f "$f"', text)
        self.assertNotIn('> "$stamp"', text)


if __name__ == "__main__":
    unittest.main()
