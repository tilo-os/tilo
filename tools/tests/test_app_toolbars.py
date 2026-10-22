import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_toolbars.py"
SPEC = importlib.util.spec_from_file_location("check_app_toolbars", SCRIPT)
app_toolbars = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = app_toolbars
SPEC.loader.exec_module(app_toolbars)


ROW = '''
impl ui::widget::WindowHeader for App {
    fn set_history_control(&mut self, control: Option<HistoryControl>) {}
}
struct App { history: ui::widget::HistoryOpener }
fn paint(&mut self, row: Rect, theme: &Theme) {
    let face = ui::chrome::HeaderRowFace::new(row, theme).with_history_if(true);
    let bar = Toolbar::new(ui::widget::toolbar::Orientation::Horizontal);
    let rail = Toolbar::new(Orientation::Vertical);
    ui::widget::toolbar::paint_band(scene, transform, theme, row);
    self.shell.set_content_header("Inbox", vec![Box::new(compose)]);
    self.shell.set_overview_header("", Vec::new());
    self.shell.set_sidebar_header("", vec![]);
    let height = ui::chrome::HeaderRowFace::height(theme);
}

#[cfg(test)]
mod tests {
    fn t() { let bar = Toolbar::new(Orientation::Horizontal); }
}
'''


class AppToolbarsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_each_rule_counts_its_row_and_skips_empty_headers_and_tests(self):
        counts = app_toolbars.count_file(ROW)
        self.assertEqual(counts["window-header"], 1)
        self.assertEqual(counts["history-in-row"], 2, "the opener and the row's History")
        self.assertEqual(counts["header-row"], 1, "a row's height alone draws no row")
        self.assertEqual(counts["toolbar-row"], 1, "a vertical rail and the test are not rows")
        self.assertEqual(counts["toolbar-band"], 1, "the band behind a row the app lays out")
        self.assertEqual(counts["nav-header-actions"], 1, "an empty list puts nothing in a row")

    def test_a_new_row_fails_and_a_removed_one_lowers_the_baseline(self):
        root = self.fixture({
            "apps/mail/src/app.rs": "fn f() { let b = Toolbar::new(Orientation::Horizontal); }\n",
            "apps/notes/src/app.rs": "fn f() {}\n",
        })
        baseline = root / "baseline.txt"
        baseline.write_text("toolbar-row 1 apps/notes/src/app.rs\n", encoding="utf-8")
        errors = app_toolbars.check(root, baseline)
        self.assertTrue(any("apps/mail/src/app.rs: 1 toolbar-row (allowed 0)" in e for e in errors))
        self.assertTrue(any("toolbar-row 1 apps/notes/src/app.rs is now 0" in e for e in errors))

    def test_the_window_bar_declaration_is_clean(self):
        root = self.fixture({
            "apps/writer/src/window_bar.rs": (
                "fn declare() { ui::window_toolbar::declare(Some(WindowToolbar::default())); }\n"
            ),
        })
        self.assertEqual(app_toolbars.check(root, root / "none.txt"), [])


if __name__ == "__main__":
    unittest.main()
