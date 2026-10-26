"""Unit tests for the no-hardware checks' reading of the action listing.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks import basics  # noqa: E402


class ListingRows(unittest.TestCase):
    def test_a_row_without_a_contract_declares_nothing_so_counts_as_offered(self):
        # The listing serializes such rows as `"contract": null` (B03 raised on one).
        self.assertEqual(basics._availability({"id": "x", "contract": None})["state"], "available")
        self.assertEqual(basics._availability({"id": "x"})["state"], "available")

    def test_a_declared_availability_is_read(self):
        row = {"id": "x", "contract": {"availability": {"state": "unavailable", "reason": "no adapter"}}}
        self.assertEqual(basics._availability(row), {"state": "unavailable", "reason": "no adapter"})

    def test_a_settings_page_row_only_shows(self):
        self.assertTrue(basics._shows_only({"operation_kind": "View"}))
        self.assertFalse(basics._shows_only({"operation_kind": "Effect"}))
        self.assertFalse(basics._shows_only({}))


if __name__ == "__main__":
    unittest.main()
