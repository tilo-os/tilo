"""Hermetic admission tests, not evidence of Android runtime compatibility."""
import argparse
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import warnings
import unittest
from unittest.mock import patch
import zipfile

MODULE = Path(__file__).resolve().parents[1] / "android/qualify_apk.py"
SPEC = importlib.util.spec_from_file_location("qualify_apk", MODULE)
qualify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qualify)


def elf(machine):
    data = bytearray(64)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", data, 18, machine)
    return bytes(data)


class Qualification(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.apk = self.root / "calculator.apk"

    def apk_with(self, extra=()):
        with zipfile.ZipFile(self.apk, "w") as archive:
            archive.writestr("AndroidManifest.xml", b"fixture manifest")
            archive.writestr("classes.dex", b"fixture dex")
            for name, data in extra:
                archive.writestr(name, data)
        return qualify.digest(self.apk)

    def inspect(self, extra=(), arch="x86_64"):
        return qualify.inspect_apk(self.apk, arch, self.apk_with(extra))

    def test_pure_dex_and_matching_native_payload(self):
        self.assertEqual(self.inspect()["native_abis"], [])
        self.assertFalse(self.inspect([("lib/x86_64/libtest.so", elf(62))])["runtime_accepted"])
        self.assertEqual(self.inspect([("lib/arm64-v8a/libtest.so", elf(183))],
                                      "aarch64")["native_abis"], ["arm64-v8a"])

    def test_no_implicit_cpu_translation(self):
        with self.assertRaisesRegex(ValueError, "no CPU translation"):
            self.inspect([("lib/arm64-v8a/libtest.so", elf(183))])

    def test_abi_directory_cannot_lie(self):
        with self.assertRaisesRegex(ValueError, "disagrees"):
            self.inspect([("lib/x86_64/libtest.so", elf(183))])

    def test_path_traversal_is_refused_without_extraction(self):
        for name in ("../escape", "/escape", "lib//escape", "lib/./escape", "lib\\escape"):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "unsafe"):
                self.inspect([(name, b"bad")])
        self.assertFalse((self.root.parent / "escape").exists())

    def test_pin_mismatch_and_expansion_limits(self):
        self.apk_with()
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            qualify.inspect_apk(self.apk, "x86_64", "0" * 64)
        with patch.object(qualify, "MAX_EXPANDED_BYTES", 1):
            with self.assertRaisesRegex(ValueError, "archive limits"):
                qualify.inspect_apk(self.apk, "x86_64", qualify.digest(self.apk))

    def test_split_apk_is_not_a_standalone_app(self):
        with zipfile.ZipFile(self.apk, "w") as archive:
            archive.writestr("AndroidManifest.xml", b"split")
        with self.assertRaisesRegex(ValueError, "standalone APK"):
            qualify.inspect_apk(self.apk, "x86_64", qualify.digest(self.apk))

    def test_duplicate_and_symlink_apk_members(self):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with self.assertRaisesRegex(ValueError, "duplicate"):
                self.inspect([("classes.dex", b"second")])
        self.apk_with()
        with zipfile.ZipFile(self.apk, "a") as archive:
            member = zipfile.ZipInfo("link")
            member.create_system = 3
            member.external_attr = 0o120777 << 16
            archive.writestr(member, "../outside")
        with self.assertRaisesRegex(ValueError, "symlink"):
            qualify.inspect_apk(self.apk, "x86_64", qualify.digest(self.apk))

    def runtime(self):
        root = self.root / "runtime-input"
        (root / "bin").mkdir(parents=True)
        (root / "licenses").mkdir()
        binary = root / "bin/android-translation-layer"
        binary.write_bytes(elf(62))
        binary.chmod(0o755)
        (root / "licenses/NOTICE").write_text("fixture, not upstream code")
        receipt = self.root / "runtime.json"
        receipt.write_text(json.dumps({"schema": 1, "architecture": "x86_64",
                                       "files": qualify.runtime_files(root)}))
        return root, receipt, qualify.digest(receipt)

    def test_runtime_changes_and_symlink_escape_are_refused(self):
        root, receipt, pin = self.runtime()
        qualify.verify_runtime(root, receipt, pin, "x86_64")
        (root / "extra").write_text("unrecorded")
        with self.assertRaisesRegex(ValueError, "files differ"):
            qualify.verify_runtime(root, receipt, pin, "x86_64")
        (root / "extra").unlink()
        (root / "escape").symlink_to("/etc/passwd")
        with self.assertRaisesRegex(ValueError, "regular files"):
            qualify.verify_runtime(root, receipt, pin, "x86_64")

    def test_runtime_architecture_and_receipt_type(self):
        root, receipt, pin = self.runtime()
        with self.assertRaisesRegex(ValueError, "architecture"):
            qualify.verify_runtime(root, receipt, pin, "aarch64")
        receipt.write_text("[]")
        with self.assertRaisesRegex(ValueError, "schema"):
            qualify.verify_runtime(root, receipt, qualify.digest(receipt), "x86_64")

    def test_signature_verifier_failure_wrong_and_multiple_signers(self):
        signer = "a" * 64
        for code, output in ((1, ""), (0, "Signer #1 certificate SHA-256 digest: " + "b" * 64),
                             (0, "\n".join(f"Signer #{n} certificate SHA-256 digest: {signer}"
                                            for n in (1, 2)))):
            with self.subTest(code=code, output=output), patch.object(qualify.subprocess, "run") as run:
                run.return_value = argparse.Namespace(returncode=code, stdout=output)
                with self.assertRaisesRegex(ValueError, "signature invalid"):
                    qualify.verify_signer(self.apk, Path("/apksigner"), signer)

    def test_prepare_uses_existing_launcher_without_grants_or_jit(self):
        apk_pin = self.apk_with()
        root, receipt, pin = self.runtime()
        output = self.root / "prepared payload"
        args = argparse.Namespace(apk=self.apk, apk_sha256=apk_pin, architecture="x86_64",
                                  apksigner=Path("/apksigner"), signer_sha256="a" * 64,
                                  runtime_root=root, runtime_receipt=receipt,
                                  runtime_receipt_sha256=pin, output=output,
                                  workspace_id="qualification", wayland_socket=Path("/isolated/socket"),
                                  activity="org/example/MainActivity")
        with patch.object(qualify.subprocess, "run") as run:
            run.return_value = argparse.Namespace(returncode=0, stdout=
                "Signer #1 certificate SHA-256 digest: " + "a" * 64)
            result = qualify.prepare(args)
        import tomllib
        manifest = tomllib.loads((output / "tilo.toml").read_text())
        self.assertEqual(manifest["sandbox"], {"seccomp": "tilo-default", "net": {"mode": "off"}})
        self.assertFalse(result["notification_provider_declared"])
        self.assertEqual(qualify.digest(output / "app.apk"), apk_pin)
        self.assertEqual(qualify.runtime_files(output / "runtime"), qualify.runtime_files(root))
        self.assertEqual((output / "activity.txt").read_text(), args.activity + "\n")
        self.assertEqual(result["desktop_action"], {"provider": "applications", "id": manifest["package"]["id"]})
        argv = result["launcher_argv"]
        self.assertEqual(argv[:2], ["tilo-launcher", "run"])
        self.assertIn("-Xusejit:false", result["runtime_policy"]["art_options"])
        self.assertNotIn("--", argv)  # GUI run does not forward arbitrary trailing argv.
        self.assertNotIn("--capabilities", argv)
        self.assertNotIn("--allow-unsigned-runtime", argv)
        self.assertTrue(result["signature_verified"])
        self.assertFalse(result["runtime_accepted"])
        with self.assertRaisesRegex(ValueError, "already exists"):
            with patch.object(qualify, "verify_signer"):
                qualify.prepare(args)
        args.output = self.root / "offline payload"
        args.workspace_id = None
        args.wayland_socket = None
        with patch.object(qualify, "verify_signer"):
            offline = qualify.prepare(args)
        self.assertIsNone(offline["launcher_argv"])
        self.assertEqual(offline["desktop_action"], result["desktop_action"])
        args.output = self.root / "explicit notification fixture"
        args.notifications = True
        with patch.object(qualify, "verify_signer"):
            declared = qualify.prepare(args)
        sandbox = tomllib.loads((args.output / "tilo.toml").read_text())["sandbox"]
        self.assertEqual(sandbox, {"seccomp": "tilo-default", "net": {"mode": "off"},
                                  "portals": {"notifications": "via-host"}})
        self.assertTrue(declared["notification_provider_declared"])
        self.assertFalse(declared["runtime_accepted"])

    def test_prepare_refuses_activity_injection_on_the_production_function(self):
        for activity in ("calculator/Calculator\nextra", "--help", "../Calculator"):
            with self.subTest(activity=activity), self.assertRaisesRegex(ValueError, "invalid ATL activity"):
                qualify.prepare(argparse.Namespace(activity=activity))


if __name__ == "__main__":
    unittest.main()
