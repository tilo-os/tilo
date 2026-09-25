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

    def test_complete_configs_and_wrapped_calls_pass(self):
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

    def test_a_config_missing_a_ban_fails(self):
        root = self.fixture(
            {"clippy.toml": clippy(), "crates/a/clippy.toml": clippy(contract.BANNED[1:])}
        )
        self.assertEqual(
            [str(v) for v in contract.audit(root)],
            [f"crates/a/clippy.toml: disallowed-methods does not ban {contract.BANNED[0]}"],
        )

    def test_a_suppressed_raw_call_fails(self):
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

    def test_other_merges_and_other_suppressions_pass(self):
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
