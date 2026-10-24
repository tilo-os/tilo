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
            "crates/session/shell/src/panel.rs": HAND_ROLLED,
        })
        found = primitives.scan(root)
        self.assertIn(("crates/session/shell/src/panel.rs", "glyph-join"), found)
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


if __name__ == "__main__":
    unittest.main()
