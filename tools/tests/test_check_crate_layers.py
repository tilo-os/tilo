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

    def test_a_recorded_dev_edge_that_becomes_production_is_reported(self) -> None:
        baseline = layers.Baseline({("engine", "viewer", ""): ""}, "")
        found = layers.violations(
            rules(ENGINE + VIEWER), [package("engine"), package("viewer", engine=frozenset({"dependency"}))]
        )
        reported, stale = layers.reconcile(found, baseline)
        self.assertEqual(1, len(reported))
        self.assertEqual([], stale)

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


class Baseline(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "baseline.toml"
        self.finding = layers.Finding("engine", "app", "crates/app/Cargo.toml", "app depends on engine")
        self.other = layers.Finding("engine", "gone", "crates/gone/Cargo.toml", "gone depends on engine")

    def tearDown(self) -> None:
        self.directory.cleanup()

    def test_a_recorded_finding_passes(self) -> None:
        baseline = layers.Baseline({self.finding.key: ""}, "")
        self.assertEqual([], layers.reconcile([self.finding], baseline)[0])
        self.assertFalse(layers.report([self.finding], baseline))

    def test_an_unrecorded_finding_fails(self) -> None:
        reported, _ = layers.reconcile([self.finding], layers.Baseline({}, ""))
        self.assertEqual([self.finding], reported)
        self.assertTrue(layers.report([self.finding], layers.Baseline({}, "")))

    def test_a_stale_entry_fails(self) -> None:
        _, stale = layers.reconcile([], layers.Baseline({self.other.key: ""}, ""))
        self.assertEqual([self.other.key], stale)
        self.assertTrue(layers.report([], layers.Baseline({self.other.key: ""}, "")))

    def test_the_first_run_records_everything(self) -> None:
        layers.write_baseline(self.path, layers.Baseline({}, "header\n"), [self.finding])
        self.assertEqual({self.finding.key}, layers.load_baseline(self.path).keys)

    def test_a_write_keeps_the_comments_and_drops_only_what_is_gone(self) -> None:
        self.path.write_text(
            "header\n\n"
            "# app draws a frame itself\n"
            "[[violation]]\n"
            'rule = "engine"\n'
            'package = "app"\n'
            "\n"
            "# gone is being rebuilt\n"
            "[[violation]]\n"
            'rule = "engine"\n'
            'package = "gone"\n'
        )
        baseline = layers.load_baseline(self.path)
        text = str(baseline.blocks[self.finding.key])
        self.assertIn("# app draws a frame itself", text)
        self.assertEqual(2, len(baseline.keys))
        layers.write_baseline(self.path, baseline, [self.finding])
        written = self.path.read_text()
        self.assertIn("# app draws a frame itself", written)
        self.assertNotIn("gone", written)
        self.assertEqual({self.finding.key}, layers.load_baseline(self.path).keys)

    def test_a_write_never_records_a_new_finding(self) -> None:
        baseline = layers.Baseline({self.other.key: ""}, "header\n")
        layers.write_baseline(self.path, baseline, [self.finding])
        self.assertEqual(frozenset(), layers.load_baseline(self.path).keys)


if __name__ == "__main__":
    unittest.main()
