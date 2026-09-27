import copy
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
from test_native_reconstruction_approval import ApprovalTests, a, ROOT
from reconstruction_cache_approval import reproduce_extensions

HELPER = '2d09fac4a71547f0c0a9c30be757ffac8a642930'
OLD = '9d7a6b259c9b75b6c5031472bfed05b6461c1893'


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


class CacheApprovalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        ApprovalTests.setUpClass()
        cls.required = ApprovalTests.required
        cls.material = dict(ApprovalTests.material,
            restore_helper=(ROOT / 'os/dev/restore-thin-cache.py').read_bytes())
        cls.resume = types.ModuleType('cache_fixture_resume')
        exec(compile(cls.material['helper'], 'pinned-cache-helper', 'exec'), cls.resume.__dict__)

    def setUp(self):
        ApprovalTests.setUp(self)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        source = Path(self.temporary.name)
        (source / 'os/dev').mkdir(parents=True)
        (source / 'os/Containerfile').write_bytes(self.material['recipe'])
        (source / 'os/dev/build-image.sh').write_bytes(self.material['driver'])
        self.material = dict(self.material, source_root=source)
        prior = copy.deepcopy(self.receipt['reconstruction'])
        prior.update(run_id='21554', resume_helper_sha256='c'*64, producer_sha256='d'*64)
        prior_sha = a.sha(a.canonical(prior))
        record = {'size': 64, 'sha256': 'b'*64}
        value = dict(schema=1, purpose=self.resume.RECONSTRUCTED_PARTIAL, architecture='aarch64',
            source_revision=a.SOURCE, helper_revision=OLD, run_id=21554, run_number=8166, attempt=1,
            inventory={'fs': record}, capture_script_sha256='e'*64,
            reconstruction=prior, reconstruction_sha256=prior_sha,
            installable=False, complete_native_builder=False, build_success=False,
            resume_admitted=False, original_recipe_executed_verbatim=False)
        raw = a.canonical(value)
        checkpoint = dict(schema=1, purpose=self.resume.RECONSTRUCTED_PARTIAL,
            reference='registry.tilo.so/tilo/tilo-development-partial-native-aarch64@sha256:'+'a'*64,
            config_digest='sha256:'+'b'*64, metadata_sha256=a.sha(raw), source_revision=a.SOURCE,
            helper_revision=OLD, run_number=8166, run_id=21554, inventory={'fs': record},
            reconstruction_sha256=prior_sha, installable=False, complete_native_builder=False,
            build_success=False, resume_admitted=False, raw_manifest_verified=True, raw_config_verified=True,
            payload_hashes_verified=True, temporary_public_context_removed_on_exit=True)
        digest = a.sha(a.canonical(checkpoint))
        self.checkpoints = self.checkpoints + [{'receipt_sha256': digest, 'receipt': checkpoint}]
        partial = dict(schema=1, purpose='tilo-reconstructed-partial-admission', source_revision=a.SOURCE,
            helper_revision=OLD, run_id=21554, run_number=8166, attempt=1, reconstruction_sha256=prior_sha,
            resume_helper_sha256='c'*64, producer_sha256='d'*64,
            receipts={digest: {'inventory': {'fs': record}, 'capture_script_sha256': 'e'*64}})
        cache = dict(purpose='tilo-incomplete-thin-cache-admission', source_revision=a.SOURCE,
            producer_helper_revision=OLD, restore_helper_sha256=a.sha(self.material['restore_helper']),
            source_metadata_sha256='3'*64, cargo_revalidation_required=True, incomplete_cache_reviewed=True,
            checkpoint_reference='registry.tilo.so/tilo/tilo-development-partial-thin-cache-aarch64@sha256:'+'4'*64,
            checkpoint_config_digest='sha256:'+'5'*64,
            source_metadata_reference='registry.tilo.so/tilo/tilo-development-thin-cache-metadata-aarch64@sha256:'+'6'*64,
            source_metadata_config_digest='sha256:'+'7'*64, archive_sha256='8'*64, metadata_sha256='9'*64)
        cache_receipt = dict(purpose='tilo-development-incomplete-thin-cargo-cache', resume_admitted=False,
            complete=False, installable=False, reference=cache['checkpoint_reference'],
            config_digest=cache['checkpoint_config_digest'], archive_sha256='8'*64, metadata_sha256='9'*64,
            files=3, retained_bytes=192)
        sources = dict(purpose='incomplete-thin-cargo-source-metadata', source_revision=a.SOURCE,
            build_root='/build', tracked_files=[{'path':'Cargo.toml'}], rpm_nevras=['fixture'],
            rpm_nevras_sha256=a.sha(b'fixture\n'))
        self.extensions = dict(partial_admission=('1'*64, partial), partial_metadata={digest: raw},
            thin_admission=('2'*64, cache), thin_receipt=('0'*64, cache_receipt), source_metadata=('3'*64, sources))

    def completed(self):
        api = types.SimpleNamespace(require=a.require, sha=a.sha, canonical=a.canonical, checkpoint_union=a.checkpoint_union)
        union, extra, options = reproduce_extensions(api, self.checkpoints, self.required,
            self.resume, self.material, self.extensions)
        recipe, missing = self.resume.derive_recipe(self.material['recipe'].decode(), union, self.required, **options)
        receipt = copy.deepcopy(self.receipt)
        receipt['reconstruction'].update(extra, checkpoints=copy.deepcopy(self.checkpoints), reused_inventory=union,
            missing_targets=missing, derived_recipe_sha256=a.sha(recipe.encode()))
        receipt['reconstruction_sha256'] = a.sha(a.canonical(receipt['reconstruction']))
        return receipt

    def approve_cache(self, receipt):
        return a.approve(receipt, a.sha(a.canonical(receipt)), self.checkpoints, self.required, self.inputs,
            self.resume, self.material, HELPER, '21461', '1', extensions=self.extensions)

    def test_cache_approval_reproduction_and_tamper(self):
        with self.subTest(phase="cache_runtime_reproduced_from_independent_inputs"):
            receipt = self.completed()
            runtime = receipt['reconstruction']['thin_cache']['runtime_evidence']
            self.assertEqual(runtime['restored_files'], 3)
            self.assertEqual(runtime['restored_bytes'], 192)
            self.assertFalse(runtime['cargo_freshness_proven'])
            result = self.approve_cache(receipt)
            self.assertEqual(result['approved_reconstruction_sha256'], receipt['reconstruction_sha256'])
            self.assertFalse(result['complete_inventory_verified'])
        restart_fixture(self)
        with self.subTest(phase="remote_runtime_or_cache_admission_tamper_rejected"):
            for field, value in [('restored_files', 4), ('dependency_timestamps_restored', True),
                                 ('cargo_freshness_proven', True)]:
                receipt = self.completed()
                receipt['reconstruction']['thin_cache']['runtime_evidence'][field] = value
                receipt['reconstruction_sha256'] = a.sha(a.canonical(receipt['reconstruction']))
                with self.subTest(field=field), self.assertRaises(ValueError): self.approve_cache(receipt)
            receipt = self.completed()
            receipt['reconstruction']['thin_cache']['approval']['source_metadata_sha256'] = 'f'*64
            receipt['reconstruction_sha256'] = a.sha(a.canonical(receipt['reconstruction']))
            with self.assertRaises(ValueError): self.approve_cache(receipt)
        restart_fixture(self)
        with self.subTest(phase="partial_metadata_and_restore_helper_tamper_rejected"):
            receipt = self.completed()
            key = next(iter(self.extensions['partial_metadata']))
            self.extensions['partial_metadata'][key] += b' '
            with self.assertRaises(ValueError): self.approve_cache(receipt)
            self.extensions['partial_metadata'][key] = self.extensions['partial_metadata'][key][:-1]
            self.material['restore_helper'] += b'\n'
            with self.assertRaises(ValueError): self.approve_cache(receipt)
        restart_fixture(self)
        with self.subTest(phase="incompatible_host_encoding_has_explicit_remedy"):
            with patch.object(Path, 'read_text', return_value='incorrect decoding'):
                with self.assertRaisesRegex(ValueError, 'Python with -X utf8'):
                    self.completed()

if __name__ == '__main__': unittest.main()
