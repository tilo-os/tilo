import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_primitives.py"
SPEC = importlib.util.spec_from_file_location("check_primitives", SCRIPT)
primitives = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = primitives
SPEC.loader.exec_module(primitives)


HAND_ROLLED = '''
fn paint(scene: &mut Scene, text: &mut TextRenderer, theme: &Theme, rect: Rect) {
    scene.fill(Fill::NonZero, t, Color::from_rgb8(30, 30, 30), None, &RoundedRect::from_rect(rect, 8.0));
    text.draw_style(scene, "Save", theme.text_primary, t, None, &style);
    let facts = format!("{} · {}", who, when);
    let mark = "✓ Done";
    tracing::info!("synced {} — {}", a, b);
    crate::stderr_guard::write_line(format_args!(
        "paint-stats: wakes {} | owner {:?}",
        a, b,
    ));
}

#[cfg(test)]
mod tests {
    fn t() { let s = "a · b"; let c = Color::WHITE; }
}
'''


class PrimitivesTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_each_rule_counts_its_class_and_skips_logs_and_tests(self):
        counts = primitives.count_file(HAND_ROLLED)
        self.assertEqual(counts["hand-rolled"], 1)
        self.assertEqual(counts["glyph-join"], 1, "the log line and the test are not UI")
        self.assertEqual(counts["glyph-icon"], 1)
        self.assertEqual(counts["raw-colour"], 1)

    def test_ui_core_is_out_of_scope_and_session_is_in(self):
        root = self.fixture({
            "crates/sdk/ui-core/src/widget/button.rs": HAND_ROLLED,
            "crates/session/session/src/panel.rs": HAND_ROLLED,
        })
        found = primitives.scan(root)
        self.assertIn(("crates/session/session/src/panel.rs", "glyph-join"), found)
        self.assertFalse(any(file.startswith("crates/sdk") for file, _ in found))

    def test_the_agents_model_text_is_out_of_the_text_rules_only(self):
        root = self.fixture({
            "apps/agent/src/tools.rs": HAND_ROLLED,
            "apps/agent/src/tools/summary.rs": HAND_ROLLED,
            "apps/agent/src/surface.rs": HAND_ROLLED,
        })
        found = primitives.scan(root)
        for model_text in ("apps/agent/src/tools.rs", "apps/agent/src/tools/summary.rs"):
            self.assertNotIn((model_text, "glyph-join"), found)
            self.assertNotIn((model_text, "glyph-icon"), found)
            self.assertIn((model_text, "hand-rolled"), found)
            self.assertIn((model_text, "raw-colour"), found)
        self.assertIn(("apps/agent/src/surface.rs", "glyph-join"), found)

    def test_the_baseline_only_shrinks(self):
        root = self.fixture({"apps/demo/src/view.rs": HAND_ROLLED})
        baseline = root / "baseline.txt"
        baseline.write_text(
            "hand-rolled 1 apps/demo/src/view.rs\n"
            "glyph-join 1 apps/demo/src/view.rs\n"
            "glyph-icon 1 apps/demo/src/view.rs\n"
            "raw-colour 1 apps/demo/src/view.rs\n"
        )
        self.assertEqual(primitives.check(root, baseline), [])
        # A new glyph join fails.
        (root / "apps/demo/src/view.rs").write_text(
            HAND_ROLLED.replace("let mark", 'let more = "a • b";\n    let mark')
        )
        errors = primitives.check(root, baseline)
        self.assertTrue(any("glyph-join" in error for error in errors), errors)
        # Fixing one asks for the count to come down, so it cannot return.
        (root / "apps/demo/src/view.rs").write_text(HAND_ROLLED.replace('"✓ Done"', '"Done"'))
        errors = primitives.check(root, baseline)
        self.assertTrue(any("lower the count" in error for error in errors), errors)

    def test_glyph_patterns(self):
        joins = ["{} · {}", "Enter to save • Esc", "a | b", "Offline — retry", "3 / 10"]
        fine = ["Mon–Fri", "read/write", "Search…", "{w}×{h}", "It's done, really."]
        for text in joins:
            self.assertTrue(primitives.GLYPH_JOIN.search(primitives.literal_value(text)), text)
        for text in fine:
            value = primitives.literal_value(text)
            self.assertFalse(
                primitives.GLYPH_JOIN.search(value) or primitives.GLYPH_ICON.search(value),
                text,
            )


    def test_raw_colour_counts_what_is_written_not_what_arrives(self):
        written = [
            "Color::from_rgb8(30, 30, 30)",
            "Color::from_rgba8(0, 0, 0, 128)",
            "Color::from_rgb8(RED)",
            "Color::new([0.1, 0.2, 0.3, 1.0])",
            "Color::new(vec![0.1, 0.2, 0.3, 1.0])",
            "Color::new(picked)",
            "Color::new(picked,)",
            "Color::WHITE",
        ]
        for text in written:
            self.assertTrue(primitives.RAW_COLOUR.search(text), text)
        # A colour the theme holds or a frame was sent is read, not picked.
        arriving = [
            "Color::new(frame.base_color)",
            "Color::new(backdrop.ground)",
            "Color::new(theme.intent.accent)",
            "Color::new(self.layers[0])",
            "Color::from_rgb8(theming.tint.r)",
        ]
        for text in arriving:
            self.assertFalse(primitives.RAW_COLOUR.search(text), text)


# A session surface that lays out its own row: its ground, its words and its
# numbers written out instead of composed from the shared components.
HAND_LAID = '''
use vello::kurbo::RoundedRect;

fn paint_listing(scene: &mut Scene, text: &mut TextRenderer, theme: &Theme, row: Rect) {
    let ground = ui::chrome::shell::panel_ground(theme);
    scene.fill(Fill::NonZero, transform, ground, None, &RoundedRect::from_rect(row, 8.0));
    text.draw_text(scene, &name, caption, theme.text_primary, transform, None);
    let _ = Rect::new(0.0, 0.0, 1.0, 1.0);
}
'''

# The same surface, composed: a shared row, and a panel that owns its ground.
COMPOSED = '''
fn listing_row(listing: &Listing) -> Element {
    Box::new(
        ListRow::new(format!("listing-{}", listing.key), listing.name.clone())
            .subtitle(listing.about.clone())
            .trailing(Box::new(Button::new("join", "Join"))),
    )
}

fn sheet(body: Element) -> Element {
    Box::new(ModalDialog::new("Join a workspace", body).open(true))
}
'''

# A decorator that paints the surface instead of the widget it wraps: the
# sheet's own ground, drawn around the shared row.
OWN_DRAW = '''
struct Listing { content: Element, rect: Rect }

impl ui::widget::Decorator for Listing {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn draw(
        &mut self,
        scene: &mut ui::advanced::Scene,
        transform: ui::advanced::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        scene.fill(Fill::NonZero, transform, ground, None, &self.shape);
        self.content.draw(scene, transform, text, theme, presence);
    }
}
'''

# A decorator that only reads input (the sheet's Enter and Escape) and
# forwards the paint: not a surface of its own.
READS_INPUT = '''
struct Searching { content: Element }

impl ui::widget::Decorator for Searching {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn draw(
        &mut self,
        scene: &mut ui::advanced::Scene,
        transform: ui::advanced::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.content.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        *ctx.handled = true;
    }
}
'''


class SurfaceLayoutTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def test_a_hand_laid_session_row_is_refused(self):
        counts = primitives.count_file(HAND_LAID, "crates/session/sheets/src/directory.rs")
        self.assertEqual(counts["hand-panel"], 2)
        self.assertEqual(counts["hand-text"], 1)
        self.assertEqual(counts["hand-row"], 1)
        self.assertEqual(counts["hand-metric"], 1)

    def test_a_surface_composed_from_the_shared_components_passes(self):
        counts = primitives.count_file(COMPOSED, "crates/session/sheets/src/directory.rs")
        self.assertEqual(sum(counts.values()), 0, counts)

    def test_only_the_shells_own_crates_lay_out_surfaces(self):
        # An app composes the shared components too, but these rules are about
        # the shell's surfaces; the compositor has no toolkit to compose from.
        for relative in ("apps/mail/src/view.rs", "crates/session/compositor/src/window_frame.rs"):
            counts = primitives.count_file(HAND_LAID, relative)
            self.assertEqual(counts["hand-panel"], 0, relative)
            self.assertEqual(counts["hand-text"], 0, relative)

    def test_a_decorator_that_paints_its_own_surface_is_refused(self):
        counts = primitives.count_file(OWN_DRAW, "crates/session/sheets/src/directory.rs")
        self.assertEqual(counts["own-draw"], 1)
        # A wrapper that only reads input and forwards the paint is how a
        # sheet keeps Enter and Escape; it draws nothing of its own.
        counts = primitives.count_file(READS_INPUT, "crates/session/sheets/src/directory.rs")
        self.assertEqual(counts["own-draw"], 0)
        root = self.fixture({})
        file = root / "crates/session/sheets/src/directory.rs"
        file.parent.mkdir(parents=True)
        file.write_text(OWN_DRAW, encoding="utf-8")
        errors = primitives.check(root, root / "baseline.txt")
        drawn = next(error for error in errors if "own-draw" in error)
        self.assertIn("ListRow", drawn)
        self.assertIn("FloatingPanel", drawn)

    def test_a_new_hand_laid_surface_fails_the_policy_and_names_the_component(self):
        root = self.fixture({})
        file = root / "crates/session/sheets/src/directory.rs"
        file.parent.mkdir(parents=True)
        file.write_text(HAND_LAID, encoding="utf-8")
        baseline = root / "baseline.txt"
        baseline.write_text("", encoding="utf-8")
        errors = primitives.check(root, baseline)
        panel = next(error for error in errors if "hand-panel" in error)
        self.assertIn("paint_surface", panel)
        self.assertIn("ListRowFace", panel)
        row = next(error for error in errors if "hand-row" in error)
        self.assertIn("ListRow", row)
        # The same file composed from the shared row passes with no baseline.
        file.write_text(COMPOSED, encoding="utf-8")
        self.assertEqual(primitives.check(root, baseline), [])

    def test_the_surface_counts_only_shrink(self):
        root = self.fixture({})
        file = root / "crates/session/sheets/src/directory.rs"
        file.parent.mkdir(parents=True)
        file.write_text(HAND_LAID, encoding="utf-8")
        baseline = root / "baseline.txt"
        baseline.write_text("hand-panel 2 crates/session/sheets/src/directory.rs\n", encoding="utf-8")
        errors = primitives.check(root, baseline)
        self.assertFalse(any("hand-panel" in error for error in errors), errors)
        file.write_text(COMPOSED, encoding="utf-8")
        errors = primitives.check(root, baseline)
        self.assertTrue(any("lower the count" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()
