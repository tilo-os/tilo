"""Focused safety and byte-identity tests for physical-media handoff."""
import hashlib
import importlib.util
import json
import io
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import tarfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / 'dev' / 'write-verified-installer-media.py'
spec = importlib.util.spec_from_file_location('write_verified_installer_media', SCRIPT)
media = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = media
spec.loader.exec_module(media)


def disk(**changes):
    value = dict(number=4, model='Safe USB', unique_id='usb-serial-4',
                 size_bytes=128 * 1000**3, bus_type='USB', is_boot=False,
                 is_system=False, is_offline=False, is_read_only=False,
                 operational_status=['Online'], partition_style='GPT')
    value.update(changes)
    return value


class AcceptanceTests(unittest.TestCase):
    def valid(self, root):
        finalized = root/'finalized-verification.json'; finalized.write_text('{"passed":true}\n')
        (root/'raw-identity.json').write_text(json.dumps(
            {'size':media.ACCEPTED_MEDIA_BYTES,'sha256':'a'*64}))
        (root/'cycle-evidence.json').write_text(json.dumps({
            'verdict':'PASS\n','exit_status':'0\n','conversion_sha256':'a  one\nb  two\n'}))
        archive_path=root/'cycle-evidence.tar'
        required=['verdict.txt','exit-status.txt','boot-timing.txt',
                  'alongside-preservation-before-target-boot.txt',
                  '07-target-greeter.png','15-desktop-config.png']
        with tarfile.open(archive_path,'w') as archive:
            for name in required:
                body = b'PASS\n' if name=='verdict.txt' else b'0\n' if name=='exit-status.txt' else b'proof'
                info=tarfile.TarInfo('cycle/'+name);info.size=len(body);archive.addfile(info,io.BytesIO(body))
        value = {
            'schema': 1, 'purpose': media.ACCEPTANCE_PURPOSE,
            'cold_boot_verified': True, 'prepared_alongside_verified': True,
            'creator_and_verifier_released': True,
            'workers_retained_for_followup': True, 'helper_revision': 'b' * 40,
            'source': 'registry.example/tilo@sha256:' + 'c' * 64,
            'finalized_verification_sha256': media.file_sha256(finalized),
            'cycle_evidence_file':'cycle-evidence.json',
            'cycle_evidence_archive': {'required_files': required,
                'path':'cycle-evidence.tar','bytes':archive_path.stat().st_size,
                'sha256':media.file_sha256(archive_path)},
            'raw_media': {'size': media.ACCEPTED_MEDIA_BYTES, 'sha256': 'a' * 64},
        }
        path = root / 'acceptance.json'
        path.write_text(json.dumps(value))
        return path

    def test_acceptance_evidence(self):
        with self.subTest(phase="acceptance_requires_completed_independent_cycle"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory); path = self.valid(root)
                _, _, identity, _ = media.load_acceptance(path)
                self.assertEqual(identity, {'size': media.ACCEPTED_MEDIA_BYTES, 'sha256': 'a' * 64})
                value = json.loads(path.read_text())
                for field in ('cold_boot_verified', 'prepared_alongside_verified',
                              'creator_and_verifier_released'):
                    with self.subTest(field=field):
                        changed = dict(value); changed[field] = False
                        path.write_text(json.dumps(changed))
                        with self.assertRaisesRegex(ValueError, 'completed independent'):
                            media.load_acceptance(path)
                path = self.valid(root)
                with (root/'cycle-evidence.tar').open('ab') as stream: stream.write(b'tamper')
                with self.assertRaisesRegex(ValueError, 'archive differs'):
                    media.load_acceptance(path)
        with self.subTest(phase="hardware_acceptance_binds_profile_image_and_boot_contracts"):
            value = {
                'schema': 1, 'purpose': media.HARDWARE_ACCEPTANCE_PURPOSE,
                'artifact': {'bytes': media.ACCEPTED_MEDIA_BYTES, 'sha256': 'a' * 64,
                             'partition_table': 'GPT'},
                'hardware': {'architecture': 'aarch64', 'profile': 'lenovo-83ed',
                             'model': 'Lenovo Yoga Slim 7 14Q8X9',
                             'devicetree': 'qcom/x1e80100-lenovo-yoga-slim7x.dtb',
                             'devicetree_sha256': 'b' * 64},
                'os_source': {'immutable_reference': 'registry.example/tilo@sha256:' + 'c' * 64,
                              'signature': 'verified with development key',
                              'composefs_verity': 'd' * 128},
                'uki': {'base_sha256': '1' * 64, 'firmware_initramfs_sha256': '2' * 64,
                        'live_sha256': '3' * 64, 'source_uki_before_live_sha256': '4' * 64,
                        'target_sha256': '5' * 64,
                        'live_cmdline_contracts': ['root=tilo-auto', 'tilo.install',
                            'systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec'],
                        'target_cmdline_contracts': ['root=tilo-auto',
                            'systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec'],
                        'target_excludes': ['tilo.install']},
                'verification': {'physical_disks_touched': False, 'usb_touched': False,
                    'proven_base': 'clean local install and cold boot',
                    'esp': 'FAT repaired and clean fsck.fat required',
                    'esp_readback': 'UKIs matched pinned SHA-256',
                    'identity_scrub': 'machine identity and credentials removed'},
            }
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'receipt.json'; path.write_text(json.dumps(value))
                _, _, identity, _ = media.load_acceptance(path)
                self.assertEqual(identity['sha256'], 'a' * 64)
                for mutation in (
                        lambda document: document['hardware'].__setitem__('architecture', 'x86_64'),
                        lambda document: document['verification'].__setitem__('usb_touched', True),
                        lambda document: document['uki']['target_excludes'].clear()):
                    changed = json.loads(json.dumps(value)); mutation(changed)
                    path.write_text(json.dumps(changed))
                    with self.assertRaisesRegex(ValueError, 'accepted Lenovo'):
                        media.load_acceptance(path)


class AdmissionTests(unittest.TestCase):
    def test_disk_admission(self):
        with self.subTest(phase="only_identity_bearing_external_removable_disk_is_admitted"):
            self.assertEqual(media.admit_target(disk(), image_size=64 * 1024**3)['number'], 4)
            cases = {
                'bus': disk(bus_type='NVMe'), 'boot': disk(is_boot=True),
                'system': disk(is_system=True), 'readonly': disk(is_read_only=True),
                'offline': disk(is_offline=True), 'identity': disk(unique_id=''),
                'model': disk(model=''), 'small': disk(size_bytes=63 * 1024**3),
                'not-online': disk(operational_status=['No Media']),
            }
            for name, value in cases.items():
                with self.subTest(name=name), self.assertRaises(ValueError):
                    media.admit_target(value, image_size=64 * 1024**3)
            with self.assertRaisesRegex(ValueError, 'contains an input'):
                media.admit_target(disk(), image_size=1, excluded_disk_numbers={4})
        with self.subTest(phase="revalidation_binds_identity_capacity_model_and_safety_flags"):
            original = disk()
            self.assertTrue(media.same_device(original, disk()))
            self.assertTrue(media.same_device(original, disk(is_offline=True), allow_offline_change=True))
            for change in ({'unique_id':'other'}, {'size_bytes':1}, {'model':'Other USB'},
                           {'bus_type':'SD'}, {'is_boot':True}, {'is_system':True},
                           {'is_read_only':True}, {'number':5}):
                with self.subTest(change=change):
                    self.assertFalse(media.same_device(original, disk(**change), allow_offline_change=True))
        with self.subTest(phase="confirmation_is_derived_from_target_and_media"):
            identity = {'size': media.ACCEPTED_MEDIA_BYTES, 'sha256': 'a' * 64}
            phrase = media.confirmation_for(disk(), identity)
            self.assertIn('PhysicalDrive4', phrase)
            self.assertNotEqual(phrase, media.confirmation_for(disk(unique_id='other'), identity))
            self.assertNotEqual(phrase, media.confirmation_for(disk(), identity | {'sha256':'b'*64}))


class WriteTests(unittest.TestCase):
    def test_write_and_readback(self):
        with self.subTest(phase="sparse_raw_bytes_are_written_and_read_back_exactly"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory); source = root / 'installer.raw'; target = root / 'device.raw'
                with source.open('wb') as output:
                    output.write(b'TILO'); output.seek(2 * 1024 * 1024 - 4); output.write(b'BOOT')
                target.write_bytes(b'X' * source.stat().st_size)
                digest = hashlib.sha256(source.read_bytes()).hexdigest()
                identity = {'size': source.stat().st_size, 'sha256': digest}
                result = media.write_and_readback(source, target, identity)
                self.assertEqual(result['readback_sha256'], digest)
                self.assertEqual(target.read_bytes(), source.read_bytes())
        with self.subTest(phase="readback_mismatch_fails_without_success"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory); source = root/'source'; target = root/'target'
                source.write_bytes(b'accepted bytes'); target.write_bytes(b'0' * len(source.read_bytes()))
                wrong = {'size': source.stat().st_size, 'sha256': '0' * 64}
                with self.assertRaisesRegex(ValueError, 'source changed'):
                    media.write_and_readback(source, target, wrong)
        with self.subTest(phase="complete_state_machine_revalidates_handle_and_emits_new_evidence"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory); image = root/'image.raw'; image.write_bytes(b'TILO')
                target = root/'target.raw'; target.write_bytes(b'X'*64)
                acceptance = root/'acceptance.json'; acceptance.write_text('{}')
                plan_path = root/'plan.json'; plan_path.write_text('{}')
                evidence = root/'evidence.json'
                identity = {'size':4, 'sha256':hashlib.sha256(b'TILO').hexdigest()}
                selected = disk(size_bytes=64)
                confirmation = media.confirmation_for(selected, identity)
                document = {'schema':media.SCHEMA_PLAN, 'destructive':False,
                    'target':selected, 'raw_media':identity, 'required_confirmation':confirmation,
                    'image':str(image), 'acceptance':str(acceptance),
                    'acceptance_sha256':'e'*64, 'evidence':str(evidence)}
                state = {'offline':False}; transitions=[]
                def inventory():
                    row = {'Number':4, 'FriendlyName':'Safe USB', 'UniqueId':'usb-serial-4',
                        'Size':64, 'BusType':'USB', 'IsBoot':False, 'IsSystem':False,
                        'IsOffline':state['offline'], 'IsReadOnly':False,
                        'OperationalStatus':['Offline' if state['offline'] else 'Online'],
                        'PartitionStyle':'GPT'}
                    return [row]
                def offline(unique_id, number, value):
                    self.assertEqual(unique_id, 'usb-serial-4'); state['offline']=value
                    self.assertEqual(number,4)
                    transitions.append(value)
                props = {'size_bytes':64, 'media_removable':False, 'device_hotplug':True,
                         'device_number':4, 'bus_type':'USB', 'vendor':'Safe',
                         'product':'USB', 'serial':'serial-4'}
                args = SimpleNamespace(plan=plan_path, confirmation=confirmation)
                with patch.object(media,'load_plan',return_value=(plan_path,document,'f'*64)), \
                     patch.object(media,'load_acceptance',return_value=(acceptance,{},identity,'e'*64)), \
                     patch.object(media,'disk_for_local_path',return_value=0), \
                     patch.object(media,'disk_inventory',side_effect=inventory), \
                     patch.object(media,'require_administrator'), \
                     patch.object(media,'set_offline',side_effect=offline), \
                     patch.object(media,'raw_device',return_value=target), \
                     patch.object(media,'handle_identity',return_value=props), \
                     patch.object(media,'partition_inventory',return_value={'PartitionStyle':'GPT','Partitions':[]}):
                    media.write(args)
                self.assertEqual(transitions, [True, False])
                receipt = json.loads(evidence.read_text())
                self.assertEqual(receipt['status'], 'complete')
                self.assertEqual(receipt['plan_sha256'], 'f'*64)
                self.assertEqual(receipt['write']['readback_sha256'], identity['sha256'])
                self.assertTrue(receipt['terminal_metadata']['cleared'])
        with self.subTest(phase="failure_on_open_handle_still_returns_same_disk_online"):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory); image=root/'image'; image.write_bytes(b'TILO')
                target=root/'target'; target.write_bytes(b'X'*64)
                acceptance=root/'acceptance'; acceptance.write_text('{}')
                plan_path=root/'plan'; plan_path.write_text('{}'); evidence=root/'evidence'
                identity={'size':4,'sha256':hashlib.sha256(b'TILO').hexdigest()}
                selected=disk(size_bytes=64); confirmation=media.confirmation_for(selected,identity)
                document={'target':selected,'raw_media':identity,'required_confirmation':confirmation,
                    'image':str(image),'acceptance':str(acceptance),'acceptance_sha256':'e'*64,
                    'evidence':str(evidence)}
                state={'offline':False};transitions=[]
                def inventory():
                    return [{'Number':4,'FriendlyName':'Safe USB','UniqueId':'usb-serial-4','Size':64,
                        'BusType':'USB','IsBoot':False,'IsSystem':False,'IsOffline':state['offline'],
                        'IsReadOnly':False,'OperationalStatus':['Offline' if state['offline'] else 'Online'],
                        'PartitionStyle':'GPT'}]
                def offline(_,number,value):self.assertEqual(number,4);state['offline']=value;transitions.append(value)
                props={'size_bytes':64,'media_removable':True,'device_hotplug':True,
                       'device_number':4,'bus_type':'USB'}
                with patch.object(media,'load_plan',return_value=(plan_path,document,'f'*64)), \
                     patch.object(media,'load_acceptance',return_value=(acceptance,{},identity,'e'*64)), \
                     patch.object(media,'disk_for_local_path',return_value=0), \
                     patch.object(media,'disk_inventory',side_effect=inventory), \
                     patch.object(media,'require_administrator'), \
                     patch.object(media,'set_offline',side_effect=offline), \
                     patch.object(media,'raw_device',return_value=target), \
                     patch.object(media,'handle_identity',return_value=props), \
                     patch.object(media,'copy_and_readback',side_effect=ValueError('injected write failure')):
                    with self.assertRaisesRegex(ValueError,'injected write failure'):
                        media.write(SimpleNamespace(plan=plan_path,confirmation=confirmation))
                self.assertEqual(transitions,[True,False])
                self.assertEqual(json.loads(evidence.read_text())['status'],'in-progress')

    def test_write_preconditions(self):
        with self.subTest(phase="confirmation_is_checked_before_device_or_admin_actions"):
            document = {
                'schema': media.SCHEMA_PLAN, 'destructive': False,
                'target': {'number': 4}, 'raw_media': {'size': 1, 'sha256': 'a' * 64},
                'required_confirmation': 'ERASE exact',
            }
            with patch.object(media, 'load_plan', return_value=(Path('plan.json'), document, 'f'*64)), \
                 patch.object(media, 'require_administrator') as administrator, \
                 patch.object(media, 'set_offline') as offline:
                with self.assertRaisesRegex(ValueError, 'confirmation phrase'):
                    media.write(SimpleNamespace(plan=Path('plan.json'), confirmation='wrong'))
                administrator.assert_not_called(); offline.assert_not_called()
        with self.subTest(phase="existing_evidence_is_never_reused"):
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory)/'evidence.json'; path.write_text('{"status":"complete"}')
                with self.assertRaisesRegex(ValueError, 'already exists'):
                    media.reserve_evidence(path, Path('plan.json'), 'a'*64)
        with self.subTest(phase="disk_transition_requires_one_exact_stable_identity"):
            with patch.object(media, 'powershell') as powershell:
                media.set_offline("quoted'id", 4, True)
            script = powershell.call_args.args[0]
            self.assertIn('$matches.Count -ne 1', script)
            self.assertIn('[int]$matches[0].Number -ne 4', script)
            self.assertIn('$matches[0] | Set-Disk', script)
            self.assertNotIn("quoted'id", script)
        with self.subTest(phase="larger_target_terminal_metadata_is_cleared_and_verified"):
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory)/'target'; path.write_bytes(b'X' * (6*1024*1024))
                with path.open('r+b', buffering=0) as target:
                    result = media.clear_and_verify_terminal_metadata(target, 6*1024*1024, 1024*1024)
                self.assertTrue(result['cleared'])
                self.assertEqual(path.read_bytes()[-4*1024*1024:], b'\0'*(4*1024*1024))


if __name__ == '__main__':
    unittest.main()
