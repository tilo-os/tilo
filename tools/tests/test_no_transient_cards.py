import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_no_transient_cards.py"
SPEC = importlib.util.spec_from_file_location("check_no_transient_cards", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = check
SPEC.loader.exec_module(check)


class NoTransientCardsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_a_window_that_paints_the_card_is_named(self):
        root = self.fixture({
            "apps/notes/src/app.rs": (
                "fn draw() {\n"
                "    let card = ui::widget::toast::ToastCard::new(\"Saved\", kind);\n"
                "    ui::widget::toast::paint_card(scene, &card);\n"
                "}\n"
            ),
            "examples/apps/demo/src/main.rs": "fn h() -> f64 { toast::card_height(t) }\n",
        })
        self.assertEqual(
            [(v.path, v.line) for v in check.audit(root)],
            [
                ("apps/notes/src/app.rs", 2),
                ("apps/notes/src/app.rs", 3),
                ("examples/apps/demo/src/main.rs", 1),
            ],
        )

    def test_the_bar_notice_comments_and_the_shell_pass(self):
        root = self.fixture({
            "apps/notes/src/app.rs": (
                "/// Not a ToastCard: the bar says it.\n"
                "fn sync() { ui::window_toolbar::notify(Some(notice)); }\n"
                "const NAME: &str = \"ToastCard\";\n"
            ),
            "crates/sdk/ui-core/src/widget/toast.rs": "pub struct ToastCard;\n",
            "crates/session/shell/src/notifications.rs": "let c = ToastCard::new(t, k);\n",
            "crates/session/desktop-surface/src/lock.rs": "toast::paint_card(s, &c);\n",
            "apps/notes/target/debug/build/out.rs": "ToastCard::new(t, k);\n",
        })
        self.assertEqual(check.audit(root), [])


if __name__ == "__main__":
    unittest.main()
