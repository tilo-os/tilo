import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_swallowed_decodes.py"
SPEC = importlib.util.spec_from_file_location("check_swallowed_decodes", SCRIPT)
decodes = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = decodes
SPEC.loader.exec_module(decodes)


def count(source: str) -> int:
    return len(decodes.findings_in(source))


class SwallowedDecodes(unittest.TestCase):
    def test_an_ok_on_a_decode_discards_its_error(self):
        self.assertEqual(count("fn f(b: &[u8]) -> Option<M> { serde_json::from_slice(b).ok() }"), 1)
        self.assertEqual(count("fn f(s: &str) -> Option<u32> { Some(s.parse::<u32>().ok()?) }"), 1)
        self.assertEqual(count("fn f(v: Vec<B>) { v.iter().filter_map(|b| Frame::decode(b).ok()); }"), 1)
        self.assertEqual(count("fn f(b: &[u8]) -> M { postcard::from_bytes(b)\n    .unwrap_or_default() }"), 1)

    def test_a_decode_bound_to_underscore_is_swallowed(self):
        self.assertEqual(count("fn f(b: &[u8]) { let _ = Frame::decode(b); }"), 1)

    def test_an_if_let_ok_without_else_is_swallowed(self):
        self.assertEqual(count("fn f(s: &str) { if let Ok(m) = serde_json::from_str::<M>(s) { use_it(m); } }"), 1)
        self.assertEqual(
            count("fn f(s: &str) { if let Ok(m) = serde_json::from_str::<M>(s) { a(m); } else { warn(); } }"),
            0,
        )

    def test_a_let_else_hears_or_not(self):
        self.assertEqual(count("fn f(s: &str) { let Ok(m) = decode(s) else { return; }; }"), 1)
        self.assertEqual(
            count("fn f(s: &str) -> R { let Ok(m) = decode(s) else { return Err(E::Bad); }; }"), 0
        )

    def test_a_match_with_a_silent_error_arm_is_swallowed(self):
        self.assertEqual(count("fn f(s: &str) { match decode(s) { Ok(m) => a(m), Err(_) => {} } }"), 1)
        self.assertEqual(
            count("fn f(s: &str) { match decode(s) { Ok(m) => a(m), Err(e) => tracing::warn!(%e) } }"), 0
        )

    def test_propagated_errors_are_heard(self):
        self.assertEqual(count("fn f(b: &[u8]) -> Result<M> { Ok(Frame::decode(b)?) }"), 0)
        self.assertEqual(count("fn f(b: &[u8]) -> Result<M> { Frame::decode(b).map_err(E::from) }"), 0)

    def test_an_absence_turned_back_into_an_error_is_heard(self):
        self.assertEqual(
            count("fn f(r: &str) -> io::Result<u32> {\n    std::str::from_utf8(r)\n        .ok()\n"
                  "        .and_then(|s| s.parse().ok())\n        .ok_or_else(|| invalid(\"bad\"))\n}"),
            0,
        )
        self.assertEqual(count("fn f(r: &str) -> Option<u32> { r.parse().ok().map(|n| n + 1) }"), 1)

    def test_an_allow_comment_marks_an_optional_read(self):
        self.assertEqual(
            count("fn f() -> Option<u32> {\n    // optional-decode: an unset override keeps the default\n"
                  "    std::env::var(\"N\").ok()?.parse().ok()\n}"),
            0,
        )

    def test_tests_and_comments_are_not_counted(self):
        self.assertEqual(count("// decode(b).ok()\nfn f() {}"), 0)
        self.assertEqual(count("#[cfg(test)]\nmod tests { fn f(b: &[u8]) { decode(b).ok(); } }"), 0)


if __name__ == "__main__":
    unittest.main()
