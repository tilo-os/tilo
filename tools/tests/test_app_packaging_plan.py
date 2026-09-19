import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_plan as plan
import app_packaging_product as product


class PlanProducerTests(unittest.TestCase):
    def setUp(self):
        self.desired = {'source_sha': 'a' * 40, 'builder_image': product.resolve('code.sandbox.example/qos/tilo-builder@sha256:' + 'b' * 64),
                        'collection': 'default', 'architectures': ['x86_64', 'aarch64'], 'policy_sha256': 'c' * 64}
        self.intent = {'id': '12345678-1234-4234-8234-123456789abc', 'desired': self.desired, 'baseline': None}
        self.env = {variable: product.dispatch_inputs(self.intent)[name] for name, variable in plan.INPUT_ENV.items()}
        self.env.update(COORDINATOR_ENABLED='true', GITHUB_RUN_ID='123', GITHUB_RUN_ATTEMPT='2',
                        GITHUB_SHA=self.desired['source_sha'], GITHUB_REF=self.desired['source_sha'],
                        GITHUB_EVENT_NAME='workflow_dispatch', RESOLVED_BUILDER=self.desired['builder_image'])
        self.zero = {'count': 0, 'apps': [], 'packages': [], 'catalog': [], 'features': []}

    def test_cli_emits_exact_bytes_bound_by_trusted_log_marker(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'app-release-plan.json').write_text(json.dumps(self.zero))
            result = subprocess.run([sys.executable, plan.__file__], cwd=root,
                                    env=dict(os.environ, **self.env), capture_output=True, text=True, check=True)
            raw = (root / 'coordinator-plan.json').read_bytes()
            self.assertEqual(result.stdout.strip(), 'TILO_COORDINATOR_PLAN ' + self.intent['id'] + ' ' + hashlib.sha256(raw).hexdigest())
            document = json.loads(raw)
            self.assertEqual(document['inputs'], product.dispatch_inputs(self.intent))
            self.assertEqual(document['plan_attempt'], 2)
            self.assertEqual(document['plan'], self.zero)

    def test_guard_and_real_plan_count_must_agree(self):
        for field, value in [('count', False), ('count', 1), ('apps', ['terminal']), ('catalog', [{}])]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                plan.create(self.env, dict(self.zero, **{field: value}))
        with self.assertRaises(ValueError):
            plan.create(dict(self.env, RESOLVED_BUILDER='other'), self.zero)
        with self.assertRaises(ValueError):
            plan.create(dict(self.env, COORDINATOR_ENABLED='false'), self.zero)


if __name__ == '__main__':
    unittest.main()
