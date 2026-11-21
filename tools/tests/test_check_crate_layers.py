import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_crate_layers.py"
SPEC = importlib.util.spec_from_file_location("check_crate_layers", SCRIPT)
layers = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = layers
SPEC.loader.exec_module(layers)


ENGINE = """\
[[rule]]
crate = "engine"
reason = "it owns the workspace"
"""
HOST = """
  [[rule.allow]]
  package = "host"
  why = "it runs the engine"
"""
VIEWER = """
  [[rule.allow]]
  package = "viewer"
  tests = true
  why = "its tests read a frame back"
"""
SESSION = """
  [[rule.allow]]
  package = "session"
  why = "the compositor in it draws"
  paths = ["src/compositor/"]
"""


def package(name: str, **dependencies: frozenset[str]) -> object:
    return layers.Package(
        name=name,
        manifest=f"crates/{name}/Cargo.toml",
        root=f"crates/{name}",
        dependencies=dict(dependencies),
    )


def rules(text: str) -> list:
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "crate_layers.toml"
        path.write_text(text)
        return layers.load_rules(path)


def tree(root: Path, files: dict[str, str]) -> None:
    for name, source in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source)


class Dependants(unittest.TestCase):
    def test_an_unallowed_dependant_is_reported_with_the_rule(self) -> None:
        found = layers.violations(
            rules(ENGINE + HOST),
            [package("engine"), package("host", engine=frozenset({"dependency"})), package("app", engine=frozenset({"dependency"}))],
        )
        self.assertEqual(1, len(found))
        self.assertIn("app takes a dependency on engine", str(found[0]))
        self.assertIn("it owns the workspace", str(found[0]))
        self.assertEqual("crates/app/Cargo.toml", found[0].file)

    def test_an_allowed_dependant_passes(self) -> None:
        found = layers.violations(
            rules(ENGINE + HOST), [package("engine"), package("host", engine=frozenset({"dependency"}))]
        )
        self.assertEqual([], found)

    def test_a_tests_allowance_rejects_a_production_edge(self) -> None:
        found = layers.violations(
            rules(ENGINE + VIEWER), [package("engine"), package("viewer", engine=frozenset({"dependency"}))]
        )
        self.assertEqual(1, len(found))
        self.assertIn("only its tests may", str(found[0]))
        self.assertEqual(("engine", "viewer", "Cargo.toml"), found[0].key)

    def test_a_tests_allowance_accepts_a_dev_edge(self) -> None:
        found = layers.violations(
            rules(ENGINE + VIEWER), [package("engine"), package("viewer", engine=frozenset({"dev-dependency"}))]
        )
        self.assertEqual([], found)

    def test_the_generated_unifying_crate_is_not_a_dependant(self) -> None:
        found = layers.violations(
            rules(ENGINE), [package("engine"), package("workspace-unify", engine=frozenset({"dependency"}))]
        )
        self.assertEqual([], found)

    def test_an_allowance_the_tree_no_longer_needs_is_reported(self) -> None:
        found = layers.violations(rules(ENGINE + HOST), [package("engine"), package("host")])
        self.assertEqual(1, len(found))
        self.assertIn("drop the allowance", str(found[0]))


class UsePaths(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.packages = [package("engine"), package("session", engine=frozenset({"dependency"}))]

    def tearDown(self) -> None:
        self.directory.cleanup()

    def test_a_name_outside_the_allowed_paths_is_reported(self) -> None:
        tree(self.root, {"crates/session/src/chrome.rs": "// the header\nuse engine::{Peer, State};\n"})
        found = layers.violations(rules(ENGINE + SESSION), self.packages, self.root)
        self.assertEqual(1, len(found))
        self.assertEqual("crates/session/src/chrome.rs", found[0].file)
        self.assertEqual(2, found[0].line)
        self.assertEqual(("engine", "session", "src/chrome.rs"), found[0].key)

    def test_a_name_inside_the_allowed_paths_passes(self) -> None:
        tree(self.root, {"crates/session/src/compositor/frame.rs": "use engine::State;\n"})
        self.assertEqual([], layers.violations(rules(ENGINE + SESSION), self.packages, self.root))

    def test_tests_may_name_the_crate(self) -> None:
        tree(
            self.root,
            {
                "crates/session/src/chrome_tests.rs": "use engine::State;\n",
                "crates/session/src/chrome.rs": "fn draw() {}\n#[cfg(test)]\nmod tests {\n    use engine::State;\n}\n",
                "crates/session/tests/drawing.rs": "use engine::State;\n",
            },
        )
        self.assertEqual([], layers.violations(rules(ENGINE + SESSION), self.packages, self.root))

    def test_a_comment_or_a_mention_does_not_count(self) -> None:
        tree(self.root, {"crates/session/src/chrome.rs": "// engine::State goes here\nlet engine_handle = 1;\n"})
        self.assertEqual([], layers.violations(rules(ENGINE + SESSION), self.packages, self.root))


class Rules(unittest.TestCase):
    def test_an_unknown_field_fails(self) -> None:
        with self.assertRaises(ValueError):
            rules((ENGINE + HOST + VIEWER + SESSION).replace("  why = \"it runs the engine\"", "  because = \"it runs the engine\""))

    def test_a_path_allowance_is_a_directory(self) -> None:
        with self.assertRaises(ValueError):
            rules((ENGINE + HOST + VIEWER + SESSION).replace('paths = ["src/compositor/"]', 'paths = ["src/compositor"]'))

    def test_an_empty_reason_fails(self) -> None:
        with self.assertRaises(ValueError):
            rules((ENGINE + HOST + VIEWER + SESSION).replace('reason = "it owns the workspace"', 'reason = " "'))

    def test_an_allowance_for_no_workspace_crate_fails(self) -> None:
        with self.assertRaises(ValueError):
            layers.violations(rules(ENGINE + HOST), [package("engine")])


if __name__ == "__main__":
    unittest.main()
