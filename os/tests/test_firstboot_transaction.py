#!/usr/bin/env python3
"""Exercise interruption/retry against the production firstboot transaction."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'os/usr/libexec/tilo-firstboot-user'
HASH = '$6$fixture$' + 'A' * 86


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        r = self.root
        (r / 'etc').mkdir()
        (r / 'state').mkdir()
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
        raise SystemExit(1)
def locked():
    assert (r / 'shadow').read_text().startswith('!'), 'delivery ran unlocked'
"""
        tools = {
            'id': "raise SystemExit(0 if (r / 'shadow').exists() else 1)",
            'stat': "print('0:600:1' if sys.argv[2] == '%u:%a:%h' else Path(sys.argv[-1]).stat().st_size)",
            'getent': "print('fixtureuser:' + (r / 'shadow').read_text() + ':1000:1000::/home/fixtureuser:/bin/bash')",
            'useradd': "assert not (r / 'shadow').exists(); (r / 'shadow').write_text(sys.argv[sys.argv.index('-p') + 1]); (r / 'useradd').write_text(json.dumps(sys.argv[1:])); fail('useradd')",
            'chpasswd': "value = sys.stdin.read().strip().split(':', 1)[1]; fail('password'); (r / 'shadow').write_text(value)",
            'chown': "locked(); fail('chown')",
            'shred': "Path(sys.argv[-1]).unlink()",
        }
        for name, body in tools.items():
            p = r / 'bin' / name
            p.write_text('#!/usr/bin/env python3\n' + common + body + '\n')
            p.chmod(0o755)
        (r / 'seeds.py').write_text(common + "locked(); fail('seeds')\n")
        (r / 'handoff').write_text('#!/usr/bin/env python3\n' + common + """if sys.argv[1] == 'required-user-id':
    assert not (r / 'shadow').exists(), 'retry repeated UID availability query'
    print('1000')
else:
    locked()
    fail('plan')
""")
        (r / 'handoff').chmod(0o755)
        (r / 'etc/prepared-setup-transport.py').write_text(common + "locked(); fail(sys.argv[1])\n")
        s = SOURCE.read_text()
        for old, new in {
            '/etc/tilo': str(r / 'etc'),
            '/var/lib/tilo': str(r / 'state'),
            '/var/home': str(r / 'home'),
            '"/home/$user"': f'"{r}/home/$user"',
            '/usr/libexec/tilo-staging-handoff': str(r / 'handoff'),
            'seed_helper=/usr/libexec/tilo-firstboot-home-seeds.py': f'seed_helper={r}/seeds.py',
        }.items():
            self.assertIn(old, s)
            s = s.replace(old, new)
        (r / 'firstboot').write_text(s)

    def run_boot(self, fail='', terminate=False):
        env = dict(os.environ, FIXTURE_ROOT=str(self.root), FAIL_STAGE=fail,
                   TERMINATE=str(int(terminate)), PATH=f"{self.root}/bin:{os.environ['PATH']}")
        result = subprocess.run(['bash', str(self.root / 'firstboot')], env=env,
                                capture_output=True, text=True, timeout=10)
        self.assertNotIn(HASH, result.stdout + result.stderr)
        return result

    def test_every_failed_delivery_is_locked_and_retryable(self):
        for stage in ['useradd', 'chown', 'seeds', 'plan', 'deliver-user', 'deliver-home-user', 'password']:
            with self.subTest(stage=stage):
                result = self.run_boot(stage)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(self.credential.exists())
                self.assertTrue(self.full_name.exists())
                self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
                created = (self.root / 'useradd').read_text()
                result = self.run_boot()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / 'shadow').read_text(), HASH)
                self.assertEqual((self.root / 'useradd').read_text(), created)
                self.assertFalse(self.credential.exists())
                self.assertFalse((self.root / 'etc/install-user.transaction').exists())
                # Reset just this completed synthetic transaction for next case.
                (self.root / 'shadow').unlink()
                self.credential.write_text('fixtureuser:' + HASH + '\n')
                self.credential.chmod(0o600)
                self.full_name.write_text('Fixture User\n')
                self.full_name.chmod(0o600)

    def test_termination_during_large_delivery_never_unlocks(self):
        result = self.run_boot('deliver-home-user', terminate=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        self.assertTrue(self.credential.exists())
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_intent_before_account_creation_can_retry(self):
        intent = self.root / 'etc/install-user.transaction'
        intent.write_text('fixtureuser:!tilo-firstboot-' + 'a' * 32 + '\n')
        intent.chmod(0o600)
        result = self.run_boot()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'shadow').read_text(), HASH)

    def test_retry_after_final_unlock_relocks_before_delivery(self):
        self.assertNotEqual(self.run_boot('seeds').returncode, 0)
        # Model interruption after chpasswd succeeded but before seed cleanup.
        (self.root / 'shadow').write_text(HASH)
        result = self.run_boot('deliver-home-user')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / 'shadow').read_text().startswith('!'))
        self.assertTrue(self.credential.exists())

    def test_unrelated_existing_account_preserves_seed_and_shadow(self):
        (self.root / 'shadow').write_text('!unrelated')
        result = self.run_boot()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(self.credential.exists())
        self.assertEqual((self.root / 'shadow').read_text(), '!unrelated')
        self.assertFalse((self.root / 'etc/install-user.transaction').exists())

    def test_intent_does_not_adopt_wrong_shadow(self):
        self.assertNotEqual(self.run_boot('seeds').returncode, 0)
        (self.root / 'shadow').write_text('!unrelated')
        self.assertNotEqual(self.run_boot().returncode, 0)
        self.assertEqual((self.root / 'shadow').read_text(), '!unrelated')
        self.assertTrue(self.credential.exists())

    def test_service_allows_size_independent_transfer_duration(self):
        unit = (ROOT / 'os/usr/lib/systemd/system/tilo-firstboot-user.service').read_text()
        self.assertIn('TimeoutStartSec=infinity\n', unit)


if __name__ == '__main__':
    unittest.main()
