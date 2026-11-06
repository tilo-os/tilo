import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_test_cargo.py"
SPEC = importlib.util.spec_from_file_location("check_test_cargo", SCRIPT)
test_cargo = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = test_cargo
SPEC.loader.exec_module(test_cargo)


def found(source: str, relative: str = "crates/x/tests/it.rs") -> list[int]:
    files = [(Path(relative), source)]
    return [line for _, line, _ in test_cargo.audit(Path("/nonexistent"), files)]


class TestCargo(unittest.TestCase):
    def test_a_test_that_builds_a_binary_is_found(self):
        self.assertEqual(
            found('fn t() { Command::new("cargo").args(["build"]).output().unwrap(); }'), [1]
        )
        self.assertEqual(
            found("fn t() {\n let mut c = Command::new(cargo_program());\n c.status().unwrap();\n}"),
            [2],
        )
        self.assertEqual(
            found('fn t() {\n let shell = shell();\n shell.program("cargo").spawn();\n}'), [3]
        )
        self.assertEqual(
            found(
                'fn t() {\n let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());\n'
                " Command::new(&cargo).spawn();\n}"
            ),
            [3],
        )

    def test_a_command_a_test_only_fills_in_is_not_a_build(self):
        self.assertEqual(found('fn t(c: &mut Command) { plan(c, "app"); c.args(["x"]); }'), [])
        self.assertEqual(
            found('fn t() { let plan = Plan { program: cargo.into_os_string() }; run(&plan); }'), []
        )
        self.assertEqual(found('fn t() { let c = Command::new("cargo"); assert!(c.get_args().count() == 0); }'), [])

    def test_another_program_is_not_cargo(self):
        self.assertEqual(
            found('fn t() { Command::new("rustc").arg("--version").output().unwrap(); }'), []
        )
        self.assertEqual(
            found('fn t() { Command::new(sibling_binary("osk", "osk")).spawn().unwrap(); }'), []
        )

    def test_production_code_is_not_a_test(self):
        source = (
            'fn build() { Command::new("cargo").arg("build").status(); }\n'
            '#[cfg(test)]\nmod tests { fn t() { Command::new("cargo").spawn(); } }\n'
        )
        self.assertEqual(found(source, "crates/x/src/lib.rs"), [3])

    def test_the_shared_test_support_crate_is_test_code(self):
        self.assertEqual(
            found("pub fn bin(p: &str) { Command::new(cargo()).arg(p).output(); }",
                  "crates/dev/test-env/src/binary.rs"),
            [1],
        )

    def test_a_tool_with_its_own_workspace_is_skipped(self):
        self.assertEqual(
            test_cargo.audit(
                test_cargo.ROOT,
                [(Path("tools/web/wasm-split/tests/it.rs"), 'fn t() { Command::new("cargo").spawn(); }')],
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
