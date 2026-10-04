"""Regression coverage for Apple inline requirements and interpreter preflight.

Apple's csreq parser is the only real platform tool used here. No test signs,
queries identities, touches Keychain, mounts an image or launches an application.
"""

import ast
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
MACOS = ROOT / 'dist/macos'
sys.path.insert(0, str(MACOS))
import collection_native as native
import collection_stages as stages


IDENTITY = {'sha1': 'A1' * 20, 'team': 'TEAM123456',
            'name': 'Developer ID Application: Fixture Developer (TEAM123456)'}
REQUIREMENT = f'=anchor apple generic and certificate leaf = H"{IDENTITY["sha1"]}"'
DETAILS = ('CodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=1+7\n'
           f'Authority={IDENTITY["name"]}\n'
           f'TeamIdentifier={IDENTITY["team"]}\n'
           'Timestamp=Oct 4, 2026 at 12:00:00\n')
APP = Path('/fixture path/Example.app')
MACHOS = [APP / 'Contents/MacOS/Example', APP / 'Contents/Frameworks/library.dylib']


class SignatureRequirementTests(unittest.TestCase):
    def verify_app(self, details=DETAILS, architecture='arm64'):
        def response(command):
            return details if '-d' in command else ''

        with patch.object(native, 'machos', return_value=MACHOS), \
                patch.object(native, 'run', side_effect=response) as run:
            native.verify_signatures(APP, IDENTITY, 'distribution', architecture)
        return [call.args[0] for call in run.call_args_list]

    def test_each_nested_object_and_bundle_use_exact_inline_requirement(self):
        commands = self.verify_app()
        checked = [command for command in commands if '-R' in command]
        self.assertEqual([command[-1] for command in checked], MACHOS + [APP])
        for command in checked:
            self.assertEqual(command[command.index('-R') + 1], REQUIREMENT)
            self.assertIn('--strict', command)
            self.assertIn('--all-architectures', command)
        self.assertEqual(commands[-1], ['/usr/bin/codesign', '--verify', '--deep',
                                        '--strict', '--all-architectures', APP])
        self.assertTrue(all('--sign' not in command for command in commands))

    def test_universal_verifies_metadata_for_both_architectures(self):
        commands = self.verify_app(architecture='universal')
        displays = [command for command in commands if '-d' in command]
        self.assertEqual([(command[-1], command[command.index('--arch') + 1])
                          for command in displays],
                         [(item, arch) for item in MACHOS + [APP]
                          for arch in ('arm64', 'x86_64')])

    def test_wrong_team_rejected(self):
        with self.assertRaisesRegex(ValueError, 'wrong signing team/authority'):
            self.verify_app(DETAILS.replace('TeamIdentifier=TEAM123456',
                                            'TeamIdentifier=OTHER12345'))

    def test_wrong_authority_rejected(self):
        with self.assertRaisesRegex(ValueError, 'wrong signing team/authority'):
            self.verify_app(DETAILS.replace('Authority=Developer ID Application:',
                                            'Authority=Apple Development:'))

    def test_missing_timestamp_rejected_for_distribution(self):
        with self.assertRaisesRegex(ValueError, 'secure timestamp missing'):
            self.verify_app('\n'.join(line for line in DETAILS.splitlines()
                                      if not line.startswith('Timestamp=')))

    def test_missing_runtime_bit_or_flags_rejected(self):
        for details in (DETAILS.replace('flags=0x10000(runtime)', 'flags=0x2(adhoc)'),
                        DETAILS.replace('flags=0x10000(runtime)', '')):
            with self.subTest(details=details), \
                    self.assertRaisesRegex(ValueError, 'hardened runtime missing'):
                self.verify_app(details)

    def test_nested_verify_failure_propagates_without_display_or_outer_verify(self):
        error = ValueError('codesign: fixture certificate requirement failed')
        with patch.object(native, 'machos', return_value=MACHOS), \
                patch.object(native, 'run', side_effect=error) as run, \
                self.assertRaises(ValueError) as caught:
            native.verify_signatures(APP, IDENTITY, 'distribution', 'arm64')
        self.assertIs(caught.exception, error)
        self.assertEqual(run.call_count, 1)

    def test_display_failure_propagates(self):
        error = ValueError('codesign: fixture display failed')
        with patch.object(native, 'machos', return_value=MACHOS), \
                patch.object(native, 'run', side_effect=['', error]) as run, \
                self.assertRaises(ValueError) as caught:
            native.verify_signatures(APP, IDENTITY, 'distribution', 'arm64')
        self.assertIs(caught.exception, error)
        self.assertEqual(run.call_count, 2)

    def test_image_uses_same_exact_inline_requirement_after_integrity_check(self):
        image = Path('/fixture path/Example.dmg')
        with patch.object(stages, 'run', return_value='') as run:
            stages.verify_image(image, IDENTITY)
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ['/usr/bin/hdiutil', 'verify', image],
            ['/usr/bin/codesign', '--verify', '--strict', '-R', REQUIREMENT, image],
        ])

    def test_image_integrity_failure_stops_before_signature_verification(self):
        error = ValueError('hdiutil: fixture image corrupted')
        with patch.object(stages, 'run', side_effect=error) as run, \
                self.assertRaises(ValueError) as caught:
            stages.verify_image(Path('/fixture path/Example.dmg'), IDENTITY)
        self.assertIs(caught.exception, error)
        self.assertEqual(run.call_count, 1)

    def test_image_signature_failure_propagates(self):
        error = ValueError('codesign: fixture image requirement failed')
        with patch.object(stages, 'run', side_effect=['', error]) as run, \
                self.assertRaises(ValueError) as caught:
            stages.verify_image(Path('/fixture path/Example.dmg'), IDENTITY)
        self.assertIs(caught.exception, error)
        self.assertEqual(run.call_count, 2)

    @unittest.skipUnless(sys.platform == 'darwin', 'requires the macOS requirement parser')
    def test_real_csreq_parses_the_exact_generated_inline_requirement(self):
        app_commands = self.verify_app()
        with patch.object(stages, 'run', return_value='') as run:
            stages.verify_image(Path('/fixture path/Example.dmg'), IDENTITY)
        commands = app_commands + [call.args[0] for call in run.call_args_list]
        requirements = {command[command.index('-R') + 1]
                        for command in commands if '-R' in command}
        self.assertEqual(requirements, {REQUIREMENT})
        for requirement in requirements:
            # -t compiles/parses and displays the requirement without writing a file.
            result = subprocess.run(['/usr/bin/csreq', '-r', requirement, '-t'],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('anchor apple generic', result.stdout)
            self.assertIn(IDENTITY['sha1'].lower(), result.stdout.lower())


class InterpreterPreflightTests(unittest.TestCase):
    def test_preflight_precedes_version_sensitive_module_imports(self):
        for filename in ('collection.py', 'collection_staged_sign.py'):
            with self.subTest(filename=filename):
                tree = ast.parse((MACOS / filename).read_text())
                guard_index = next(index for index, node in enumerate(tree.body)
                                   if isinstance(node, ast.If)
                                   and 'sys.version_info' in ast.unparse(node.test))
                guard = tree.body[guard_index]
                self.assertIn('(3, 11)', ast.unparse(guard.test))
                self.assertTrue(any(isinstance(node, ast.Raise)
                                    for node in ast.walk(guard)))
                self.assertIn('Python 3.11+', ast.unparse(guard))
                imports = [name.name for node in tree.body[:guard_index]
                           if isinstance(node, ast.Import) for name in node.names]
                from_imports = [node.module for node in tree.body[:guard_index]
                                if isinstance(node, ast.ImportFrom)]
                self.assertEqual(imports, ['sys'])
                self.assertTrue(all(name == '__future__' for name in from_imports))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS system Python preflight')
    def test_older_system_python_reports_friendly_preflight_before_tomllib(self):
        executable = Path('/usr/bin/python3')
        if not executable.is_file():
            self.skipTest('system Python is not installed')
        version = subprocess.run([str(executable), '-c',
                                  'import sys; print(sys.version_info[0], sys.version_info[1])'],
                                 capture_output=True, text=True, timeout=10)
        self.assertEqual(version.returncode, 0, version.stderr)
        if tuple(map(int, version.stdout.split())) >= (3, 11):
            self.skipTest('system Python already meets the minimum version')
        for filename in ('collection.py', 'collection_staged_sign.py'):
            with self.subTest(filename=filename):
                result = subprocess.run([str(executable), str(MACOS / filename), '--help'],
                                        capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('requires Python 3.11+', result.stderr)
                self.assertIn('existing compatible interpreter', result.stderr)
                self.assertNotIn('Traceback', result.stderr)
                self.assertNotIn('tomllib', result.stderr)
                self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()
