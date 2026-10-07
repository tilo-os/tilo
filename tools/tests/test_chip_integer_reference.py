"""Contract checks; RTL certification additionally requires verify_tile.py."""

import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "integer_reference", Path(__file__).resolve().parents[2] / "hardware" / "llm-asic" / "integer_reference.py")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)


class IntegerReferenceTests(unittest.TestCase):
    def test_signed_extremes_and_independent_expected_results(self):
        self.assertEqual(REFERENCE.group_dot([-128, 127, -1, 0],
                         [[-8, 7, -8, 7], [7, -8, 7, -8]]), [1921, -1919])

    def test_complete_hidden_dimension_fits_int32(self):
        self.assertEqual(REFERENCE.group_dot([-128] * 5120, [[-8] * 5120]), [5242880])

    def test_invalid_shape_and_out_of_range_values_fail(self):
        for activations, weights in [([], [[]]), ([1], []), ([1], [[1, 2]]),
                                     ([128], [[1]]), ([1], [[8]]), ([True], [[1]]),
                                     ([1] * 5121, [[1] * 5121])]:
            with self.subTest(activations=activations[:2], rows=len(weights)):
                with self.assertRaises(ValueError):
                    REFERENCE.group_dot(activations, weights)


if __name__ == "__main__":
    unittest.main()
