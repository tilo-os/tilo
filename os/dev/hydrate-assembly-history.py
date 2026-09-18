#!/usr/bin/env python3
"""Materialize bounded compatibility blobs before an offline tools checkout."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

PATHS = {'os/Containerfile', 'os/dev/build-wpe.sh', 'os/dev/build-image.sh',
         'os/dev/windows_provider.py', 'os/tests/test_windows_provider.py',
         'docs/security/boot-chain.md', 'os/scripts/verify-runtime-crypto-packages.sh',
         'os/usr/libexec/tilo-security-audit'}
REVISION_FIELDS = ('native_source_revision', 'assembly_source_revision', 'provider_revision',
                   'rootfs_revision', 'policy_revision')

def hydrate(repo, raw, digest, source, environment):
    if len(raw) > 65536 or hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError('bounded compatibility approval digest differs')
    approval = json.loads(raw)
    if approval.get('assembly_source_revision') != source:
        raise ValueError('compatibility source differs')
    paths = set(approval['changes']) | {'os/dev/build-wpe.sh'}
    if not paths <= PATHS:
        raise ValueError('unexpected historical source path')
    revisions = {approval[key] for key in REVISION_FIELDS if key in approval}
    if not revisions or any(not re.fullmatch('[0-9a-f]{40}', revision) for revision in revisions):
        raise ValueError('exact source revisions required')
    def git(*args, env=environment):
        return subprocess.check_output(['git', '-C', str(repo), *args], env=env,
                                       stderr=subprocess.DEVNULL, timeout=60)
    if git('rev-parse', 'HEAD').decode().strip() != source:
        raise ValueError('checkout source differs')
    objects = set()
    for revision in sorted(revisions):
        # Tree metadata is available in a blob-filtered checkout. Request only
        # reviewed paths, including intermediate Containerfile revisions.
        entries = git('ls-tree', '-z', revision, '--', *sorted(paths)).split(b'\0')
        for entry in filter(None, entries):
            metadata, name = entry.split(b'\t', 1)
            mode, kind, oid = metadata.split()
            if kind != b'blob' or mode not in (b'100644', b'100755'):
                raise ValueError('historical input must be a regular file')
            objects.add(oid.decode())
    for oid in sorted(objects):
        payload = git('cat-file', 'blob', oid)
        if len(payload) > 2 * 1024 * 1024:
            raise ValueError('historical source file exceeds bound')
    offline = dict(environment, GIT_CONFIG_COUNT='1', GIT_CONFIG_KEY_0='protocol.allow', GIT_CONFIG_VALUE_0='never')
    for oid in sorted(objects):
        git('cat-file', 'blob', oid, env=offline)
    return {'source_revision': source, 'blob_count': len(objects), 'offline_blobs_verified': True}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    args = parser.parse_args()
    env = os.environ.copy()
    token = env.pop('TILO_HISTORY_TOKEN')
    user = env.pop('TILO_HISTORY_USER')
    remote = subprocess.check_output(['git', '-C', args.repo, 'remote', 'get-url', 'origin'], text=True).strip()
    if remote not in ('https://code.sandbox.example/qos/qos', 'https://code.sandbox.example/qos/qos.git'):
        raise ValueError('unexpected source remote')
    header = 'Authorization: Basic ' + base64.b64encode((user+':'+token).encode()).decode()
    env.update(GIT_CONFIG_COUNT='1', GIT_CONFIG_KEY_0='http.https://code.sandbox.example/.extraHeader', GIT_CONFIG_VALUE_0=header, GIT_TERMINAL_PROMPT='0')
    receipt = hydrate(args.repo, os.environ['NATIVE_SOURCE_COMPATIBILITY'].encode(),
                      os.environ['NATIVE_SOURCE_COMPATIBILITY_SHA256'], os.environ['SOURCE_REVISION'], env)
    print(json.dumps(receipt, sort_keys=True))

if __name__ == '__main__':
    main()
