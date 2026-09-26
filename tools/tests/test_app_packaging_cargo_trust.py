import base64
import hashlib
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_product as product


class CargoTrustTests(unittest.TestCase):
    def setUp(self):
        root = Path(product.__file__).resolve().parents[1]
        self.files = {path: (root / path).read_bytes() for path in product.REQUIRED_PATHS}
        policy = {'enabled': True, 'qualification_only': True,
                  'final_receipt_policy_sha256': 'f' * 64,
                  'trusted_files': {path: hashlib.sha256(raw).hexdigest() for path, raw in self.files.items()}}
        self.api = product.ProductForgejo(policy)
        self.listings = {'': [{'name': '.cargo', 'type': 'dir'}],
                         '.cargo': [{'name': 'config.toml', 'type': 'file'}]}
        self.receipt = {'source_sha': 'a' * 40, 'builder_image': 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'b' * 64,
                        'collection': 'default', 'architectures': ['x86_64'], 'policy_sha256': self.api.policy_sha}
        self.api.call = self.read

    def read(self, path):
        if path == 'repos/' + product.REPO:
            return {'id': 23, 'full_name': product.REPO}
        source = path.split('/contents/', 1)[1].split('?', 1)[0]
        if source in self.listings:
            return self.listings[source]
        return {'type': 'file', 'content': base64.b64encode(self.files[source]).decode()}

    def test_cargo_trust_accepts_only_the_reviewed_pinned_toolchain_and_config(self):
        with self.subTest(phase='real reviewed policy passes'):
            self.doCleanups()
            self.setUp()
            self.api.qualify_receipt(self.receipt)
        with self.subTest(phase='controller only changes preserve workflow policy and baseline inputs'):
            self.doCleanups()
            self.setUp()
            original_policy = product.core.canonical(self.api.policy)
            self.api.qualify_receipt(self.receipt)
            for name in ('app_packaging_product.py', 'app_packaging_coordinator.py',
                         'app_packaging_service.py', 'accept_app_artifacts.py'):
                path = 'tools/' + name
                self.assertNotIn(path, product.REQUIRED_PATHS)
                self.files[path] = b'raise AssertionError("controller is not workflow code")\n'
            desired = dict(self.receipt, source_sha='c' * 40)
            self.api.qualify_receipt(desired)
            self.assertEqual(product.core.canonical(self.api.policy), original_policy)
            evidence = {'schema': 'tilo.coordinator-verification/v1', 'run_id': 12,
                        'desired': self.receipt, 'backend': 'canonical-final-artifact-v1',
                        'receipts': {'x86_64': 'd' * 64}, 'final_policy_sha256': 'f' * 64}
            intent = {'id': '12345678-1234-4234-8234-123456789abc', 'desired': desired,
                      'baseline': self.receipt, 'baseline_evidence': evidence}
            self.api.validate_baseline_policy(intent)
            inputs = product.dispatch_inputs(intent)
            self.assertEqual(inputs['baseline_sha'], self.receipt['source_sha'])
            self.assertEqual(inputs['baseline_run_id'], '12')
            self.assertEqual(inputs['baseline_evidence'], product.core.canonical(evidence))
        with self.subTest(phase='changed or unpinned shared contract is rejected'):
            self.doCleanups()
            self.setUp()
            path = 'tools/app_packaging_contract.py'
            self.assertIn(path, product.REQUIRED_PATHS)
            self.files[path] += b'\n# changed validation contract\n'
            with self.assertRaisesRegex(ValueError, 'differs from trusted policy: ' + path):
                self.api.qualify_receipt(self.receipt)
            del self.api.policy['trusted_files'][path]
            desired = dict(self.receipt, policy_sha256=self.api.policy_sha)
            with self.assertRaisesRegex(ValueError, 'incomplete trusted workflow policy'):
                self.api.qualify_receipt(desired)
        with self.subTest(phase='candidate path toolchain cannot match reviewed policy'):
            self.doCleanups()
            self.setUp()
            self.files['rust-toolchain.toml'] = b'[toolchain]\npath="candidate/toolchain"\n'
            with self.assertRaisesRegex(ValueError, 'differs from trusted policy: rust-toolchain.toml'):
                self.api.qualify_receipt(self.receipt)
        with self.subTest(phase='override presence and unobservable directory fail closed'):
            self.doCleanups()
            self.setUp()
            for directory, name in [('', 'rust-toolchain'), ('.cargo', 'config')]:
                with self.subTest(name=name):
                    self.listings[directory].append({'name': name, 'type': 'file'})
                    with self.assertRaisesRegex(ValueError, 'precedence override'):
                        self.api.qualify_receipt(self.receipt)
                    self.listings[directory].pop()
            self.listings[''] = {'message': 'unavailable'}
            with self.assertRaisesRegex(ValueError, 'listing unavailable'):
                self.api.qualify_receipt(self.receipt)
        with self.subTest(phase='path toolchain and config includes rejected even if reviewed'):
            self.doCleanups()
            self.setUp()
            with self.assertRaisesRegex(ValueError, 'never a candidate path'):
                product.validate_cargo_inputs(b'[toolchain]\npath="candidate/toolchain"', b'')
            for config in (b'include="candidate.toml"', b'[unstable]\nconfig-include=true'):
                with self.subTest(config=config), self.assertRaisesRegex(ValueError, 'includes'):
                    product.validate_cargo_inputs(self.files['rust-toolchain.toml'], config)
        with self.subTest(phase='local guard checks same inputs and legacy precedence'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / '.cargo').mkdir()
                for path in ('rust-toolchain.toml', '.cargo/config.toml'):
                    (root / path).write_bytes(self.files[path])
                product.validate_local_cargo_inputs(root)
                for override in ('rust-toolchain', '.cargo/config'):
                    (root / override).write_text('candidate')
                    with self.assertRaisesRegex(ValueError, 'precedence override'):
                        product.validate_local_cargo_inputs(root)
                    (root / override).unlink()
                (root / 'rust-toolchain.toml').write_text('[toolchain]\npath="candidate/toolchain"')
                with self.assertRaisesRegex(ValueError, 'never a candidate path'):
                    product.validate_local_cargo_inputs(root)


if __name__ == '__main__':
    unittest.main()
