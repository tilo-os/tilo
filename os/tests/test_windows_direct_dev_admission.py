#!/usr/bin/env python3
"""Physical development admission must bind CI, tested media and installed GPT."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

SPEC = importlib.util.spec_from_file_location(
    'admission', Path(__file__).resolve().parents[1] / 'dev/windows-direct-dev-admission.py')
admission = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(admission)


def fixture(directory):
    image = 'registry.tilo.so/tilo/tilo-vm@sha256:' + 'b' * 64
    origin = 'registry.tilo.so/tilo/tilo-vm:main-ddddddd-w51-lenovo-83ed'
    ci = {'image': image, 'source_revision': 'd' * 40, 'invocation_id': '51',
          'update_origin': origin, 'image_kind': 'lenovo-83ed'}
    ids = {role: str(uuid.UUID(int=i)) for i, role in enumerate(('efi', 'boot', 'root'), 1)}
    guest = {'architecture': 'arm64', 'schema': 'tilo.vm-install-image-proof.v4',
             'booted_image_digest': 'sha256:' + 'c' * 64, 'installed_image_digest': 'sha256:' + 'b' * 64,
             'composefs_digest': 'a' * 128, 'installed_composefs_digest': 'a' * 128,
             'booted_composefs_digest': 'f' * 128, 'target_uki_sha256': 'e' * 64,
             'disk_guid': str(uuid.UUID(int=80)), 'partuuids': ids,
             'installed_update_origin': 'ostree-unverified-image:docker://' + origin,
             'selected_target': {'schema': 'tilo.installer-dev-target.v1', 'architecture': 'aarch64',
                                'hardware_profile': 'lenovo-83ed', 'target_ref': image,
                                'target_digest': 'sha256:' + 'b' * 64, 'update_origin': origin,
                                'provenance': {'source_revision': 'd' * 40, 'invocation_id': '51'}}}
    source = {'schema': 'tilo.windows-direct-vm-template-check.v1', 'disk_guid': guest['disk_guid'],
              'preserved_gpt_verified': True, 'preserved_edge_samples_verified': True,
              'final_partition_count': 3, 'final_partuuids': ids}
    board = {'schema': 'tilo.windows-vm-install-proof-check.v4', 'guest_proof': guest,
             'installer_code_runtime': {'schema':'tilo.installer-code-runtime.v1',
                 'channel':{'schema':'tilo.installer-code-runtime.v1', 'enabled':False, 'port':None, 'source':None}, 'history':[]},
             'source_check': source, 'installer_composefs': 'f' * 128,
             'first_boot_verified': False, 'installer_medium_sha256': '1' * 64}
    generic = json.loads(json.dumps(board))
    generic.update(schema='tilo.windows-vm-install-proof-check.v3', first_boot_verified=True,
                   installer_composefs='a' * 128)
    generic['guest_proof'].update(schema='tilo.vm-install-image-proof.v3',
        installed_image_digest='sha256:' + '9' * 64, copied_manifest_digest='sha256:' + '9' * 64,
        booted_composefs_digest='a' * 128)
    del generic['guest_proof']['selected_target']
    root = Path(directory)
    paths = [root / name for name in ('board.json', 'generic.json')]
    paths[0].write_text(json.dumps(board))
    paths[1].write_text(json.dumps(generic))
    return ci, board, generic, paths


class AdmissionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # The reviewed assembly revisions, in a repository of the fixture's own: the checkout
        # under test may have no .git (t's checks get none) or a shallow one without this
        # separately published assembly branch. The admission validator still refuses
        # unavailable or mismatched objects and bytes.
        cls.repository = tempfile.TemporaryDirectory()
        repository = Path(cls.repository.name)
        clean = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        subprocess.run(['git', 'init', '-q', str(repository)], env=clean, check=True)
        revisions = sorted({*admission.PROFILE_FIX_PAIR, *admission.SEALED_FIX_PAIR})
        subprocess.run(['git', '-C', str(repository), 'fetch', '-q', '--no-tags', '--depth=1',
                        'https://code.tilo.so/tilo/tilo.git', *revisions],
                       env=clean, capture_output=True, check=True, timeout=300)
        cls.repository_patch = patch.object(admission, 'REPOSITORY', repository)
        cls.repository_patch.start()

    @classmethod
    def tearDownClass(cls):
        cls.repository_patch.stop()
        cls.repository.cleanup()

    def test_execute_never_trusts_development_receipt_without_fresh_admission(self):
        copy = admission.module('dev_copy_entry', 'copy-windows-direct-final-install.py')
        with tempfile.TemporaryDirectory() as directory:
            ci, _, _, paths = fixture(directory)
            plan = {'windows_disk': {'unique_id': 'disk-id'}}
            args = SimpleNamespace(plan=paths[0], source_receipt=paths[0], confirm_disk_id='disk-id',
                command='execute', ci_child_manifest=paths[0], vm_proof_checked=paths[0],
                ci_image=ci['image'], verify_key=None, ci_index_manifest=None)
            elevated = SimpleNamespace(shell32=SimpleNamespace(IsUserAnAdmin=lambda: True))
            with patch.object(copy, 'load_plan', return_value=plan), \
                    patch.object(copy, 'load_json', return_value={'ci_image': ci['image']}), \
                    patch.object(copy.platform, 'system', return_value='Windows'), \
                    patch.object(copy.ctypes, 'windll', elevated, create=True), \
                    patch.object(copy.stage, 'snapshot') as snapshot:
                with self.assertRaisesRegex(ValueError, 'verification key'):
                    copy.run(args)
                snapshot.assert_not_called()
            args.development = True
            provenance = {**ci, 'key_sha256': None, 'generation': None, 'firmware_candidate': None,
                          'firmware_manifest_sha256': None, 'statement_sha256': None}
            receipt = {'ci_image': ci['image'], 'ci_index_digest': 'sha256:' + 'b' * 64,
                       'ci_child_digest': 'sha256:' + 'b' * 64, 'ci_verification_key_sha256': None,
                       'ci_source_revision': ci['source_revision'], 'ci_invocation_id': '51',
                       'ci_generation': None, 'ci_image_kind': 'lenovo-83ed',
                       'ci_firmware_candidate': None, 'ci_firmware_manifest_sha256': None,
                       'ci_slsa_statement_sha256': None, 'installed_uki_sha256': 'f' * 64,
                       'vm_proof_checked_sha256': admission.file_hash(paths[0]),
                       'development_admission': {'stale': True}}
            checked = {'guest_proof': {'target_uki_sha256': 'f' * 64}}
            fresh = {'development_admission': {'fresh': True}}
            with patch.object(copy, 'load_plan', return_value=plan), \
                    patch.object(copy, 'load_json', return_value=receipt), \
                    patch.object(copy.platform, 'system', return_value='Windows'), \
                    patch.object(copy.ctypes, 'windll', elevated, create=True), \
                    patch.object(copy.development, 'verify', return_value=(provenance, checked, fresh)) as live, \
                    patch.object(copy.stage, 'snapshot') as snapshot:
                with self.assertRaisesRegex(ValueError, 'paired CI media'):
                    copy.run(args)
                live.assert_called_once_with(args)
                snapshot.assert_not_called()

    def test_proofs_require_full_generic_boot_and_real_board_ci_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            ci, board, generic, paths = fixture(directory)
            admission.checked_proof(paths[0], ci)
            admission.checked_proof(paths[1])
            cases = [('first_boot_verified', False), ('schema', 'tilo.windows-vm-install-proof-check.v1')]
            for field, value in cases:
                changed = {**generic, field: value}
                paths[1].write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    admission.checked_proof(paths[1])
            for field, value in (('target_ref', ci['image'][:-1] + '0'),
                                 ('verifier_key_sha256', '4' * 64)):
                changed = json.loads(json.dumps(board))
                changed['guest_proof']['selected_target'][field] = value
                paths[0].write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    admission.checked_proof(paths[0], ci)
            changed = json.loads(json.dumps(board))
            changed['source_check']['final_partuuids']['root'] = str(uuid.UUID(int=99))
            paths[0].write_text(json.dumps(changed))
            with self.assertRaisesRegex(ValueError, 'source GPT'):
                admission.checked_proof(paths[0], ci)

    def test_profile_must_preserve_every_ci_byte(self):
        with tempfile.TemporaryDirectory() as directory:
            base, overlay = Path(directory) / 'base.qcow2', Path(directory) / 'profile.qcow2'
            base.write_bytes(b'CI base')
            overlay.write_bytes(b'private profile')
            handoff = Path(directory) / 'handoff'
            payload_file = handoff / 'etc/tilo/bringup-ssh.pub'
            payload_file.parent.mkdir(parents=True)
            payload_file.write_bytes(b'owner fixture')
            network_file = handoff / 'etc/NetworkManager/system-connections/tilo-setup.nmconnection'
            network_file.parent.mkdir(parents=True)
            network_file.write_bytes(b'network fixture')
            tail = admission.expected_tail(handoff)
            chain = [{'format': 'qcow2', 'virtual-size': 1048576 + len(tail)},
                     {'format': 'qcow2', 'virtual-size': 1048576, 'filename': str(base)}]
            hashes = (hashlib.sha256(base.read_bytes()).hexdigest(),
                      hashlib.sha256(overlay.read_bytes()).hexdigest())
            valid = [{'depth': 0, 'data': True, 'start': 1048576, 'length': len(tail)}]
            with patch.object(admission.profile, 'read_tail', return_value=tail), \
                    patch.object(admission, 'qemu_json', side_effect=[chain, valid]):
                admission.checked_profile(overlay, *hashes, handoff)
            with patch.object(admission.profile, 'read_tail', return_value=tail[:-1] + b'X'), \
                    patch.object(admission, 'qemu_json', return_value=chain):
                with self.assertRaisesRegex(ValueError, 'authorized Wi-Fi/SSH-only'):
                    admission.checked_profile(overlay, *hashes, handoff)
            with patch.object(admission, 'qemu_json', side_effect=[chain,
                    [{'depth': 0, 'data': True, 'start': 0, 'length': 65536}]]), \
                    patch.object(admission.profile, 'read_tail', return_value=tail):
                with self.assertRaisesRegex(ValueError, 'CI installer contents'):
                    admission.checked_profile(overlay, *hashes, handoff)
            with patch.object(admission, 'qemu_json', side_effect=[chain,
                    [{'depth': 0, 'present': True, 'zero': True, 'data': False,
                      'start': 0, 'length': 65536}]]), \
                    patch.object(admission.profile, 'read_tail', return_value=tail):
                with self.assertRaisesRegex(ValueError, 'CI installer contents'):
                    admission.checked_profile(overlay, *hashes, handoff)
            with patch.object(admission, 'qemu_json', return_value=chain):
                with self.assertRaisesRegex(ValueError, 'differs from CI'):
                    admission.checked_profile(overlay, '0' * 64, hashes[1], handoff)

    def test_live_medium_pair_binds_same_cohort_and_assembly(self):
        with tempfile.TemporaryDirectory() as directory:
            ci, _, _, paths = fixture(directory)
            generic_ci = {**ci, 'image_kind': 'generic', 'image': ci['image'].replace('b' * 64, '7' * 64)}
            common = {'base_image': generic_ci['image'], 'base_source': ci['source_revision']}
            board_medium = {'assembly_source': '6' * 40, 'source': {**common,
                            'target_image': ci['image'], 'target_ci_run_id': '51',
                            'target_update_origin': ci['update_origin']},
                            'tilo-dev-installer-aarch64.qcow2.sha256': '2' * 64}
            generic_medium = {**board_medium, 'source': common}
            args = SimpleNamespace(**{field: '51' for field in
                ('dev_ci_run_id', 'dev_generic_ci_run_id', 'dev_board_medium_run_id',
                 'dev_generic_medium_run_id')},
                ci_image=ci['image'], generic_ci_image=generic_ci['image'],
                ci_child_manifest=paths[0], generic_child_manifest=paths[1],
                dev_ci_config=paths[0], dev_generic_ci_config=paths[1],
                dev_board_medium=paths[0], dev_generic_medium=paths[1],
                dev_setup_handoff=Path(directory),
                vm_proof_checked=paths[0], generic_vm_proof_checked=paths[1], verify_key=None)
            for mismatched in (False, True):
                medium = {**generic_medium, 'assembly_source': '8' * 40} if mismatched else generic_medium
                with patch.object(admission.ci, 'verify_image', side_effect=[ci, generic_ci]) as live, \
                        patch.object(admission.media, 'verify_medium', side_effect=[board_medium, medium]), \
                        patch.object(admission, 'checked_profile', return_value={'verified': True}):
                    if mismatched:
                        with self.assertRaisesRegex(ValueError, 'assembly identities'):
                            admission.verify(args)
                    else:
                        provenance, checked, gate = admission.verify(args)
                        self.assertIsNone(provenance['key_sha256'])
                        self.assertEqual(checked['schema'], 'tilo.windows-vm-install-proof-check.v4')
                        self.assertEqual(gate['development_admission']['board_ci'], ci)
                    self.assertEqual(live.call_count, 2)

    def test_reviewed_profile_packaging_compatibility_uses_immutable_git_bytes(self):
        generic, board = admission.PROFILE_FIX_PAIR
        record = admission.assembly_compatibility(generic, board)
        self.assertEqual(record['kind'], 'runtime-profile-packaging-fix')
        self.assertEqual(record['runtime_file_sha256'], admission.PROFILE_FIX_BLOBS)
        for revisions in ((board, generic), (generic, 'e' * 40), ('e' * 40, board)):
            with self.subTest(revisions=revisions):
                with self.assertRaisesRegex(ValueError, 'unreviewed assembly identities'):
                    admission.assembly_compatibility(*revisions)
        with patch.object(admission, 'PROFILE_FIX_BLOBS', {
                path: {revision: '0' * 64 for revision in pins}
                for path, pins in admission.PROFILE_FIX_BLOBS.items()}):
            with self.assertRaisesRegex(ValueError, 'helper bytes differ'):
                admission.assembly_compatibility(generic, board)
        with patch.object(admission, 'PROFILE_FIX_CHANGED', {'os/Containerfile'}):
            with self.assertRaisesRegex(ValueError, 'input closure differs'):
                admission.assembly_compatibility(generic, board)

    def test_sealed_ci_uki_compatibility_binds_exact_helpers_and_all_diffs(self):
        generic, board = admission.SEALED_FIX_PAIR
        record = admission.assembly_compatibility(generic, board)
        self.assertEqual(record['kind'], 'sealed-ci-uki-projection-fix')
        self.assertEqual(record['runtime_file_sha256'], admission.SEALED_FIX_BLOBS)
        self.assertEqual(record['file_diff_sha256'], admission.SEALED_FIX_DIFFS)
        for revisions in ((board, generic), (generic, 'e' * 40)):
            with self.assertRaisesRegex(ValueError, 'unreviewed assembly identities'):
                admission.assembly_compatibility(*revisions)
        with patch.object(admission, 'SEALED_FIX_BLOBS', {
                path: {revision: '0' * 64 for revision in pins}
                for path, pins in admission.SEALED_FIX_BLOBS.items()}):
            with self.assertRaisesRegex(ValueError, 'helper bytes differ'):
                admission.assembly_compatibility(generic, board)
        with patch.object(admission, 'SEALED_FIX_DIFFS', {}):
            with self.assertRaisesRegex(ValueError, 'assembly diffs differ'):
                admission.assembly_compatibility(generic, board)
        with patch.object(admission, 'PROFILE_FIX_CHANGED', {'os/Containerfile'}):
            with self.assertRaisesRegex(ValueError, 'input closure differs'):
                admission.assembly_compatibility(generic, board)

    def test_sealed_diff_pins_do_not_depend_on_git_abbreviation(self):
        for abbreviation in ('7', '12', '40'):
            with self.subTest(abbreviation=abbreviation), patch.dict(os.environ, {
                    'GIT_CONFIG_COUNT': '1', 'GIT_CONFIG_KEY_0': 'core.abbrev',
                    'GIT_CONFIG_VALUE_0': abbreviation}):
                record = admission.assembly_compatibility(*admission.SEALED_FIX_PAIR)
                self.assertEqual(record['file_diff_sha256'], admission.SEALED_FIX_DIFFS)
                self.assertEqual(record['runtime_file_sha256'], admission.SEALED_FIX_BLOBS)

    def test_pagefile_is_only_a_shrink_gate(self):
        common = admission.module('common_gate', 'windows-direct-common.py')
        state = {'volume_status': 1, 'protection_status': 0, 'encryption_percentage': 100,
                 'lock_status': 0, 'system_pagefile_count': 1}
        with patch.object(common, 'ps_json', return_value=state):
            with self.assertRaisesRegex(ValueError, 'pagefile'):
                common.windows_install_gate()
            common.windows_install_gate(require_no_pagefile=False)
        with patch.object(common, 'ps_json', return_value={**state, 'protection_status': 1}):
            with self.assertRaisesRegex(ValueError, 'explicitly suspended'):
                common.windows_install_gate(require_no_pagefile=False)


if __name__ == '__main__':
    unittest.main()
