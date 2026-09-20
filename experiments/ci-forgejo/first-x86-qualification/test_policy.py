"""Offline acceptance of the concrete policy through production contracts."""
import base64
import copy
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("prepare_qualification", HERE / "prepare.py")
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)
product = prepare.product
import app_packaging_service as service
import app_packaging_qualification as qualification
import app_channel_manifest as channel


class QualificationPolicyTests(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads((HERE / "coordinator.json").read_text())
        self.final_bytes = (HERE / "final-policy.json").read_bytes()
        self.review = json.loads((HERE / "review.json").read_text())
        self.source = self.review["source_sha"]
        self.desired = {"source_sha": self.source, "builder_image": self.policy["builder_image"],
                        "collection": self.policy["collection"], "architectures": self.policy["architectures"],
                        "policy_sha256": self.review["coordinator_policy_sha256"]}

    def test_generation_reproduces_reviewed_files_byte_for_byte(self):
        for name, raw in prepare.generate().items():
            self.assertEqual(raw, (HERE / name).read_bytes(), name)

    def test_actual_service_accepts_qualified_x86_policy_and_exact_final_bytes(self):
        # This checks schema/identity only; the retained native Pod is the ABI proof.
        with patch.object(service.bundle, "native_arch", return_value="x86_64"):
            self.assertEqual(service.validate_policy(self.policy, self.final_bytes),
                             self.review["final_receipt_policy_sha256"])
            with self.assertRaisesRegex(ValueError, "trusted digest mismatch"):
                service.validate_policy(self.policy, self.final_bytes + b" ")
        final = json.loads(self.final_bytes)
        apps, runtime = channel.trusted_keys(final)
        self.assertEqual(apps, {json.loads((HERE / "inputs.json").read_text())["app_publisher"]})
        self.assertEqual(runtime, json.loads((HERE / "runtime-publisher.json").read_text())["runtime_publisher"])

    def local_api(self, policy=None, corrupt=None):
        api = product.ProductForgejo(policy or self.policy)
        def call(path, *args):
            prefix = "repos/tilo/tilo"
            if path == prefix:
                return {"id": 23, "full_name": "tilo/tilo"}
            suffix = path[len(prefix + "/contents/"):]
            name, source = suffix.split("?ref=", 1)
            self.assertEqual(source, self.source)
            if name == "":
                return [{"name": ".cargo", "type": "dir"}]
            if name == ".cargo":
                return [{"name": "config.toml", "type": "file"}]
            raw = prepare.raw_source(source, name)
            if name == corrupt:
                raw += b"# changed\n"
            return {"type": "file", "content": base64.b64encode(raw).decode()}
        api.call = call
        return api

    def test_real_product_contract_accepts_exact_committed_trusted_closure(self):
        self.local_api().qualify_receipt(self.desired)
        self.assertEqual(set(self.policy["trusted_files"]), product.REQUIRED_PATHS)
        self.assertEqual(len(self.policy["trusted_files"]), self.review["trusted_file_count"])

    def test_changed_workflow_or_missing_trusted_input_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "differs from trusted policy"):
            self.local_api(corrupt=".forgejo/workflows/release-apps.yml").qualify_receipt(self.desired)
        policy = copy.deepcopy(self.policy)
        del policy["trusted_files"]["tools/ci/python.py"]
        desired = dict(self.desired, policy_sha256=product.core.receipt_key(policy))
        with self.assertRaisesRegex(ValueError, "incomplete trusted workflow policy"):
            self.local_api(policy).qualify_receipt(desired)

    def test_first_dispatch_is_full_collection_nonpublishing_without_baseline(self):
        intent = {"id": "00000000-0000-4000-8000-000000000001", "desired": self.desired,
                  "baseline": None, "baseline_evidence": None}
        inputs = product.dispatch_inputs(intent)
        self.assertEqual(inputs["publish"], "false")
        self.assertEqual(inputs["apps"], "")
        self.assertEqual(inputs["architectures"], "x86_64")
        self.assertEqual(inputs["collection"], "default")
        for key in ("baseline_sha", "baseline_run_id", "baseline_evidence", "baseline_receipt_sha256"):
            self.assertEqual(inputs[key], "")
        env = {"COORDINATOR_ENABLED": "true", "GITHUB_EVENT_NAME": "workflow_dispatch",
               "GITHUB_SHA": self.source, "GITHUB_REF": self.source,
               "COORDINATOR_INTENT": intent["id"], "REQUESTED_APPS": inputs["apps"],
               "REQUESTED_PUBLISH": inputs["publish"], "REQUESTED_BUILDER": inputs["builder_image"],
               "COORDINATOR_POLICY_SHA256": inputs["coordinator_policy_sha256"]}
        self.assertTrue(qualification.validate(env))
        for change in ({"REQUESTED_PUBLISH": "true"}, {"COORDINATOR_ENABLED": "false"},
                       {"GITHUB_REF": "refs/heads/main"}):
            with self.assertRaises(ValueError):
                qualification.validate(dict(env, **change))

    def test_disabled_or_unqualified_policy_and_arm_environment_fail_closed(self):
        with patch.object(service.bundle, "native_arch", return_value="x86_64"):
            for key in ("enabled", "qualification_only", "bundle_compatibility_approved"):
                with self.assertRaises(ValueError):
                    service.validate_policy(dict(self.policy, **{key: False}), self.final_bytes)
        with patch.object(service.bundle, "native_arch", return_value="aarch64"):
            with self.assertRaises(KeyError):
                service.validate_policy(self.policy, self.final_bytes)


if __name__ == "__main__":
    unittest.main()
