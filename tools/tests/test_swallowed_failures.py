import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_swallowed_failures.py"
SPEC = importlib.util.spec_from_file_location("check_swallowed_failures", SCRIPT)
failures = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = failures
SPEC.loader.exec_module(failures)

FALLIBLE = {"decode", "read_row", "publish", "write_frame", "sync"}


def count(source: str, fallible: set[str] = FALLIBLE) -> int:
    return len(failures.findings_in(source, fallible))


class SwallowedFailures(unittest.TestCase):
    def test_an_ok_on_a_decode_discards_its_error(self):
        self.assertEqual(count("fn f(b: &[u8]) -> Option<M> { serde_json::from_slice(b).ok() }"), 1)
        self.assertEqual(count("fn f(v: Vec<B>) { v.iter().filter_map(|b| Frame::decode(b).ok()); }"), 1)
        self.assertEqual(count("fn f(b: &[u8]) -> M { postcard::from_bytes(b)\n    .unwrap_or_default() }"), 1)

    def test_an_absence_handed_to_the_caller_is_the_callers_contract(self):
        self.assertEqual(count("fn f(s: &str) -> Option<u32> { Some(s.parse::<u32>().ok()?) }"), 0)
        self.assertEqual(count("fn f(r: &str) -> Option<u32> { r.parse().ok().map(|n| n + 1) }"), 1)

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

    def test_a_failure_logged_only_at_debug_is_not_heard(self):
        self.assertEqual(
            count('fn f(r: R) { match r { Ok(m) => a(m), Err(error) => { tracing::debug!(%error, "no"); } } }'),
            1,
        )
        self.assertEqual(
            count('fn f(r: R) { match r { Ok(m) => a(m), Err(error) => { tracing::warn!(%error, "no"); } } }'),
            0,
        )
        self.assertEqual(
            count('fn f() { let m = read_row().map_err(|error| tracing::debug!(%error, "no")); }'),
            1,
        )

    def test_a_routine_failure_names_its_reason_in_code(self):
        self.assertEqual(
            count(
                "fn f(r: R) {\n    if let Err(error) = r {\n"
                '        ignored!("the peer left, and the next tick asks again", '
                'tracing::debug!(%error, "peer gone"));\n    }\n}'
            ),
            0,
        )
        # A call in the branch body, before what it excuses, says the same.
        self.assertEqual(
            count(
                "fn f(r: R) {\n    if let Err(error) = r {\n"
                '        ignored!("the peer left, and the next tick asks again");\n'
                '        tracing::debug!(%error, "peer gone");\n    }\n}'
            ),
            0,
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

    def test_an_error_branch_that_hears_nothing_is_swallowed(self):
        self.assertEqual(count("fn f(r: R) { if let Err(e) = publish(r) { } }"), 1)
        self.assertEqual(count("fn f(r: R) { if let Err(e) = publish(r) { tracing::warn!(%e); } }"), 0)
        self.assertEqual(count("fn f(r: R) -> Res { if let Err(e) = publish(r) { return Err(e); } }"), 0)
        # A struct literal in the pattern has braces of its own; the body after
        # it is what counts.
        self.assertEqual(
            count("fn f(s: S) { if let Err(e) = publish(Ask { s }) { tracing::warn!(%e); } }"), 0
        )

    def test_a_discarded_result_of_a_fallible_function_is_swallowed(self):
        self.assertEqual(count("fn f(p: &P) { let _ = write_frame(p); }"), 1)
        self.assertEqual(count("fn f(p: &P) { write_frame(p).ok(); }"), 1)
        self.assertEqual(count("fn f(p: &P) { let _ = p.write_all(b\"x\"); }"), 1)
        # A channel send is the receiver's absence, not a dropped failure.
        self.assertEqual(count("fn f(t: &T) { let _ = t.send(x); }"), 0)

    def test_an_ignored_call_keeps_the_reason_with_the_failure(self):
        self.assertEqual(
            count("fn f() -> Option<u32> {\n"
                  '    Some(ignored!("an unset override keeps the default", std::env::var("N").ok()?)'
                  ".parse().ok()?)\n}"),
            0,
        )
        self.assertEqual(
            count("fn f(p: &P) {\n"
                  '    let _ = ignored!("the socket is already gone, and the frame was for nobody", '
                  "write_frame(p));\n}"),
            0,
        )

    def test_the_reason_lives_in_code_not_in_the_comments_around_it(self):
        exempt = (
            "fn f(p: &P) {\n"
            '    let _ = ignored!("the socket is already gone, and the frame was for nobody", '
            "write_frame(p));\n}"
        )
        with_comment = exempt.replace("    let _ =", "    // the frame was for nobody\n    let _ =")
        self.assertEqual(count(exempt), 0)
        self.assertEqual(count(with_comment), 0, "a comment neither grants nor takes an exemption")

    def test_a_comment_is_not_a_way_out(self):
        self.assertEqual(
            count("fn f(p: &P) {\n"
                  "    // ignored-failure: the socket is already gone, and the frame was for nobody\n"
                  "    let _ = write_frame(p);\n}"),
            1,
        )

    def test_a_reason_of_a_word_or_two_is_not_a_way_out(self):
        for reason in ('""', '"fine"', '"n/a here"'):
            with self.subTest(reason=reason):
                self.assertEqual(
                    count(f'fn f(p: &P) {{\n    let _ = ignored!({reason}, write_frame(p));\n}}'),
                    1,
                )

    def test_tests_and_comments_are_not_counted(self):
        self.assertEqual(count("// decode(b).ok()\nfn f() {}"), 0)
        self.assertEqual(count("#[cfg(test)]\nmod tests { fn f(b: &[u8]) { decode(b).ok(); } }"), 0)


class DefinitionsOfALargeCommit(unittest.TestCase):
    """A commit of hundreds of files calls thousands of names: more than one
    expression of a `git grep` holds."""

    NAMES = [f"reads_row_{n:05}_of_a_commit_that_calls_a_great_many_names" for n in range(4000)]

    def test_the_search_does_not_grow_with_the_names(self):
        """One constant expression asks for definitions, however many names a commit calls."""
        scanned = failures.SCANNED[0]
        with tempfile.TemporaryDirectory() as held:
            root = Path(held)

            def git(*arguments):
                subprocess.run(["git", "-C", held, *arguments], check=True, capture_output=True)

            git("init", "--quiet")
            source = root / scanned / "src"
            source.mkdir(parents=True)
            (source / "calls.rs").write_text(
                "fn all() {\n" + "".join(f"    {name}();\n" for name in self.NAMES) + "}\n")
            (source / "first.rs").write_text(f"fn {self.NAMES[0]}() {{}}\n")
            git("add", "--all")
            files = failures.judged(root, True)
            judged_module = sys.modules["judged"]
            calls = []
            original = judged_module.git

            def recording(where, *arguments, **keywords):
                calls.append(arguments)
                return original(where, *arguments, **keywords)

            judged_module.git = recording
            try:
                failures.definitions_of(files)
            finally:
                judged_module.git = original
        greps = [arguments for arguments in calls if arguments[0] == "grep"]
        self.assertEqual(len(greps), 1, calls)
        expressions = [arguments[index + 1] for arguments in greps
                       for index, argument in enumerate(arguments) if argument == "-e"]
        self.assertEqual(expressions, [r"fn[[:space:]]+[A-Za-z_]"])

    def test_a_commit_calling_a_great_many_names_finds_their_definitions(self):
        scanned = failures.SCANNED[0]
        with tempfile.TemporaryDirectory() as held:
            root = Path(held)

            def git(*arguments):
                subprocess.run(["git", "-C", held, *arguments], check=True, capture_output=True)

            git("init", "--quiet")
            source = root / scanned / "src"
            source.mkdir(parents=True)
            calls = "".join(f"    {name}();\n" for name in self.NAMES)
            (source / "calls.rs").write_text("fn all() {\n" + calls + "}\n")
            (source / "first.rs").write_text(f"fn {self.NAMES[0]}() {{}}\n")
            (source / "last.rs").write_text(f"fn {self.NAMES[-1]}() {{}}\n")
            # A definition reads as a call of its own name, so the file left out is one
            # that names nothing a commit could call.
            (source / "other.rs").write_text("pub const NOTHING_IS_CALLED_HERE: u8 = 0;\n")
            git("add", "--all")
            files = failures.judged(root, True)
            called = failures.judged_names(files)
            self.assertGreater(sum(len(name) + 1 for name in called), 64 * 1024)
            defining = failures.definitions_of(files).files(scanned)
        self.assertIn(scanned / "src/first.rs", defining)
        self.assertIn(scanned / "src/last.rs", defining)
        self.assertNotIn(scanned / "src/other.rs", defining)


if __name__ == "__main__":
    unittest.main()
