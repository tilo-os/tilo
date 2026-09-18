from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


MODULE_PATH = Path(__file__).parents[1] / "check_changed_rust_allows.py"
SPEC = importlib.util.spec_from_file_location("check_changed_rust_allows", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class ChangedRustAllowTests(unittest.TestCase):
    def test_new_single_and_multiline_allows_need_reasons(self) -> None:
        source = """\
#[allow(dead_code)]
fn old() {}

#[allow(
    clippy::too_many_arguments,
)]
fn changed() {}
"""
        violations = checker.unexplained_added_allows(source, {5})
        self.assertEqual([(item.start_line, item.end_line) for item in violations], [(4, 6)])

    def test_reasoned_allow_and_unrelated_existing_allow_pass(self) -> None:
        source = """\
#[allow(dead_code)]
fn old() {}

#[allow(
    clippy::too_many_arguments,
    reason = "typed protocol callback shape",
)]
fn changed() {}
"""
        self.assertEqual(checker.unexplained_added_allows(source, {5, 6}), [])

    def test_cfg_attr_allow_is_also_an_exception(self) -> None:
        source = '#[cfg_attr(not(test), allow(dead_code))]\nfn hidden() {}\n'
        violations = checker.unexplained_added_allows(source, {1})
        self.assertEqual(len(violations), 1)

    def test_added_lines_reads_zero_length_and_normal_hunks(self) -> None:
        diff = """\
@@ -2,0 +3,2 @@
+one
+two
@@ -9 +12,0 @@
-gone
@@ -20 +22 @@
-old
+new
"""
        self.assertEqual(checker.added_lines(diff), {3, 4, 22})


if __name__ == "__main__":
    unittest.main()
