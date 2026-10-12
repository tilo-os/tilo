#!/usr/bin/env python3
"""Boundary regressions; these do not establish authenticated installer provenance."""
import copy
import importlib.util
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
