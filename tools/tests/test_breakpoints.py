import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_breakpoints.py"
SPEC = importlib.util.spec_from_file_location("check_breakpoints", SCRIPT)
breakpoints = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = breakpoints
SPEC.loader.exec_module(breakpoints)


class BreakpointLiterals(unittest.TestCase):
    def audit(self, source: str, tree: str = "apps", allowances: str = "") -> int:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / tree / "app" / "src" / "lib.rs"
            path.parent.mkdir(parents=True)
            path.write_text(source, encoding="utf-8")
            if allowances:
                table = root / "crates/sdk/ui-core/tests/support/allowances.rs"
                table.parent.mkdir(parents=True)
                table.write_text(allowances, encoding="utf-8")
            found, _, self.stale = breakpoints.audit(root)
            return sum(found.values())

    def test_a_dimension_compared_to_a_size_is_one(self):
        self.assertEqual(
            self.audit("fn f(w: f64) { if w < 640.0 { narrow() } }"), 1
        )
        self.assertEqual(
            self.audit("fn f(r: Rect) { if r.width >= 900.0 { wide() } }"), 1
        )
        self.assertEqual(
            self.audit("fn f(r: Rect) { if 480.0 > self.sidebar_height { x() } }"), 1
        )

    def test_small_numbers_are_arithmetic_not_layout(self):
        self.assertEqual(self.audit("fn f(w: f64) { if w < 1.0 { empty() } }"), 0)
        self.assertEqual(self.audit("fn f(w: f64) { let half = w / 2.0; }"), 0)

    def test_an_app_defining_a_breakpoint_owns_a_grammar(self):
        self.assertEqual(
            self.audit("const COMPACT_WIDTH: f64 = 720.0;"), 1
        )
        # The toolkit is where breakpoints are allowed to live.
        self.assertEqual(
            self.audit("const COMPACT_WIDTH: f64 = 720.0;", tree="session"), 0
        )

    def test_an_allowed_item_is_not_a_breakpoint(self):
        self.assertEqual(
            self.audit(
                "fn f(w: f64) { if w < 640.0 { hairline() } }",
                allowances='allowance("apps/app/src/lib.rs", "f", Rule::Breakpoint, "a drawing bound"),',
            ),
            0,
        )
        self.assertEqual(self.stale, [])

    def test_an_allowance_that_covers_nothing_is_stale(self):
        self.assertEqual(
            self.audit(
                "fn f(w: f64) { if w < 640.0 { hairline() } }",
                allowances='allowance("apps/app/src/lib.rs", "gone", Rule::Breakpoint, "a drawing bound"),',
            ),
            1,
        )
        self.assertEqual(self.stale, ["apps/app/src/lib.rs gone"])

    def test_the_baseline_only_shrinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "apps" / "app" / "src" / "lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("const NARROW_WIDTH: f64 = 640.0;", encoding="utf-8")
            found, _, _ = breakpoints.audit(root)
            self.assertEqual(sum(found.values()), 1)
            # No baseline entry: the comparison is new and must fail.
            self.assertGreater(sum(found.values()), breakpoints.baseline(root)["apps/app/src/lib.rs"])


class ItemSpans(unittest.TestCase):
    def item_at(self, source: str, needle: str) -> str:
        source = breakpoints.rust_source.code_only(source)
        line = source.splitlines().index(
            next(line for line in source.splitlines() if needle in line)
        )
        return breakpoints.rust_source.enclosing_item(
            breakpoints.rust_source.item_spans(source), line
        )

    def test_the_innermost_named_item_holds_the_hit(self):
        source = (
            "const TOP: f64 = 3.0;\n"
            "fn outer() {\n"
            "    let f = |x: f64| { x < 640.0 };\n"
            "    x < 650.0\n"
            "}\n"
            "impl Widget for Probe {\n"
            "    fn draw(&self) { x < 700.0 }\n"
            "}\n"
        )
        self.assertEqual(self.item_at(source, "TOP"), "TOP")
        self.assertEqual(self.item_at(source, "640.0"), "outer")
        self.assertEqual(self.item_at(source, "650.0"), "outer")
        self.assertEqual(self.item_at(source, "700.0"), "draw")

    def test_a_literal_or_lifetime_does_not_move_an_item_end(self):
        source = (
            "fn draw() {\n"
            "    let s = \"}\";\n"
            "    let life: &'static str = \"x\";\n"
            "    x < 640.0\n"
            "}\n"
            "const AFTER: f64 = 4.0;\n"
        )
        self.assertEqual(self.item_at(source, "640.0"), "draw")
        self.assertEqual(self.item_at(source, "AFTER"), "AFTER")


if __name__ == "__main__":
    unittest.main()
