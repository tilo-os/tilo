#!/usr/bin/env python3
"""Acquire signed copies from an operator-owned OIDC broker; never acquire a key."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile


BROKER = Path('/usr/local/libexec/tilo-desktop-rpm-broker')
MAGIC = bytes.fromhex('8eade801')


def regular(path):
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(f'not a regular file: {path.name}')


def digest(path, offset=0):
    regular(path)
    result = hashlib.sha256()
    with path.open('rb') as source:
        source.seek(offset)
        while block := source.read(1024 * 1024):
            result.update(block)
    return result.hexdigest()


def immutable_digest(path):
    """Exclude only the aligned RPM signature header; retain lead/header/payload."""
    regular(path)
    with path.open('rb') as source:
        lead = source.read(96)
        header = source.read(16)
        if len(lead) != 96 or lead[:4] != bytes.fromhex('edabeedb'):
            raise ValueError('invalid RPM lead')
        if len(header) != 16 or header[:4] != MAGIC:
            raise ValueError('invalid RPM signature header')
        count, size = struct.unpack('>II', header[8:])
        if count > 4096 or size > 16 * 1024 * 1024:
            raise ValueError('unbounded RPM signature header')
        offset = 96 + ((16 + count * 16 + size + 7) // 8) * 8
        source.seek(offset)
        if source.read(4) != MAGIC:
            raise ValueError('invalid RPM immutable header')
        source.seek(offset)
        result = hashlib.sha256(lead)
        while block := source.read(1024 * 1024):
            result.update(block)
    return result.hexdigest()


def trusted_broker(path, env):
    if not path.is_absolute() or path.resolve() != path:
        raise ValueError('broker must be absolute and canonical')
    for name in ('GITHUB_WORKSPACE', 'RUNNER_TEMP'):
        root = Path(env[name]).resolve()
        if path == root or root in path.parents:
            raise ValueError('broker must be outside checkout and temporary directory')
    regular(path)
    if not os.access(path, os.X_OK):
        raise ValueError('broker must be executable')
    for item in (path, *path.parents):
        info = item.lstat()
        if info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
            raise ValueError('broker and parents must be root-owned and not writable by others')


def protected_fingerprint(env):
    expected = env.get('TILO_RPM_SIGNING_FINGERPRINT', '').upper()
    if not re.fullmatch(r'(?:[A-F0-9]{40}|[A-F0-9]{64})', expected):
        raise ValueError('full independently pinned OpenPGP fingerprint required')
    return expected


def request_for(directory, env):
    if any(env.get(name) for name in ('TILO_RPM_SIGNING_KEY_B64', 'TILO_RPM_SIGNING_KEY_ID')):
        raise ValueError('private-key environment is forbidden')
    for name in ('ACTIONS_ID_TOKEN_REQUEST_URL', 'ACTIONS_ID_TOKEN_REQUEST_TOKEN'):
        if not env.get(name):
            raise ValueError('Forgejo native OIDC is required')
    if env.get('GITHUB_REPOSITORY') != 'tilo/tilo' or env.get('GITHUB_EVENT_NAME') != 'push':
        raise ValueError('only tilo/tilo protected-tag pushes may request signing')
    ref, sha = env.get('GITHUB_REF', ''), env.get('GITHUB_SHA', '')
    if not re.fullmatch(r'refs/tags/desktop-v[0-9][A-Za-z0-9.+-]*', ref):
        raise ValueError('invalid desktop release tag')
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise ValueError('invalid source revision')
    arch = env.get('TILO_RPM_ARCH')
    if arch not in ('amd64', 'arm64'):
        raise ValueError('invalid release architecture')
    run_id, attempt = env['GITHUB_RUN_ID'], env['GITHUB_RUN_ATTEMPT']
    if not all(re.fullmatch(r'[1-9][0-9]*', x) for x in (run_id, attempt)):
        raise ValueError('invalid run identity')
    expected = protected_fingerprint(env)
    files = sorted(directory.glob('*.rpm'))
    suffix = 'x86_64' if arch == 'amd64' else 'aarch64'
    names = {}
    for file in files:
        match = re.fullmatch(r'(tilo-session(?:-apps|-devel)?)-[0-9][A-Za-z0-9.+_-]*\.fc44\.' + suffix + r'\.rpm', file.name)
        if not match or match[1] in names:
            raise ValueError('unexpected or duplicate RPM package')
        names[match[1]] = {'name': file.name, 'sha256': digest(file), 'immutable_sha256': immutable_digest(file)}
    if set(names) != {'tilo-session', 'tilo-session-apps', 'tilo-session-devel'}:
        raise ValueError('exact three-package Fedora release required')
    return {'schema': 1, 'repository': 'tilo/tilo', 'repository_id': 23,
            'ref': ref, 'sha': sha, 'run_id': run_id, 'run_attempt': attempt,
            'arch': arch, 'artifact': f'fedora-44-{arch}', 'fingerprint': expected,
            'rpms': list(names.values())}


def run(*args, env=None):
    return subprocess.run(args, check=True, capture_output=True, text=True, env=env).stdout


def verify_response(request, request_path, response, work):
    allowed = {rpm['name'] for rpm in request['rpms']} | {'receipt.json', 'tilo-rpm-signing-key.asc'}
    if {p.name for p in response.iterdir()} != allowed:
        raise ValueError('unexpected or missing broker output')
    for file in response.iterdir():
        regular(file)
    receipt_path = response / 'receipt.json'
    key = response / 'tilo-rpm-signing-key.asc'
    if receipt_path.stat().st_size > 65536 or key.stat().st_size > 1048576:
        raise ValueError('unbounded broker metadata')
    receipt = json.loads(receipt_path.read_text())
    if set(receipt) != {'schema', 'request_sha256', 'fingerprint', 'signed_sha256'}:
        raise ValueError('invalid broker receipt schema')
    if receipt['schema'] != 1 or receipt['request_sha256'] != digest(request_path):
        raise ValueError('broker receipt belongs to another request')
    if receipt['fingerprint'] != request['fingerprint']:
        raise ValueError('broker fingerprint does not match protected pin')
    if set(receipt['signed_sha256']) != {x['name'] for x in request['rpms']}:
        raise ValueError('broker receipt package set mismatch')
    armor = key.read_text(encoding='ascii')
    if 'PRIVATE KEY' in armor or not armor.startswith('-----BEGIN PGP PUBLIC KEY BLOCK-----'):
        raise ValueError('broker must return only an armored public key')
    home = work / 'gnupg'
    home.mkdir(mode=0o700)
    listing = run('gpg', '--homedir', str(home), '--batch', '--with-colons', '--import-options', 'show-only', '--import', str(key))
    primaries, fingerprints, primary = [], [], False
    for line in listing.splitlines():
        fields = line.split(':')
        if fields[0] == 'pub':
            primary = True
        elif fields[0] == 'fpr':
            fingerprint = fields[9].upper()
            fingerprints.append(fingerprint)
            if primary:
                primaries.append(fingerprint)
                primary = False
    if primaries != [request['fingerprint']]:
        raise ValueError('substituted or extra primary public key')
    rpmdb = work / 'rpmdb'
    rpmdb.mkdir()
    run('rpmkeys', '--dbpath', str(rpmdb), '--import', str(key))
    for rpm in request['rpms']:
        file = response / rpm['name']
        if digest(file) != receipt['signed_sha256'][file.name]:
            raise ValueError('signed artifact digest mismatch')
        if immutable_digest(file) != rpm['immutable_sha256']:
            raise ValueError('broker changed package header or payload')
        checked = run('rpmkeys', '--dbpath', str(rpmdb), '--checksig', '--verbose', str(file))
        signatures = re.findall(r'Signature, key ID ([a-fA-F0-9]+): OK', checked)
        full_fingerprints = re.findall(r'signature, key fingerprint: ([a-fA-F0-9]+): OK', checked)
        if not (signatures or full_fingerprints) or re.search(r'NOT OK|NOKEY|BAD', checked):
            raise ValueError('RPM signature was not verified')
        if not all(len(key_id) >= 8 and any(f.lower().endswith(key_id.lower()) for f in fingerprints) for key_id in signatures):
            raise ValueError('RPM signed by an unexpected key')
        if not all(len(f) in (40, 64) and f.upper() in fingerprints for f in full_fingerprints):
            raise ValueError('RPM signed by an unexpected full fingerprint')
    return receipt


def verify_published(directory, arch, version, env):
    protected = protected_fingerprint(env)
    request_path = directory / f'tilo-rpm-signing-request-{arch}.json'
    receipt_path = directory / f'tilo-rpm-signing-receipt-{arch}.json'
    for path in (request_path, receipt_path):
        regular(path)
        if path.stat().st_size > 65536:
            raise ValueError('unbounded published broker metadata')
    request, receipt = (json.loads(path.read_text()) for path in (request_path, receipt_path))
    expected = (directory / 'tilo-rpm-signing-key.fingerprint').read_text().strip()
    if protected != expected:
        raise ValueError('published fingerprint differs from protected pin')
    required = {'schema', 'repository', 'repository_id', 'ref', 'sha', 'run_id',
                'run_attempt', 'arch', 'artifact', 'fingerprint', 'rpms'}
    if set(request) != required or request['schema'] != 1:
        raise ValueError('invalid published request schema')
    if (request['repository'], request['repository_id'], request['ref'], request['arch'],
        request['artifact'], request['fingerprint']) != (
            'tilo/tilo', 23, f'refs/tags/desktop-v{version}', arch, f'fedora-44-{arch}', expected):
        raise ValueError('published signing context mismatch')
    if not re.fullmatch(r'[0-9a-f]{40}', request['sha']):
        raise ValueError('invalid published source')
    for name, field in (('GITHUB_SHA', 'sha'), ('GITHUB_RUN_ID', 'run_id'), ('GITHUB_RUN_ATTEMPT', 'run_attempt')):
        if env.get(name) and env[name] != request[field]:
            raise ValueError('published receipt belongs to another source or run')
    if not all(re.fullmatch(r'[1-9][0-9]*', request[x]) for x in ('run_id', 'run_attempt')):
        raise ValueError('invalid published run identity')
    suffix = {'amd64': 'x86_64', 'arm64': 'aarch64'}[arch]
    names = {f'{name}-{version}-1.fc44.{suffix}.rpm' for name in ('tilo-session', 'tilo-session-apps', 'tilo-session-devel')}
    if len(request['rpms']) != 3 or {x['name'] for x in request['rpms']} != names:
        raise ValueError('published request package mismatch')
    if set(receipt) != {'schema', 'request_sha256', 'fingerprint', 'signed_sha256'}:
        raise ValueError('invalid published receipt schema')
    if (receipt['schema'], receipt['request_sha256'], receipt['fingerprint']) != (1, digest(request_path), expected):
        raise ValueError('published receipt request binding mismatch')
    if set(receipt['signed_sha256']) != names:
        raise ValueError('published receipt package mismatch')
    for rpm in request['rpms']:
        if set(rpm) != {'name', 'sha256', 'immutable_sha256'} or not re.fullmatch(r'[0-9a-f]{64}', rpm['sha256']):
            raise ValueError('invalid published RPM manifest')
        file = directory / rpm['name']
        if digest(file) != receipt['signed_sha256'][file.name] or immutable_digest(file) != rpm['immutable_sha256']:
            raise ValueError('published signed RPM differs from qualified broker copy')


def sign(directory, env):
    request = request_for(directory, env)
    trusted_broker(BROKER, env)
    with tempfile.TemporaryDirectory(prefix='desktop-rpm-', dir=env['RUNNER_TEMP']) as temporary:
        work = Path(temporary)
        request_path = work / 'request.json'
        request_path.write_text(json.dumps(request, sort_keys=True, separators=(',', ':')) + '\n')
        response = work / 'response'
        response.mkdir(mode=0o700)
        # The approved client obtains the OIDC token itself and transmits it
        # directly to its pinned service. No token or key enters this manifest.
        subprocess.run([str(BROKER), 'desktop-rpm', str(request_path), str(response)], check=True, stdout=subprocess.DEVNULL)
        verify_response(request, request_path, response, work)
        # Validate all copies before replacing any install-tested unsigned RPM.
        for rpm in request['rpms']:
            shutil.copyfile(response / rpm['name'], directory / rpm['name'])
        arch = request['arch']
        shutil.copyfile(request_path, directory / f'tilo-rpm-signing-request-{arch}.json')
        shutil.copyfile(response / 'receipt.json', directory / f'tilo-rpm-signing-receipt-{arch}.json')
        if arch == 'amd64':
            shutil.copyfile(response / 'tilo-rpm-signing-key.asc', directory / 'tilo-rpm-signing-key.asc')
            (directory / 'tilo-rpm-signing-key.fingerprint').write_text(request['fingerprint'] + '\n')
            for name in ('tilo-rpm-signing-key.asc', 'tilo-rpm-signing-key.fingerprint'):
                (directory / (name + '.sha256')).write_text(f'{digest(directory / name)}  {name}\n')
    print(f"verified three broker-signed {request['arch']} RPMs with {request['fingerprint']}")


if __name__ == '__main__':
    try:
        if len(sys.argv) == 5 and sys.argv[1] == '--verify-published':
            verify_published(Path(sys.argv[2]).resolve(strict=True), sys.argv[3], sys.argv[4], os.environ)
        elif len(sys.argv) == 2:
            sign(Path(sys.argv[1]).resolve(strict=True), os.environ)
        else:
            raise ValueError('usage: sign-rpms-broker.py RELEASE_DIR')
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        # Never print broker subprocess stderr: it may contain an OIDC token.
        print(f'RPM broker qualification failed: {type(error).__name__}', file=sys.stderr)
        sys.exit(1)
