import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_source_content.py"
SPEC = importlib.util.spec_from_file_location("source_content", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)


class SourceContentTests(unittest.TestCase):
    def test_every_ascii_control_has_the_same_content_classification(self):
        for byte in range(32):
            with self.subTest(byte=byte):
                data = b"before" + bytes([byte]) + b"after"
                if byte == 13:
                    expected = ["non-LF line endings"]
                elif byte in (9, 10, 11, 12):
                    expected = []
                else:
                    expected = ["non-text content"]
                self.assertEqual(contract.content_issues(data), expected)
        self.assertEqual(contract.content_issues(b"before\x7fafter"), [])
        self.assertEqual(contract.content_issues("é\ttext\n".encode()), [])
        self.assertEqual(contract.content_issues(b"bad\r\xff"), ["non-text content"])

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q")
        self.git("config", "commit.gpgsign", "false")
        self.manifest([])

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args])

    def manifest(self, entries):
        path = self.root / contract.EXCEPTIONS
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(entries) + "\n")
        self.git("add", "--", contract.EXCEPTIONS)

    def stage(self, name, data):
        (self.root / name).write_bytes(data)
        self.git("add", "--", name)

    def test_content_not_extension_decides(self):
        self.stage("source.rs", b"\x00binary")
        self.stage("image.png", "Unicode text: café\n".encode())
        failures, count = contract.audit(self.root, staged=True)
        self.assertEqual(count, 3)
        self.assertEqual(failures, ["source.rs: non-text content; new/modified non-text or non-LF blobs are forbidden"])

    def test_checks_staged_bytes_even_when_worktree_is_fixed(self):
        self.stage("source.txt", b"bad\r\n")
        (self.root / "source.txt").write_bytes(b"good\n")
        failures, _ = contract.audit(self.root, staged=True)
        self.assertEqual(failures, ["source.txt: non-LF line endings; new/modified non-text or non-LF blobs are forbidden"])

    def test_rejects_bare_cr_invalid_utf8_and_controls(self):
        for name, data in [("cr", b"a\rb"), ("utf8", b"\xff"), ("control", b"a\x1bb")]:
            self.stage(name, data)
        failures, _ = contract.audit(self.root, staged=True)
        self.assertEqual(len(failures), 3)

    def test_exact_exception_does_not_allow_replacement_binary(self):
        self.stage("fixture", b"\x00original")
        oid = self.git("rev-parse", ":fixture").decode().strip()
        self.manifest([{"path": "fixture", "oid": oid, "reason": "Required regression fixture"}])
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org", "-c", "core.hooksPath=/dev/null", "commit", "-qm", "test: legacy fixture")
        self.assertEqual(contract.audit(self.root, staged=True)[0], [])
        self.stage("fixture", b"\x00replacement")
        self.assertEqual(contract.audit(self.root, staged=True)[0], ["fixture: non-text content; new/modified non-text or non-LF blobs are forbidden"])

    def test_merge_preserves_parent_asset_but_rejects_new_binary_bytes(self):
        def commit(message):
            self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                     "-c", "core.hooksPath=/dev/null", "commit", "-qm", message)

        commit("test: base")
        base = self.git("rev-parse", "HEAD").decode().strip()
        self.git("checkout", "-qb", "asset-parent")
        self.stage("fixture", b"\x00original")
        oid = self.git("rev-parse", ":fixture").decode().strip()
        self.manifest([{"path": "fixture", "oid": oid, "reason": "Required upstream asset"}])
        commit("test: inherited upstream asset")
        self.git("checkout", "-qb", "task", base)
        self.stage("source", b"text\n")
        commit("test: task source")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                 "merge", "--no-commit", "asset-parent")
        self.assertEqual(contract.audit(self.root, staged=True)[0], [])
        # Even an updated exception cannot admit new bytes during a merge.
        self.stage("fixture", b"\x00replacement")
        replacement = self.git("rev-parse", ":fixture").decode().strip()
        self.manifest([{"path": "fixture", "oid": replacement, "reason": "Changed asset"}])
        self.assertEqual(contract.audit(self.root, staged=True)[0],
                         ["fixture: non-text content; new/modified non-text or non-LF blobs are forbidden"])

    def test_duplicate_exception_fails(self):
        entry = {"path": "fixture", "oid": "0" * 40, "reason": "Test fixture"}
        self.manifest([entry, entry])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            contract.audit(self.root, staged=True)

    def test_head_mode_inspects_commit_not_worktree(self):
        self.stage("source", b"text\n")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org", "-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture")
        (self.root / "source").write_bytes(b"\x00uncommitted")
        self.assertEqual(contract.audit(self.root)[0], [])


if __name__ == "__main__":
    unittest.main()
