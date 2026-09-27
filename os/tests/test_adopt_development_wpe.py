from contextlib import nullcontext
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import test_wpe_stage_artifact as fixtures

spec = importlib.util.spec_from_file_location('adoption',
    Path(__file__).resolve().parents[1] / 'dev/adopt-development-wpe.py')
adoption = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adoption)


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class AdoptionTests(unittest.TestCase):
    dependencies = fixtures.WpeArtifactTests.dependencies

    def setUp(self):
        fixtures.WpeArtifactTests.setUp(self)
        self.audit = dict(schema=1, purpose=adoption.AUDIT_PURPOSE, architecture='x86_64',
            source_image_id='sha256:' + 'a' * 64, buildroot_image_id='sha256:' + 'b' * 64,
            source_script_sha256=adoption.wpe.sha((self.repo / 'os/dev/build-wpe.sh').read_bytes()),
            archives=adoption.source_archives((self.repo / 'os/dev/build-wpe.sh').read_text(), self.assets),
            compatible_recipes=self.inputs['recipes'], original_build_epoch=None,
            original_source_revision=None, original_jobs=2,
            compatibility_review='explicit reviewer acceptance of retained recipe and current consumer')
        self.audit_digest = 'd' * 64
        self.adopted = dict(schema=1, purpose=adoption.PURPOSE, audit_sha256=self.audit_digest,
            audit=self.audit, projection_epoch='0', config_digest=self.identity,
            reference=adoption.repository('x86_64') + '@sha256:' + 'e' * 64,
            payload_sha256='f' * 64, production_signed=False)

    def validate(self, audit):
        return adoption.validate_audit(audit, self.repo, self.assets, 'x86_64')

    def test_adoption_compatibility(self):
        with self.subTest(phase="reviewed_audit_preserves_unknown_build_identity"):
            self.assertEqual(self.validate(self.audit), self.audit)
            adoption.validate_receipt(self.adopted, self.audit, self.audit_digest)
            for field, value in [('original_build_epoch', 0), ('original_source_revision', 'a' * 40),
                                 ('architecture', 'aarch64'), ('source_image_id', 'mutable:tag'),
                                 ('compatibility_review', 'automatic')]:
                with self.subTest(field=field), self.assertRaises(ValueError):
                    self.validate(self.audit | {field: value})
        restart_fixture(self)
        with self.subTest(phase="recipe_and_archive_changes_reject_compatibility"):
            path = self.repo / 'os/Containerfile.wpe'
            original = path.read_bytes()
            path.write_bytes(original + b'# changed consumer\n')
            with self.assertRaises(ValueError):
                self.validate(self.audit)
            path.write_bytes(original)
            (self.assets / 'fdo-1.tar.xz').write_bytes(b'changed')
            with self.assertRaises(ValueError):
                self.validate(self.audit)
        restart_fixture(self)
        with self.subTest(phase="independent_pins_and_duplicate_json_rejected"):
            path = self.repo / 'audit.json'
            path.write_text(json.dumps(self.audit))
            digest = adoption.wpe.sha(path.read_bytes())
            self.assertEqual(adoption.pinned_json(path, digest), self.audit)
            with self.assertRaises(ValueError):
                adoption.pinned_json(path, '0' * 64)
            path.write_bytes(b'{"schema":1,"schema":1}')
            with self.assertRaises(ValueError):
                adoption.pinned_json(path, adoption.wpe.sha(path.read_bytes()))
        restart_fixture(self)
        with self.subTest(phase="ancestry_and_mounted_script_must_match_before_projection"):
            (self.repo / 'build-wpe.sh').write_bytes((self.repo / 'os/dev/build-wpe.sh').read_bytes())
            records = [json.dumps([{'RootFS': {'Layers': ['base', 'engine']}}]).encode(),
                       json.dumps([{'RootFS': {'Layers': ['base']}}]).encode()]
            for mismatch in (None, 'ancestry', 'script'):
                current = records if mismatch != 'ancestry' else [records[0], b'[{"RootFS":{"Layers":["other"]}}]']
                audit = self.audit if mismatch != 'script' else self.audit | {'source_script_sha256': '0' * 64}
                with self.subTest(mismatch=mismatch), patch.object(adoption.wpe, 'inspect_local'), \
                        patch.object(adoption.wpe, 'command', side_effect=current), \
                        patch.object(adoption.wpe, 'mounted', return_value=nullcontext(self.repo)), \
                        patch.object(adoption, 'payload_digest', return_value='f' * 64):
                    if mismatch:
                        with self.assertRaises(ValueError):
                            adoption.verify_local(audit, self.assets)
                    else:
                        self.assertEqual(adoption.verify_local(audit, self.assets), 'f' * 64)

    def test_import_verification(self):
        with self.subTest(phase="bad_receipt_fails_before_any_registry_or_build_command"):
            for field, value in [('purpose', fixtures.wpe.PURPOSE), ('audit_sha256', '0' * 64),
                                 ('reference', 'registry.tilo.so/tilo/tilo:latest'),
                                 ('projection_epoch', '-1'), ('production_signed', True)]:
                with self.subTest(field=field), patch.object(adoption.wpe, 'command') as command:
                    with self.assertRaises(ValueError):
                        adoption.import_adopted(self.adopted | {field: value}, self.audit, self.audit_digest)
                    command.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="import_checks_config_and_entire_payload_without_build"):
            for mismatch in (None, 'config', 'payload'):
                with self.subTest(mismatch=mismatch), patch.object(adoption.wpe, 'native_host'), \
                        patch.object(adoption.wpe, 'registry_identity', return_value=(
                            'sha256:' + '0' * 64 if mismatch == 'config' else self.identity)), \
                        patch.object(adoption.wpe, 'command') as command, \
                        patch.object(adoption.wpe, 'inspect_local', return_value=self.identity), \
                        patch.object(adoption.wpe, 'mounted', return_value=nullcontext(self.repo)), \
                        patch.object(adoption, 'payload_digest', return_value=('0' * 64 if mismatch == 'payload' else 'f' * 64)):
                    if mismatch:
                        with self.assertRaises(ValueError):
                            adoption.import_adopted(self.adopted, self.audit, self.audit_digest)
                    else:
                        self.assertEqual(adoption.import_adopted(self.adopted, self.audit, self.audit_digest), self.identity)
                    self.assertTrue(all('pull' in call.args for call in command.call_args_list))
        restart_fixture(self)
        with self.subTest(phase="tree_digest_binds_extra_files_and_rejects_redirects"):
            payload = self.repo / 'wpe'
            payload.mkdir()
            (payload / 'file').write_bytes(b'first')
            with patch.object(adoption.wpe, 'payload_identity'):
                before = adoption.payload_digest(self.repo, 'x86_64')
                (payload / 'extra').write_bytes(b'extra')
                self.assertNotEqual(before, adoption.payload_digest(self.repo, 'x86_64'))
                (payload / 'outside').symlink_to(self.repo / 'os/Containerfile')
                with self.assertRaises(ValueError):
                    adoption.payload_digest(self.repo, 'x86_64')


if __name__ == '__main__':
    unittest.main()
