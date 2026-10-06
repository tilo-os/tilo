"""Check architecture-specific cache calculations and provenance failures."""

import copy
import importlib.util
from pathlib import Path
import unittest


PATH = Path(__file__).resolve().parents[1] / "chip" / "model_profile.py"
SPEC = importlib.util.spec_from_file_location("model_profile", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ModelProfileTests(unittest.TestCase):
    def setUp(self):
        self.config = {
            "text_config": {
                "model_type": "qwen3_5_text",
                "num_hidden_layers": 64,
                "layer_types": ["linear_attention"] * 48 + ["full_attention"] * 16,
                "num_key_value_heads": 4,
                "head_dim": 256,
                "max_position_embeddings": 262144,
            },
        }
        self.record = {
            "schema_version": 1,
            "model": "Qwen/Qwen3.8-27B",
            "revision": "a" * 40,
            "config_url": "https://huggingface.co/Qwen/Qwen3.8-27B/resolve/" + "a" * 40 + "/config.json",
            "config": self.config,
            "config_sha256": MODULE.config_digest(self.config),
        }
        self.assumptions = {
            "logical_parameters": 27_000_000_000,
            "weight_bits": 4,
            "kv_bits": 16,
            "context_tokens": 32768,
        }

    def test_hybrid_cache_counts_only_full_attention_and_declares_exclusions(self):
        result = MODULE.profile(self.record, self.assumptions)
        self.assertEqual(result["packed_weight_bytes"], 13_500_000_000)
        cache = result["full_attention_cache"]
        self.assertEqual(cache["full_attention_layers"], 16)
        self.assertEqual(cache["packed_bytes"], 2_147_483_648)
        self.assertIn("recurrent_state", cache["excludes"])
        self.assertIn("coding_quality", result["not_estimated"])

    def test_metadata_mutation_invalidates_profile(self):
        self.config["text_config"]["head_dim"] = 128
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            MODULE.profile(self.record, self.assumptions)

    def test_specialized_cache_is_unknown_not_zero_or_gqa(self):
        config = copy.deepcopy(self.config["text_config"])
        config["model_type"] = "deepseek_v41_text"
        result = MODULE.attention_cache(config, 32768, 16)
        self.assertEqual(result["status"], "unsupported")
        self.assertNotIn("packed_bytes", result)

    def test_bit_packing_rounds_up_without_float_precision_loss(self):
        assumptions = self.assumptions | {"logical_parameters": 2**54 + 1, "weight_bits": 1}
        result = MODULE.profile(self.record, assumptions)
        self.assertEqual(result["packed_weight_bytes"], 2**51 + 1)
        self.assertEqual(result["batch_size"], 1)

    def test_rejects_moving_revision_and_impossible_inputs(self):
        self.record["revision"] = "main"
        with self.assertRaisesRegex(ValueError, "immutable"):
            MODULE.profile(self.record, self.assumptions)
        self.record["revision"] = "a" * 40
        for name, value in [("context_tokens", 0), ("context_tokens", 262145),
                            ("logical_parameters", -1), ("weight_bits", 33),
                            ("kv_bits", True)]:
            assumptions = self.assumptions | {name: value}
            with self.subTest(name=name, value=value), self.assertRaises(ValueError):
                MODULE.profile(self.record, assumptions)

    def test_invalid_layer_topology_fails(self):
        self.config["text_config"]["layer_types"].pop()
        self.record["config_sha256"] = MODULE.config_digest(self.config)
        with self.assertRaisesRegex(ValueError, "layer_types"):
            MODULE.profile(self.record, self.assumptions)

    def test_hybrid_missing_topology_and_misattributed_model_fail(self):
        self.record["model"] = "Different/Unrelated"
        with self.assertRaisesRegex(ValueError, "config URL"):
            MODULE.profile(self.record, self.assumptions)
        self.record["model"] = "Qwen/Qwen3.8-27B"
        del self.config["text_config"]["layer_types"]
        self.record["config_sha256"] = MODULE.config_digest(self.config)
        with self.assertRaisesRegex(ValueError, "explicit layer_types"):
            MODULE.profile(self.record, self.assumptions)

    def test_committed_snapshots_have_valid_identity_and_config_digest(self):
        import json
        root = Path(__file__).resolve().parents[2]
        for name in ("Qwen3.8-27B", "Qwen3.8-Flash-Next", "DeepSeek-V4.1-Flash",
                     "Mistral-Medium-3.5-128B"):
            with self.subTest(model=name):
                record = json.loads((root / "docs/projects/llm-asic/models" / f"{name}.json").read_text())
                result = MODULE.profile(record, self.assumptions)
                self.assertEqual(result["revision"], record["revision"])
                self.assertEqual(result["evidence"], "analytical_storage_bound")


if __name__ == "__main__":
    unittest.main()
