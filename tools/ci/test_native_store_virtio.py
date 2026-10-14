#!/usr/bin/env python3
"""Nonprivileged refusal/ownership tests for the disposable virtio fixture."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'),
                                                Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stores = load('native-store-virtio')
vm = load('disposable-native-vm')


class VirtioFixture(unittest.TestCase):
    def receipt(self):
        return {'schema': 1, 'vm': 'native-store-private-0123456789abcdef',
                'base_sha256': 'a' * 64,
                'disks': [{'name': n, 'serial': f'tns-0123456789ab-{n}', 'size': 1024 ** 3}
                          for n in ('a', 'b')]}

    def test_read_provenance_refuses_nonroot_without_reading(self):
        with patch.object(os, 'geteuid', return_value=1000):
            with self.assertRaisesRegex(ValueError, 'requires root'):
                stores.provenance(Path('/unread'), Path('/unread/provenance.json'))

    def test_unknown_serial_refuses_before_open_or_format(self):
        with patch.object(stores.Path, 'glob', return_value=[]), \
                patch.object(stores.os, 'open') as opened:
            with self.assertRaisesRegex(ValueError, 'exactly one'):
                stores.device(self.receipt()['disks'][0])
            opened.assert_not_called()

    def test_duplicate_serial_refuses_before_open_or_format(self):
        class Disk:
            def __truediv__(self, _):
                return self

            def exists(self):
                return True

            def read_text(self):
                return 'tns-0123456789ab-a'

        with patch.object(stores.Path, 'glob', return_value=[Disk(), Disk()]), \
                patch.object(stores.os, 'open') as opened:
            with self.assertRaisesRegex(ValueError, 'exactly one'):
                stores.device(self.receipt()['disks'][0])
            opened.assert_not_called()

    def test_partition_and_holders_refuse_before_format(self):
        for obstruction in ('partition', 'holders/owner', 'vdz1'):
            with self.subTest(obstruction=obstruction), tempfile.TemporaryDirectory() as tmp:
                disk = Path(tmp) / 'vdz'
                (disk / 'holders').mkdir(parents=True)
                (disk / 'serial').write_text('tns-0123456789ab-a')
                (disk / obstruction).touch()
                original = stores.Path.glob

                def glob(path, pattern):
                    return [disk] if str(path) == '/sys/class/block' else original(path, pattern)

                with patch.object(stores.Path, 'glob', glob), patch.object(stores, 'run') as run:
                    with self.assertRaisesRegex(ValueError, 'partitions or holders'):
                        stores.device(self.receipt()['disks'][0])
                    run.assert_not_called()

    def test_btrfs_anonymous_mount_number_cannot_hide_root_device(self):
        with tempfile.TemporaryDirectory() as tmp:
            disk = Path(tmp) / 'vdz'
            (disk / 'holders').mkdir(parents=True)
            (disk / 'serial').write_text('tns-0123456789ab-a')
            (disk / 'dev').write_text('252:48')
            (disk / 'size').write_text(str(1024 ** 3 // 512))
            original_glob, original_read, original_stat = stores.Path.glob, stores.Path.read_text, os.stat

            def glob(path, pattern):
                return [disk] if str(path) == '/sys/class/block' else original_glob(path, pattern)

            def read(path, *args, **kwargs):
                if str(path) == '/proc/self/mountinfo':
                    return '1 0 0:65 / / rw - btrfs /dev/root rw\n'
                return original_read(path, *args, **kwargs)

            def metadata(path, *args, **kwargs):
                if str(path) == '/dev/root':
                    from types import SimpleNamespace
                    return SimpleNamespace(st_rdev=os.makedev(252, 48))
                return original_stat(path, *args, **kwargs)

            with patch.object(stores.Path, 'glob', glob), patch.object(stores.Path, 'read_text', read), \
                    patch.object(os, 'stat', metadata), patch.object(stores, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'mounted .*root'):
                    stores.device(self.receipt()['disks'][0])
                run.assert_not_called()

    def test_prior_format_attempt_refuses_without_device_access(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(stores, 'device') as device:
            directory = Path(tmp)
            (directory / 'format-started').touch()
            with self.assertRaises(FileExistsError):
                stores.up(directory, self.receipt()['disks'])
            device.assert_not_called()

    def test_kernel_exclusive_claim_failure_never_runs_mkfs(self):
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(stores, 'device', return_value=(Path('/dev/fixture'), 123)), \
                patch.object(stores, 'run') as run:
            real_open = os.open
            calls = []

            def opened(path, flags, *args, **kwargs):
                if path == Path('/dev/fixture'):
                    calls.append(flags)
                    raise OSError('kernel claim refused')
                return real_open(path, flags, *args, **kwargs)

            with patch.object(stores.os, 'open', side_effect=opened):
                with self.assertRaisesRegex(OSError, 'kernel claim refused'):
                    stores.up(Path(tmp), self.receipt()['disks'])
            self.assertEqual(len(calls), 1)
            self.assertTrue(calls[0] & os.O_EXCL)
            self.assertTrue(calls[0] & os.O_NOFOLLOW)
            run.assert_not_called()

    def test_zero_scan_checks_beyond_first_block(self):
        with tempfile.TemporaryFile() as stream:
            stream.truncate(8 * 1024 ** 2)
            stream.seek(7 * 1024 ** 2)
            stream.write(b'x')
            stream.flush()
            stream.seek(0)
            with self.assertRaisesRegex(ValueError, 'not wholly zero'):
                stores.require_zero(stream.fileno(), 8 * 1024 ** 2)

    def test_zero_scan_refuses_truncated_disk(self):
        with tempfile.TemporaryFile() as stream:
            stream.truncate(1024)
            with self.assertRaisesRegex(ValueError, 'not wholly zero'):
                stores.require_zero(stream.fileno(), 2048)

    def test_zero_scan_accepts_only_complete_zero_extent(self):
        with tempfile.TemporaryFile() as stream:
            stream.truncate(8 * 1024 ** 2)
            stores.require_zero(stream.fileno(), 8 * 1024 ** 2)
            self.assertEqual(stream.tell(), 8 * 1024 ** 2)

    def test_down_empty_fixture_is_idempotent(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(stores, 'run') as run:
            stores.down(Path(tmp), self.receipt()['disks'])
            stores.down(Path(tmp), self.receipt()['disks'])
            run.assert_not_called()

    def test_aux_disks_are_owned_exclusive_serial_bound_raw_files(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(vm, 'run') as run:
            directory = Path(tmp)
            rows = self.receipt()['disks']
            vm.attach('owned', directory, rows)
            self.assertEqual(run.call_count, 2)
            for row in rows:
                raw = directory / (row['name'] + '.raw')
                self.assertEqual(raw.stat().st_size, 1024 ** 3)
                self.assertEqual(raw.stat().st_mode & 0o777, 0o600)
                tree = vm.ET.parse(directory / (row['name'] + '.xml'))
                self.assertEqual(tree.findtext('serial'), row['serial'])
                self.assertLessEqual(len(row['serial']), 20)
                self.assertEqual(tree.find('source').get('file'), str(raw))
            with self.assertRaises(FileExistsError):
                vm.attach('owned', directory, rows)
            self.assertEqual(run.call_count, 2)

    def test_failed_case_still_tears_down_via_existing_transport(self):
        calls = []

        def guest(cli, name, command, **kwargs):
            calls.append(command)
            if ' up ' in command:
                return subprocess.CompletedProcess([], 0, stdout=(
                    'TILO_FS_NATIVE_STORES=/fixture/a:/fixture/b\n'
                    'TILO_FS_NATIVE_STORE=/fixture/a\nTILO_REQUIRE_FANOTIFY=1\n'))
            if command.startswith('env TILO_FS_NATIVE_STORES'):
                raise RuntimeError('case failed')
            return subprocess.CompletedProcess([], 0, stdout='')

        with patch.object(vm, 'guest', side_effect=guest):
            with self.assertRaisesRegex(RuntimeError, 'case failed'):
                vm.exercise('tilo-vm', 'owned', Path('/fixture'), self.receipt(), ['false'])
        self.assertIn(' down ', calls[-1])
        self.assertIn('TILO_NATIVE_STORE_VIRTIO_PROVENANCE=', calls[1])

    def test_failed_domain_removal_preserves_all_backing_files(self):
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(vm, 'run', side_effect=RuntimeError('cannot stop')):
            directory = Path(tmp)
            (directory / 'vms.toml').touch()
            (directory / 'a.raw').touch()
            with self.assertRaisesRegex(RuntimeError, 'cannot stop'):
                vm.cleanup('tilo-vm', 'owned', directory)
            self.assertTrue((directory / 'a.raw').exists())

    def test_unknown_domain_source_is_never_undefined_or_deleted(self):
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(vm, 'run', side_effect=[
                    subprocess.CompletedProcess([], 0, stdout='owned\n'),
                    subprocess.CompletedProcess([], 0, stdout=(
                        '<domain><devices><disk><source file="/other"/>'
                        '</disk></devices></domain>'))]) as run:
            directory = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'refusing cleanup'):
                vm.cleanup('tilo-vm', 'owned', directory)
            self.assertEqual(run.call_count, 2)
            self.assertTrue(directory.exists())

    def test_failed_libvirt_inventory_preserves_partial_create_storage(self):
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(vm, 'run', side_effect=RuntimeError('cannot observe libvirt')):
            directory = Path(tmp)
            (directory / 'root.qcow2').touch()
            with self.assertRaisesRegex(RuntimeError, 'cannot observe'):
                vm.cleanup('tilo-vm', 'owned', directory)
            self.assertTrue((directory / 'root.qcow2').exists())

    def test_noncache_base_is_refused(self):
        with tempfile.NamedTemporaryFile(suffix='.qcow2') as stream:
            with self.assertRaisesRegex(ValueError, 'published qcow2'):
                vm.verify_base(Path(stream.name))


if __name__ == '__main__':
    unittest.main()
