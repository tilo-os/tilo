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


# A declared row offered as it is: its type's effects, nothing chained onto it.
TYPED = """
fn creator_row(model: &TubeModel, id: String) -> Option<LiveRow> {
    Some(LiveRow::typed::<Widget>(id, label))
}
"""

# The same row, with an effect chained on it: the turbofish must not hide it.
TYPED_CHAINED = """
fn creator_row(model: &TubeModel, id: String) -> Option<LiveRow> {
    Some(LiveRow::typed::<Widget>(id, label).effect(&TubeEffect::OpenCreator { creator }))
}
"""

# A control-binding row: typed, then given operations through its variable.
BOUND_BY_VARIABLE = """
fn control(effect: &MusicEffect) -> Option<ActionBinding<()>> {
    let mut row = LiveRow::typed::<Library>("music.library", "Library");
    row = row.operation(spec.clone());
    ActionBinding::unit_for(&row, effect.id()).ok()
}
"""

# A row a helper returned and a variable holds, given an operation there: the
# helper's own definition is elsewhere, so only the call site names the row.
BOUND_FROM_HELPER = """
pub(super) fn published_rows(&self) -> Vec<LiveRow> {
    self.items()
        .into_iter()
        .map(|item| {
            let row = item.kind.row(item.id, item.label).presentation(item.presentation);
            let row = match item.operation {
                Some(operation) => row.operation(operation),
                None => row,
            };
            row
        })
        .collect()
}
"""

# A value that is not a row: its operations are the plan's, not a row's.
NOT_A_ROW = """
fn publish(plan: Plan) {
    let batch = plan.batches();
    batch.operation(sweep);
}
"""


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

    def test_a_typed_row_offered_as_it_is_is_not_counted(self):
        self.assertEqual(live_rows.count_file(TYPED), {},
                         "a declared row offers its type's effects")

    def test_an_effect_chained_onto_a_turbofish_row_is_counted(self):
        self.assertEqual(live_rows.count_file(TYPED_CHAINED), {"row-operation": 1},
                         "the typed constructor's turbofish hides no chain")

    def test_an_operation_added_to_a_rows_variable_is_counted(self):
        self.assertEqual(live_rows.count_file(BOUND_BY_VARIABLE), {"row-operation": 1},
                         "a row given operations through its variable is hand-built too")

    def test_a_row_a_helper_returned_and_a_variable_holds_is_counted(self):
        self.assertEqual(live_rows.count_file(BOUND_FROM_HELPER), {"row-operation": 1},
                         "a helper-built row given an operation is hand-built too")

    def test_an_operation_on_something_that_is_not_a_row_is_not_counted(self):
        self.assertEqual(live_rows.count_file(NOT_A_ROW), {},
                         "only a binding a row builder made is a row's variable")

    def test_an_untyped_row_is_counted_and_tests_are_not(self):
        code = (
            "fn row() -> LiveRow { LiveRow::new(\"id\", \"kind\", \"Label\") }\n"
            "fn kept() -> LiveRow {\n"
            "    let mut row = LiveRow::new(\"a\", \"b\", \"c\");\n"
            "    row = row.operation(spec);\n"
            "    row\n"
            "}\n"
            "#[cfg(test)]\n"
            "mod tests { fn t() { LiveRow::new(\"a\", \"b\", \"c\"); } }\n"
        )
        self.assertEqual(
            live_rows.count_file(code),
            {"untyped-live-row": 2, "row-operation": 1},
            "a test's row is not production",
        )

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
