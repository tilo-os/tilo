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
    def test_single_and_multiline_allows_need_reasons(self) -> None:
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

    def test_reasoned_allow_passes(self) -> None:
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

    def test_cfg_attr_allow_is_also_an_exception(self) -> None:
        source = '#[cfg_attr(not(test), allow(dead_code))]\nfn hidden() {}\n'
        violations = checker.unexplained_allows(source)
        self.assertEqual(len(violations), 1)


if __name__ == "__main__":
    unittest.main()
