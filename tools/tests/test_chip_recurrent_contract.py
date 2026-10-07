"""Independent exact golden cases and source/config identity rejection."""

from copy import deepcopy
from fractions import Fraction as F
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/chip"))
SPEC = importlib.util.spec_from_file_location("recurrent_contract", REPO / "tools/chip/recurrent_contract.py")
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)
SPEC = importlib.util.spec_from_file_location("recurrent_reference", REPO / "hardware/llm-asic/recurrent_reference.py")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)
SNAPSHOT = REPO / "docs/projects/llm-asic/models/Qwen3.8-27B.json"


class RecurrentReferenceTests(unittest.TestCase):
    def test_decay_before_prediction_and_post_update_readout_have_exact_golden_values(self):
        old = [[1, 2], [3, 4]]
        state, output = REFERENCE.gated_delta_step(old, [F(1, 2), F(-1, 2)], [1, 0], [5, 7], F(1, 2), F(1, 4))
        self.assertEqual(state, [[F(13, 8), F(5, 2)], [F(3, 2), F(2)]])
        self.assertEqual(output, [F(1, 16), F(1, 4)])
        self.assertEqual(old, [[1, 2], [3, 4]], "tokens cannot mutate another branch's state")

    def test_residual_overwrites_predicted_key_direction_and_preserves_other_direction(self):
        state, output = REFERENCE.gated_delta_step([[11], [13]], [1, 0], [1, 0], [7], F(1, 2), 1)
        self.assertEqual(state, [[F(7)], [F(13, 2)]])
        self.assertEqual(output, [F(7)])
        decayed, output = REFERENCE.gated_delta_step(state, [0, 1], [0, 1], [999], F(1, 2), 0)
        self.assertEqual(decayed, [[F(7, 2)], [F(13, 4)]])
        self.assertEqual(output, [F(13, 4)])

    def test_two_tokens_and_context_forks_keep_separate_state(self):
        first, _ = REFERENCE.gated_delta_step([[0, 0]], [1], [1], [2, -4], 1, F(1, 2))
        next_state, output = REFERENCE.gated_delta_step(first, [1], [1], [10, 2], F(1, 2), F(1, 2))
        self.assertEqual(next_state, [[F(21, 4), F(1, 2)]])
        self.assertEqual(output, [F(21, 4), F(1, 2)])
        branch, _ = REFERENCE.gated_delta_step(first, [1], [1], [-2, 6], 0, 1)
        self.assertEqual(branch, [[F(-2), F(6)]])
        self.assertEqual(first, [[F(1), F(-2)]])

    def test_invalid_shape_gates_and_unspecified_float_rounding_fail(self):
        for state, query, key, value, decay, beta in [
                ([], [], [], [], 1, 1), ([[1], [2, 3]], [1, 1], [1, 1], [1], 1, 1),
                ([[1]], [1, 2], [1], [1], 1, 1), ([[1]], [1], [1], [1], -1, 1),
                ([[1]], [1], [1], [1], 1, F(3, 2)), ([[1.0]], [1], [1], [1], 1, 1)]:
            with self.subTest(state=state, decay=decay, beta=beta), self.assertRaises(ValueError):
                REFERENCE.gated_delta_step(state, query, key, value, decay, beta)


class RecurrentContractTests(unittest.TestCase):
    def snapshot(self):
        return json.loads(SNAPSHOT.read_text())

    def test_actual_pinned_schedule_state_repeat_and_conv_dimensions(self):
        contract = CONTRACT.state_contract(self.snapshot())
        self.assertEqual(contract["per_layer_state_shape"], [1, 48, 128, 128])
        self.assertEqual(contract["query_key_head_repeat"], 3)
        self.assertEqual(contract["all_recurrent_state_bytes"], 150_994_944)
        self.assertEqual(contract["per_layer_conv_cache_shape"], [1, 10240, 4])
        self.assertEqual(contract["all_conv_cache_bytes_under_assumption"], 3_932_160)
        self.assertEqual(contract["recurrent_layer_indices"][:6], [0, 1, 2, 4, 5, 6])
        self.assertEqual(len(contract["recurrent_layer_indices"]), 48)
        larger = CONTRACT.state_contract(self.snapshot(), batch=2, conv_element_bytes=4)
        self.assertEqual(larger["all_recurrent_state_bytes"], 301_989_888)
        self.assertEqual(larger["all_conv_cache_bytes_under_assumption"], 15_728_640)

    def test_moving_identity_implicit_schedule_or_changed_dimensions_are_rejected(self):
        for mutate in [lambda r: r.update(revision="main"),
                       lambda r: r.update(config_url=r["config_url"].replace(CONTRACT.REVISION, "main")),
                       lambda r: r["config"]["text_config"].pop("layer_types"),
                       lambda r: r["config"]["text_config"].update(linear_num_value_heads=32)]:
            snapshot = deepcopy(self.snapshot())
            mutate(snapshot)
            snapshot["config_sha256"] = CONTRACT.config_digest(snapshot["config"])
            with self.assertRaises(ValueError):
                CONTRACT.state_contract(snapshot)
        with self.assertRaises(ValueError):
            CONTRACT.state_contract(self.snapshot(), batch=True)

    def test_source_change_or_missing_source_is_not_silently_observed(self):
        with self.assertRaisesRegex(ValueError, "identity"):
            CONTRACT.audit_source("modeling_qwen3_5.py", b"def torch_recurrent_gated_delta_rule(): pass\n")
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                CONTRACT.extract(self.snapshot(), Path(directory))
            bad = self.snapshot()
            bad["revision"] = "main"
            source = Path(directory) / "bad-snapshot.json"
            source.write_text(json.dumps(bad))
            with patch.object(CONTRACT.urllib.request, "urlopen") as network, \
                    patch.object(sys, "argv", ["recurrent_contract.py", str(source), "--fetch",
                                              "--source-dir", str(Path(directory) / "sources"),
                                              "--out", str(Path(directory) / "receipt.json")]):
                with self.assertRaises(SystemExit) as failed:
                    CONTRACT.main()
                self.assertEqual(failed.exception.code, 1)
                network.assert_not_called()
                self.assertFalse((Path(directory) / "receipt.json").exists())


if __name__ == "__main__":
    unittest.main()
