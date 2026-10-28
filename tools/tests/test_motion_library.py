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
            "spring timed by a literal duration": (
                "fn f() { let s = SpringSpec::settling_within(self.duration); }\n"
            ),
            "literal motion duration": "const SNAP_ANIM_MS: u32 = 200;\n",
            "literal duration given to a motion": (
                "fn f() { let d = m.context(source, Duration::from_millis(200)); }\n"
            ),
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

    def test_literal_motion_durations_in_every_spelling_are_found(self):
        widget = "crates/session/shell/src/shell/thing.rs"
        for source in (
            "pub const OSD_FADE: Duration = Duration::from_millis(210);\n",
            "pub(crate) const BAR_FADE_MS: u64 = 320;\n",
            "const SLIDE_IN: crate::time::Duration = crate::time::Duration::from_millis(90);\n",
            "fn f() { IdleFade::new(hold, std::time::Duration::from_millis(320)); }\n",
        ):
            with self.subTest(source=source):
                root = self.fixture({widget: source})
                self.assertEqual(len(motion.scan(root)), 1, source)

    def test_timings_that_are_not_motions_pass(self):
        root = self.fixture(
            {
                "crates/session/shell/src/shell/thing.rs": (
                    "const ANIMATION_LABEL_REFRESH: Duration = Duration::from_secs(1);\n"
                    "const DRAG_SPRING_LOAD: Duration = Duration::from_millis(500);\n"
                    "const FADE_STOPS: usize = 9;\n"
                    "const INSERT_FADE_Y_OFFSET: f64 = 10.0;\n"
                    "pub const POINTER_FADE: Duration = crate::motion::Follow::FADE;\n"
                    "fn f() { IdleFade::new(Duration::from_millis(BAR_HOLD_MS), CHROME_FADE); }\n"
                    "fn g() { let d = m.context(source, m.durations.fast()); }\n"
                ),
                "crates/sdk/ui-core/src/motion/named.rs": (
                    "pub const LIFE: Duration = Duration::from_millis(1200);\n"
                    "fn f() { SpringSpec::settling_within(d); }\n"
                ),
            }
        )
        self.assertEqual(self.whats(root), [])

    def test_a_spring_built_by_hand_is_found(self):
        widget = "crates/sdk/ui-core/src/widget/group_row.rs"
        for source in (
            "fn f() -> (f64, f64) { crate::motion::SpringSpec::critical(omega).state(x, v, t) }\n",
            "fn f() { let s = SpringSpec::from_params(theme.tokens.motion.springs.heavy); }\n",
            "fn f() { let s = SpringSpec { response: 0.3, damping: 1.0 }; }\n",
        ):
            with self.subTest(source=source):
                root = self.fixture({widget: source})
                self.assertEqual(self.whats(root), ["spring built outside the library"])

    def test_time_bounded_interpolation_is_found(self):
        widget = "crates/session/shell/src/shell/thing.rs"
        for source in (
            "fn f(now: Instant) -> f32 { now.duration_since(t).as_millis() as f32 / FADE_MS as f32 }\n",
            "fn f(now: Instant) -> f64 { (now - start).as_secs_f64() / SLIDE_SECS }\n",
            "fn f() -> f32 { (started.elapsed().as_micros() as f32) / f32::from(GLOW_US) }\n",
            "fn f() -> f64 { elapsed.as_millis() as f64 / REVEAL_MS }\n",
        ):
            with self.subTest(source=source):
                root = self.fixture({widget: source})
                self.assertEqual(self.whats(root), ["time-bounded interpolation"])

    def test_sample_interpolation_rates_and_buckets_are_not_motion(self):
        root = self.fixture(
            {
                # A finger path interpolates between its reports.
                "crates/sdk/ui-core/src/widget/finger_path.rs": (
                    "fn f() -> f64 {\n"
                    "    let span = later.0.saturating_duration_since(sample_at).as_secs_f64();\n"
                    "    let into = at.saturating_duration_since(sample_at).as_secs_f64();\n"
                    "    let r = last + (last - prev) * ahead.as_secs_f64() / interval.as_secs_f64();\n"
                    "    travel + (later.1 - travel) * into / span\n"
                    "}\n"
                ),
                # A transfer rate over the time it took.
                "apps/speed-test/src/measure.rs": (
                    "fn mbps(bytes: u64, elapsed: Duration) -> f64 {\n"
                    "    let seconds = elapsed.as_secs_f64();\n"
                    "    bytes as f64 * 8.0 / seconds / 1_000_000.0\n"
                    "}\n"
                ),
                # How many days old a file is.
                "crates/session/shell/src/shell/search/files.rs": (
                    "fn f() -> u64 { age.as_secs() / DAY }\n"
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
