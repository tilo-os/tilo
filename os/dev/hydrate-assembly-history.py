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
    extra_revisions = set()
    extra_paths = set()
    allowed_paths = PATHS
    if approval.get('purpose') == 'tilo-reviewed-cache-source-compatibility':
        from native_source_compatibility import (CACHE_BASE, CACHE_TRUST, CACHE_API,
                                                CACHE_VIEWER, CACHE_SOURCE,
                                                DRACUT_SOURCE, DRACUT_PATHS)
        if source not in (CACHE_SOURCE, DRACUT_SOURCE):
            raise ValueError('cache correction source differs')
        previous = approval['previous_approval_json'].encode()
        if hashlib.sha256(previous).hexdigest() != approval['previous_approval_sha256']:
            raise ValueError('previous admission digest differs')
        approval = json.loads(previous)
        if approval.get('assembly_source_revision') != CACHE_BASE:
            raise ValueError('previous assembly source differs')
        extra_revisions = {CACHE_BASE, CACHE_TRUST, CACHE_API, CACHE_VIEWER, CACHE_SOURCE}
        extra_paths = {'os/dev/build-image.sh'}
        if source == DRACUT_SOURCE:
            # The offline reuse gate reads every pinned successor file, including
            # its tests. Hydration supplies those bytes; that gate still verifies
            # the complete reviewed ancestry, tree diff and producer isolation.
            extra_revisions.add(DRACUT_SOURCE)
            extra_paths.update(DRACUT_PATHS)
            allowed_paths = PATHS | set(DRACUT_PATHS)
    paths = set(approval['changes']) | {'os/dev/build-wpe.sh'} | extra_paths
    if not paths <= allowed_paths:
        raise ValueError('unexpected historical source path')
    revisions = {approval[key] for key in REVISION_FIELDS if key in approval} | extra_revisions
    if not revisions or any(not re.fullmatch('[0-9a-f]{40}', revision) for revision in revisions):
        raise ValueError('exact source revisions required')
    def git(*args, env=environment, stdin=None):
        return subprocess.check_output(['git', '-C', str(repo), *args], env=env, input=stdin,
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
    ordered = sorted(objects)
    request = ''.join(oid + '\n' for oid in ordered).encode()
    # Look up what is already local without letting git fetch lazily, then
    # fetch every missing reviewed blob in one request rather than one
    # round trip per object.
    local = dict(environment, GIT_NO_LAZY_FETCH='1')
    lines = git('cat-file', '--batch-check=%(objectname) present', env=local, stdin=request)
    present = {line.split()[0].decode() for line in lines.splitlines() if line.endswith(b' present')}
    missing = set(ordered) - present
    if missing:
        git('-c', 'fetch.negotiationAlgorithm=noop', 'fetch', 'origin', '--no-tags',
            '--no-write-fetch-head', '--recurse-submodules=no', '--filter=blob:none', '--stdin',
            stdin=''.join(oid + '\n' for oid in sorted(missing)).encode())
    offline = dict(local, GIT_CONFIG_COUNT='1', GIT_CONFIG_KEY_0='protocol.allow', GIT_CONFIG_VALUE_0='never')
    # Read every blob back in one offline pass: each must now be local, a blob
    # and within the size bound.
    output = git('cat-file', '--batch', env=offline, stdin=request)
    offset = 0
    for oid in ordered:
        end = output.index(b'\n', offset)
        header = output[offset:end].split()
        if len(header) != 3 or header[0].decode() != oid or header[1] != b'blob':
            raise ValueError('historical source blob unavailable offline')
        size = int(header[2])
        if size > 2 * 1024 * 1024:
            raise ValueError('historical source file exceeds bound')
        offset = end + 1 + size + 1
    if offset != len(output):
        raise ValueError('historical source blob unavailable offline')
    return {'source_revision': source, 'blob_count': len(objects), 'offline_blobs_verified': True}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    args = parser.parse_args()
    env = os.environ.copy()
    token = env.pop('TILO_HISTORY_TOKEN')
    user = env.pop('TILO_HISTORY_USER')
    remote = subprocess.check_output(['git', '-C', args.repo, 'remote', 'get-url', 'origin'], text=True).strip()
    if remote not in ('https://code.tilo.so/tilo/tilo', 'https://code.tilo.so/tilo/tilo.git'):
        raise ValueError('unexpected source remote')
    header = 'Authorization: Basic ' + base64.b64encode((user+':'+token).encode()).decode()
    env.update(GIT_CONFIG_COUNT='1', GIT_CONFIG_KEY_0='http.https://code.tilo.so/.extraHeader', GIT_CONFIG_VALUE_0=header, GIT_TERMINAL_PROMPT='0')
    receipt = hydrate(args.repo, os.environ['NATIVE_SOURCE_COMPATIBILITY'].encode(),
                      os.environ['NATIVE_SOURCE_COMPATIBILITY_SHA256'], os.environ['SOURCE_REVISION'], env)
    print(json.dumps(receipt, sort_keys=True))

if __name__ == '__main__':
    main()
