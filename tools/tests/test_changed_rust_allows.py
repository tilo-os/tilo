from __future__ import annotations

import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "ci/check_changed_rust_allows.py"
SPEC = importlib.util.spec_from_file_location("check_changed_rust_allows", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class ChangedRustAllowTests(unittest.TestCase):
    def test_changed_allows_need_reasons_including_cfg_attr(self):
        with self.subTest(phase='single and multiline allows need reasons'):
            source = """\
#[allow(dead_code)]
fn old() {}

#[allow(
    clippy::too_many_arguments,
)]
fn changed() {}
"""
            violations = checker.unexplained_allows(source)
            self.assertEqual(
                [(item.start_line, item.end_line) for item in violations],
                [(1, 1), (4, 6)],
            )
        with self.subTest(phase='reasoned allow passes'):
            source = """\
#[allow(dead_code, reason = "platform callback is linked dynamically")]
fn old() {}

#[allow(
    clippy::too_many_arguments,
    reason = "typed protocol callback shape",
)]
fn changed() {}
"""
            self.assertEqual(checker.unexplained_allows(source), [])
        with self.subTest(phase='cfg attr allow is also an exception'):
            source = '#[cfg_attr(not(test), allow(dead_code))]\nfn hidden() {}\n'
            violations = checker.unexplained_allows(source)
            self.assertEqual(len(violations), 1)

    def test_generated_attributes_are_lexed_once(self):
        # A saved, generated-style source reproduces the attribute density that
        # made repeated full-source lexing and prefix line counts quadratic.
        source = (Path(__file__).parent / "fixtures/generated_rust_allows.rs.txt").read_text()
        class NoPrefixCounts(str):
            def count(self, *args):
                raise AssertionError("source prefixes must not be rescanned per attribute")

        source = NoPrefixCounts(source)
        with mock.patch.object(checker, "lex", wraps=checker.lex) as lex:
            self.assertEqual(checker.unexplained_allows(source), [])
        lex.assert_called_once_with(source)
        attributes = checker.rust_attributes(source)
        self.assertEqual(len(attributes), 1024)
        self.assertEqual(attributes[-1].start_line, 4093)
        self.assertEqual(attributes[-1].end_line, 4093)

        # Neither generated status nor the surrounding valid reasons exempt
        # an unexplained exception anywhere in the file.
        broken = source.replace('allow(dead_code, reason = "capnpc emits optional accessors")',
                                'allow(dead_code)', 1)
        self.assertEqual([a.start_line for a in checker.unexplained_allows(broken)], [1])
        broken = source + '#[allow(dead_code)]\nfn handwritten() {}\n'
        self.assertEqual([a.start_line for a in checker.unexplained_allows(broken)], [4097])

    def test_generated_reason_does_not_excuse_parameter_limit(self):
        # This gate enforces reasons; the unconditional parameter-limit gate
        # still rejects a silencing attribute even when its reason is valid.
        spec = importlib.util.spec_from_file_location(
            "parameter_limit", MODULE_PATH.parents[1] / "policy/check_too_many_arguments.py"
        )
        limit = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(limit)
        source = (Path(__file__).parent / "fixtures/generated_rust_allows.rs.txt").read_text()
        source += ('#[allow(clippy::too_many_arguments, reason = "generated callback")]\n'
                   'fn unsafe_exception() {}\n')
        self.assertEqual(checker.unexplained_allows(source), [])
        self.assertEqual(len(limit.audit_source(Path("generated.rs"), source)), 1)

    def test_comments_and_literals_cannot_supply_attributes_or_reasons(self):
        source = '''\
// #[allow(dead_code)]
/* #[allow(dead_code)] /* nested */ */
const TEXT: &str = r##"#[allow(dead_code)]"##;
#[allow(dead_code /* reason = "not metadata" */)]
fn unexplained() {}
#[allow(dead_code, reason = "mentions reason = and ] and \\\"quotes\\\"")]
fn explained() {}
#[allow(dead_code, reason = r###"raw ] reason"###)]
fn raw_reason() {}
'''
        self.assertEqual([a.start_line for a in checker.unexplained_allows(source)], [4])

    def test_each_allow_needs_its_own_nonempty_string_reason(self):
        for attribute in (
            '#[allow(dead_code, reason = " ")]',
            '#[allow(dead_code, reason = b"bytes")]',
            '#[allow(dead_code, reason = concat!("computed"))]',
            '#[allow(dead_code, reason = "valid" + "expression")]',
            '#[cfg_attr(test, allow(dead_code), allow(unused, reason = "other"))]',
            '#[doc = "reason = ", allow(dead_code)]',
        ):
            with self.subTest(attribute=attribute):
                self.assertEqual(len(checker.unexplained_allows(attribute)), 1)

    def test_staged_check_reads_entire_exact_blob(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "-C", directory, "init", "-q"], check=True)
            path = root / "generated.rs"
            source = (Path(__file__).parent / "fixtures/generated_rust_allows.rs.txt").read_text()
            path.write_text('#[allow(dead_code)]\n' + source)
            subprocess.run(["git", "-C", directory, "add", "generated.rs"], check=True)
            path.write_text(source)  # An unstaged correction cannot hide the violation.
            with contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertEqual(checker.main(["--root", directory, "--staged"]), 1)
            self.assertIn("generated.rs:1:", errors.getvalue())

            subprocess.run(["git", "-C", directory, "add", "generated.rs"], check=True)
            path.write_text('#[allow(dead_code)]\n' + source)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(checker.main(["--root", directory, "--staged"]), 0)

    def test_batch_index_reads_preserve_unusual_names_and_blob_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "-C", directory, "init", "-q"], check=True)
            paths = [Path("name with spaces.rs"), Path("name\nwith\ttabs.rs")]
            for index, path in enumerate(paths):
                (root / path).write_text(f"// blob {index}\n\0\n#[allow(dead_code)]\n")
            subprocess.run(["git", "-C", directory, "add", "."], check=True)
            self.assertEqual(
                list(checker.staged_sources(root, paths)),
                [(path, checker.staged_source(root, path)) for path in paths],
            )
            with self.assertRaises(ValueError):
                list(checker.staged_sources(root, [Path("missing.rs")]))

    def test_batch_index_reads_reject_partial_or_nonblob_responses(self):
        for output in (b"abc tree 1\nx\n", b"abc blob 10\nx\n", b"abc blob 0\n\nextra"):
            with self.subTest(output=output):
                with mock.patch.object(checker.subprocess, "run") as run:
                    run.return_value.stdout = output
                    with self.assertRaises(ValueError):
                        list(checker.staged_sources(Path("."), [Path("generated.rs")]))


if __name__ == "__main__":
    unittest.main()
