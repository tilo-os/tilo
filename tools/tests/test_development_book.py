import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("development_book", ROOT / "tools/docs/check_development_book.py")
book = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(book)


class CargoRecipes(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.path = Path(self.temporary.name)
        (self.path / "tests").mkdir()
        (self.path / "tests/interaction_test.rs").write_text("// module of it, not a target\n")
        self.manifest = {"package": {"name": "shell", "autotests": False},
                         "test": [{"name": "it", "path": "tests/it.rs"}]}
        self.inventory = {"shell": (self.path, self.manifest)}

    def tearDown(self):
        self.temporary.cleanup()

    def test_registered_integration_target_passes(self):
        self.assertEqual(book.check_command(
            ["cargo", "test", "--locked", "-p", "shell", "--test", "it", "interaction_test::"],
            self.inventory), ([], True))

    def test_module_is_not_an_automatic_test_target(self):
        errors, observed = book.check_command(
            ["cargo", "test", "-p", "shell", "--test", "interaction_test"], self.inventory)
        self.assertTrue(observed)
        self.assertIn("no test target", errors[0])

    def test_stale_package_name_fails(self):
        errors, _ = book.check_command(["cargo", "check", "-p", "ui"], self.inventory)
        self.assertEqual(errors, ["no Cargo package named 'ui'"])

    def test_explicit_substitutions_are_not_claimed_as_checked_examples(self):
        self.assertEqual(book.check_command(["cargo", "test", "-p", "PACKAGE"], self.inventory), ([], False))

    def test_application_arguments_do_not_become_cargo_selectors(self):
        self.assertEqual(book.check_command(
            ["cargo", "run", "-p", "shell", "--", "--test", "not-a-cargo-target"],
            self.inventory), ([], True))

    def test_only_shell_code_is_audited_and_continuations_are_joined(self):
        text = "# Guide\n\n```sh\ncargo test --locked \\\n  -p shell --test it # comment\n```\n\n```rust\ncargo is not a command here\n```\n"
        examples = list(book.cargo_examples(text))
        self.assertEqual(len(examples), 1)
        self.assertEqual(examples[0][1], ["cargo", "test", "--locked", "-p", "shell", "--test", "it"])

    def test_entry_pages_stay_short_and_sources_exist(self):
        import tomllib
        manifest = tomllib.loads((ROOT / "docs/development/pages.toml").read_text())
        for page in manifest["page"]:
            self.assertTrue((ROOT / page["source"]).is_file(), page["source"])
        for source in ("docs/development/README.md", "docs/development/setup.md"):
            self.assertLessEqual(len((ROOT / source).read_text().splitlines()), 100, source)


if __name__ == "__main__":
    unittest.main()
