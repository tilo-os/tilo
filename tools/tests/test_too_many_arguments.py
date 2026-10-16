import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_too_many_arguments.py"
SPEC = importlib.util.spec_from_file_location("check_too_many_arguments", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = check
SPEC.loader.exec_module(check)


class TooManyArgumentsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_every_escape_hatch_is_rejected(self):
        cases = {
            "outer allow": '#[allow(clippy::too_many_arguments, reason = "x")]\nfn f() {}\n',
            "inner allow": "#![allow(clippy::too_many_arguments)]\n",
            "expect across lines": (
                "#[expect(\n    clippy::too_many_lines,\n    clippy::too_many_arguments,\n)]\nfn f() {}\n"
            ),
            "cfg_attr": "#[cfg_attr(test, allow(clippy::too_many_arguments))]\nfn f() {}\n",
        }
        for name, source in cases.items():
            with self.subTest(name):
                root = self.fixture({"crates/a/src/lib.rs": source})
                violations = check.audit(root)
                self.assertEqual(len(violations), 1, violations)
                self.assertIn("crates/a/src/lib.rs:1:", violations[0])

    def test_a_manifest_cannot_lower_the_lint(self):
        root = self.fixture(
            {
                "crates/a/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = "warn"\n',
                "crates/b/Cargo.toml": '[lints.clippy]\ntoo_many_arguments = { level = "allow" }\n',
                "Cargo.toml": '[workspace.lints.clippy]\ntoo_many_arguments = "deny"\n',
            }
        )
        violations = check.audit(root)
        self.assertEqual(
            sorted(v.split(":")[0] for v in violations),
            ["crates/a/Cargo.toml", "crates/b/Cargo.toml"],
        )

    def test_malformed_manifests_cannot_pass_the_audit(self):
        for text in ('[lints.clippy]\ntoo_many_arguments = "allow"\nbroken = [',
                     '[package\nname = "broken"\n'):
            with self.subTest(text=text):
                root = self.fixture({"crates/a/Cargo.toml": text})
                violations = check.audit(root)
                self.assertEqual(len(violations), 1, violations)
                self.assertIn("crates/a/Cargo.toml: cannot audit malformed Cargo manifest:",
                              violations[0])
                self.assertRegex(violations[0], r"\(at (?:line|end of document)")

    def test_denying_mentioning_and_vendored_code_pass(self):
        root = self.fixture(
            {
                "crates/a/src/lib.rs": (
                    "#![allow(clippy::complexity)]\n#![deny(clippy::too_many_arguments)]\n"
                    "// the too_many_arguments lint keeps signatures short\n"
                ),
                "vendor/dep/src/lib.rs": "#[allow(clippy::too_many_arguments)]\nfn f() {}\n",
            }
        )
        self.assertEqual(check.audit(root), [])


if __name__ == "__main__":
    unittest.main()
