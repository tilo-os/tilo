"""Real Forgejo input serialization must not change dispatch authorization."""
import copy
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.append(str(Path(__file__).resolve().parents[1] / 'apps'))
import accept_app_artifacts as acceptance
import app_packaging_product as product


class RunInputTests(unittest.TestCase):
    def setUp(self):
        fixture = json.loads((Path(__file__).parent / 'fixtures/forgejo-run-23344.json').read_text())
        self.intent, self.run = fixture['intent'], fixture['run']
        self.api = product.ProductForgejo(fixture['policy'])
        self.inputs = json.loads(self.run['event_payload'])['inputs']

    def test_actual_run_passes_both_identity_validators_without_changing_intent(self):
        before = copy.deepcopy(self.intent)
        self.assertIsNone(self.api.validate_run(self.intent, self.run))
        # This exercises only final-run identity validation, not artifact acceptance.
        acceptance.validate_run(self.api, self.intent, dict(self.run, status='success'))
        self.assertEqual(self.intent, before)

    def test_explicit_empty_request_and_persisted_defaults_have_identical_meaning(self):
        inputs = product.dispatch_inputs(self.intent)
        self.assertTrue(product.run_inputs_match(self.intent, inputs))
        self.assertTrue(product.run_inputs_match(self.intent, dict(inputs, verify_final_receipt='false')))
        self.assertTrue(product.run_inputs_match(self.intent, self.inputs))

    def test_unknown_types_fields_and_any_changed_value_fail_both_validators(self):
        variants = [None, [], {}, dict(self.inputs, unknown=''), dict(self.inputs, unknown='false')]
        expected = dict(product.dispatch_inputs(self.intent), verify_final_receipt='false')
        for key in expected:
            for value in (None, False, True, 0, 1, [], {}, 'unexpected', 'true'):
                variants.append(dict(expected, **{key: value}))
        for key in self.inputs:
            if key != 'verify_final_receipt':
                variant = dict(self.inputs)
                del variant[key]
                variants.append(variant)
        for inputs in variants:
            with self.subTest(inputs=inputs):
                run = dict(self.run, event_payload=json.dumps({'ref': self.run['commit_sha'], 'inputs': inputs}))
                self.assertFalse(product.run_inputs_match(self.intent, inputs))
                with self.assertRaises(ValueError):
                    self.api.validate_run(self.intent, run)
                with self.assertRaises(ValueError):
                    acceptance.validate_run(self.api, self.intent, dict(run, status='success'))

    def test_nonempty_incremental_baseline_cannot_be_omitted_or_reencoded(self):
        baseline = dict(self.intent['desired'], source_sha='a' * 40)
        proof = {'schema': 'tilo.coordinator-verification/v1', 'run_id': 123,
                 'desired': baseline, 'backend': 'canonical-final-artifact-v1',
                 'receipts': {'x86_64': 'e' * 64}}
        intent = dict(self.intent, baseline=baseline, baseline_evidence=proof)
        inputs = product.dispatch_inputs(intent)
        self.assertTrue(product.run_inputs_match(intent, inputs))
        for key in ('baseline_sha', 'baseline_run_id', 'baseline_evidence', 'baseline_receipt_sha256'):
            for value in ('', None):
                with self.subTest(key=key, value=value):
                    changed = dict(inputs)
                    if value is None:
                        del changed[key]
                    else:
                        changed[key] = value
                    self.assertFalse(product.run_inputs_match(intent, changed))
        changed = dict(inputs, baseline_evidence=json.dumps(proof, indent=2))
        self.assertFalse(product.run_inputs_match(intent, changed))


if __name__ == '__main__':
    unittest.main()
