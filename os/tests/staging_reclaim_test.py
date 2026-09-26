#!/usr/bin/python3
"""Installed-system staging reclaim (USB-free install, design spec §7).

Runs the real `tilo-staging-reclaim` against fake lsblk/sfdisk/partx/
cryptsetup/btrfs/findmnt/loginctl tools that share one simulated GPT. The fakes
apply sfdisk's delete and resize requests to that table, so the tests observe
the resulting partition map rather than the command lines alone. No real disk
is touched; os/tests/staging_reclaim_loop.sh repeats the core path on a loop
device as root.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
RECLAIM = REPO / 'os/usr/libexec/tilo-staging-reclaim'
ESP_TYPE = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
LINUX_TYPE = '0fc63daf-8483-4772-8e79-3d69d8477de4'
STAGING_ROOT_TYPE = 'd4c4daf6-f72c-43ee-b8e3-5ba32550311d'
TILO_ROOT = '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab'
MSFT_TYPE = 'ebd0a0a2-b9e5-4433-87c0-68b6b72699c7'
GIB_SECTORS = 1024 ** 3 // 512
DISK = '/dev/vda'
DISK_GUID = '11111111-2222-4333-8444-555555555555'
TARGET_ESP = (100 * GIB_SECTORS + 2048, 100 * GIB_SECTORS + 2048 + 2 * GIB_SECTORS - 1)
TARGET_ROOT = (TARGET_ESP[1] + 1, TARGET_ESP[1] + 62 * GIB_SECTORS)
STAGING_ESP = (TARGET_ROOT[1] + 1, TARGET_ROOT[1] + GIB_SECTORS)
STAGING_ROOT = (STAGING_ESP[1] + 1, STAGING_ESP[1] + 40 * GIB_SECTORS)
RECOVERY = (STAGING_ROOT[1] + 1, STAGING_ROOT[1] + GIB_SECTORS)

FAKE = r'''#!/usr/bin/python3
import json, os, re, sys
state_path = os.environ['FAKE_DISK_STATE']
state = json.load(open(state_path))
tool = os.path.basename(sys.argv[0])
args = sys.argv[1:]
state['calls'].append([tool] + args)
def save():
    json.dump(state, open(state_path, 'w'))
def part_by_number(number):
    return next(p for p in state['partitions'] if p['node'] == state['disk'] + str(number))
def node(part):
    return {'name': part['node'], 'type': 'part', 'mountpoints': part.get('mounts') or [None],
            'children': part.get('children') or None}
out = None
if tool == 'lsblk':
    if '-d' in args:
        out = {'blockdevices': [{'name': state['disk'], 'type': 'disk', 'pttype': 'gpt',
                                 'ptuuid': state['ptuuid'], 'size': state['size']}]}
    else:
        wanted = args[-1]
        part = next(p for p in state['partitions'] if p['node'] == wanted)
        out = {'blockdevices': [node(part)]}
elif tool == 'sfdisk':
    if args[0] == '--json':
        out = {'partitiontable': {'label': 'gpt', 'id': state['ptuuid'].upper(), 'device': state['disk'],
                                  'unit': 'sectors', 'sectorsize': 512,
                                  'partitions': [{'node': p['node'], 'start': p['start'], 'size': p['size'],
                                                  'type': p['type'].upper(), 'uuid': p['uuid'].upper()}
                                                 for p in state['partitions']]}}
    elif '--delete' in args:
        if state.get('fail_delete'):
            sys.exit(1)
        number = int(args[-1])
        state['partitions'].remove(part_by_number(number))
    elif '-N' in args:
        number = int(args[args.index('-N') + 1])
        request = sys.stdin.read()
        match = re.match(r'start=(\d+), size=(\d+)', request)
        part = part_by_number(number)
        assert int(match.group(1)) == part['start']
        part['size'] = int(match.group(2))
    else:
        sys.exit(2)
elif tool == 'cryptsetup':
    if state.get('fail_cryptsetup'):
        sys.stderr.write('no key available\n')
        save()
        sys.exit(1)
elif tool == 'findmnt':
    print(state.get('fstype', 'btrfs'))
elif tool == 'loginctl':
    if args[0] == 'list-sessions':
        for session in state['sessions']:
            print('%s %s user seat0' % (session['id'], session['Uid']))
    else:
        session = next(s for s in state['sessions'] if s['id'] == args[1])
        for key in ('Class', 'Uid', 'State'):
            print('%s=%s' % (key, session[key]))
elif tool in ('partx', 'btrfs'):
    pass
else:
    sys.exit(3)
save()
if out is not None:
    print(json.dumps(out))
'''


def initial_state():
    def part(number, uuid, kind, extent, **extra):
        value = {'node': DISK + str(number), 'uuid': uuid, 'type': kind, 'start': extent[0],
                 'size': extent[1] - extent[0] + 1}
        value.update(extra)
        return value
    return {
        'disk': DISK, 'ptuuid': DISK_GUID, 'size': (RECOVERY[1] + 4096) * 512, 'calls': [],
        'sessions': [{'id': '2', 'Class': 'user', 'Uid': '1000', 'State': 'active'}],
        'partitions': [
            part(1, '00000000-0000-4000-8000-00000000000a', ESP_TYPE, (2048, 206847)),
            part(2, '00000000-0000-4000-8000-00000000000b', MSFT_TYPE, (206848, 100 * GIB_SECTORS - 1)),
            part(5, 'cccccccc-0000-4000-8000-000000000003', ESP_TYPE, TARGET_ESP, mounts=['/boot/efi']),
            part(6, 'eeeeeeee-0000-4000-8000-000000000005', TILO_ROOT, TARGET_ROOT,
                 children=[{'name': '/dev/mapper/luks-1234', 'type': 'crypt', 'mountpoints': ['/sysroot']}]),
            part(3, 'aaaaaaaa-0000-4000-8000-000000000001', ESP_TYPE, STAGING_ESP),
            part(4, 'bbbbbbbb-0000-4000-8000-000000000002', STAGING_ROOT_TYPE, STAGING_ROOT),
            part(7, '00000000-0000-4000-8000-00000000000d', MSFT_TYPE, RECOVERY),
        ],
    }


def record():
    return {
        'schema': 1, 'operation_id': '0f0e0d0c-0b0a-4908-8706-050403020100',
        'disk_guid': DISK_GUID, 'disk_size_bytes': (RECOVERY[1] + 4096) * 512, 'sector_bytes': 512,
        'staging_esp': {'partuuid': 'aaaaaaaa-0000-4000-8000-000000000001',
                        'first_lba': STAGING_ESP[0], 'last_lba': STAGING_ESP[1]},
        'staging_root': {'partuuid': 'bbbbbbbb-0000-4000-8000-000000000002',
                         'first_lba': STAGING_ROOT[0], 'last_lba': STAGING_ROOT[1]},
        'target_root': {'partuuid': 'eeeeeeee-0000-4000-8000-000000000005',
                        'first_lba': TARGET_ROOT[0], 'last_lba': TARGET_ROOT[1]},
        'target_esp_partuuid': 'cccccccc-0000-4000-8000-000000000003',
    }


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


class ReclaimTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.bin = root / 'bin'
        self.bin.mkdir()
        fake = self.bin / 'fake-tool'
        fake.write_text(FAKE.replace('#!/usr/bin/python3', '#!' + sys.executable))
        fake.chmod(0o755)
        for tool in ('lsblk', 'sfdisk', 'partx', 'cryptsetup', 'btrfs', 'findmnt', 'loginctl'):
            (self.bin / tool).symlink_to(fake)
        self.state_path = root / 'state.json'
        self.record_path = root / 'var/lib/tilo/staging-reclaim.json'
        self.record_path.parent.mkdir(parents=True)
        self.write_state(initial_state())
        self.record_path.write_text(json.dumps(record()))

    def tearDown(self):
        self.temporary.cleanup()

    def write_state(self, state):
        self.state_path.write_text(json.dumps(state))

    def state(self):
        return json.loads(self.state_path.read_text())

    def reclaim(self):
        env = dict(os.environ, PATH='%s:%s' % (self.bin, os.environ['PATH']),
                   FAKE_DISK_STATE=str(self.state_path), TILO_STAGING_RECLAIM_RECORD=str(self.record_path),
                   TILO_STAGING_RECLAIM_SKIP_LOCK='1')
        return subprocess.run([sys.executable, str(RECLAIM)], env=env, capture_output=True, text=True)

    def partitions(self):
        return {p['uuid']: (p['start'], p['start'] + p['size'] - 1) for p in self.state()['partitions']}

    def mutations(self):
        return [call for call in self.state()['calls']
                if call[0] in ('cryptsetup', 'btrfs') or (call[0] == 'sfdisk' and call[1] != '--json')]

    def test_reclaims_and_resumes(self):
        with self.subTest(phase="deletes_both_staging_partitions_and_grows_the_adjacent_root"):
            result = self.reclaim()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            parts = self.partitions()
            self.assertNotIn('aaaaaaaa-0000-4000-8000-000000000001', parts)
            self.assertNotIn('bbbbbbbb-0000-4000-8000-000000000002', parts)
            self.assertEqual(parts['eeeeeeee-0000-4000-8000-000000000005'], (TARGET_ROOT[0], STAGING_ROOT[1]))
            self.assertEqual(parts['00000000-0000-4000-8000-00000000000d'], RECOVERY, 'WinRE must be untouched')
            self.assertEqual(parts['00000000-0000-4000-8000-00000000000b'], (206848, 100 * GIB_SECTORS - 1))
            calls = self.state()['calls']
            self.assertIn(['cryptsetup', 'resize', '--batch-mode', 'luks-1234'], calls)
            self.assertIn(['btrfs', 'filesystem', 'resize', 'max', '/sysroot'], calls)
            self.assertFalse(self.record_path.exists())
            self.assertEqual(result.stdout.splitlines()[-1], 'tilo-staging-reclaim: done')
        restart_fixture(self)
        with self.subTest(phase="resumes_after_a_crash_between_the_two_deletions"):
            state = initial_state()
            state['partitions'] = [p for p in state['partitions'] if p['node'] != DISK + '3']
            self.write_state(state)
            result = self.reclaim()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(self.partitions()['eeeeeeee-0000-4000-8000-000000000005'],
                             (TARGET_ROOT[0], STAGING_ROOT[1]))
            self.assertFalse(self.record_path.exists())
        restart_fixture(self)
        with self.subTest(phase="resumes_after_growth_but_before_the_filesystem_grew"):
            state = initial_state()
            state['partitions'] = [p for p in state['partitions'] if p['node'] not in (DISK + '3', DISK + '4')]
            root = next(p for p in state['partitions'] if p['node'] == DISK + '6')
            root['size'] = STAGING_ROOT[1] - root['start'] + 1
            self.write_state(state)
            result = self.reclaim()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            calls = self.state()['calls']
            self.assertFalse([c for c in calls if c[0] == 'sfdisk' and c[1] != '--json'], 'no second GPT write')
            self.assertIn(['btrfs', 'filesystem', 'resize', 'max', '/sysroot'], calls)
        restart_fixture(self)
        with self.subTest(phase="waits_for_the_first_user_session"):
            state = initial_state()
            state['sessions'] = [{'id': '1', 'Class': 'greeter', 'Uid': '972', 'State': 'active'}]
            self.write_state(state)
            result = self.reclaim()
            self.assertEqual(result.returncode, 0)
            self.assertEqual(self.mutations(), [])
            self.assertTrue(self.record_path.exists())
        restart_fixture(self)
        with self.subTest(phase="locked_luks_key_defers_growth_to_the_next_boot"):
            state = initial_state()
            state['fail_cryptsetup'] = True
            self.write_state(state)
            result = self.reclaim()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('grows at the next boot', result.stdout)
            self.assertNotIn(['btrfs', 'filesystem', 'resize', 'max', '/sysroot'], self.state()['calls'])
            self.assertFalse(self.record_path.exists())

    def assertUntouched(self, fragment):
        before = self.partitions()
        result = self.reclaim()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(fragment, result.stdout)
        self.assertIn('tilo-staging-reclaim: refused: ', result.stdout)
        self.assertNotIn('tilo-staging-reclaim: done', result.stdout)
        self.assertEqual(self.partitions(), before)
        self.assertEqual(self.mutations(), [])
        self.assertTrue(self.record_path.exists(), 'a refused reclaim keeps its evidence')

    def test_mismatch_leaves_everything_untouched(self):
        with self.subTest(phase="foreign_partition_in_the_staging_space_leaves_everything_untouched"):
            state = initial_state()
            state['partitions'] = [p for p in state['partitions'] if p['node'] != DISK + '4']
            state['partitions'].append({'node': DISK + '8', 'uuid': '00000000-0000-4000-8000-00000000000e',
                                        'type': LINUX_TYPE, 'start': STAGING_ROOT[0], 'size': 2048})
            self.write_state(state)
            self.assertUntouched('now occupies the recorded staging space')
        restart_fixture(self)
        with self.subTest(phase="changed_staging_identity_leaves_everything_untouched"):
            state = initial_state()
            next(p for p in state['partitions'] if p['node'] == DISK + '4')['type'] = TILO_ROOT
            self.write_state(state)
            self.assertUntouched('staging root type changed')
            # A generic Linux partition is not a staging root, whatever its extent.
            state = initial_state()
            next(p for p in state['partitions'] if p['node'] == DISK + '4')['type'] = LINUX_TYPE
            self.write_state(state)
            self.assertUntouched('staging root type changed')
        restart_fixture(self)
        with self.subTest(phase="mounted_staging_partition_leaves_everything_untouched"):
            state = initial_state()
            next(p for p in state['partitions'] if p['node'] == DISK + '3')['mounts'] = ['/mnt']
            self.write_state(state)
            self.assertUntouched('staging ESP is in use')
        restart_fixture(self)
        with self.subTest(phase="other_disk_guid_or_moved_root_leaves_everything_untouched"):
            state = initial_state()
            state['ptuuid'] = '99999999-2222-4333-8444-555555555555'
            self.write_state(state)
            self.assertUntouched('expected exactly one disk')
            state = initial_state()
            next(p for p in state['partitions'] if p['node'] == DISK + '6')['size'] -= 2048
            self.write_state(state)
            self.assertUntouched('ends neither at its recorded end')
        restart_fixture(self)
        with self.subTest(phase="malformed_record_leaves_everything_untouched"):
            broken = record()
            broken['staging_esp']['partuuid'] = 'AAAAAAAA-0000-4000-8000-000000000001'
            self.record_path.write_text(json.dumps(broken))
            self.assertUntouched('not a lowercase GUID')


class UnitWiringTests(unittest.TestCase):
    def test_units_are_scoped_to_their_side_of_the_install(self):
        units = REPO / 'os/usr/lib/systemd/system'
        reclaim = (units / 'tilo-staging-reclaim.service').read_text()
        self.assertIn('ConditionKernelCommandLine=!tilo.install', reclaim)
        self.assertIn('ConditionPathExists=/var/lib/tilo/staging-reclaim.json', reclaim)
        self.assertIn('After=local-fs.target tilo-firstboot-user.service tilo-boot-health.service', reclaim)
        for live in ('tilo-staging-handoff.service', 'tilo-installer-grow-root.service'):
            text = (units / live).read_text()
            self.assertIn('ConditionKernelCommandLine=tilo.install', text)
            self.assertIn('Before=greetd.service', text)
        self.assertIn('ExecStart=/usr/lib/systemd/systemd-growfs /sysroot',
                      (units / 'tilo-installer-grow-root.service').read_text())
        containerfile = (REPO / 'os/Containerfile.installer').read_text()
        self.assertIn('systemctl enable tilo-staging-handoff.service tilo-installer-grow-root.service '
                      'tilo-staging-reclaim.timer', containerfile)
        for script in ('tilo-staging-handoff', 'tilo-staging-reclaim'):
            self.assertTrue(os.access(REPO / 'os/usr/libexec' / script, os.X_OK), script)


if __name__ == '__main__':
    unittest.main()
