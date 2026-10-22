import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_search_fields.py"
SPEC = importlib.util.spec_from_file_location("check_app_search_fields", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = check
SPEC.loader.exec_module(check)


class AppSearchFieldsTests(unittest.TestCase):
    def root(self, files: dict[str, str], baseline: str | None = None) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        if baseline is not None:
            (root / check.BASELINE).parent.mkdir(parents=True, exist_ok=True)
            (root / check.BASELINE).write_text(baseline, encoding="utf-8")
        return root

    # USER REQUIREMENT (2026-10-21): apps draw no search field of their own;
    # the window bar's query field is the only search entry.
    def test_an_app_drawing_its_own_search_field_is_refused(self):
        for line in [
            "let f = SearchField::filter();",
            "let f = ui::widget::SearchField::new();",
            "let f = Filtered::new(list);",
            "let i = TextInput::new().with_kind(TextInputKind::Search);",
            "let s = NavSection::new(a, b, c).filterable();",
            'let i = TextInput::new().with_placeholder("Search cities");',
            'let i = TextInput::new().with_placeholder(&t("store-search-placeholder"));',
        ]:
            with self.subTest(line=line):
                root = self.root({"apps/demo/src/app.rs": f"fn view() {{ {line} }}\n"})
                problems = check.audit(root)
                self.assertTrue(problems, line)
                self.assertIn("apps/demo/src/app.rs:1", problems[0])

    def test_tests_comments_and_other_fields_pass(self):
        root = self.root(
            {
                "apps/demo/src/app.rs": (
                    "// SearchField::new() was here\n"
                    'fn view() { TextInput::new().with_placeholder("Name"); }\n'
                    "#[cfg(test)]\nmod tests { fn t() { SearchField::new(); } }\n"
                ),
                "apps/demo/src/app_tests.rs": "fn t() { SearchField::new(); }\n",
            }
        )
        self.assertEqual(check.audit(root), [])

    def test_the_baseline_only_shrinks(self):
        files = {"apps/demo/src/app.rs": "fn v() { SearchField::filter(); }\n"}
        allowed = '[files]\n"apps/demo/src/app.rs" = 1\n'
        self.assertEqual(check.audit(self.root(files, allowed)), [])
        stale = '[files]\n"apps/demo/src/app.rs" = 2\n'
        self.assertTrue(check.audit(self.root(files, stale)), "a lower count must be written")
        more = {"apps/demo/src/app.rs": "fn v() { SearchField::filter(); SearchField::new(); }\n"}
        self.assertTrue(check.audit(self.root(more, allowed)))


if __name__ == "__main__":
    unittest.main()
