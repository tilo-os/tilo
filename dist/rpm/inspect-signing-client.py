#!/usr/bin/env python3
"""Read-only operator metadata; never execute a broker or inspect private keys."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat

BROKER = Path('/usr/local/libexec/tilo-desktop-rpm-broker')


def inspect_client(path=BROKER):
    result = {'path': str(path), 'present': False, 'safe_operator_client': False}
    try:
        info = path.lstat()
    except FileNotFoundError:
        return result
    result.update(present=True, uid=info.st_uid, gid=info.st_gid,
                  mode=oct(stat.S_IMODE(info.st_mode)), bytes=info.st_size)
    safe = stat.S_ISREG(info.st_mode) and 0 < info.st_size <= 16 * 1024**2
    for parent in (path, *path.parents):
        metadata = parent.lstat()
        safe = safe and metadata.st_uid == 0 and not metadata.st_mode & 0o022
        safe = safe and not stat.S_ISLNK(metadata.st_mode)
    safe = safe and os.access(path, os.X_OK)
    result['safe_operator_client'] = bool(safe)
    if safe:
        with path.open('rb') as stream:
            result['public_client_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
    return result


def report(env):
    pin = env.get('TILO_RPM_SIGNING_FINGERPRINT', '')
    valid = re.fullmatch(r'(?:[A-Fa-f0-9]{40}|[A-Fa-f0-9]{64})', pin) is not None
    return {'schema': 'tilo.rpm-signing-client-inspection/v1',
            'repository': env.get('GITHUB_REPOSITORY'), 'source_sha': env.get('GITHUB_SHA'),
            'run_id': env.get('GITHUB_RUN_ID'), 'architecture': platform.machine(),
            'client': inspect_client(),
            'public_fingerprint': {'configured': bool(pin), 'valid': valid,
                                   'value': pin.upper() if valid else None},
            'tool_paths': {tool: shutil.which(tool) for tool in ('gpg', 'rpmkeys')},
            'broker_executed': False, 'oidc_requested': False,
            'private_key_read': False, 'backend_custody_verified': False,
            'release_authority_qualified': False}


if __name__ == '__main__':
    print(json.dumps(report(os.environ), sort_keys=True, indent=2))
