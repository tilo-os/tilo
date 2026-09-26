#!/usr/bin/python3
"""Live-installer internal mode (USB-free install, design spec §6).

Drives the real `tilo-staging-handoff` helper against fixture efivars, a
fixture staging ESP, fixture lsblk output and a fake efibootmgr. Nothing here
touches a real disk or firmware variable, so it runs unprivileged anywhere.
"""
import copy
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import uuid

REPO = Path(__file__).resolve().parents[2]
HELPER = REPO / 'os/usr/libexec/tilo-staging-handoff'
BACKEND = REPO / 'os/usr/libexec/tilo-install'
GUIDED = REPO / 'os/usr/libexec/tilo-guided-partition'
EFI_GLOBAL = '8be4df61-93ca-11d2-aa0d-00e098032b8c'
LOADER_VENDOR = '4a67b082-0a4c-41cf-b6c7-440b29bb8c4f'
ESP_TYPE = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
LINUX_TYPE = '0fc63daf-8483-4772-8e79-3d69d8477de4'
STAGING_ROOT_TYPE = 'd4c4daf6-f72c-43ee-b8e3-5ba32550311d'
MSFT_TYPE = 'ebd0a0a2-b9e5-4433-87c0-68b6b72699c7'
GIB = 1024 ** 3

DISK = '/dev/nvme0n1'
DISK_GUID = '11111111-2222-4333-8444-555555555555'
ESP_UUID = 'aaaaaaaa-0000-4000-8000-000000000001'
ROOT_UUID = 'bbbbbbbb-0000-4000-8000-000000000002'
TARGET_ESP_UUID = 'cccccccc-0000-4000-8000-000000000003'
DISK_SECTORS = 256 * GIB // 512
# [ Windows ESP | C: | 1 MiB gap | target 64 GiB | staging ESP 1 GiB | staging root 40 GiB | free ]
WIN_ESP = (2048, 2048 + 204800 - 1)
WINDOWS = (206848, 100 * GIB // 512 - 1)
TARGET = (100 * GIB // 512 + 2048, 100 * GIB // 512 + 2048 + 64 * GIB // 512 - 1)
STAGING_ESP = (TARGET[1] + 1, TARGET[1] + GIB // 512)
STAGING_ROOT = (STAGING_ESP[1] + 1, STAGING_ESP[1] + 40 * GIB // 512)


def handoff_document():
    return {
        'schema': 1,
        'operation_id': '0f0e0d0c-0b0a-4908-8706-050403020100',
        'host_os': 'windows',
        'disk_guid': DISK_GUID.upper(),
        'disk_size_bytes': DISK_SECTORS * 512,
        'staging_esp': {'partuuid': ESP_UUID, 'first_lba': STAGING_ESP[0], 'last_lba': STAGING_ESP[1]},
        'staging_root': {'partuuid': ROOT_UUID, 'first_lba': STAGING_ROOT[0], 'last_lba': STAGING_ROOT[1]},
        'target_extent': {'first_lba': TARGET[0], 'last_lba': TARGET[1]},
        'owned_boot_entry': '0104',
        'original_boot_order': ['0001', '0000'],
        'profile': {'schema': 1, 'hostname': 'studio', 'username': 'quinten', 'locale': 'en_US.UTF-8',
                    'keyboard': 'de', 'timezone': 'Europe/Berlin'},
    }


def partition(name, part_uuid, part_type, extent, mountpoints=()):
    return {'name': name, 'type': 'part', 'start': extent[0], 'size': (extent[1] - extent[0] + 1) * 512,
            'partuuid': part_uuid, 'parttype': part_type, 'pttype': 'gpt', 'ptuuid': DISK_GUID,
            'log-sec': 512, 'mountpoints': list(mountpoints) or [None]}


def lsblk_document():
    return {'blockdevices': [{
        'name': DISK, 'type': 'disk', 'start': None, 'size': DISK_SECTORS * 512, 'partuuid': None,
        'parttype': None, 'pttype': 'gpt', 'ptuuid': DISK_GUID, 'log-sec': 512, 'mountpoints': [None],
        'children': [
            partition(DISK + 'p1', '00000000-0000-4000-8000-00000000000a', ESP_TYPE, WIN_ESP),
            partition(DISK + 'p2', '00000000-0000-4000-8000-00000000000b', MSFT_TYPE, WINDOWS),
            partition(DISK + 'p3', ESP_UUID, ESP_TYPE, STAGING_ESP),
            partition(DISK + 'p4', ROOT_UUID, STAGING_ROOT_TYPE, STAGING_ROOT, ['/sysroot', '/var', '/etc']),
        ]}]}


def load_option(part_uuid, start, size, number=3, path='\\EFI\\BOOT\\BOOTX64.EFI', description='Tilo installer'):
    hd = struct.pack('<BBHIQQ', 4, 1, 42, number, start, size) + uuid.UUID(part_uuid).bytes_le + bytes([2, 2])
    encoded = (path + '\0').encode('utf-16-le')
    file_node = struct.pack('<BBH', 4, 4, 4 + len(encoded)) + encoded
    device_path = hd + file_node + bytes([0x7F, 0xFF, 4, 0])
    body = struct.pack('<IH', 1, len(device_path)) + (description + '\0').encode('utf-16-le') + device_path
    return b'\x07\x00\x00\x00' + body


def ids(values):
    return b'\x07\x00\x00\x00' + b''.join(struct.pack('<H', int(value, 16)) for value in values)


FAKE_EFIBOOTMGR = r'''#!/usr/bin/python3
import os, struct, sys
efivars = os.environ['TILO_STAGING_EFIVARS']
guid = '8be4df61-93ca-11d2-aa0d-00e098032b8c'
with open(os.environ['FAKE_EFIBOOT_LOG'], 'a') as log:
    log.write(' '.join(sys.argv[1:]) + '\n')
args = [arg for arg in sys.argv[1:] if arg != '-q']
def ids(values):
    return b'\x07\x00\x00\x00' + b''.join(struct.pack('<H', int(v, 16)) for v in values)
if os.environ.get('FAKE_EFIBOOT_FAIL'):
    sys.exit(1)
if args[:1] == ['-b'] and args[2:] == ['-B']:
    os.unlink(os.path.join(efivars, 'Boot%s-%s' % (args[1], guid)))
elif args[:1] == ['-n']:
    with open(os.path.join(efivars, 'BootNext-' + guid), 'wb') as handle:
        handle.write(ids([args[1]]))
elif args[:1] == ['-c']:
    created = os.environ['FAKE_EFIBOOT_CREATE']
    with open(os.path.join(efivars, 'Boot0105-' + guid), 'wb') as handle:
        handle.write(bytes.fromhex(created))
    order_path = os.path.join(efivars, 'BootOrder-' + guid)
    with open(order_path, 'rb') as handle:
        order = handle.read()[4:]
    with open(order_path, 'wb') as handle:
        handle.write(b'\x07\x00\x00\x00' + struct.pack('<H', 0x105) + order)
elif args[:1] == ['-o']:
    with open(os.path.join(efivars, 'BootOrder-' + guid), 'wb') as handle:
        handle.write(ids(args[1].split(',')))
else:
    sys.exit(2)
'''


class Fixture:
    def __init__(self, root):
        self.root = Path(root)
        self.efivars = self.root / 'efivars'
        self.esp = self.root / 'esp'
        self.efivars.mkdir()
        (self.esp / 'EFI/tilo-staging').mkdir(parents=True)
        self.copy = self.root / 'handoff.json'
        self.lsblk = self.root / 'lsblk.json'
        self.log = self.root / 'efiboot.log'
        self.log.write_text('')
        self.efibootmgr = self.root / 'efibootmgr'
        self.efibootmgr.write_text(FAKE_EFIBOOTMGR.replace('#!/usr/bin/python3', '#!' + sys.executable))
        self.efibootmgr.chmod(0o755)
        self.set_handoff(handoff_document())
        self.set_lsblk(lsblk_document())
        self.set_loader(ESP_UUID.upper())
        self.write_var('BootOrder', ids(['0001', '0000']))
        self.write_var('Boot0104', load_option(ESP_UUID, STAGING_ESP[0], STAGING_ESP[1] - STAGING_ESP[0] + 1))
        self.write_var('Boot0001', load_option('00000000-0000-4000-8000-00000000000a', WIN_ESP[0], 204800, 1,
                                               '\\EFI\\Microsoft\\Boot\\bootmgfw.efi', 'Windows Boot Manager'))

    def write_var(self, name, data, vendor=EFI_GLOBAL):
        (self.efivars / ('%s-%s' % (name, vendor))).write_bytes(data)

    def var(self, name):
        path = self.efivars / ('%s-%s' % (name, EFI_GLOBAL))
        return path.read_bytes() if path.exists() else None

    def set_loader(self, value):
        self.write_var('LoaderDevicePartUUID', b'\x06\x00\x00\x00' + (value + '\0').encode('utf-16-le'),
                       LOADER_VENDOR)

    def set_handoff(self, document, esp_only=False):
        data = json.dumps(document).encode()
        (self.esp / 'EFI/tilo-staging/handoff.json').write_bytes(data)
        if not esp_only:
            self.copy.write_bytes(data)

    def set_lsblk(self, document):
        self.lsblk.write_text(json.dumps(document))

    def env(self):
        return dict(os.environ, TILO_STAGING_EFIVARS=str(self.efivars), TILO_STAGING_ESP_DIR=str(self.esp),
                    TILO_STAGING_LSBLK_JSON=str(self.lsblk), TILO_STAGING_EFIBOOTMGR=str(self.efibootmgr),
                    FAKE_EFIBOOT_LOG=str(self.log))

    def run(self, *argv, env=None):
        return subprocess.run([sys.executable, str(HELPER)] + list(argv), env=env or self.env(),
                              capture_output=True, text=True)

    def check(self, mode='guided', **overrides):
        values = {
            'free-start-bytes': TARGET[0] * 512,
            'free-size-bytes': (TARGET[1] - TARGET[0] + 1) * 512,
            'efi-start-bytes': TARGET[0] * 512,
            'efi-size-bytes': 2 * GIB,
            'boot-start-bytes': TARGET[0] * 512 + 2 * GIB,
            'boot-size-bytes': GIB,
            'root-start-bytes': TARGET[0] * 512 + 3 * GIB,
            'root-size-bytes': (TARGET[1] - TARGET[0] + 1) * 512 - 3 * GIB,
            'sector-size': 512,
        }
        values.update(overrides)
        argv = ['check-install', '--handoff', str(self.copy), '--disk', DISK, '--mode', mode]
        for key, value in values.items():
            argv += ['--' + key, str(value)]
        return self.run(*argv)


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


class InternalModeGuardTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.fixture = Fixture(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def assertRefused(self, result, fragment):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(fragment, result.stderr)
        self.assertEqual(result.stdout, '', 'a refusal must not report facts')

    def test_handoff_record_parsing(self):
        with self.subTest(phase="exact_staged_plan_is_accepted_and_reports_its_facts"):
            result = self.fixture.check()
            self.assertEqual(result.returncode, 0, result.stderr)
            facts = dict(line.split('=', 1) for line in result.stdout.splitlines())
            self.assertEqual(facts['INTERNAL_OWNED_BOOT_ENTRY'], '0104')
            self.assertEqual(facts['INTERNAL_DISK_GUID'], DISK_GUID)
            self.assertEqual(facts['INTERNAL_STAGING_ROOT_PARTUUID'], ROOT_UUID)
            self.assertEqual(facts['INTERNAL_HOST_OS'], 'windows')
        restart_fixture(self)
        # Catches the guard depending on udev: in a container or early boot lsblk
        # reports no GPT identities, and the facts must then come from the GPT.
        with self.subTest(phase="identities_missing_from_lsblk_are_read_from_the_gpt"):
            result = self.run_check(self.udevless(lambda table: None))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('INTERNAL_DISK_GUID=' + DISK_GUID, result.stdout)
        restart_fixture(self)
        with self.subTest(phase="a_gpt_that_disagrees_with_lsblk_is_refused"):
            def swap(table):
                table['id'] = '99999999-2222-4333-8444-555555555555'
            self.assertRefused(self.run_check(self.udevless(swap)), 'disk GUID differs')
        restart_fixture(self)
        with self.subTest(phase="strict_parser_rejects_unknown_duplicate_and_malformed_fields"):
            cases = []
            extra = handoff_document()
            extra['target_disk'] = DISK
            cases.append(json.dumps(extra))
            cases.append(json.dumps(handoff_document())[:-1] + ', "schema": 1}')
            bad_entry = handoff_document()
            bad_entry['owned_boot_entry'] = '0104; reboot'
            cases.append(json.dumps(bad_entry))
            profile = handoff_document()
            profile['profile']['password'] = 'secret'
            cases.append(json.dumps(profile))
            in_order = handoff_document()
            in_order['original_boot_order'].append('0104')
            cases.append(json.dumps(in_order))
            boolean = handoff_document()
            boolean['disk_size_bytes'] = True
            cases.append(json.dumps(boolean))
            cases.append(json.dumps(handoff_document()) + ' ' * (64 * 1024))
            for text in cases:
                self.fixture.copy.write_text(text)
                (self.fixture.esp / 'EFI/tilo-staging/handoff.json').write_text(text)
                self.assertEqual(self.fixture.check().returncode, 1, text[:200])

    def udevless(self, sfdisk_table):
        """lsblk without a udev database: every GPT identity field is empty,
        and a fake sfdisk on PATH reports the GPT itself."""
        document = lsblk_document()
        disk = document['blockdevices'][0]
        table = {'label': 'gpt', 'id': disk['ptuuid'], 'partitions': []}
        disk['pttype'] = disk['ptuuid'] = None
        for child in disk['children']:
            table['partitions'].append({'node': child['name'], 'uuid': child['partuuid'].upper(),
                                        'type': child['parttype'].upper()})
            child['partuuid'] = child['parttype'] = child['pttype'] = child['ptuuid'] = None
        sfdisk_table(table)
        self.fixture.set_lsblk(document)
        tools = self.fixture.root / 'bin'
        tools.mkdir(exist_ok=True)
        fake = tools / 'sfdisk'
        fake.write_text('#!%s\nimport json\nprint(json.dumps(%r))\n'
                        % (sys.executable, {'partitiontable': table}))
        fake.chmod(0o755)
        env = self.fixture.env()
        env['PATH'] = str(tools) + os.pathsep + env['PATH']
        return env

    def run_check(self, env):
        saved = self.fixture.env
        self.fixture.env = lambda: env
        try:
            return self.fixture.check()
        finally:
            self.fixture.env = saved

    def test_refuses_disk_or_extent_mismatch(self):
        with self.subTest(phase="whole_disk_and_prepared_modes_are_refused_on_the_live_disk"):
            for mode in ('erase', 'prepared'):
                self.assertRefused(self.fixture.check(mode=mode), 'accepts only guided alongside')
        restart_fixture(self)
        with self.subTest(phase="mismatched_disk_guid_or_size_is_refused"):
            document = lsblk_document()
            document['blockdevices'][0]['ptuuid'] = '99999999-2222-4333-8444-555555555555'
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'disk GUID differs')
            document = lsblk_document()
            document['blockdevices'][0]['size'] += 512
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'disk size differs')
        restart_fixture(self)
        with self.subTest(phase="extent_that_is_no_longer_unallocated_is_refused"):
            document = lsblk_document()
            document['blockdevices'][0]['children'].append(partition(
                DISK + 'p5', '00000000-0000-4000-8000-00000000000c', LINUX_TYPE, (TARGET[0] + 2048, TARGET[0] + 4095)))
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'no longer unallocated')
        restart_fixture(self)
        with self.subTest(phase="handoff_whose_extents_overlap_is_refused"):
            document = handoff_document()
            document['target_extent']['last_lba'] = STAGING_ESP[0]
            self.fixture.set_handoff(document)
            self.assertRefused(self.fixture.check(), 'overlap')
        restart_fixture(self)
        with self.subTest(phase="guided_plan_must_equal_the_staged_extent_and_end_at_staging"):
            self.assertRefused(self.fixture.check(**{'free-start-bytes': TARGET[0] * 512 + 1024 * 1024}),
                               'differs from the staged target extent')
            shorter = (TARGET[1] - TARGET[0] + 1) * 512 - 2 * GIB - 1024 * 1024
            self.assertRefused(self.fixture.check(**{'root-size-bytes': shorter}),
                               'end exactly where the staged installer begins')

    def test_refuses_esp_or_mount_mismatch(self):
        with self.subTest(phase="handoff_naming_a_different_esp_than_the_booted_one_is_refused"):
            self.fixture.set_loader('dddddddd-0000-4000-8000-000000000004')
            self.assertRefused(self.fixture.check(), 'different ESP')
        restart_fixture(self)
        with self.subTest(phase="ui_copy_must_match_the_booted_staging_esp"):
            document = handoff_document()
            document['target_extent']['first_lba'] += 2048
            self.fixture.set_handoff(document, esp_only=True)
            self.assertRefused(self.fixture.check(), 'differs from the staging ESP')
        restart_fixture(self)
        with self.subTest(phase="live_root_elsewhere_or_foreign_mounts_are_refused"):
            document = lsblk_document()
            document['blockdevices'][0]['children'][3]['mountpoints'] = [None]
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'not the staging root')
            document = lsblk_document()
            document['blockdevices'][0]['children'][1]['mountpoints'] = ['/mnt/windows']
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'only the staged installer may be in use')
        restart_fixture(self)
        with self.subTest(phase="staging_partition_with_changed_identity_is_refused"):
            document = lsblk_document()
            document['blockdevices'][0]['children'][2]['parttype'] = LINUX_TYPE
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'staging ESP has an unexpected partition type')
            document = lsblk_document()
            document['blockdevices'][0]['children'][3]['start'] += 2048
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'staging root extent differs')
            document = lsblk_document()
            document['blockdevices'][0]['children'][3]['parttype'] = LINUX_TYPE
            self.fixture.set_lsblk(document)
            self.assertRefused(self.fixture.check(), 'staging root has an unexpected partition type')


class DetectTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.fixture = Fixture(self.temporary.name)
        self.out = Path(self.temporary.name) / 'run/handoff.json'
        self.error = Path(self.temporary.name) / 'run/handoff.error'

    def tearDown(self):
        self.temporary.cleanup()

    def detect(self):
        result = self.fixture.run('detect', '--out', str(self.out), '--error', str(self.error))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_detect_publishes_only_matching_handoff(self):
        with self.subTest(phase="publishes_a_normalized_copy_for_the_unprivileged_ui"):
            self.detect()
            published = json.loads(self.out.read_text())
            self.assertEqual(published['disk_guid'], DISK_GUID)
            self.assertEqual(published['staging_esp']['partuuid'], ESP_UUID)
            self.assertEqual(self.out.stat().st_mode & 0o777, 0o644)
            self.assertFalse(self.error.exists())
        restart_fixture(self)
        with self.subTest(phase="usb_media_without_a_handoff_publish_nothing"):
            (self.fixture.esp / 'EFI/tilo-staging/handoff.json').unlink()
            self.detect()
            self.assertFalse(self.out.exists())
            self.assertFalse(self.error.exists())
        restart_fixture(self)
        with self.subTest(phase="handoff_naming_another_esp_is_reported_not_published"):
            self.fixture.set_loader('dddddddd-0000-4000-8000-000000000004')
            self.detect()
            self.assertFalse(self.out.exists())
            self.assertIn('different ESP', self.error.read_text())


class FirmwareFinalizeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.fixture = Fixture(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def finalize(self, *extra, env=None):
        return self.fixture.run('firmware-finalize', '--handoff', str(self.fixture.copy),
                                '--target-esp-partuuid', TARGET_ESP_UUID, *extra, env=env)

    def add_target_entry(self, entry_id='0006'):
        self.fixture.write_var('Boot' + entry_id, load_option(TARGET_ESP_UUID, TARGET[0], 4194304, 5,
                                                             '\\EFI\\systemd\\systemd-bootx64.efi', 'Linux Boot Manager'))

    def test_owned_entry_deleted_only_when_matching(self):
        with self.subTest(phase="matching_owned_entry_is_deleted_and_bootnext_targets_tilo"):
            self.add_target_entry()
            before = self.fixture.var('BootOrder')
            result = self.finalize()
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertIsNone(self.fixture.var('Boot0104'))
            self.assertEqual(self.fixture.var('BootNext'), ids(['0006']))
            self.assertEqual(self.fixture.var('BootOrder'), before, 'BootOrder must stay unchanged')
            self.assertIsNotNone(self.fixture.var('Boot0001'))
        restart_fixture(self)
        with self.subTest(phase="owned_entry_that_no_longer_points_at_staging_is_left_alone"):
            self.add_target_entry()
            foreign = load_option('00000000-0000-4000-8000-00000000000a', WIN_ESP[0], 204800, 1)
            self.fixture.write_var('Boot0104', foreign)
            result = self.finalize()
            self.assertEqual(result.returncode, 3, result.stderr + result.stdout)
            self.assertEqual(self.fixture.var('Boot0104'), foreign)
            self.assertNotIn('-B', self.fixture.log.read_text())
            # Tilo is still what the next restart starts.
            self.assertEqual(self.fixture.var('BootNext'), ids(['0006']))
        restart_fixture(self)
        with self.subTest(phase="owned_entry_with_a_different_extent_is_left_alone"):
            self.add_target_entry()
            moved = load_option(ESP_UUID, STAGING_ESP[0] + 2048, STAGING_ESP[1] - STAGING_ESP[0] + 1)
            self.fixture.write_var('Boot0104', moved)
            self.assertEqual(self.finalize().returncode, 3)
            self.assertEqual(self.fixture.var('Boot0104'), moved)

    def test_target_entry_creation_and_bootnext(self):
        with self.subTest(phase="missing_target_entry_is_created_without_changing_bootorder"):
            created = load_option(TARGET_ESP_UUID, TARGET[0], 4194304, 5, '\\EFI\\systemd\\systemd-bootx64.efi', 'Tilo')
            env = dict(self.fixture.env(), FAKE_EFIBOOT_CREATE=created.hex())
            before = self.fixture.var('BootOrder')
            result = self.finalize('--target-disk', DISK, '--target-esp-partition-number', '5',
                                   '--loader', '\\EFI\\systemd\\systemd-bootx64.efi', env=env)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertEqual(self.fixture.var('BootNext'), ids(['0105']))
            self.assertEqual(self.fixture.var('BootOrder'), before)
            self.assertIn('-o 0001,0000', self.fixture.log.read_text())
        restart_fixture(self)
        with self.subTest(phase="ambiguous_or_absent_target_entry_does_not_set_bootnext"):
            self.assertEqual(self.finalize().returncode, 1)
            self.assertIsNone(self.fixture.var('BootNext'))
            self.add_target_entry('0006')
            self.add_target_entry('0007')
            result = self.finalize()
            self.assertEqual(result.returncode, 1)
            self.assertIn('exactly one firmware entry', result.stderr)
            self.assertIsNone(self.fixture.var('BootNext'))


class ReclaimRecordTests(unittest.TestCase):
    def test_record_binds_both_staging_identities_and_the_adjacent_target_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(temporary)
            var = Path(temporary) / 'state/os/default/var'
            var.mkdir(parents=True)
            root_start = TARGET[0] * 512 + 3 * GIB
            root_size = (TARGET[1] + 1) * 512 - root_start
            argv = ['write-reclaim-record', '--handoff', str(fixture.copy), '--target-var', str(var),
                    '--target-root-partuuid', 'EEEEEEEE-0000-4000-8000-000000000005',
                    '--target-esp-partuuid', TARGET_ESP_UUID, '--target-root-start-bytes', str(root_start),
                    '--sector-size', '512']
            result = fixture.run(*argv, '--target-root-size-bytes', str(root_size))
            self.assertEqual(result.returncode, 0, result.stderr)
            record_path = var / 'lib/tilo/staging-reclaim.json'
            record = json.loads(record_path.read_text())
            self.assertEqual(record_path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(record['disk_guid'], DISK_GUID)
            self.assertEqual(record['staging_esp']['partuuid'], ESP_UUID)
            self.assertEqual(record['staging_root']['last_lba'], STAGING_ROOT[1])
            self.assertEqual(record['target_root']['partuuid'], 'eeeeeeee-0000-4000-8000-000000000005')
            self.assertEqual(record['target_root']['last_lba'] + 1, STAGING_ESP[0])
            short = fixture.run(*argv, '--target-root-size-bytes', str(root_size - 1024 * 1024))
            self.assertEqual(short.returncode, 1, 'a root that does not end at staging is not reclaimable')


class BackendWiringTests(unittest.TestCase):
    """The backend must route every live-disk install through the helper."""

    source = BACKEND.read_text()

    def test_backend_wiring_order(self):
        with self.subTest(phase="live_disk_refusal_remains_for_usb_and_is_replaced_only_by_verification"):
            self.assertIn("is the live medium's own disk — refusing to install onto it", self.source)
            guard = self.source.split('INTERNAL_MODE=0\nif [ "$TARGET_MAJ_MIN" = "$LIVE_MAJ_MIN" ]; then', 1)[1]
            guard = guard.split('\nfi\n', 1)[0]
            self.assertLess(guard.index('[ -n "$INTERNAL_HANDOFF" ]'), guard.index('check-install'))
            self.assertIn('--mode guided', guard)
            self.assertIn('INTERNAL_MODE=1', guard)
            self.assertIn('requires the disk that holds the staged installer', self.source)
        with self.subTest(phase="internal_handoff_rejects_erase_and_prepared_before_any_disk_work"):
            check = self.source.index('--internal-handoff accepts only --guided-alongside')
            self.assertLess(check, self.source.index('validate_disk_identity\n'))
        with self.subTest(phase="firmware_is_finalized_only_after_the_encrypted_root_is_closed"):
            finish = self.source.index('"$DISK_ENCRYPT" finish')
            record = self.source.index('write-reclaim-record')
            finalize = self.source.index('firmware-finalize --handoff')
            self.assertLess(record, finish)
            self.assertLess(finish, finalize)
            self.assertIn('[ "$INTERNAL_MODE" -eq 0 ] || GUIDED_ARGS+=(--in-use-disk)', self.source)
            self.assertIn('check-mounts --handoff', self.source)
        with self.subTest(phase="guided_helper_skips_only_the_whole_table_reread_on_an_in_use_disk"):
            guided = GUIDED.read_text()
            self.assertIn('if [ "$IN_USE_DISK" -eq 0 ]; then\n  blockdev --rereadpt "$DISK"\nfi', guided)
            self.assertIn('if [ "$IN_USE_DISK" -eq 1 ]; then\n    partx -a -n "$new_number:$new_number" "$DISK" || true\n  fi', guided)
            self.assertIn('kernel_partition_matches "$new_node" "$new_number" "$new_start" "$new_sectors" "$new_uuid"', guided)
            self.assertIn('blkid -p -s PART_ENTRY_UUID -o value "$node"', guided)
            self.assertLess(guided.index('kernel_partition_matches() {'), guided.index('mkfs.vfat -n TILO-EFI'))
            self.assertIn('[ $((ROOT_START + ROOT_SIZE)) -eq $((FREE_START + FREE_SIZE)) ]', guided)


if __name__ == '__main__':
    unittest.main()
