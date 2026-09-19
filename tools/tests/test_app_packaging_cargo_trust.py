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
        self.receipt = {'source_sha': 'a' * 40, 'builder_image': 'code.sandbox.example/qos/tilo-builder@sha256:' + 'b' * 64,
                        'collection': 'default', 'architectures': ['x86_64'], 'policy_sha256': self.api.policy_sha}
        self.api.call = self.read

    def read(self, path):
        if path == 'repos/' + product.REPO:
            return {'id': 23, 'full_name': product.REPO}
        source = path.split('/contents/', 1)[1].split('?', 1)[0]
        if source in self.listings:
            return self.listings[source]
        return {'type': 'file', 'content': base64.b64encode(self.files[source]).decode()}

    def test_real_reviewed_policy_passes(self):
        self.api.qualify_receipt(self.receipt)

    def test_candidate_path_toolchain_cannot_match_reviewed_policy(self):
        self.files['rust-toolchain.toml'] = b'[toolchain]\npath="candidate/toolchain"\n'
        with self.assertRaisesRegex(ValueError, 'differs from trusted policy: rust-toolchain.toml'):
            self.api.qualify_receipt(self.receipt)

    def test_override_presence_and_unobservable_directory_fail_closed(self):
        for directory, name in [('', 'rust-toolchain'), ('.cargo', 'config')]:
            with self.subTest(name=name):
                self.listings[directory].append({'name': name, 'type': 'file'})
                with self.assertRaisesRegex(ValueError, 'precedence override'):
                    self.api.qualify_receipt(self.receipt)
                self.listings[directory].pop()
        self.listings[''] = {'message': 'unavailable'}
        with self.assertRaisesRegex(ValueError, 'listing unavailable'):
            self.api.qualify_receipt(self.receipt)

    def test_path_toolchain_and_config_includes_rejected_even_if_reviewed(self):
        with self.assertRaisesRegex(ValueError, 'never a candidate path'):
            product.validate_cargo_inputs(b'[toolchain]\npath="candidate/toolchain"', b'')
        for config in (b'include="candidate.toml"', b'[unstable]\nconfig-include=true'):
            with self.subTest(config=config), self.assertRaisesRegex(ValueError, 'includes'):
                product.validate_cargo_inputs(self.files['rust-toolchain.toml'], config)

    def test_local_guard_checks_same_inputs_and_legacy_precedence(self):
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
