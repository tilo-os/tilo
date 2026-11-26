"""The public hosted CLI must keep the tested program and share one restricted Mac identity."""

import hashlib
import gzip
import importlib.util
import io
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from test_macos_signing_macho import fixture

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "dist/apple/hosted-cli-sign.py"
TEAM = "TESTTEAM00"
LEAF = "ABCDEF1234" * 4


class HostedCliSigning(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)

    def archive(self, name, programs):
        archive = self.root / name
        with tarfile.open(archive, "w:gz") as package:
            for member_name, data in programs:
                member = tarfile.TarInfo(member_name)
                member.size = len(data)
                member.mode = 0o755
                member.uname = member.gname = "root"
                package.addfile(member, io.BytesIO(data))
        return archive

    def run_helper(self, mode, raw, signed, *args, digest=None):
        pinned = digest or hashlib.sha256(raw.read_bytes()).hexdigest()
        return subprocess.run(
            [sys.executable, SCRIPT, mode, raw, pinned, "aarch64", signed, *args],
            capture_output=True, text=True,
        )

    def compiler_fixture(self):
        src = self.root / "sample.c"
        src.write_text("int main(void) { return 0; }\n")
        cli = self.root / "tilo"
        subprocess.run(["/usr/bin/clang", "-arch", "arm64", src, "-o", cli], check=True)
        return cli

    def test_payload_compares_both_named_programs_to_the_pinned_archive(self):
        names = ("tilo", "tilo-runtime")
        raw = self.archive("raw", [(name, fixture()) for name in names])
        signed = self.archive("signed", [(name, fixture(128)) for name in names])
        success = self.run_helper("payload", raw, signed)
        self.assertEqual(success.returncode, 0, success.stderr)
        receipt = json.loads(success.stdout)
        self.assertEqual(receipt["raw_sha256"], hashlib.sha256(raw.read_bytes()).hexdigest())
        self.assertEqual(receipt["signed_sha256"], hashlib.sha256(signed.read_bytes()).hexdigest())
        self.assertEqual(set(receipt["programs"]), set(names))
        for malformed in (
            [("tilo", fixture(128)), ("tilo-runtime", fixture(128, extra_data=b"other code"))],
            [("tilo", fixture(128)), ("tilo", fixture(128))],
            [("tilo", fixture(128)), ("../tilo-runtime", fixture(128))],
        ):
            with self.subTest(member_names=[item[0] for item in malformed]):
                changed = self.archive("changed", malformed)
                self.assertNotEqual(self.run_helper("payload", raw, changed).returncode, 0)
        self.assertNotEqual(self.run_helper("payload", raw, signed, digest="0" * 64).returncode, 0)

    def test_signed_snapshot_pin_and_exact_ustar_headers(self):
        programs = [(name, fixture(128)) for name in ("tilo", "tilo-runtime")]
        raw = self.archive("raw", programs)
        signed = self.archive("signed", programs)
        original = gzip.decompress(signed.read_bytes())
        global_pax = tarfile.TarInfo("global")
        global_pax.type = tarfile.XGLTYPE
        signed.write_bytes(gzip.compress(global_pax.tobuf(format=tarfile.USTAR_FORMAT) + original))
        rejected = self.run_helper("payload", raw, signed)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("expected the regular executable tilo", rejected.stderr)
        signed.write_bytes(b"not a gzip stream")
        mismatch = self.run_helper("payload", raw, signed, "--signed-sha256", "a" * 64)
        self.assertNotEqual(mismatch.returncode, 0)
        self.assertIn("SHA-256 differs", mismatch.stderr)

    @unittest.skipUnless(sys.platform == "darwin", "requires Apple's codesign and clang")
    def test_a_valid_ad_hoc_signature_with_the_product_identifier_cannot_qualify(self):
        cli = self.compiler_fixture()
        raw_data = cli.read_bytes()
        other = self.root / "signed-sample"
        shutil.copyfile(cli, other)
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", "--identifier",
                        "so.tilo.cli.hosted", "--options", "runtime", other],
                       check=True, capture_output=True)
        names = ("tilo", "tilo-runtime")
        raw = self.archive("raw", [(n, raw_data) for n in names])
        signed = self.archive("signed", [(n, other.read_bytes()) for n in names])
        refused = self.run_helper("verify", raw, signed, "--team", TEAM, "--identity", LEAF)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("codesign", refused.stderr)
        self.assertEqual(refused.stdout, "")

    @unittest.skipUnless(sys.platform == "darwin", "requires Apple's codesign and csreq")
    def test_signature_qualification_also_checks_each_embedded_requirement_and_entitlements(self):
        spec = importlib.util.spec_from_file_location("hosted_cli_sign", SCRIPT)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        import collection_native

        cli = self.compiler_fixture()
        daemon = self.root / "tilo-runtime"
        shutil.copy(cli, daemon)
        identity = {"team": TEAM, "sha1": LEAF, "name": "Developer ID Application:"}
        requirement = helper.product_requirement(TEAM)
        def sign(path, req, *options):
            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", "--identifier",
                            helper.IDENTIFIER, "--requirements", "=designated => " + req, *options, path],
                           check=True, capture_output=True)

        sign(cli, requirement)
        sign(daemon, requirement)
        # No Apple private identity in this test. Only the Apple trust-chain verification
        # is mocked here. Apple reads the actual requirements and entitlements of both fixtures.
        with patch.object(collection_native, "verify_code_signature") as chain:
            helper.check_apple(self.root, "arm64", identity, requirement)
            self.assertEqual([call.args[0].name for call in chain.call_args_list], ["tilo", "tilo-runtime"])
            sign(daemon, 'identifier "other.product"')
            with self.assertRaisesRegex(ValueError, "requirement in tilo-runtime"):
                helper.check_apple(self.root, "arm64", identity, requirement)
            entitlements = self.root / "unexpected.plist"
            entitlements.write_bytes(plistlib.dumps({"get-task-allow": True}))
            sign(daemon, requirement, "--entitlements", entitlements)
            with self.assertRaisesRegex(ValueError, "entitlements differ: tilo-runtime"):
                helper.check_apple(self.root, "arm64", identity, requirement)


if __name__ == "__main__":
    unittest.main()
