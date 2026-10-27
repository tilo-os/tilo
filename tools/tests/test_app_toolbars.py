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

    def test_an_app_drawing_its_own_search_or_find_is_refused(self):
        for line in [
            "let f = SearchField::filter();",
            "let f = ui::widget::SearchField::new();",
            "let f = Filtered::new(list);",
            "let i = TextInput::new().with_kind(TextInputKind::Search);",
            'let i = TextInput::new().with_placeholder("Search cities");',
            'let i = TextInput::new().with_placeholder("Filter processes");',
            'field.set_placeholder("Find in page");',
            'let i = TextInput::new().with_placeholder(&t("store-search-placeholder"));',
            'let i = TextInput::new().with_placeholder(&t("input-source-add-placeholder"));',
            "let b = ui::search::search_button(SearchRequest::this_app());",
            "let bar = FindBar::new().with_replace();",
            "let scope = FindScope::new(Box::new(terminal));",
            "let palette = CommandPalette::commands();",
            'palette.set_query_placeholder("Commands");',
        ]:
            with self.subTest(line=line):
                counts = app_toolbars.count_file(
                    f"fn view() {{ {line} }}\n",
                    {"input-source-add-placeholder": "Search layouts"},
                )
                self.assertEqual(counts["own-search"], 1, line)

    def test_a_field_and_its_placeholder_are_one_search(self):
        code = (
            "fn view() {\n"
            '    let f = SearchField::new()\n        .with_placeholder("Search cities");\n'
            "}\n"
        )
        self.assertEqual(app_toolbars.count_file(code)["own-search"], 1)

    def test_a_placeholder_is_read_within_its_own_call(self):
        # The old pattern ran on to the next `;`, so a struct literal's next
        # field named `search…` made an event title a search field.
        code = (
            "fn view() { Editor {\n"
            "    title: TextInput::new().with_placeholder(TITLE_PLACEHOLDER),\n"
            "    search_index: Vec::new(),\n"
            '    name: TextInput::new().with_placeholder("Name"),\n'
            "} }\n"
            "// SearchField::new() was here\n"
            "fn nav() { NavSection::new(a, b, c).filterable(); }\n"
        )
        self.assertEqual(app_toolbars.count_file(code)["own-search"], 0)

    def test_a_header_leading_widget_and_a_subpage_back_are_counted(self):
        code = (
            "fn f() {\n"
            "    shell.set_content_header_leading(Some(Box::new(tabs)));\n"
            "    shell.set_content_header_leading(None);\n"
            "    let nav = NavShell::new().content_header_leading(Box::new(path));\n"
            "    column().push(subpage_header(title, tx));\n"
            "}\n"
            "fn subpage_header(title: &str, tx: &Door) -> El { todo!() }\n"
        )
        counts = app_toolbars.count_file(code)
        self.assertEqual(counts["nav-header-leading"], 2, "clearing the slot draws nothing")
        self.assertEqual(counts["own-back"], 1, "the definition is not a header")

    def test_a_reasoned_exemption_covers_its_line_or_the_statement_after_it(self):
        code = (
            "fn f() {\n"
            "    // window-bar: picker, the Open with chooser\n"
            "    self.chooser\n"
            '        .set_query_placeholder("Search apps");\n'
            "    let f = SearchField::new(); // window-bar: service, no window bar\n"
            "    let g = SearchField::new();\n"
            "}\n"
        )
        self.assertEqual(app_toolbars.count_file(code)["own-search"], 1)

    def test_an_exemption_with_an_unknown_reason_fails(self):
        root = self.fixture({
            "apps/demo/src/app.rs": "fn f() {\n    // window-bar: legacy\n    SearchField::new();\n}\n",
        })
        errors = app_toolbars.check(root, root / "none.txt")
        self.assertTrue(any("apps/demo/src/app.rs:2: window-bar exemption 'legacy'" in e
                            for e in errors), errors)
        self.assertTrue(any("1 own-search (allowed 0)" in e for e in errors), errors)

    def test_messages_are_read_from_each_apps_english_catalogue(self):
        root = self.fixture({
            "apps/demo/locales/en-US.ftl": "demo-pick = Search layouts\ndemo-name = Name\n",
            "apps/demo/src/app.rs": (
                'fn f() { a.with_placeholder(&t("demo-pick")); b.with_placeholder(&t("demo-name")); }\n'
            ),
        })
        self.assertEqual(app_toolbars.scan(root), {("apps/demo/src/app.rs", "own-search"): 1})


if __name__ == "__main__":
    unittest.main()
