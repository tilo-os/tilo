"""Published collaboration methods keep their ordinals and wire slots."""

import contextlib
import importlib.util
import io
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "crates/runtime/runtime-collab"
spec = importlib.util.spec_from_file_location(
    "check_capnp_bindings", ROOT / "tools/policy/check_capnp_bindings.py"
)
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class CollabWireCompatibility(unittest.TestCase):
    def test_published_method_ordinals(self):
        schema = (CRATE / "schema/collab.capnp").read_text()
        for method, ordinal in (
            ("publishActionSurface", 61),
            ("nearbyTransferChanged", 62),
            ("browserServiceSessionCommand", 63),
            ("speakerChanged", 68),
        ):
            self.assertRegex(schema, rf"\b{method}\s+@{ordinal}\b")
        self.assertNotIn("retiredSurfaceBrowserOrdinalCollision", schema)

    def test_published_task_parameter_and_result_slots(self):
        bindings = (CRATE / "src/generated/collab_capnp.rs").read_text()
        params = bindings.split("pub mod invoke_task_action_params {", 1)[1]
        results = bindings.split("pub mod invoke_task_action_results {", 1)[1]
        # These are the original reader offsets, not source field ordinals.
        for section, fields in (
            (params, {
                "run_id": "get_data_field::<u64>(0)",
                "lease": "get_pointer_field(0)",
                "peer_id": "get_data_field::<u64>(1)",
                "resource": "get_pointer_field(1)",
                "action": "get_pointer_field(2)",
                "argument": "get_pointer_field(3)",
                "has_argument": "get_bool_field(128)",
                "confirmation_json": "get_pointer_field(4)",
                "invocation": "get_pointer_field(5)",
                "wait_ms": "get_data_field::<u32>(5)",
                "seen_json": "get_pointer_field(6)",
                "provider": "get_pointer_field(7)",
            }),
            (results, {
                "id": "get_data_field::<u64>(0)",
                "update_json": "get_pointer_field(0)",
            }),
        ):
            for field, offset in fields.items():
                with self.subTest(field=field):
                    getter = re.search(
                        rf"pub fn get_{field}\(self\)[^{{]+\{{([^}}]+)\}}",
                        section,
                    )
                    self.assertIsNotNone(getter)
                    self.assertIn(offset, getter[1])

    def test_fingerprint_policy_detects_schema_and_binding_edits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "schema.capnp").write_text("schema")
            (root / "src/generated").mkdir(parents=True)
            binding = root / "src/generated/schema_capnp.rs"
            binding.write_text("binding")
            entry = {"path": ".", "schemas": ["schema.capnp"]}
            with patch.object(policy, "ROOT", root):
                entry["schema_sha256"], entry["bindings_sha256"] = policy.digests(entry)
                with patch.object(policy, "load", return_value=[entry]):
                    self.assertEqual(policy.check(), 0)
                    for path in (root / "schema.capnp", binding):
                        original = path.read_text()
                        path.write_text(original + " changed")
                        with contextlib.redirect_stderr(io.StringIO()):
                            self.assertEqual(policy.check(), 1)
                        path.write_text(original)


if __name__ == "__main__":
    unittest.main()
