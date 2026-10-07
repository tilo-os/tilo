"""Specified numerical boundaries and production compiled-sample consumption."""

from fractions import Fraction
import importlib.util
import json
from pathlib import Path
import random
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("scaled_linear_reference", ROOT / "hardware/llm-asic/scaled_linear_reference.py")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)
import tools.tests.test_chip_compile_weight_tile as compiler_fixtures


def host_value(word):
    return struct.unpack("<f", struct.pack("<I", word))[0]


def host_bits(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


class ScaledLinearReferenceTests(unittest.TestCase):
    def test_each_finite_bf16_storage_word_roundtrips_independent_host_decode(self):
        for word in range(65536):
            if word & 0x7f80 == 0x7f80:
                continue
            self.assertEqual(REFERENCE.finite_value(word, 7), Fraction.from_float(host_value(word << 16)))
            self.assertEqual(REFERENCE.bf16_output(word << 16), word)

    def test_rne_subnormals_signed_zero_and_overflow(self):
        self.assertEqual(REFERENCE.round_bits(Fraction(1) + Fraction(1, 1 << 24), 23), 0x3f800000)
        self.assertEqual(REFERENCE.round_bits(Fraction(1) + Fraction(3, 1 << 24), 23), 0x3f800002)
        self.assertEqual(REFERENCE.round_bits(Fraction(1, 1 << 150), 23), 0)
        self.assertEqual(REFERENCE.round_bits(Fraction(3, 1 << 150), 23), 2)
        self.assertEqual(REFERENCE.round_bits(-Fraction(1, 1 << 150), 23), 0x80000000)
        self.assertEqual(REFERENCE.round_bits(Fraction(1, 1 << 126), 23), 0x00800000)
        self.assertEqual(REFERENCE.bf16_output(0x80000001), 0x8000)
        self.assertEqual(REFERENCE.fp32_add(0x80000000, 0x80000000), 0x80000000)
        self.assertEqual(REFERENCE.fp32_add(0x80000000, 0), 0)
        self.assertEqual(REFERENCE.fp32_multiply(0x80000000, 0x3f800000), 0x80000000)
        for operation in [lambda: REFERENCE.round_bits(1 << 128, 23),
                          lambda: REFERENCE.bf16_output(0x7f7fffff),
                          lambda: REFERENCE.positive_scale(Fraction(1, 1 << 150))]:
            with self.assertRaises((FloatingPointError, ValueError)):
                operation()

    def test_binary_operations_match_independent_host_fp32_rounding(self):
        rng = random.Random(20261006)
        for _ in range(500):
            # Products remain finite and normal for this independent host oracle.
            words = [(rng.randrange(90, 160) << 23) | rng.randrange(1 << 23) | (rng.randrange(2) << 31) for _ in range(2)]
            a, b = map(host_value, words)
            self.assertEqual(REFERENCE.fp32_add(*words), host_bits(a + b))
            self.assertEqual(REFERENCE.fp32_multiply(*words), host_bits(a * b))

    def test_activation_ties_and_zero_scale(self):
        # absmax/127 = 2; 1/2 rounds to even zero, 3/2 to even two.
        q, scale, bits = REFERENCE.quantize_activations([0x437e, 0x3f80, 0x4040, 0xbf80, 0xc040])
        self.assertEqual(q, [127, 0, 2, 0, -2])
        self.assertEqual(scale, 2)
        self.assertEqual(bits, 0x40000000)
        self.assertEqual(REFERENCE.quantize_activations([0, 0x8000]), ([0, 0], Fraction(1), 0x3f800000))

    def test_bounded_integer_frontend_matches_independent_exact_ratio(self):
        # Finite BF16 absmax extremes, normal/subnormal boundary and ties.
        for maximum in [1, 0x0080, 0x3f80, 0x437e, 0x7f7f]:
            limit = REFERENCE.finite_value(maximum, 7)
            magnitudes = [0, maximum, max(0, maximum - 1), *range(0, maximum, max(1, maximum // 257))]
            for magnitude in magnitudes:
                expected = round(127 * REFERENCE.finite_value(magnitude, 7) / limit)
                self.assertEqual(REFERENCE.activation_code_integer(magnitude, maximum), expected)
                self.assertEqual(REFERENCE.activation_code_integer(magnitude | 0x8000, maximum), -expected)
        self.assertEqual(REFERENCE.activation_code_integer(0x3f80, 0x437e), 0)
        self.assertEqual(REFERENCE.activation_code_integer(0x4040, 0x437e), 2)
        for value, maximum in [(0x4000, 0x3f80), (0, 0x8000), (0x7f80, 0x7f7f)]:
            with self.assertRaises(ValueError):
                REFERENCE.activation_code_integer(value, maximum)

    def test_multi_group_golden_and_ordered_rounding(self):
        # Group A=[127,-127], group B=[254,0]. Their scales are 1 and 2.
        result = REFERENCE.linear_projection([0x42fe, 0xc2fe, 0x437e, 0], [[7, -7, 1, 0]], [[Fraction(1, 2), Fraction(1, 4)]], group_size=2)
        self.assertEqual(result["integer_partials"], [[1778, 127]])
        self.assertEqual(result["outputs"], [{"fp32_bits": 0x446e2000, "bf16_bits": 0x446e}])  # 952.5 -> 952 BF16.
        one = 0x3f800000
        ordered = REFERENCE.rescale_partials([1 << 24, 1, -(1 << 24)], [one] * 3, [one] * 3)
        different_order = REFERENCE.rescale_partials([1 << 24, -(1 << 24), 1], [one] * 3, [one] * 3)
        self.assertEqual(ordered[0], 0)
        self.assertEqual(different_order[0], one)

    def test_invalid_finite_shape_and_coefficient_contract_refused(self):
        for operation in [lambda: REFERENCE.quantize_activations([0x7f80]),
                          lambda: REFERENCE.quantize_activations([True]),
                          lambda: REFERENCE.linear_projection([0x3f80], [[-8]], [[1]]),
                          lambda: REFERENCE.linear_projection([0x3f80], [[1]], [[]]),
                          lambda: REFERENCE.rescale_partials([1], [0], [0x3f800000]),
                          lambda: REFERENCE.rescale_partials([1 << 31], [0x3f800000], [0x3f800000])]:
            with self.assertRaises(ValueError):
                operation()

    def test_compiled_sample_production_cli_consumes_scales_and_preserves_failure_receipts(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample = root / "sample"
            compiler_fixtures.WeightCompilerTests().compile(sample)
            activations = root / "activations.json"
            activations.write_text(json.dumps([0x3f80] * 4))
            out = root / "result.json"
            command = [sys.executable, str(ROOT / "hardware/llm-asic/scaled_linear_reference.py"), "--sample", str(sample), "--activations", str(activations), "--out", str(out)]
            subprocess.run(command, check=True, capture_output=True)
            prior = out.read_bytes()
            receipt = json.loads(prior)
            self.assertEqual(receipt["status"], "passed")
            manifest = json.loads((sample / "sample.json").read_bytes())
            self.assertEqual(receipt["projection"]["source_weight_scales_exact"], manifest["quantization"]["group_scales_exact"])
            self.assertEqual(len(receipt["projection"]["source_dot_comparison"]), 2)
            (sample / "sample.bf16").write_bytes(b"wrong")
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(out.read_bytes(), prior)
            command[-1] = str(root / "failure.json")
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            failed = json.loads((root / "failure.json").read_bytes())
            self.assertEqual(failed["status"], "failed")
            self.assertIn("artifact identity", failed["failure"])
            self.assertNotIn("projection", failed)


if __name__ == "__main__":
    unittest.main()
