"""tools/docs/codemap.py turns crate descriptions into one short line each."""

import importlib.util
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("codemap", ROOT / "tools/docs/codemap.py")
assert SPEC is not None and SPEC.loader is not None
codemap = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = codemap
SPEC.loader.exec_module(codemap)


class SummarizeTest(unittest.TestCase):
    def test_keeps_the_first_sentence_without_links(self):
        self.assertEqual(
            codemap.summarize("Reads [`Foo`] records. Then more.", "reader"),
            "Reads Foo records",
        )

    def test_drops_boilerplate_and_a_title_naming_the_crate(self):
        self.assertEqual(
            codemap.summarize("Disk Imager — Tilo native app for writing an OS image to a drive "
                              "(CRDT-native, built on the `ui` toolkit).", "imager"),
            "For writing an OS image to a drive",
        )

    def test_a_summary_that_only_repeats_the_name_is_empty(self):
        self.assertEqual(codemap.summarize("Hex — a hex editor for Tilo, built on the `ui` toolkit.", "hex-editor"), "")

    def test_keeps_lowercase_brand_names(self):
        self.assertEqual(codemap.summarize("iCloud Drive provider.", "icloud"), "iCloud Drive provider")

    def test_truncates_at_a_word(self):
        summary = codemap.summarize("word " * 40, "x")
        self.assertLessEqual(len(summary), codemap.MAX_SUMMARY)
        self.assertTrue(summary.endswith("word…"))


if __name__ == "__main__":
    unittest.main()
