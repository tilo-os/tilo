"""tilo-esp-write applies only its caller's allowed ESP changes, atomically,
and never follows a symlink; tilo-deployment's plans are what it accepts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import struct
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

    def test_staged_counters_stay_in_the_same_directory(self):
        staged = self.esp / "loader/entries.staged"
        staged.mkdir()
        source = staged / "bootc_fedora-44-1.conf"
        source.write_text("incoming\n")
        for src, dst in (
            ("loader/entries.staged/bootc_fedora-44-1.conf", "loader/entries/bootc_fedora-44-1+3.conf"),
            ("loader/entries/bootc_fedora-44-1.conf", "loader/entries.staged/bootc_fedora-44-1+3.conf"),
        ):
            self.assertRefused(self.run_plan([{"op": "rename", "src": src, "dst": dst}]), "may not rename")
        result = self.run_plan([{"op": "rename",
                                "src": "loader/entries.staged/bootc_fedora-44-1.conf",
                                "dst": "loader/entries.staged/bootc_fedora-44-1+3.conf"}])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((staged / "bootc_fedora-44-1+3.conf").read_text(), "incoming\n")
        self.assertFalse(source.exists())

    def make_uki(self, name, digest):
        # Minimal PE fixture read by the shipped UKI parser, rather than a fake
        # parser that could conceal a sealed-command-line mismatch.
        payload = f"root=tilo-auto composefs={digest} tilo.mode=normal".encode() + b"\0"
        image = bytearray(512 + len(payload))
        struct.pack_into("<I", image, 0x3c, 128)
        image[128:132] = b"PE\0\0"
        struct.pack_into("<H", image, 134, 1)
        image[152:160] = b".cmdline"
        struct.pack_into("<IIII", image, 160, len(payload), 4096, len(payload), 512)
        image[512:] = payload
        directory = self.esp / "EFI/Linux/bootc"
        directory.mkdir(exist_ok=True)
        (directory / name).write_bytes(image)

    def run_entries(self, source=None, fail_writer=False):
        text = DEPLOYMENT.read_text()
        helpers = text[text.index("esp_path() {"):text.index("# --- listing")]
        counters = text[text.index("cmd_entries() {"):text.index("# Derive the repair-mode boot entry")]
        script = helpers + counters + """
id() { printf '0\n'; }
findmnt() { printf '%s\n' "$FIXTURE_ROOT_SOURCE"; }
bootctl() { printf 'Current Entry: bootc_fedora-44-1.conf\n'; }
die() { echo "$*" >&2; exit 1; }
log() { echo "$*"; }
cmd_repair_entry() { :; }
cmd_entries
"""
        env = self.env(TILO_ESP_PLAN_DIR=str(self.plan_dir),
                       TILO_ESP_WRITE_CMD="false" if fail_writer else f"{HELPER} boot-entries",
                       UKI_CMDLINE=str(REPO / "os/dev/uki-cmdline.py"), BOOT_TRIES="3",
                       FIXTURE_ROOT_SOURCE=source or "composefs:" + "a" * 128)
        return subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True)

    def bootc_entry(self, directory, name, uki):
        path = self.esp / "loader" / directory
        path.mkdir(exist_ok=True)
        (path / name).write_text(f"title Tilo\nuki /EFI/Linux/bootc/{uki}\n")

    def test_staging_counts_incoming_and_preserves_booted_identity(self):
        self.make_uki("old.efi", "a" * 128)
        self.make_uki("new.efi", "b" * 128)
        self.bootc_entry("entries", "bootc_fedora-44-1.conf", "old.efi")
        self.bootc_entry("entries.staged", "bootc_fedora-44-0.conf", "old.efi")
        self.bootc_entry("entries.staged", "bootc_fedora-44-1.conf", "new.efi")
        result = self.run_entries()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1.conf").exists())
        self.assertTrue((self.esp / "loader/entries.staged/bootc_fedora-44-0.conf").exists())
        counted = self.esp / "loader/entries.staged/bootc_fedora-44-1+3.conf"
        self.assertTrue(counted.exists())
        self.assertFalse((counted.parent / "bootc_fedora-44-1.conf").exists())
        # A repeated maintenance pass must not reset tries already spent.
        counted.rename(counted.with_name("bootc_fedora-44-1+2-1.conf"))
        self.assertEqual(self.run_entries().returncode, 0)
        self.assertTrue(counted.with_name("bootc_fedora-44-1+2-1.conf").exists())

    def test_finalized_filename_collision_does_not_count_old_fallback(self):
        self.make_uki("old.efi", "a" * 128)
        self.make_uki("new.efi", "b" * 128)
        self.bootc_entry("entries", "bootc_fedora-44-0.conf", "old.efi")
        self.bootc_entry("entries", "bootc_fedora-44-1.conf", "new.efi")
        result = self.run_entries()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-0.conf").exists())
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1+3.conf").exists())

    def test_unobserved_identity_cannot_change_the_esp(self):
        self.make_uki("new.efi", "b" * 128)
        self.bootc_entry("entries", "bootc_fedora-44-1.conf", "new.efi")
        result = self.run_entries()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no retained entry matches", result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1.conf").exists())
        self.assertFalse((self.esp / "loader/entries/bootc_fedora-44-1+3.conf").exists())
        (self.esp / "EFI/Linux/bootc/new.efi").write_bytes(b"unreadable UKI")
        result = self.run_entries()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot read entry identity", result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1.conf").exists())

    def test_counter_writer_failure_is_fatal(self):
        self.make_uki("old.efi", "a" * 128)
        self.make_uki("new.efi", "b" * 128)
        self.bootc_entry("entries", "bootc_fedora-44-1.conf", "old.efi")
        self.bootc_entry("entries.staged", "bootc_fedora-44-1.conf", "new.efi")
        result = self.run_entries(fail_writer=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not apply the boot counters", result.stderr)

    def test_non_composefs_keeps_bootloader_selected_entry(self):
        self.bootc_entry("entries", "bootc_fedora-43-1.conf", "other.efi")
        result = self.run_entries(source="/dev/mapper/root")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-44-1.conf").exists())
        self.assertTrue((self.esp / "loader/entries/bootc_fedora-43-1+3.conf").exists())

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
