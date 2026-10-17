"""Exercise reseal command boundaries without executing image code or builds."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
IMAGE = 'sha256:' + '1' * 64


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


class ResealCommands(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in ('reseal-image.sh', '_common.sh'):
            (self.root / name).write_text((REPO / 'os/dev' / name).read_text())
        (self.root / 'podman').write_text('''#!/usr/bin/python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps(args) + '\\n')
if args[0] == '--remote=false': args = args[1:]
if args[:2] == ['image', 'inspect']:
    print(json.dumps([{'Id': os.environ.get('INSPECT_ID', 'sha256:' + '1'*64),
                      'Architecture': os.environ.get('ARCH', 'arm64'), 'Os': 'linux'}]))
elif args[:2] == ['build', '--help']:
    print('--source-date-epoch' if os.environ.get('PODMAN_MAJOR', '5') == '5' else '--timestamp')
elif args[0] == 'create':
    print('fixture-container')
elif args[0] == 'cp':
    if '/boot/EFI/Linux/.' in args[1]:
        open(os.path.join(args[2], '7.2.7.efi'), 'wb').write(b'fixture')
    else:
        sys.exit(1)
elif args[0] in ('rm', 'run'):
    pass
elif args[0] == 'build':
    if '--iidfile' in args:
        open(args[args.index('--iidfile')+1], 'w').write('sha256:' + '2'*64)
    open(os.environ['CONTAINERFILE'], 'w').write(sys.stdin.read())
elif args[:2] != ['image', 'exists']: sys.exit(99)
''')
        (self.root / 'podman').chmod(0o755)
        (self.root / 'uname').write_text('#!/bin/sh\necho aarch64\n')
        (self.root / 'uname').chmod(0o755)
        # Supply only the common helpers and path needed by this script.
        common = (REPO / 'os/dev/_common.sh').read_text()
        helper = common[common.index('podman_epoch_args() {'):]
        helper = helper[:helper.index('\n}\n') + 3]
        (self.root / '_common.sh').write_text(
            'TILO_OS_DIR="$FIXTURE"\ndie() { echo "$*" >&2; exit 1; }\n' + helper)
        self.env = {'PATH': f'{self.root}:/usr/bin:/bin', 'HOME': str(self.root),
                    'CALLS': str(self.root / 'calls'), 'FIXTURE': str(self.root),
                    'CONTAINERFILE': str(self.root / 'Containerfile')}

    def run_script(self, immutable=True, image=IMAGE, epoch='1700000000', **environment):
        args = [image, 'a' * 128]
        if immutable:
            args += ['--iidfile', str(self.root / 'result.iid')]
            if epoch is not None:
                args += ['--source-date-epoch', epoch]
        result = subprocess.run(['bash', str(self.root / 'reseal-image.sh'), *args],
                                env=self.env | environment, capture_output=True, text=True)
        path = self.root / 'calls'
        calls = [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
        return result, calls

    def test_immutable_mode(self):
        with self.subTest(phase="immutable_mode_never_retags_or_fetches"):
            result, calls = self.run_script()
            self.assertEqual(result.returncode, 0, result.stderr)
            build = next(call for call in calls if 'build' in call and '--help' not in call)
            for flag in ('--remote=false', '--pull=never', '--network=none', '--iidfile'):
                self.assertIn(flag, build)
            self.assertNotIn('-t', build)
            self.assertIn('BASE_IMAGE=' + IMAGE, build)
            containerfile = (self.root / 'Containerfile').read_text()
            self.assertIn('COPY uki.efi /boot/EFI/Linux/7.2.7.efi', containerfile)
            self.assertNotIn('RUN ', containerfile)
            self.assertTrue(any('create' in call for call in calls))
            self.assertTrue(any('cp' in call for call in calls))
            # The digest is rewritten by the image's own editor, offline.
            edit = next(call for call in calls if 'run' in call)
            self.assertEqual(edit[-4:], [IMAGE, 'set', 'composefs=' + 'a' * 128, '/stage/uki.efi'])
            for flag in ('--rm', '--pull=never', '--network=none'):
                self.assertIn(flag, edit)
            self.assertEqual(edit[edit.index('--entrypoint') + 1], '/usr/libexec/tilo-uki-cmdline')
        restart_fixture(self)
        with self.subTest(phase="podman5_clamps_to_the_epoch"):
            result, calls = self.run_script()
            self.assertEqual(result.returncode, 0, result.stderr)
            build = next(call for call in calls if 'build' in call and '--help' not in call)
            self.assertEqual(build[build.index('--source-date-epoch') + 1], '1700000000')
            self.assertIn('--rewrite-timestamp', build)
            self.assertNotIn('--timestamp', build)
        restart_fixture(self)
        with self.subTest(phase="podman4_pins_timestamps_to_the_epoch"):
            for immutable in (True, False):
                with self.subTest(immutable=immutable):
                    (self.root / 'calls').unlink(missing_ok=True)
                    (self.root / 'result.iid').unlink(missing_ok=True)
                    result, calls = self.run_script(immutable=immutable, PODMAN_MAJOR='4',
                                                    SOURCE_DATE_EPOCH='1700000000')
                    self.assertEqual(result.returncode, 0, result.stderr)
                    build = next(call for call in calls if 'build' in call and '--help' not in call)
                    self.assertEqual(build[build.index('--timestamp') + 1], '1700000000')
                    self.assertNotIn('--source-date-epoch', build)
                    self.assertNotIn('--rewrite-timestamp', build)
        restart_fixture(self)
        with self.subTest(phase="preserves_existing_receipt"):
            path = self.root / 'result.iid'
            path.write_text('previous result')
            result, calls = self.run_script()
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(path.read_text(), 'previous result')
            self.assertEqual(calls, [])
        restart_fixture(self)
        with self.subTest(phase="requires_explicit_canonical_epoch"):
            for epoch in (None, '01', '-1', '253402300800'):
                with self.subTest(epoch=epoch):
                    result, calls = self.run_script(epoch=epoch, SOURCE_DATE_EPOCH='1700000000')
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('explicit canonical source date epoch', result.stderr)
                    self.assertEqual(calls, [])

    def test_immutable_mode_rejections(self):
        with self.subTest(phase="rejects_foreign_architecture_before_build"):
            result, calls = self.run_script(ARCH='amd64')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('native architecture differs', result.stderr)
            self.assertFalse(any('build' in call for call in calls))
        restart_fixture(self)
        with self.subTest(phase="rejects_wrong_local_identity"):
            result, calls = self.run_script(INSPECT_ID='sha256:' + '3' * 64)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('identity or native architecture differs', result.stderr)
            self.assertFalse(any('build' in call for call in calls))
        restart_fixture(self)
        with self.subTest(phase="rejects_remote_configuration"):
            result, calls = self.run_script(CONTAINER_HOST='ssh://elsewhere')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('remote container configuration', result.stderr)
            self.assertEqual(calls, [])
        restart_fixture(self)
        with self.subTest(phase="rejects_mutable_reference"):
            result, calls = self.run_script(image='localhost/tilo:latest')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('full local image ID', result.stderr)
            self.assertEqual(calls, [])

    def test_legacy_mode(self):
        with self.subTest(phase="legacy_call_still_retags"):
            result, calls = self.run_script(immutable=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            build = next(call for call in calls if 'build' in call and '--help' not in call)
            self.assertEqual(build[build.index('-t') + 1], IMAGE)
        restart_fixture(self)
        with self.subTest(phase="empty_options_cannot_fall_back_to_mutable_legacy_mode"):
            for options in (['--iidfile', ''], ['--source-date-epoch', ''],
                            ['--iidfile', '', '--source-date-epoch', '']):
                with self.subTest(options=options):
                    result = subprocess.run(
                        ['bash', str(self.root / 'reseal-image.sh'),
                         'localhost/tilo:latest', 'a' * 128, *options],
                        env=self.env, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('empty option value', result.stderr)
                    self.assertFalse((self.root / 'calls').exists())


if __name__ == '__main__':
    unittest.main()
