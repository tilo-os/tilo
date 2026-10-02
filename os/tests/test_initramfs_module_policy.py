"""Exercise the shipped boot gate with synthetic initramfs module evidence."""
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest


OS = Path(__file__).resolve().parents[1]
GATE = OS / 'tests/boot_chain_invariants.sh'
REQUIRED = {'crypt', 'systemd-cryptsetup', 'tpm2-tss', 'plymouth', 'tilo-rollback'}
# Run 9404 selected the Clevis providers and nvmf despite omitting their parents.
# The pinned base's dracut 108 module audit also found nbd/fcoe/fcoe-uefi consumers.
DEPENDENT_MODULES = {
    'clevis-pin-null', 'clevis-pin-sss', 'clevis-pin-tang', 'clevis-pin-tpm2', 'nvmf',
    'nbd', 'fcoe', 'fcoe-uefi',
}
FORBIDDEN = DEPENDENT_MODULES | {
    'network', 'network-legacy', 'network-manager', 'nfs', 'iscsi', 'nbde',
    'clevis', 'crypt-ssh',
}


def bash_executable():
    if os.name == 'nt':
        git_bash = Path(os.environ.get('ProgramFiles', 'C:/Program Files')) / 'Git/bin/bash.exe'
        if git_bash.is_file():
            return str(git_bash)
    bash = shutil.which('bash')
    if bash is None:
        raise RuntimeError('bash is required to exercise the production boot-chain gate')
    return bash


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


class InitramfsModulePolicyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='tilo-initramfs-policy-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.write('usr/lib/modules/test-kernel/initramfs.img', 'fixture initramfs\n')
        self.write('usr/lib/tilo/initramfs-omitted-drivers.txt', 'xfs\n')
        for name in ('tilo-disk-encrypt', 'tilo-secureboot'):
            self.write('usr/libexec/' + name, '#!/bin/sh\nexit 0\n', executable=True)
        self.write('usr/lib/systemd/system/tilo-growfs-sysroot.service',
                   'ConditionPathIsMountPoint=/sysroot\n'
                   "ExecStart=/usr/bin/sh -c 'mount -o remount,rw /sysroot && exec /usr/lib/systemd/systemd-growfs /sysroot'\n")
        self.write('usr/lib/systemd/system/tilo-growfs-sysroot.timer',
                   'Unit=tilo-growfs-sysroot.service\nOnBootSec=5s\n')
        self.write('usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf',
                   'ConditionPathIsMountPoint=!/sysroot\n')
        # Isolate module policy from the host's systemd and Windows symlink privileges.
        # The mock accepts only the exact read-only enablement query made by the gate.
        self.write('test-bin/systemctl', '''#!/bin/sh
set -eu
[ "$#" -eq 3 ]
[ "$1" = "--root=$OS_ROOT" ]
[ "$2" = is-enabled ]
[ "$3" = tilo-growfs-sysroot.timer ]
printf 'enabled\\n'
''', executable=True)

    def write(self, relative, contents, *, executable=False):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open('w', encoding='utf-8', newline='\n') as stream:
            stream.write(contents)
        if executable:
            path.chmod(0o755)

    def run_gate(self, modules):
        self.write('usr/lib/tilo/initramfs-modules.txt', '\n'.join(sorted(modules)) + '\n')
        environment = os.environ.copy()
        environment.pop('BASH_ENV', None)
        environment.pop('ENV', None)
        return subprocess.run([
            bash_executable(), '--noprofile', '--norc', '-c', '''
set -eu
export PATH=/usr/bin:/bin
root=$1
gate=$2
if command -v cygpath >/dev/null 2>&1; then
    root=$(cygpath -u "$root")
    gate=$(cygpath -u "$gate")
fi
export OS_ROOT="$root"
export PATH="$root/test-bin:$PATH"
chmod +x "$root/test-bin/systemctl" "$root/usr/libexec/tilo-disk-encrypt" "$root/usr/libexec/tilo-secureboot"
exec bash "$gate"
''', 'module-policy-test', str(self.root), str(GATE)],
            env=environment, capture_output=True, text=True, encoding='utf-8',
            errors='replace', timeout=30)

    def test_module_policy(self):
        with self.subTest(phase="valid_local_root_modules_pass"):
            result = self.run_gate(REQUIRED)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('all boot-chain invariants hold', result.stdout)
        restart_fixture(self)
        with self.subTest(phase="each_forbidden_module_is_rejected"):
            for module in sorted(FORBIDDEN):
                with self.subTest(module=module):
                    result = self.run_gate(REQUIRED | {module})
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    failures = [line for line in result.stderr.splitlines() if line.startswith('FAIL')]
                    self.assertEqual(len(failures), 1, result.stdout + result.stderr)
                    self.assertIn('unsupported', failures[0])
                    self.assertIn('module ' + module, failures[0])
        restart_fixture(self)
        with self.subTest(phase="each_required_module_must_remain"):
            for module in sorted(REQUIRED):
                with self.subTest(module=module):
                    result = self.run_gate(REQUIRED - {module})
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    failures = [line for line in result.stderr.splitlines() if line.startswith('FAIL')]
                    self.assertEqual(len(failures), 1, result.stdout + result.stderr)
                    self.assertIn('initramfs has no ' + module + ' module', failures[0])
        restart_fixture(self)
        with self.subTest(phase="ext4_root_driver_must_remain"):
            self.write('usr/lib/tilo/initramfs-omitted-drivers.txt', 'ext4\nxfs\n')
            result = self.run_gate(REQUIRED)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn('initramfs excludes Ext4', result.stderr)
        restart_fixture(self)
        with self.subTest(phase="production_dracut_omits_dependency_consumers"):
            source = (OS / 'scripts/build-initramfs.sh').read_text(encoding='utf-8')
            logical_lines = re.sub(r'\\\r?\n\s*', ' ', source)
            command = re.search(r'\bdracut\s+--force\b[^\n]*', logical_lines)
            self.assertIsNotNone(command, 'production dracut invocation is missing')
            arguments = shlex.split(command.group())
            self.assertIn('--omit', arguments)
            omitted = set(arguments[arguments.index('--omit') + 1].split())
            self.assertFalse(FORBIDDEN - omitted, f'missing omitted modules: {sorted(FORBIDDEN - omitted)}')
            self.assertFalse(REQUIRED & omitted, f'required modules omitted: {sorted(REQUIRED & omitted)}')
            self.assertIn('--add', arguments)
            included = set(arguments[arguments.index('--add') + 1].split())
            self.assertFalse(REQUIRED - included, f'missing required modules: {sorted(REQUIRED - included)}')
            self.assertIn('--compress', arguments)
            self.assertEqual(arguments[arguments.index('--compress') + 1], 'zstd')
            self.assertIn('--omit-drivers', arguments)
            self.assertEqual(set(arguments[arguments.index('--omit-drivers') + 1].split()),
                             {'amdgpu', 'nouveau', 'xe', 'xfs'})
            self.assertIn('--add-drivers', arguments)
            self.assertEqual(set(arguments[arguments.index('--add-drivers') + 1].split()), {'ext4'})


if __name__ == '__main__':
    unittest.main()
