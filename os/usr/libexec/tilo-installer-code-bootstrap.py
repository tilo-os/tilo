#!/usr/bin/env python3
"""VM-only /etc bootstrap for old immutable CI installer media."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess

ROOT = Path('/etc/tilo/installer-code-bootstrap')
DESTINATION = Path('/usr/libexec/tilo-vm-install-proof.py')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def run(*args):
    result = subprocess.run(args, check=True, capture_output=True, timeout=30)
    return result.stdout


def load_daemon():
    # Reuse the immutable bootstrap's exact VM/PID1/development/port gate.
    path = ROOT / 'tilo-installer-code-refresh.py'
    spec = importlib.util.spec_from_file_location('code_bootstrap_daemon', path)
    daemon = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(daemon)
    return daemon


def start():
    daemon = load_daemon()
    daemon.gate()
    receipt_path = ROOT / 'receipt.json'
    require(receipt_path.is_file() and not receipt_path.is_symlink()
            and receipt_path.stat().st_uid == 0 and receipt_path.stat().st_size <= 65536,
            'unsafe VM bootstrap receipt')
    receipt = json.loads(receipt_path.read_text())
    require(receipt.get('schema') == 'tilo.installer-code-bootstrap.v1', 'invalid VM bootstrap receipt')
    for name, expected in receipt['files'].items():
        require(name in {'tilo-installer-code-bootstrap.py', 'tilo-installer-code-refresh.py',
                         'tilo-installer-code-protocol.py', 'tilo-installer-code-target-probe.py', 'tilo-vm-install-proof.py'},
                'unexpected bootstrap file')
        file = ROOT / name
        require(file.is_file() and not file.is_symlink() and file.stat().st_uid == 0
                and file.stat().st_size <= 512 * 1024
                and hashlib.sha256(file.read_bytes()).hexdigest() == expected,
                'VM bootstrap content differs from host manifest')
    require(set(receipt['files']) == {'tilo-installer-code-bootstrap.py', 'tilo-installer-code-refresh.py',
                                    'tilo-installer-code-protocol.py', 'tilo-installer-code-target-probe.py', 'tilo-vm-install-proof.py'},
            'incomplete bootstrap manifest')
    source = ROOT / 'tilo-vm-install-proof.py'
    require(DESTINATION.is_file() and not DESTINATION.is_symlink(), 'missing original proof emitter')
    # Retain the ORIGINAL immutable executable label; no policy is disabled.
    require('security.selinux' in os.listxattr(DESTINATION), 'original proof emitter lacks SELinux label')
    os.setxattr(source, 'security.selinux', os.getxattr(DESTINATION, 'security.selinux'))
    source.chmod(DESTINATION.stat().st_mode & 0o777)
    run('mount', '--bind', str(source), str(DESTINATION))
    try:
        run('mount', '-o', 'remount,bind,ro', str(DESTINATION))
    except Exception:
        run('umount', str(DESTINATION))
        raise
    daemon.BASE.mkdir(mode=0o700, parents=True, exist_ok=True)
    (daemon.BASE / 'bootstrap.json').write_text(json.dumps(receipt, sort_keys=True) + '\n')
    (daemon.BASE / 'bootstrap.json').chmod(0o600)
    os.environ['TILO_CODE_BOOTSTRAP_HANDSHAKE'] = '1'
    os.execv('/usr/bin/python3', ['/usr/bin/python3', str(ROOT / 'tilo-installer-code-refresh.py')])


if __name__ == '__main__':
    start()
