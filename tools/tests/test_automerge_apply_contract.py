import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_automerge_apply_contract.py"
SPEC = importlib.util.spec_from_file_location("check_automerge_apply_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


def clippy(paths=contract.BANNED) -> str:
    entries = "".join(f'    {{ path = "{path}", reason = "x" }},\n' for path in paths)
    return f"disallowed-methods = [\n{entries}]\n"


class AutomergeApplyContractTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_raw_automerge_apply_calls_are_banned_and_cannot_be_suppressed(self):
        with self.subTest(phase='complete configs and wrapped calls pass'):
            root = self.fixture(
                {
                    "clippy.toml": clippy(),
                    "crates/a/clippy.toml": clippy(),
                    "crates/a/src/lib.rs": "fn f(d: &mut AutoCommit) { d.load_incoming(b).unwrap(); }\n",
                    "crates/doc/tilo-automerge/src/lib.rs": (
                        "#[expect(clippy::disallowed_methods, reason = \"wrapper\")]\n"
                        "fn f(d: &mut AutoCommit) { d.load_incremental(b); }\n"
                    ),
                }
            )
            self.assertEqual(contract.audit(root), [])
        with self.subTest(phase='a config missing a ban fails'):
            root = self.fixture(
                {"clippy.toml": clippy(), "crates/a/clippy.toml": clippy(contract.BANNED[1:])}
            )
            self.assertEqual(
                [str(v) for v in contract.audit(root)],
                [f"crates/a/clippy.toml: disallowed-methods does not ban {contract.BANNED[0]}"],
            )
        with self.subTest(phase='a suppressed raw call fails'):
            root = self.fixture(
                {
                    "clippy.toml": clippy(),
                    "crates/a/src/lib.rs": (
                        "#![allow(clippy::disallowed_methods)]\n"
                        "fn f(a: &mut AutoCommit, b: &mut AutoCommit) {\n    a.merge(&mut b);\n}\n"
                    ),
                }
            )
            violations = contract.audit(root)
            self.assertEqual([(v.path, v.line) for v in violations], [("crates/a/src/lib.rs", 3)])
        with self.subTest(phase='other merges and other suppressions pass'):
            root = self.fixture(
                {
                    "clippy.toml": clippy(),
                    "crates/a/src/lib.rs": (
                        "#[allow(clippy::disallowed_methods, reason = \"env\")]\n"
                        "fn f(a: &mut Config, b: Config) { a.merge(b); }\n"
                    ),
                }
            )
            self.assertEqual(contract.audit(root), [])


if __name__ == "__main__":
    unittest.main()
