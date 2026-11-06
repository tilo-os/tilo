import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_live_rows.py"
SPEC = importlib.util.spec_from_file_location("check_live_rows", SCRIPT)
live_rows = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = live_rows
SPEC.loader.exec_module(live_rows)


TYPED = '''
fn creator_row(model: &TubeModel, id: String) -> Option<LiveRow> {
    Some(LiveRow::typed::<Channel>(id, label).effect(&TubeEffect::OpenCreator { creator }))
}
'''


class LiveRowsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_a_typed_row_is_not_counted(self):
        self.assertEqual(live_rows.count_file(TYPED), 0)

    def test_an_untyped_row_is_counted_and_tests_are_not(self):
        code = (
            "fn row() -> LiveRow { LiveRow::new(\"id\", \"kind\", \"Label\") }\n"
            "#[cfg(test)]\n"
            "mod tests { fn t() { LiveRow::new(\"a\", \"b\", \"c\"); } }\n"
        )
        self.assertEqual(live_rows.count_file(code), 1, "a test's row is not production")

    def test_a_new_untyped_row_fails_and_a_migrated_one_lowers_the_baseline(self):
        root = self.fixture({
            "apps/mail/src/app.rs": "fn f() { LiveRow::new(\"a\", \"b\", \"c\"); }\n",
            "apps/notes/src/app.rs": TYPED,
        })
        baseline = root / "baseline.txt"
        baseline.write_text("untyped-live-row 1 apps/notes/src/app.rs\n", encoding="utf-8")
        errors = live_rows.check(root, baseline)
        self.assertTrue(
            any("apps/mail/src/app.rs: 1 untyped-live-row (allowed 0)" in e for e in errors)
        )
        self.assertTrue(
            any(
                "untyped-live-row 1 apps/notes/src/app.rs is now 0" in e
                for e in errors
            ),
            "a migrated app must lower its count, not leave a stale line",
        )

    def test_a_fully_typed_tree_passes(self):
        root = self.fixture({"apps/tube/src/rows.rs": TYPED})
        self.assertEqual(live_rows.check(root, root / "none.txt"), [])


if __name__ == "__main__":
    unittest.main()
