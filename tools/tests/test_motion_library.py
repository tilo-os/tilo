import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_motion_library.py"
SPEC = importlib.util.spec_from_file_location("check_motion_library", SCRIPT)
motion = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = motion
SPEC.loader.exec_module(motion)


class MotionLibraryTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def whats(self, root: Path) -> list[str]:
        return [finding.what for finding in motion.scan(root)]

    def test_hand_rolled_motion_is_found_outside_the_library(self):
        widget = "crates/sdk/ui-core/src/widget/thing.rs"
        cases = {
            "easing curve": "fn f() { t.set_open(true, d, Curve::EaseOut); }\n",
            "easing helper": "fn f() -> f64 { crate::ease_out(x) }\n",
            "hand-rolled easing helper": "fn ease_in_quad(t: f64) -> f64 { t }\n",
            "cubic ease written out": "fn f(t: f64) -> f64 { 1.0 - (1.0 - t).powi(3) }\n",
            "ad-hoc spring": "fn f() { let s = Spring::new(0.0, p); }\n",
            "lerp over time": "fn f() -> f32 { started.elapsed().as_secs_f32() / d.as_secs_f32() }\n",
            "hand-built tween": "fn f() { let a = Animated::new(0.0, 1.0, d, c); }\n",
            "hand-rolled time ratio": "fn f() { let t = duration_fraction(e, d); }\n",
        }
        for what, source in cases.items():
            with self.subTest(what=what):
                root = self.fixture({widget: source})
                self.assertIn(what, self.whats(root))

    def test_the_library_tests_comments_and_content_curves_are_exempt(self):
        root = self.fixture(
            {
                "crates/sdk/ui-core/src/motion/named.rs": "fn f() { Curve::EaseOut; }\n",
                "crates/sdk/ui-core/src/animation/animated.rs": "fn f() { Curve::EaseOut; }\n",
                "crates/sdk/ui-core/src/widget/a_tests.rs": "fn f() { Curve::EaseOut; }\n",
                "crates/sdk/ui-core/src/widget/b.rs": (
                    "// Curve::EaseOut in a comment\n"
                    "#[cfg(test)]\nmod tests { fn f() { Curve::EaseOut; } }\n"
                ),
                "apps/video-editor/src/curve.rs": "fn ease_in_out(t: f64) -> f64 { t }\n",
                "crates/sdk/ui-core/src/widget/sankey.rs": (
                    "fn f(t: f64) -> f64 { t * t * (3.0 - 2.0 * t) }\n"
                ),
            }
        )
        self.assertEqual(self.whats(root), [])

    def test_any_site_fails_the_check(self):
        root = self.fixture(
            {"crates/sdk/ui-core/src/widget/thing.rs": "fn f() { Curve::Linear; }\n"}
        )
        findings = motion.scan(root)
        self.assertEqual(len(findings), 1)
        self.assertIn("ui::motion", str(findings[0]))

if __name__ == "__main__":
    unittest.main()
