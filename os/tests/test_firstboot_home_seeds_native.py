#!/usr/bin/env python3
"""Exercise the production seed executable in an isolated mount namespace.

Run with --binary target/debug/tilo-firstboot-home-seeds. The launcher needs
root (or passwordless sudo) and mount namespaces; missing isolation is an error,
not a skipped test. Python is only the test driver and compatibility oracle.
No fixtures, account entries or mounts escape the private namespace.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
ACCOUNT = 'tilo_seed_native_fixture'
UID = 19991
GID = 19992
LIMIT = 4 << 20


def command(*args):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


class NativeSeeds(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory(prefix='home-seeds-native-', dir=os.environ.get('TMPDIR'))
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.home = self.root / 'home'
        self.skel = self.root / 'skel'
        self.state = self.root / 'state'
        self.skel.mkdir()
        self.state.mkdir()
        (self.state / 'tilo').mkdir()
        self.passwd = self.root / 'passwd'
        self.passwd.write_text(f'{ACCOUNT}:x:{UID}:{GID}::{self.home}:/bin/sh\n')
        self.mount(self.skel, '/etc/skel')
        self.mount(self.state, '/var/lib')
        self.mount(self.passwd, '/etc/passwd')
        self.proof = self.state / f'tilo/firstboot-home-seeds/{UID}.json'

    def mount(self, source, target):
        command('mount', '--bind', str(source), target)
        self.addCleanup(lambda: command('umount', target))

    def run_seed(self, *args, oracle=False, ok=True, file_limit=None):
        executable = [sys.executable, str(ROOT / 'os/tests/oracles/tilo-firstboot-home-seeds.py')] if oracle else [BINARY]
        def limit_writes():
            signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
            resource.setrlimit(resource.RLIMIT_FSIZE, (file_limit, file_limit))
        result = subprocess.run(executable + list(args or [ACCOUNT]), capture_output=True,
                                preexec_fn=limit_writes if file_limit is not None else None)
        self.assertEqual(result.stdout, b'')
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
            self.assertEqual(result.stderr, b'')
        else:
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertTrue(result.stderr.startswith(b'firstboot home defaults: '), result.stderr)
        return result

    def body(self):
        return json.loads(self.proof.read_bytes())

    def write_proof(self, body):
        self.proof.write_text(json.dumps(body))

    def default(self, name='default', content=b'generated'):
        path = self.skel / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def assert_no_temporary(self):
        self.assertEqual(list(self.root.rglob('.tilo-firstboot-*')), [])

    def test_cli_capabilities_and_errors_match_oracle(self):
        oracle = [sys.executable, str(ROOT / 'os/tests/oracles/tilo-firstboot-home-seeds.py')]
        for args in [['--capabilities'], [], ['one', 'two'], ['tilo_no_such_native_fixture']]:
            expected = subprocess.run(oracle + args, capture_output=True)
            actual = subprocess.run([BINARY] + args, capture_output=True)
            self.assertEqual((actual.returncode, actual.stdout, actual.stderr),
                             (expected.returncode, expected.stdout, expected.stderr))

    def test_unprivileged_creation_fails_before_changing_any_home(self):
        # Copy the exact production artifact so an inaccessible build-directory
        # ancestor cannot turn this into a test of exec permissions instead.
        self.root.chmod(0o755)
        native = self.root / 'native'
        shutil.copyfile(BINARY, native)
        native.chmod(0o755)
        oracle = self.root / 'tilo-firstboot-home-seeds.py'
        shutil.copyfile(ROOT / 'os/tests/oracles/tilo-firstboot-home-seeds.py', oracle)
        shutil.copyfile(ROOT / 'os/tests/oracles/tilo-firstboot-files.py', self.root / 'tilo-firstboot-files.py')
        # /run is private to this namespace and avoids private TMPDIR ancestors.
        self.mount(self.root, '/run')
        def demote():
            os.setgroups([])
            os.setgid(GID)
            os.setuid(UID)
        for executable in [['/run/native'], [sys.executable, '/run/tilo-firstboot-home-seeds.py']]:
            result = subprocess.run(executable + [ACCOUNT], capture_output=True, preexec_fn=demote)
            self.assertEqual((result.returncode, result.stdout, result.stderr),
                             (1, b'', b'firstboot home defaults: firstboot seed creation requires root\n'))
        self.assertFalse(self.home.exists())
        self.assertFalse(self.proof.exists())

    def test_native_creates_oracle_verifiable_receipt_and_resumes(self):
        self.default()
        self.default('nested/second', b'\x00\xffbinary\n')
        self.default('migrated', b'matching')
        self.home.mkdir()
        (self.home / 'migrated').write_bytes(b'matching')
        os.chmod(self.skel / 'nested', 0o751)
        os.chmod(self.skel / 'default', 0o6750)
        self.run_seed()
        body = self.body()
        self.assertEqual([f['path'] for f in body['files']], ['default', 'nested/second'])
        self.assertEqual((self.home.stat().st_uid, self.home.stat().st_gid), (UID, GID))
        self.assertEqual((self.home / 'nested').stat().st_mode & 0o7777, 0o751)
        self.assertEqual((self.home / 'default').stat().st_mode & 0o7777, 0o750)
        self.assertEqual(self.proof.stat().st_uid, 0)
        self.assertEqual(self.proof.stat().st_mode & 0o7777, 0o644)
        for entry in body['files']:
            path = self.home / entry['path']
            self.assertEqual(entry['sha256'], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(entry['stamp']['inode'], path.stat().st_ino)
            self.assertEqual(entry['stamp']['uid'], UID)
            self.assertEqual(entry['stamp']['ctime_ns'], path.stat().st_ctime_ns)
            self.assertEqual(entry['stamp']['mtime_ns'], path.stat().st_mtime_ns)
        receipt = self.proof.read_bytes()
        self.run_seed(oracle=True)
        self.default('later')
        self.run_seed()
        self.assertFalse((self.home / 'later').exists())
        self.assertEqual(self.proof.read_bytes(), receipt)
        self.assert_no_temporary()

    def test_non_utf8_names_and_literal_escapes_round_trip_through_both_receipts(self):
        names = [b'invalid-\xff-\x80', b'literal-\\udc80', 'unicode-😀-\U000f0000'.encode(), b'quote-"-newline-\n']
        for name in names:
            with open(os.fsencode(self.skel) + b'/' + name, 'wb') as stream:
                stream.write(name)
        self.run_seed()
        self.run_seed(oracle=True)
        self.assertEqual({os.fsencode(item['path']) for item in self.body()['files']}, set(names))
        for name in names:
            self.assertEqual(Path(os.fsdecode(os.fsencode(self.home) + b'/' + name)).read_bytes(), name)
        self.proof.unlink()
        for path in self.home.iterdir():
            path.unlink()
        self.run_seed(oracle=True)
        self.run_seed()

    def test_native_verifies_oracle_receipt(self):
        self.default()
        self.run_seed(oracle=True)
        before = self.proof.read_bytes()
        self.run_seed()
        self.assertEqual(self.proof.read_bytes(), before)

    def test_fresh_home_mode_and_empty_skeleton(self):
        self.run_seed()
        self.assertEqual(self.home.stat().st_mode & 0o7777, 0o700)
        self.assertEqual(self.body()['files'], [])

    def test_links_and_custody_are_seeded_but_never_attested(self):
        self.default('ordinary')
        self.default('folder/default')
        for name in ['.config/tilo/identity', '.local/state/tilo/secret']:
            self.default(name)
        (self.skel / 'directory-link').symlink_to('folder')
        (self.skel / 'dangling').symlink_to('../absent')
        (self.skel / 'absolute').symlink_to('/outside/untouched')
        self.run_seed()
        self.assertEqual([f['path'] for f in self.body()['files']], ['ordinary', 'folder/default'])
        for name in ['directory-link', 'dangling', 'absolute']:
            self.assertEqual(os.readlink(self.home / name), os.readlink(self.skel / name))
            self.assertEqual((self.home / name).lstat().st_uid, UID)
        (self.home / '.config/tilo/identity').write_text('user custody')
        self.run_seed()
        self.run_seed(oracle=True)

    def test_existing_destination_links_and_blocked_directories_stay_untouched(self):
        self.default('file')
        self.default('nested/child')
        self.home.mkdir()
        outside = self.root / 'outside'
        outside.mkdir()
        (self.home / 'nested').symlink_to(outside)
        (self.home / 'file').symlink_to('/missing')
        self.run_seed()
        self.assertEqual(list(outside.iterdir()), [])
        self.assertEqual(os.readlink(self.home / 'file'), '/missing')
        self.assertEqual(self.body()['files'], [])

    def test_home_symlink_is_rejected_without_ownership_change(self):
        outside = self.root / 'outside'
        outside.mkdir()
        self.home.symlink_to(outside)
        self.run_seed(ok=False)
        self.assertEqual(outside.stat().st_uid, 0)
        self.assertFalse(self.proof.exists())

    def test_changed_attested_files_fail_both_verifiers(self):
        self.default()
        self.run_seed()
        target = self.home / 'default'
        original = target.read_bytes()
        for change in ['contents', 'mode', 'uid', 'hardlink', 'symlink', 'fifo']:
            with self.subTest(change=change):
                # A fresh proof is needed because even restoring content changes ctime.
                self.proof.unlink()
                target.unlink()
                self.run_seed()
                if change == 'contents':
                    target.write_bytes(original + b'changed')
                elif change == 'mode':
                    target.chmod(0o600)
                elif change == 'uid':
                    os.chown(target, UID + 1, GID)
                elif change == 'hardlink':
                    os.link(target, self.home / 'extra-link')
                elif change == 'symlink':
                    target.unlink()
                    target.symlink_to(self.skel / 'default')
                else:
                    target.unlink()
                    os.mkfifo(target)
                self.run_seed(ok=False)
                self.run_seed(oracle=True, ok=False)
                extra = self.home / 'extra-link'
                if extra.exists():
                    extra.unlink()

    def test_invalid_receipt_paths_and_inventory_fail_closed(self):
        self.default()
        self.run_seed()
        original = self.body()
        for path in ['', '.', './default', 'default/', 'a//b', '../default', '/default',
                     '.config/tilo', '.config/tilo/identity', '.local/state/tilo/secret']:
            with self.subTest(path=path):
                body = json.loads(json.dumps(original))
                body['files'][0]['path'] = path
                self.write_proof(body)
                self.run_seed(ok=False)
                self.run_seed(oracle=True, ok=False)
        for key in ['uid', 'home_inode', 'filesystem', 'schema']:
            body = json.loads(json.dumps(original))
            body[key] += 1
            self.write_proof(body)
            self.run_seed(ok=False)
        body = json.loads(json.dumps(original))
        body['files'] *= 2
        self.write_proof(body)
        self.run_seed(ok=False)
        body['files'] = [{}] * 4097
        self.write_proof(body)
        self.run_seed(ok=False)
        self.proof.write_bytes(b'{"schema":')
        self.run_seed(ok=False)
        self.proof.write_bytes(b' ' * (LIMIT + 1))
        self.run_seed(ok=False)

    def test_attested_parent_redirection_fails_without_following_it(self):
        self.default('nested/default')
        self.run_seed()
        (self.home / 'nested').rename(self.root / 'original-directory')
        (self.home / 'nested').symlink_to(self.root / 'original-directory')
        self.run_seed(ok=False)
        self.run_seed(oracle=True, ok=False)
        self.assertEqual((self.root / 'original-directory/default').read_bytes(), b'generated')

    def test_receipt_links_and_nonroot_authority_fail_closed(self):
        self.default()
        self.run_seed()
        os.chown(self.proof, UID, GID)
        self.run_seed(ok=False)
        os.chown(self.proof, 0, 0)
        self.proof.chmod(0o666)
        self.run_seed(ok=False)
        self.proof.chmod(0o644)
        os.link(self.proof, self.root / 'extra-proof')
        self.run_seed(ok=False)
        (self.root / 'extra-proof').unlink()
        self.proof.rename(self.root / 'real-proof')
        self.proof.symlink_to(self.root / 'real-proof')
        self.run_seed(ok=False)

    def test_untrusted_skeleton_and_foreign_symlink_fail_closed(self):
        self.default()
        self.skel.chmod(0o777)
        self.run_seed(ok=False)
        self.assertFalse((self.home / 'default').exists())
        self.skel.chmod(0o755)
        (self.skel / 'default').unlink()
        link = self.skel / 'link'
        link.symlink_to('missing')
        os.chown(link, UID, GID, follow_symlinks=False)
        self.run_seed(ok=False)
        self.assertFalse((self.home / 'link').is_symlink())

    def test_interrupted_inventory_retry_does_not_adopt_published_files(self):
        self.default('a-complete')
        os.mkfifo(self.skel / 'z-unsupported')
        self.run_seed(ok=False)
        self.assertEqual((self.home / 'a-complete').read_bytes(), b'generated')
        self.assertFalse(self.proof.exists())
        self.assert_no_temporary()
        (self.skel / 'z-unsupported').unlink()
        self.run_seed()
        self.assertEqual(self.body()['files'], [])

    def test_receipt_publication_failure_is_atomic_and_retryable(self):
        self.default()
        # A real short write interrupts the receipt after defaults are durable.
        self.run_seed(ok=False, file_limit=128)
        self.assertEqual((self.home / 'default').read_bytes(), b'generated')
        self.assertFalse(self.proof.exists())
        self.assert_no_temporary()
        self.run_seed()
        self.assertEqual(self.body()['files'], [])

    def test_file_publication_failure_never_leaves_final_or_temporary(self):
        self.default(content=b'x' * 4096)
        self.run_seed(ok=False, file_limit=1024)
        self.assertFalse((self.home / 'default').exists())
        self.assertFalse(self.proof.exists())
        self.assert_no_temporary()
        self.run_seed()
        self.assertEqual([f['path'] for f in self.body()['files']], ['default'])

    def test_parallel_publications_are_no_replace_and_retryable(self):
        for index in range(32):
            self.default(f'file{index:02}')
        writers = [subprocess.Popen([BINARY, ACCOUNT], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                   for _ in range(2)]
        for writer in writers:
            stdout, stderr = writer.communicate(timeout=20)
            self.assertEqual(stdout, b'')
            self.assertIn(writer.returncode, [0, 1])
            if writer.returncode:
                self.assertTrue(stderr.startswith(b'firstboot home defaults: '), stderr)
        self.run_seed()
        self.run_seed(oracle=True)
        self.assert_no_temporary()
        self.assertEqual({path.name for path in self.home.iterdir()},
                         {path.name for path in self.skel.iterdir()})

    def test_file_size_and_file_count_bounds(self):
        huge = self.default()
        with huge.open('wb') as stream:
            stream.truncate(LIMIT + 1)
        self.run_seed(ok=False)
        self.assertFalse((self.home / 'default').exists())
        huge.unlink()
        for index in range(4097):
            self.default(f'f{index:04}')
        self.run_seed(ok=False)
        self.assertFalse(self.proof.exists())
        self.assertEqual(len(list(self.home.iterdir())), 4096)
        self.assert_no_temporary()
        (self.skel / 'f4096').unlink()
        self.run_seed()
        self.assertEqual(self.body()['files'], [])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--inside-namespace', action='store_true', help=argparse.SUPPRESS)
    args, remaining = parser.parse_known_args()
    BINARY = str(Path(args.binary).resolve(strict=True))
    if not os.environ.get('TMPDIR'):
        raise SystemExit('set TMPDIR to a disk-backed scratch directory for native seed tests')
    if not args.inside_namespace:
        prefix = [] if os.geteuid() == 0 else ['sudo', '-n']
        raise SystemExit(subprocess.call(prefix + ['unshare', '--mount', '--fork', '--propagation', 'private',
            'env', 'TMPDIR=' + os.environ['TMPDIR'], sys.executable, str(Path(__file__).resolve()), '--binary', BINARY,
            '--inside-namespace'] + remaining))
    if os.geteuid() != 0 or os.readlink('/proc/self/ns/mnt') == os.readlink('/proc/1/ns/mnt'):
        raise SystemExit('native firstboot tests require root in a private mount namespace')
    unittest.main(argv=[sys.argv[0]] + remaining)
