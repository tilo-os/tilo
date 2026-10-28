#!/usr/bin/env python3
"""Production firstboot transaction retries with an explicit Python seed oracle.

The native seed namespace suite separately exercises the shipped executable.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/tests'))
from hang import HANG_BOUND  # noqa: E402
SOURCE = ROOT / 'os/usr/libexec/tilo-firstboot-user'
FILES_BINARY = Path(os.environ.get('TILO_FIRSTBOOT_FILES_BINARY', ROOT / 'target/debug/tilo-firstboot-files'))
HASH = '$6$fixture$' + 'A' * 86


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        r = self.root
        (r / 'etc').mkdir()
        (r / 'state').mkdir()
        (r / 'state/device-private').mkdir(mode=0o700)
        (r / 'state/device-private/keypair.bin').write_text('key')
        (r / 'state/device-private/keypair.bin').chmod(0o600)
        (r / 'bin').mkdir()
        (r / 'home/fixtureuser').mkdir(parents=True)
        self.credential = r / 'etc/install-user.conf'
        self.credential.write_text('fixtureuser:' + HASH + '\n')
        self.credential.chmod(0o600)
        self.full_name = r / 'etc/install-user-full-name'
        self.full_name.write_text('Fixture User\n')
        self.full_name.chmod(0o600)
        for name in ['migration-plan.json', 'prepared-setup', 'prepared-home']:
            (r / 'state' / name).write_text('fixture')
        # Every external tool is inert, but maintains shadow/account state across
        # separate real Bash invocations. A delivery observes actual mock shadow.
        common = """import json, os, signal, sys
from pathlib import Path
r = Path(os.environ['FIXTURE_ROOT'])
stage = os.environ.get('FAIL_STAGE', '')
def fail(name):
    if stage == name:
        if os.environ.get('TERMINATE') == '1':
            os.kill(os.getppid(), signal.SIGTERM)
        if stage == 'crash':
            os.kill(os.getppid(), signal.SIGKILL)
        raise SystemExit(1)
def expired():
    assert (r / 'expiry').read_text() == '1970-01-02', 'delivery ran unexpired'
def locked():
    expired()
    assert (r / 'shadow').read_text().startswith('!'), 'delivery ran unlocked'
"""
        tools = {
            # The built helper, unless the case fails the intent's publication,
            # whose own syncs (record, then directory) make the intent durable.
            'files-helper': f"fail('intent') if sys.argv[1] == 'intent' else None; os.execv({str(FILES_BINARY)!r}, [{str(FILES_BINARY)!r}] + sys.argv[1:])",
            'id': "raise SystemExit(0 if (r / 'shadow').exists() else 1)",
            'stat': "print('0:600:1' if sys.argv[2] == '%u:%a:%h' else '0:700' if sys.argv[2] == '%u:%a' else Path(sys.argv[-1]).stat().st_size)",
            'getent': "print('fixtureuser:' + ((r / 'shadow').read_text() if sys.argv[1] == 'shadow' else 'x') + ':' + str(os.getuid()) + ':' + str(os.getgid()) + '::' + str(r / 'home/fixtureuser') + ':/bin/bash')",
            'useradd': "fail('useradd-before'); assert not (r / 'shadow').exists(); (r / 'expiry').write_text(sys.argv[sys.argv.index('-e') + 1]); expired(); (r / 'shadow').write_text(sys.argv[sys.argv.index('-p') + 1]); (r / 'useradd').write_text(json.dumps(sys.argv[1:])); fail('useradd'); fail('crash')",
            'chpasswd': "expired(); value = sys.stdin.read().strip().split(':', 1)[1]; fail('password'); (r / 'shadow').write_text(value)",
            'chage': "assert sys.argv[1] == '-E' and sys.argv[3] == 'fixtureuser'; value = sys.argv[2]; fail('expiry-release' if value == '-1' else 'expiry-retry'); assert value != '-1' or (r / 'shadow').read_text() == '$6$fixture$' + 'A' * 86; (r / 'expiry').write_text(value)",
            'chgrp': "locked(); fail('key-repair')",
            'chmod': "locked(); fail('key-mode')",
            'chown': "locked(); fail('chown')",
            'shred': "Path(sys.argv[-1]).unlink()",
        }
        for name, body in tools.items():
            p = r / 'bin' / name
            p.write_text('#!/usr/bin/env python3\n' + common + body + '\n')
            p.chmod(0o755)
        (r / 'skel').mkdir()
        (r / 'skel/default').write_text('generated')
        (r / 'skel/seeded').write_text('skel')
        (r / 'home/fixtureuser/seeded').write_text('installer')
        (r / 'seeds.py').write_text('#!/usr/bin/env python3\n' + common + f"""locked()
fail('seeds')
import importlib.util, stat
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('seeds', {str(ROOT / 'os/tests/oracles/tilo-firstboot-home-seeds.py')!r})
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
# Simulated NSS account and root authority; production seed I/O is unchanged.
def trusted(path):
    info = path.lstat()
    if info.st_uid != os.getuid() or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
        raise ValueError('untrusted fixture authority')
writer = helper.atomic.write_file
def write(directory, name, source, mode, uid, gid):
    writer(directory, name, source, mode, os.getuid() if uid == 0 else uid,
           os.getgid() if gid == 0 else gid)
with patch.object(helper.os, 'geteuid', return_value=0), \\
        patch.object(helper, 'trusted', side_effect=trusted), \\
        patch.object(helper.atomic, 'write_file', side_effect=write):
    helper.seed(r / 'home/fixtureuser', r / 'skel', os.getuid(), os.getgid(), r / 'state/seeds')
fail('seeds-after')
""")
        (r / 'seeds.py').chmod(0o755)
        (r / 'handoff').write_text('#!/usr/bin/env python3\n' + common + """if sys.argv[1] == 'required-user-id':
    assert not (r / 'shadow').exists(), 'retry repeated UID availability query'
    print('1000')
else:
    locked()
    fail('plan')
""")
        (r / 'handoff').chmod(0o755)
        (r / 'etc/prepared-setup-transport').write_text('#!/usr/bin/env python3\n' + common + "locked(); fail(sys.argv[1])\n")
        (r / 'etc/prepared-setup-transport').chmod(0o755)
        s = SOURCE.read_text()
        for old, new in {
            '/etc/tilo': str(r / 'etc'),
            '/var/lib/tilo': str(r / 'state'),
            '/var/home': str(r / 'home'),
            '"/home/$user"': f'"{r}/home/$user"',
            'FILES_HELPER="$(dirname -- "${BASH_SOURCE[0]}")/tilo-firstboot-files"': f'FILES_HELPER="{r}/bin/files-helper"',
            '/usr/libexec/tilo-staging-handoff': str(r / 'handoff'),
            'seed_helper=/usr/libexec/tilo-firstboot-home-seeds': f'seed_helper={r}/seeds.py',
        }.items():
            self.assertIn(old, s)
            s = s.replace(old, new)
        (r / 'firstboot').write_text(s)

    def run_boot(self, fail='', terminate=False):
        env = dict(os.environ, FIXTURE_ROOT=str(self.root), FAIL_STAGE=fail,
                   TERMINATE=str(int(terminate)), PATH=f"{self.root}/bin:{os.environ['PATH']}")
        # A boot spawns a dozen Python tool stand-ins and fsyncs its intent;
        # on a loaded runner that took longer than a 10 s guess. The bound
        # only catches a hang: a timed-out boot leaves the fixture between
        # stages, which turned one slow boot into every later case failing.
        result = subprocess.run(['bash', str(self.root / 'firstboot')], env=env,
                                capture_output=True, text=True, timeout=HANG_BOUND)
        self.assertNotIn(HASH, result.stdout + result.stderr)
        return result

    def test_every_failed_delivery_is_locked_and_retryable(self):
        for stage in ['useradd', 'crash', 'seeds', 'seeds-after', 'key-repair', 'key-mode', 'plan', 'deliver-user', 'deliver-home-user', 'password', 'expiry-release']:
            with self.subTest(stage=stage):
                result = self.run_boot(stage)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(self.credential.exists())
                self.assertTrue(self.full_name.exists())
                self.assertEqual((self.root / 'expiry').read_text(), '1970-01-02')
                if stage != 'expiry-release':
                    self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
                else:
                    self.assertEqual((self.root / 'shadow').read_text(), HASH)
                created = (self.root / 'useradd').read_text()
                result = self.run_boot()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / 'shadow').read_text(), HASH)
                self.assertEqual((self.root / 'expiry').read_text(), '-1')
                self.assertEqual((self.root / 'useradd').read_text(), created)
                self.assertFalse(self.credential.exists())
                self.assertFalse((self.root / 'etc/install-user.transaction').exists())
                # Reset just this completed synthetic transaction for next case.
                (self.root / 'shadow').unlink()
                (self.root / 'expiry').unlink()
                self.credential.write_text('fixtureuser:' + HASH + '\n')
                self.credential.chmod(0o600)
                self.full_name.write_text('Fixture User\n')
                self.full_name.chmod(0o600)

    def test_termination_during_large_delivery_never_unlocks(self):
        result = self.run_boot('deliver-home-user', terminate=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        self.assertTrue(self.credential.exists())
        self.assertEqual((self.root / 'expiry').read_text(), '1970-01-02')
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'expiry').read_text(), '-1')

    def test_termination_before_release_is_retryable(self):
        for stage in ['key-repair', 'password', 'expiry-release']:
            with self.subTest(stage=stage):
                result = self.run_boot(stage, terminate=True)
                self.assertEqual(result.returncode, -15)
                self.assertEqual((self.root / 'expiry').read_text(), '1970-01-02')
                self.assertTrue(self.credential.exists())
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'expiry').read_text(), '-1')

    def test_intent_before_account_creation_can_retry(self):
        intent = self.root / 'etc/install-user.transaction'
        intent.write_text('fixtureuser:!tilo-firstboot-' + 'a' * 32 + '\n')
        intent.chmod(0o600)
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'shadow').read_text(), HASH)

    def test_retry_after_final_unlock_relocks_before_delivery(self):
        self.assertNotEqual(self.run_boot('seeds').returncode, 0)
        # Model interruption after expiry release but before seed cleanup.
        (self.root / 'shadow').write_text(HASH)
        (self.root / 'expiry').write_text('-1')
        result = self.run_boot('deliver-home-user')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        self.assertTrue(self.credential.exists())
        self.assertEqual((self.root / 'expiry').read_text(), '1970-01-02')
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'expiry').read_text(), '-1')

    def test_retry_expiry_failure_does_not_provision(self):
        self.assertNotEqual(self.run_boot('key-repair').returncode, 0)
        (self.root / 'shadow').write_text(HASH)
        (self.root / 'expiry').write_text('-1')
        result = self.run_boot('expiry-retry')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / 'shadow').read_text(), HASH)
        self.assertTrue(self.credential.exists())
        self.assertEqual(self.run_boot().returncode, 0)

    def test_unrelated_existing_account_preserves_seed_and_shadow(self):
        (self.root / 'shadow').write_text('!unrelated')
        (self.root / 'expiry').write_text('-1')
        result = self.run_boot()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(self.credential.exists())
        self.assertEqual((self.root / 'shadow').read_text(), '!unrelated')
        self.assertFalse((self.root / 'etc/install-user.transaction').exists())
        self.assertEqual((self.root / 'expiry').read_text(), '-1')

    def test_intent_does_not_adopt_wrong_shadow(self):
        self.assertNotEqual(self.run_boot('seeds').returncode, 0)
        (self.root / 'shadow').write_text('!unrelated')
        (self.root / 'expiry').write_text('-1')
        self.assertNotEqual(self.run_boot().returncode, 0)
        self.assertEqual((self.root / 'shadow').read_text(), '!unrelated')
        self.assertEqual((self.root / 'expiry').read_text(), '-1')
        self.assertTrue(self.credential.exists())

    def test_intent_is_durable_before_account_creation(self):
        self.assertNotEqual(self.run_boot('intent').returncode, 0)
        self.assertFalse((self.root / 'shadow').exists())
        self.assertTrue(self.credential.exists())
        self.assertEqual(self.run_boot().returncode, 0)

    def test_useradd_failure_before_creation_retries(self):
        self.assertNotEqual(self.run_boot('useradd-before').returncode, 0)
        self.assertFalse((self.root / 'shadow').exists())
        self.assertTrue(self.credential.exists())
        self.assertTrue((self.root / 'etc/install-user.transaction').exists())
        self.assertEqual(self.run_boot().returncode, 0)

    def test_seeded_home_keeps_installer_content_and_gets_skel(self):
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        home = self.root / 'home/fixtureuser'
        self.assertEqual((home / 'seeded').read_text(), 'installer')
        self.assertEqual((home / 'default').read_text(), 'generated')
        args = json.loads((self.root / 'useradd').read_text())
        self.assertIn('-M', args)
        self.assertIn('Fixture User', args)

    def test_unseeded_home_is_repaired_after_account_committed(self):
        import shutil
        shutil.rmtree(self.root / 'home/fixtureuser')
        result = self.run_boot('useradd')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'home/fixtureuser').exists())
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        home = self.root / 'home/fixtureuser'
        self.assertTrue(home.is_dir())
        self.assertEqual((home / 'default').read_text(), 'generated')
        self.assertIn('-M', json.loads((self.root / 'useradd').read_text()))
        self.assertEqual((home.stat().st_uid, home.stat().st_gid),
                         (os.getuid(), os.getgid()))

    def test_home_symlink_cannot_unlock_or_modify_outside(self):
        import shutil
        home = self.root / 'home/fixtureuser'
        shutil.rmtree(home)
        outside = self.root / 'outside'
        outside.mkdir()
        home.symlink_to(outside, target_is_directory=True)
        result = self.run_boot()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        self.assertEqual(list(outside.iterdir()), [])
        self.assertTrue(self.credential.exists())
        self.assertEqual((self.root / 'expiry').read_text(), '1970-01-02')

    def test_attested_output_tampering_blocks_retry_before_custody_or_unlock(self):
        result = self.run_boot('plan')
        self.assertNotEqual(result.returncode, 0)
        receipt = self.root / 'state/seeds' / (str(os.getuid()) + '.json')
        before = receipt.read_bytes()
        (self.root / 'home/fixtureuser/default').write_text('changed')
        result = self.run_boot()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('home defaults could not be attested', result.stderr)
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        self.assertTrue(self.credential.exists())
        self.assertEqual(receipt.read_bytes(), before)

    def test_service_allows_size_independent_transfer_duration(self):
        unit = (ROOT / 'os/usr/lib/systemd/system/tilo-firstboot-user.service').read_text()
        self.assertIn('TimeoutStartSec=infinity\n', unit)


class AtomicFilesTests(unittest.TestCase):
    def setUp(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location(
            'firstboot_files', ROOT / 'os/tests/oracles/tilo-firstboot-files.py')
        self.helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.helper)
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)

    def test_interrupted_file_copy_has_no_final_and_retry_is_complete(self):
        import io
        directory = os.open(self.root, self.helper.DIR)
        self.addCleanup(os.close, directory)
        class Interrupted(io.BytesIO):
            def read(self, size):
                if self.tell():
                    raise InterruptedError('copy interrupted')
                return super().read(3)
        with self.assertRaises(InterruptedError):
            self.helper.publish(directory, 'default', lambda name:
                self.helper.write_file(directory, name, Interrupted(b'complete'),
                                       0o640, os.getuid(), os.getgid()))
        self.assertFalse((self.root / 'default').exists())
        self.helper.publish(directory, 'default', lambda name:
            self.helper.write_file(directory, name, io.BytesIO(b'complete'),
                                   0o640, os.getuid(), os.getgid()))
        self.assertEqual((self.root / 'default').read_bytes(), b'complete')
        self.assertEqual((self.root / 'default').stat().st_mode & 0o777, 0o640)

    def test_truncated_intent_input_is_never_published(self):
        path = self.root / 'intent'
        helper = FILES_BINARY
        result = subprocess.run([str(helper), 'intent', str(path)],
                                input=b'fixtureuser:!tilo-firstboot-', capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(path.exists())
        complete = b'fixtureuser:!tilo-firstboot-' + b'a' * 32 + b'\n'
        result = subprocess.run([str(helper), 'intent', str(path)],
                                input=complete, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(path.read_bytes(), complete)

    def test_killed_partial_temporary_never_becomes_default_or_intent(self):
        import io
        code = '''import importlib.util, os, signal, sys
spec = importlib.util.spec_from_file_location('files', sys.argv[1])
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
directory = os.open(sys.argv[2], helper.DIR)
def interrupted(name):
    fd = os.open(name, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600, dir_fd=directory)
    os.write(fd, b'partial')
    os.fsync(fd)
    os.kill(os.getpid(), signal.SIGKILL)
helper.publish(directory, sys.argv[3], interrupted)
'''
        directory = os.open(self.root, self.helper.DIR)
        self.addCleanup(os.close, directory)
        for name in ['intent', 'default']:
            result = subprocess.run(['python3', '-c', code,
                                     str(ROOT / 'os/tests/oracles/tilo-firstboot-files.py'),
                                     str(self.root), name], capture_output=True)
            self.assertEqual(result.returncode, -9, result.stderr)
            self.assertFalse((self.root / name).exists())
            self.helper.publish(directory, name, lambda temporary:
                self.helper.write_file(directory, temporary, io.BytesIO(b'complete'),
                                       0o600, os.getuid(), os.getgid()))
            self.assertEqual((self.root / name).read_bytes(), b'complete')
            self.assertEqual((self.root / name).stat().st_nlink, 1)

    def test_truncated_intent_before_publication_never_blocks_retry(self):
        from unittest.mock import patch
        import io
        directory = os.open(self.root, self.helper.DIR)
        self.addCleanup(os.close, directory)
        def interrupted(name):
            (self.root / name).write_bytes(b'fixtureuser:!tilo-')
            raise InterruptedError('before publication')
        with self.assertRaises(InterruptedError):
            self.helper.publish(directory, 'intent', interrupted)
        self.assertFalse((self.root / 'intent').exists())
        calls = []
        fsync = os.fsync
        with patch.object(self.helper.os, 'fsync', side_effect=lambda fd:
                          (calls.append(os.fstat(fd).st_mode), fsync(fd))):
            self.helper.publish(directory, 'intent', lambda name:
                self.helper.write_file(directory, name, io.BytesIO(b'complete\n'),
                                       0o600, os.getuid(), os.getgid()))
        self.assertEqual((self.root / 'intent').read_bytes(), b'complete\n')
        self.assertEqual((self.root / 'intent').stat().st_nlink, 1)
        import stat
        self.assertTrue(stat.S_ISREG(calls[0]))
        self.assertTrue(stat.S_ISDIR(calls[1]))
        with self.assertRaises(FileExistsError):
            self.helper.publish(directory, 'intent', lambda name:
                (self.root / name).write_text('replacement'))
        self.assertEqual((self.root / 'intent').read_bytes(), b'complete\n')

    def test_partial_defaults_retry_preserves_seeds_links_modes_and_ownership(self):
        from unittest.mock import patch
        skel = self.root / 'skel'
        home = self.root / 'home'
        skel.mkdir()
        home.mkdir()
        (skel / 'first').write_text('one')
        (skel / 'second').write_text('two')
        (skel / 'second').chmod(0o640)
        (skel / 'seed').write_text('default')
        (home / 'seed').write_text('installer')
        (skel / 'link').symlink_to('../literal')
        src = os.open(skel, self.helper.DIR)
        dst = os.open(home, self.helper.DIR)
        self.addCleanup(os.close, src)
        self.addCleanup(os.close, dst)
        publish = self.helper.publish
        def interrupt(directory, name, writer):
            if name == 'second':
                raise InterruptedError('between defaults')
            publish(directory, name, writer)
        with patch.object(self.helper.os, 'listdir', return_value=['first', 'second']), \
                patch.object(self.helper, 'publish', side_effect=interrupt):
            with self.assertRaises(InterruptedError):
                self.helper.defaults(src, dst, os.getuid(), os.getgid())
        self.assertEqual((home / 'first').read_text(), 'one')
        self.assertFalse((home / 'second').exists())
        self.helper.defaults(src, dst, os.getuid(), os.getgid())
        self.helper.ownership(dst, os.getuid(), os.getgid())
        self.assertEqual((home / 'seed').read_text(), 'installer')
        self.assertEqual((home / 'second').read_text(), 'two')
        self.assertEqual((home / 'second').stat().st_mode & 0o777, 0o640)
        self.assertEqual(os.readlink(home / 'link'), '../literal')
        for file in home.iterdir():
            self.assertEqual((file.lstat().st_uid, file.lstat().st_gid),
                             (os.getuid(), os.getgid()))

    def test_destination_directory_symlink_is_never_followed(self):
        skel = self.root / 'skel'
        home = self.root / 'home'
        outside = self.root / 'outside'
        (skel / 'nested').mkdir(parents=True)
        (skel / 'nested/default').write_text('default')
        home.mkdir()
        outside.mkdir()
        (home / 'nested').symlink_to(outside)
        src = os.open(skel, self.helper.DIR)
        dst = os.open(home, self.helper.DIR)
        self.addCleanup(os.close, src)
        self.addCleanup(os.close, dst)
        with self.assertRaises(OSError):
            self.helper.defaults(src, dst, os.getuid(), os.getgid())
        self.assertEqual(list(outside.iterdir()), [])

class HomeSeedTests(unittest.TestCase):
    """Real seed I/O; substitute only root authority for unprivileged fixtures."""
    def setUp(self):
        import importlib.util
        from unittest.mock import patch
        import stat
        spec = importlib.util.spec_from_file_location(
            'home_seeds', ROOT / 'os/tests/oracles/tilo-firstboot-home-seeds.py')
        self.helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.helper)
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.home = self.root / 'home'
        self.skel = self.root / 'skel'
        self.skel.mkdir()
        self.proofs = self.root / 'state/proofs'
        self.proofs.parent.mkdir()
        (self.skel / 'default').write_text('generated')
        (self.skel / 'migrated').write_text('matching')
        self.home.mkdir()
        (self.home / 'migrated').write_text('matching')
        def fixture_authority(path):
            info = path.lstat()
            if info.st_uid != os.getuid() or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
                raise ValueError('untrusted fixture authority')
        writer = self.helper.atomic.write_file
        def fixture_writer(directory, name, source, mode, uid, gid):
            writer(directory, name, source, mode,
                   os.getuid() if uid == 0 else uid, os.getgid() if gid == 0 else gid)
        for mock in [patch.object(self.helper.os, 'geteuid', return_value=0),
                     patch.object(self.helper, 'trusted', side_effect=fixture_authority),
                     patch.object(self.helper.atomic, 'write_file', side_effect=fixture_writer)]:
            mock.start()
            self.addCleanup(mock.stop)

    def seed(self):
        return self.helper.seed(self.home, self.skel, os.getuid(), os.getgid(), self.proofs)

    def test_existing_receipt_verifies_and_resumes_without_new_proofs(self):
        body = self.seed()
        self.assertEqual([entry['path'] for entry in body['files']], ['default'])
        before = (self.proofs / (str(os.getuid()) + '.json')).read_bytes()
        # The same ownership pass used by firstboot must not change witnesses.
        fd = os.open(self.home, self.helper.atomic.DIR)
        try:
            self.helper.atomic.ownership(fd, os.getuid(), os.getgid())
        finally:
            os.close(fd)
        (self.skel / 'later').write_text('not in original transaction')
        self.assertEqual(self.seed(), body)
        self.assertFalse((self.home / 'later').exists())
        self.assertEqual((self.proofs / (str(os.getuid()) + '.json')).read_bytes(), before)
        self.assertEqual((self.home / 'migrated').read_text(), 'matching')

    def test_changed_attested_output_fails_closed(self):
        self.seed()
        (self.home / 'default').write_text('tampered')
        with self.assertRaises(ValueError):
            self.seed()

    def test_redirected_attested_output_fails_closed(self):
        self.seed()
        (self.home / 'default').unlink()
        (self.home / 'default').symlink_to(self.skel / 'default')
        with self.assertRaises((ValueError, OSError)):
            self.seed()

    def test_partial_file_and_receipt_never_publish(self):
        from unittest.mock import patch
        def interrupted(directory, name, source, mode, uid, gid):
            fd = os.open(name, os.O_CREAT | os.O_EXCL | os.O_WRONLY,
                         0o600, dir_fd=directory)
            try:
                os.write(fd, b'partial')
            finally:
                os.close(fd)
            raise InterruptedError('interrupted write')
        with patch.object(self.helper.atomic, 'write_file', side_effect=interrupted):
            with self.assertRaises(InterruptedError):
                self.seed()
        self.assertFalse((self.home / 'default').exists())
        self.assertEqual(list(self.proofs.iterdir()), [])
        self.assertEqual([entry['path'] for entry in self.seed()['files']], ['default'])

    def test_receipt_interruption_does_not_fabricate_migrated_file_proofs(self):
        from unittest.mock import patch
        publish = self.helper.publish_bytes
        def interrupted(directory, name, data, mode, uid, gid):
            if directory == self.proofs:
                # Fail during the actual atomic write, not before the call.
                def partial(fd, temporary, source, mode, uid, gid):
                    descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY,
                                         0o600, dir_fd=fd)
                    os.write(descriptor, b'{"schema":')
                    os.close(descriptor)
                    raise InterruptedError('receipt interrupted')
                with patch.object(self.helper.atomic, 'write_file', side_effect=partial):
                    publish(directory, name, data, mode, uid, gid)
            else:
                publish(directory, name, data, mode, uid, gid)
        with patch.object(self.helper, 'publish_bytes', side_effect=interrupted):
            with self.assertRaises(InterruptedError):
                self.seed()
        self.assertEqual((self.home / 'default').read_text(), 'generated')
        self.assertEqual(list(self.proofs.iterdir()), [])
        # No durable receipt means no provenance: neither matching existing file
        # may be fabricated into an automatically replaceable default.
        self.assertEqual(self.seed()['files'], [])

    def test_truncated_existing_receipt_fails_closed(self):
        self.seed()
        (self.proofs / (str(os.getuid()) + '.json')).write_text('{"schema":')
        with self.assertRaises(ValueError):
            self.seed()


if __name__ == '__main__':
    unittest.main()
