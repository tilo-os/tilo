"""Exercise reseal command boundaries without executing image code or builds."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
IMAGE = 'sha256:' + '1' * 64


class ResealCommands(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in ('reseal-image.sh', '_common.sh', 'uki-cmdline.py'):
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
elif args[0] == 'build':
    if '--iidfile' in args:
        open(args[args.index('--iidfile')+1], 'w').write('sha256:' + '2'*64)
    sys.stdin.read()
elif args[:2] != ['image', 'exists']: sys.exit(99)
''')
        (self.root / 'podman').chmod(0o755)
        (self.root / 'uname').write_text('#!/bin/sh\necho aarch64\n')
        (self.root / 'uname').chmod(0o755)
        # Supply only the common helpers and path needed by this script.
        (self.root / '_common.sh').write_text('TILO_OS_DIR="$FIXTURE"\ndie() { echo "$*" >&2; exit 1; }\n')
        (self.root / 'dev').mkdir()
        (self.root / 'dev/uki-cmdline.py').write_text('')
        self.env = {'PATH': f'{self.root}:/usr/bin:/bin', 'HOME': str(self.root),
                    'CALLS': str(self.root / 'calls'), 'FIXTURE': str(self.root)}

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

    def test_immutable_mode_never_retags_or_fetches(self):
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        build = next(call for call in calls if 'build' in call)
        for flag in ('--remote=false', '--pull=never', '--network=none', '--iidfile'):
            self.assertIn(flag, build)
        self.assertNotIn('-t', build)
        self.assertIn('BASE_IMAGE=' + IMAGE, build)

    def test_rejects_foreign_architecture_before_build(self):
        result, calls = self.run_script(ARCH='amd64')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('native architecture differs', result.stderr)
        self.assertFalse(any('build' in call for call in calls))

    def test_rejects_wrong_local_identity(self):
        result, calls = self.run_script(INSPECT_ID='sha256:' + '3' * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('identity or native architecture differs', result.stderr)
        self.assertFalse(any('build' in call for call in calls))

    def test_rejects_remote_configuration(self):
        result, calls = self.run_script(CONTAINER_HOST='ssh://elsewhere')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('remote container configuration', result.stderr)
        self.assertEqual(calls, [])

    def test_requires_explicit_canonical_epoch(self):
        for epoch in (None, '01', '-1', '253402300800'):
            with self.subTest(epoch=epoch):
                result, calls = self.run_script(epoch=epoch, SOURCE_DATE_EPOCH='1700000000')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('explicit canonical source date epoch', result.stderr)
                self.assertEqual(calls, [])

    def test_rejects_mutable_reference(self):
        result, calls = self.run_script(image='localhost/tilo:latest')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('full local image ID', result.stderr)
        self.assertEqual(calls, [])

    def test_preserves_existing_receipt(self):
        path = self.root / 'result.iid'
        path.write_text('previous result')
        result, calls = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(path.read_text(), 'previous result')
        self.assertEqual(calls, [])

    def test_legacy_call_still_retags(self):
        result, calls = self.run_script(immutable=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        build = next(call for call in calls if 'build' in call)
        self.assertEqual(build[build.index('-t') + 1], IMAGE)

    def test_empty_options_cannot_fall_back_to_mutable_legacy_mode(self):
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
