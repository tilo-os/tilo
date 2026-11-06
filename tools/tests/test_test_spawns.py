import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_test_spawns.py"
SPEC = importlib.util.spec_from_file_location("check_test_spawns", SCRIPT)
test_spawns = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = test_spawns
SPEC.loader.exec_module(test_spawns)


def found(source: str, relative: str = "crates/x/tests/it.rs", *others) -> list[str]:
    files = [(Path(relative), source), *((Path(p), s) for p, s in others)]
    return [method for _, _, method in test_spawns.audit(Path("/nonexistent"), files)]


class TestSpawns(unittest.TestCase):
    def test_a_command_started_in_a_test_is_found(self):
        self.assertEqual(found('fn t() { Command::new("x").arg("y").spawn().unwrap(); }'), ["spawn"])
        self.assertEqual(
            found("fn t() { let mut c = std::process::Command::new(\"x\");\n c.output().unwrap(); }"),
            ["output"],
        )
        self.assertEqual(found("fn t(c: &mut Command) { c.status().unwrap(); }"), ["status"])
        self.assertEqual(
            found("async fn t() { tokio::process::Command::new(\"x\").output().await; }"),
            ["output"],
        )

    def test_a_command_from_a_helper_or_closure_is_found(self):
        helper = ("crates/x/tests/support.rs", 'pub fn tool() -> Command { Command::new("t") }')
        self.assertEqual(found("fn t() { tool().output().unwrap(); }", "crates/x/tests/it.rs", helper), ["output"])
        self.assertEqual(
            found("fn t() { let make = || { let mut c = tool(); c }; make().output().unwrap(); }",
                  "crates/x/tests/it.rs", helper),
            ["output"],
        )
        self.assertEqual(
            found("fn t(f: &Fixture) { f.command(\"a\").status().unwrap(); }", "crates/x/tests/it.rs",
                  ("crates/x/tests/f.rs", "impl Fixture { fn command(&self, a: &str) -> Command { todo!() } }")),
            ["status"],
        )

    def test_locked_starts_and_other_types_are_not_found(self):
        self.assertEqual(found('fn t() { Command::new("x").spawn_locked().unwrap(); }'), [])
        self.assertEqual(found("fn t() { let r = client.get(u).send(); r.status(); Command::new; }"), [])
        self.assertEqual(found("fn t() { Router::builder(e).accept(A, p).spawn(); Command::new; }"), [])

    def test_production_code_is_not_a_test(self):
        source = 'fn run() { Command::new("x").spawn(); }\n#[cfg(test)]\nmod tests { fn t() { Command::new("y").status(); } }'
        self.assertEqual(found(source, "crates/x/src/lib.rs"), ["status"])

    def test_the_shared_test_support_crate_is_test_code(self):
        source = 'pub fn bus() { Command::new("dbus-daemon").arg("--nofork").spawn(); }'
        self.assertEqual(found(source, "crates/dev/test-env/src/bus.rs"), ["spawn"])

    def test_a_fixture_payload_is_not_test_code(self):
        source = 'fn main() { Command::new("tar").arg("x").output().unwrap(); }'
        self.assertEqual(
            found(source, "crates/x/tests/fixtures/payload/src/main.rs"), []
        )
        self.assertEqual(found(source, "crates/x/fixtures/payload/src/main.rs"), [])


if __name__ == "__main__":
    unittest.main()
