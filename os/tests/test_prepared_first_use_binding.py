#!/usr/bin/env python3
"""Boundary regressions; these do not establish authenticated installer provenance."""
import copy
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'usr/libexec/tilo-installer-prepared-setup-extract.py'
spec = importlib.util.spec_from_file_location('prepared_transport', SCRIPT)
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)


class FirstUseBinding(unittest.TestCase):
    def origin(self):
        return dict(schema=1, identity_public_key=[1] * 32, setup_binding='setup-actual-person',
                    sealed_bytes=48, ciphertext_sha256='a' * 64)

    def test_prepared_profile_binding_is_preserved_and_mutations_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ciphertext = b'TILOMIG1' + b'encrypted-transport-fixture' * 3
            sealed = root / transport.FILE
            sealed.write_bytes(ciphertext)
            sealed.chmod(0o600)
            expected = dict(schema=1, sealed_bytes=len(ciphertext),
                            ciphertext_sha256=hashlib.sha256(ciphertext).hexdigest())
            profile = dict(schema=1, purpose='server', device_label='Travel computer',
                           prepared_setup=expected)
            (root / 'profile.json').write_text(json.dumps(profile))
            transport.preflight_profile(root)
            self.assertEqual(json.loads((root / transport.RECEIPT_FILE).read_text()), expected)
            for mutation in (dict(purpose='other-os'), dict(password='never-log'),
                             dict(device_label='bad\nlabel'), dict(prepared_setup=None)):
                changed = dict(profile, **mutation)
                (root / 'profile.json').write_text(json.dumps(changed))
                with self.subTest(fields=list(mutation)), self.assertRaises(ValueError):
                    transport.preflight_profile(root)
                self.assertEqual(json.loads((root / transport.RECEIPT_FILE).read_text()), expected)
            (root / 'profile.json').write_text(json.dumps(profile))
            sealed.write_bytes(b'x' * len(ciphertext))
            with self.assertRaises(ValueError):
                transport.preflight_profile(root)
            self.assertEqual(json.loads((root / transport.RECEIPT_FILE).read_text()), expected)

    def test_legacy_profile_without_preparation_still_passes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'profile.json').write_text(json.dumps(dict(schema=1, hostname='legacy')))
            transport.preflight_profile(root)
            self.assertFalse((root / transport.RECEIPT_FILE).exists())

    def test_closed_intended_person_and_ciphertext_shape(self):
        original = self.origin()
        self.assertIs(transport.validate_first_use_origin(original), original)
        mutations = [dict(schema=True), dict(identity_public_key=[0] * 32),
                     dict(identity_public_key=[True] * 32), dict(setup_binding='a\n'),
                     dict(sealed_bytes=True), dict(sealed_bytes=47),
                     dict(ciphertext_sha256='A' * 64), dict(unrelated_permission=True)]
        for mutation in mutations:
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                changed = copy.deepcopy(original)
                changed.update(mutation)
                transport.validate_first_use_origin(changed)

    def test_directory_walk_refuses_alias_before_read(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'actual').mkdir()
            (root / 'alias').symlink_to(root / 'actual', target_is_directory=True)
            with self.assertRaises(OSError):
                transport.directory(root / 'alias')

    def test_binding_parent_write_access_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            os.chmod(temporary, 0o777)
            fd = transport.directory(temporary)
            try:
                with self.assertRaises(ValueError):
                    transport.protected_root_directory(fd)
            finally:
                os.close(fd)

    def test_home_bracket_observes_permission_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            before = os.stat(temporary)
            os.chmod(temporary, 0o755)
            after = os.stat(temporary)
            self.assertNotEqual(transport.binding_home_snapshot(before),
                                transport.binding_home_snapshot(after))

    def test_publication_replays_only_identical_binding(self):
        with tempfile.TemporaryDirectory() as temporary:
            fd = transport.directory(temporary)
            try:
                uid, gid = os.geteuid(), os.getegid()
                transport.publish_home_json(fd, 'origin.json', self.origin(), uid, gid)
                transport.publish_home_json(fd, 'origin.json', self.origin(), uid, gid)
                changed = self.origin()
                changed['identity_public_key'] = [2] * 32
                with self.assertRaises(ValueError):
                    transport.publish_home_json(fd, 'origin.json', changed, uid, gid)
            finally:
                os.close(fd)


if __name__ == '__main__':
    unittest.main()
