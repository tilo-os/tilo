#!/usr/bin/python3
"""Unprivileged checks for the USB-free VM round trip (design spec §9).

The round trip itself needs KVM, OVMF and a Fedora guest (the
internal-install-cycle job of .forgejo/workflows/release-imager.yml). This
proves the parts that decide its verdict: efivarfs load-option decoding, and
that every evidence comparison accepts exactly the contract and rejects each
violation. It also runs the harness far enough to show it fails, rather than
skips, when an input or tool is missing.
"""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest
import uuid

REPO = Path(__file__).resolve().parents[2]
HARNESS = REPO / 'os/dev/test-internal-install-cycle.sh'
spec = importlib.util.spec_from_file_location('evidence', REPO / 'os/dev/internal_cycle_evidence.py')
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)

GIB = (1 << 30) // 512
FEDORA_ESP = '11111111-0000-4000-8000-000000000002'
STAGING_ESP = 'aaaaaaaa-0000-4000-8000-000000000001'
STAGING_ROOT = 'bbbbbbbb-0000-4000-8000-000000000002'
TILO_ESP = 'cccccccc-0000-4000-8000-000000000003'
TILO_BOOT = 'dddddddd-0000-4000-8000-000000000004'
TILO_ROOT = 'eeeeeeee-0000-4000-8000-000000000005'
ROOT_END = 45 * GIB - 1
TARGET = (ROOT_END + 1 + 2048, ROOT_END + 2048 + 64 * GIB)
S_ESP = (TARGET[1] + 1, TARGET[1] + GIB)
S_ROOT = (S_ESP[1] + 1, S_ESP[1] + 40 * GIB)


def load_option(partuuid, start, size, path, description, number=1):
    hd = struct.pack('<BBHIQQ', 4, 1, 42, number, start, size) + uuid.UUID(partuuid).bytes_le + bytes([2, 2])
    encoded = (path + '\0').encode('utf-16-le')
    device = hd + struct.pack('<BBH', 4, 4, 4 + len(encoded)) + encoded + bytes([0x7F, 0xFF, 4, 0])
    return struct.pack('<IH', 1, len(device)) + (description + '\0').encode('utf-16-le') + device


def entry(partuuid, start, size, path, description):
    data = load_option(partuuid, start, size, path, description)
    decoded = evidence.decode_load_option(data)
    decoded['hex'] = data.hex()
    return decoded


LINUX_TYPE = '0fc63daf-8483-4772-8e79-3d69d8477de4'


def part(uuid_, kind, first, last, name=''):
    return {'uuid': uuid_.upper(), 'type': kind.upper(), 'start': first, 'size': last - first + 1, 'name': name,
            'node': '/dev/vda'}


def baseline():
    fedora = entry(FEDORA_ESP, 4096, 204800, '\\EFI\\fedora\\shimx64.efi', 'Fedora')
    return {
        'schema': 1, 'disk': '/dev/vda', 'disk_size_bytes': (S_ROOT[1] + 10 * GIB) * 512,
        'partition_table': {'sectorsize': 512, 'partitions': [
            part('11111111-0000-4000-8000-000000000001', '21686148-6449-6e6f-744e-656564454649', 2048, 4095),
            part(FEDORA_ESP, evidence.ESP_TYPE, 4096, 208895),
            part('11111111-0000-4000-8000-000000000003', LINUX_TYPE, 208896, 2306047),
            part('11111111-0000-4000-8000-000000000004', LINUX_TYPE, 2306048, ROOT_END),
        ]},
        'firmware': {'boot_order': ['0001', '0000'], 'boot_next': None, 'boot_current': '0001',
                     'entries': {'0001': fedora, '0000': {'description': 'UiApp', 'partuuid': None,
                                                          'path': None, 'hex': 'aa'}}},
        'efibootmgr_v': '',
        'esp_loaders': {'EFI/fedora/shimx64.efi': 'a' * 64, 'EFI/BOOT/BOOTX64.EFI': 'b' * 64},
        'machine_id': '0123456789abcdef0123456789abcdef',
    }


def summary():
    return {'ok': True, 'operation_id': 'op',
            'target_extent': {'first_lba': TARGET[0], 'last_lba': TARGET[1]},
            'staging_esp': {'first_lba': S_ESP[0], 'last_lba': S_ESP[1], 'partition_guid': STAGING_ESP},
            'staging_root': {'first_lba': S_ROOT[0], 'last_lba': S_ROOT[1], 'partition_guid': STAGING_ROOT}}


def staged():
    doc = baseline()
    doc['partition_table']['partitions'] += [
        part(STAGING_ESP, evidence.ESP_TYPE, *S_ESP, name='Tilo installer boot'),
        part(STAGING_ROOT, evidence.STAGING_ROOT_TYPE, *S_ROOT, name='Tilo installer'),
    ]
    doc['firmware']['entries']['0100'] = entry(STAGING_ESP, S_ESP[0], S_ESP[1] - S_ESP[0] + 1,
                                               '\\EFI\\BOOT\\BOOTX64.EFI', 'Tilo installer')
    doc['firmware']['boot_next'] = '0100'
    return doc


def final():
    doc = baseline()
    esp_end = TARGET[0] + 2 * GIB - 1
    boot_end = esp_end + GIB
    doc['partition_table']['partitions'] += [
        part(TILO_ESP, evidence.ESP_TYPE, TARGET[0], esp_end, name='Tilo EFI'),
        part(TILO_BOOT, evidence.LINUX_TYPE, esp_end + 1, boot_end, name='Tilo boot'),
        part(TILO_ROOT, evidence.TILO_ROOT_TYPE, boot_end + 1, S_ROOT[1], name='Tilo root'),
    ]
    doc['firmware']['entries']['0003'] = entry(TILO_ESP, TARGET[0], 2 * GIB, '\\EFI\\systemd\\systemd-bootx64.efi',
                                               'Linux Boot Manager')
    return doc


class EvidenceTests(unittest.TestCase):
    def failures(self, verdict):
        return verdict.failures

    def test_firmware_evidence_decoding(self):
        with self.subTest(phase="decodes_the_gpt_hard_drive_node_from_load_option_bytes"):
            decoded = evidence.decode_load_option(load_option(STAGING_ESP, 42, 2048, '\\EFI\\BOOT\\BOOTX64.EFI', 'Tilo installer', 3))
            self.assertEqual(decoded, {'description': 'Tilo installer', 'partuuid': STAGING_ESP,
                                       'path': '\\EFI\\BOOT\\BOOTX64.EFI', 'partition_number': 3,
                                       'start_lba': 42, 'size_lba': 2048})
        with self.subTest(phase="firmware_reads_order_next_and_entries_from_efivarfs"):
            with tempfile.TemporaryDirectory() as directory:
                guid = evidence.EFI_GLOBAL
                attrs = b'\x07\x00\x00\x00'
                Path(directory, 'BootOrder-' + guid).write_bytes(attrs + struct.pack('<HH', 1, 0))
                Path(directory, 'BootNext-' + guid).write_bytes(attrs + struct.pack('<H', 0x100))
                Path(directory, 'Boot0100-' + guid).write_bytes(attrs + load_option(STAGING_ESP, 1, 2, '\\x', 'Tilo installer'))
                Path(directory, 'Timeout-' + guid).write_bytes(attrs + b'\0\0')
                state = evidence.firmware(directory)
            self.assertEqual(state['boot_order'], ['0001', '0000'])
            self.assertEqual(state['boot_next'], '0100')
            self.assertEqual(list(state['entries']), ['0100'])
            self.assertEqual(state['entries']['0100']['partuuid'], STAGING_ESP)
        with self.subTest(phase="cli_verdict_exit_status"):
            with tempfile.TemporaryDirectory() as directory:
                paths = {}
                for name, document in (('b', baseline()), ('s', staged()), ('m', summary()), ('f', final())):
                    paths[name] = Path(directory, name + '.json')
                    paths[name].write_text(json.dumps(document))
                ok = subprocess.run(['python3', str(REPO / 'os/dev/internal_cycle_evidence.py'), 'check-final',
                                     '--baseline', str(paths['b']), '--staged', str(paths['s']),
                                     '--summary', str(paths['m']), '--final', str(paths['f'])],
                                    capture_output=True, text=True)
                self.assertEqual(ok.returncode, 0, ok.stdout)
                self.assertTrue(json.loads(ok.stdout)['ok'])
                bad = subprocess.run(['python3', str(REPO / 'os/dev/internal_cycle_evidence.py'), 'check-final',
                                      '--baseline', str(paths['b']), '--staged', str(paths['s']),
                                      '--summary', str(paths['m']), '--final', str(paths['s'])],
                                     capture_output=True, text=True)
                self.assertEqual(bad.returncode, 1)
                self.assertFalse(json.loads(bad.stdout)['ok'])

    def test_layout_contracts(self):
        with self.subTest(phase="staged_contract_accepts_the_spec_layout"):
            self.assertEqual(self.failures(evidence.check_staged(baseline(), staged(), summary())), [])
        with self.subTest(phase="staged_contract_rejects_each_violation"):
            cases = {}
            doc = staged(); doc['firmware']['boot_order'] = ['0000', '0001']; cases['BootOrder changed'] = doc
            doc = staged(); doc['firmware']['boot_next'] = None; cases['BootNext was not set'] = doc
            doc = staged(); doc['firmware']['entries']['0100'] = entry(FEDORA_ESP, 4096, 204800, '\\EFI\\BOOT\\BOOTX64.EFI', 'Tilo installer')
            cases['does not point at the staging ESP'] = doc
            doc = staged(); doc['partition_table']['partitions'][-1]['type'] = evidence.TILO_ROOT_TYPE.upper()
            cases['must be the Tilo staging root type'] = doc
            doc = staged(); doc['partition_table']['partitions'][-1]['type'] = LINUX_TYPE.upper()
            cases['staging root type or extent differs'] = doc
            doc = staged(); doc['esp_loaders']['EFI/fedora/shimx64.efi'] = 'c' * 64
            cases['retained ESP loader bytes changed'] = doc
            doc = staged(); doc['partition_table']['partitions'][3]['size'] += 2048
            cases['retained partitions changed'] = doc
            doc = staged(); doc['firmware']['entries']['0001']['hex'] = '00'
            cases['retained firmware entries changed'] = doc
            for message, document in cases.items():
                with self.subTest(message):
                    failures = self.failures(evidence.check_staged(baseline(), document, summary()))
                    self.assertTrue(any(message in failure for failure in failures), failures)
            adjacent = summary()
            adjacent['target_extent']['first_lba'] = ROOT_END + 1
            failures = self.failures(evidence.check_staged(baseline(), staged(), adjacent))
            self.assertTrue(any('within 1 MiB' in failure for failure in failures), failures)
        with self.subTest(phase="final_contract_accepts_reclaimed_layout_and_tilo_entry_only"):
            self.assertEqual(self.failures(evidence.check_final(baseline(), staged(), summary(), final())), [])
            listed = final()
            listed['firmware']['boot_order'] = ['0003', '0001', '0000']
            self.assertEqual(self.failures(evidence.check_final(baseline(), staged(), summary(), listed)), [])
        with self.subTest(phase="final_contract_rejects_each_violation"):
            cases = {}
            doc = final(); doc['partition_table']['partitions'][-1]['size'] -= GIB
            cases['not at the end of the former staging region'] = doc
            doc = final(); doc['partition_table']['partitions'][-2]['size'] -= GIB
            cases['Tilo boot is not 1 GiB'] = doc
            doc = final(); doc['partition_table']['partitions'].pop(-2)
            cases['expected exactly one new Tilo ESP, boot, and encrypted root'] = doc
            doc = final(); doc['partition_table']['partitions'].append(part(STAGING_ROOT, evidence.STAGING_ROOT_TYPE, *S_ROOT))
            cases['a staging partition survived'] = doc
            doc = final(); doc['firmware']['entries']['0100'] = staged()['firmware']['entries']['0100']
            cases['owned staging entry Boot0100 survived'] = doc
            doc = final(); doc['firmware']['boot_order'] = ['0000', '0001']
            cases['BootOrder differs'] = doc
            doc = final(); doc['firmware']['boot_next'] = '0003'
            cases['BootNext is still set'] = doc
            doc = final(); doc['machine_id'] = 'f' * 32
            cases['machine-id changed'] = doc
            for message, document in cases.items():
                with self.subTest(message):
                    failures = self.failures(evidence.check_final(baseline(), staged(), summary(), document))
                    self.assertTrue(any(message in failure for failure in failures), failures)
        with self.subTest(phase="unstaged_contract_for_remove_and_refusal"):
            self.assertEqual(self.failures(evidence.check_unstaged(baseline(), baseline())), [])
            busy = baseline(); busy['firmware']['boot_next'] = '0001'
            self.assertEqual(self.failures(evidence.check_unstaged(busy, busy, '0001')), [])
            self.assertTrue(evidence.check_unstaged(baseline(), staged()).failures)
            leftover = baseline(); leftover['firmware']['entries']['0100'] = staged()['firmware']['entries']['0100']
            failures = evidence.check_unstaged(baseline(), leftover).failures
            self.assertTrue(any('left behind' in failure for failure in failures), failures)


class HarnessTests(unittest.TestCase):
    def run_harness(self, *args, path=None):
        env = dict(os.environ, TILO_VM_ARCH='x86_64')
        if path is not None:
            env['PATH'] = path
        return subprocess.run([shutil.which('bash'), str(HARNESS)] + list(args), env=env, capture_output=True, text=True,
                              cwd=tempfile.gettempdir())

    def test_harness_requirements(self):
        with self.subTest(phase="inputs_are_required_and_pinned"):
            result = self.run_harness('--fedora-image', '/nonexistent', '--bundle-dir', '/nonexistent')
            self.assertEqual(result.returncode, 1)
            self.assertIn('--fedora-sha256 must pin', result.stderr)
            result = self.run_harness('--fedora-sha256', '0' * 64, '--bundle-dir', '/x')
            self.assertIn('exactly one of --fedora-image or --fedora-url', result.stderr)
            result = self.run_harness('--fedora-sha256', '0' * 64, '--fedora-image', '/x')
            self.assertIn('exactly one of --bundle-dir or --installer-disk', result.stderr)
            result = self.run_harness('--fedora-sha256', '0' * 64, '--fedora-image', '/x', '--bundle-dir', '/x',
                                      '--scenario', 'maybe')
            self.assertIn('unknown scenario', result.stderr)
        with self.subTest(phase="missing_tools_fail_instead_of_skipping"):
            with tempfile.TemporaryDirectory() as directory:
                for tool in ('dirname', 'uname'):
                    found = shutil.which(tool)
                    self.assertIsNotNone(found, tool)
                    os.symlink(found, os.path.join(directory, tool))
                result = self.run_harness('--fedora-sha256', '0' * 64, '--fedora-image', '/x', '--bundle-dir', '/x',
                                          path=directory)
            self.assertEqual(result.returncode, 1)
            self.assertIn('missing required tool: qemu-img', result.stderr)
        with self.subTest(phase="every_scenario_has_its_evidence_check"):
            source = HARNESS.read_text()
            for needle in ('check-staged', 'check-final', 'check-unstaged', 'refused:boot-next-busy',
                           'systemctl kill --signal=KILL tilo-cycle-helper', '--action remove',
                           "tilo-staging-reclaim: (done|refused: .*)", '--serial "$disk_serial"',
                           'Also[[:space:]]+holds[[:space:]]+the[[:space:]]+running[[:space:]]+installer',
                           'growpart: {mode: \'off\'}', 'gpt-bak-std'):
                self.assertIn(needle, source)
            self.assertTrue(os.access(HARNESS, os.X_OK))


if __name__ == '__main__':
    unittest.main()
