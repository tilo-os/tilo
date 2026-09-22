#!/usr/bin/env python3
"""Development-only Wine RPM signing and separately anchored guest verification."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

PIN = Path(__file__).with_name('wine-source.json')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def reference(value):
    if not re.fullmatch(r'code\.podesta\.ai/qos/[a-z0-9-]+@sha256:[0-9a-f]{64}', value):
        raise ValueError('expected immutable tilo development artifact reference')
    return value


def regular(path):
    if any(parent.is_symlink() for parent in path.parents):
        raise ValueError('symlink in development Wine input path')
    if path.is_symlink() or not path.is_file():
        raise ValueError(f'expected regular file: {path.name}')
    return path


def wine_identity(identity, pin):
    if (len(identity) != 6 or not all(isinstance(value, str) for value in identity)
            or not (re.fullmatch(r'wine(?:-[A-Za-z0-9+_-]+)?', identity[0])
                    or identity[0] == 'ntsync-autoload')
            or identity[1:3] != [pin['version'], pin['build_release']]
            or identity[3] not in ('i686', 'x86_64', 'noarch')
            or identity[4] != f"wine-{pin['version']}-{pin['build_release']}.src.rpm"):
        raise ValueError('expected pinned traditional Wine package identity')


def command(*args, env=None):
    # Signing diagnostics never leave the temporary trust boundary.
    result = subprocess.run(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise ValueError(f'{args[0]} failed (exit {result.returncode})')
    return result.stdout.decode()


def pair_inventory(pair, pin):
    record = json.loads(regular(pair / 'paired-contract.json').read_text())
    if (record.get('schema') != 1 or record.get('source') != pin
            or record.get('execution_validated') is not False
            or set(record.get('input_refs', {})) != {'i686', 'x86_64'}):
        raise ValueError('paired Wine provenance differs from the reviewed source')
    for ref in record['input_refs'].values():
        reference(ref)
    inventory = record.get('rpms', {})
    if not inventory or {p.name for p in (pair / 'rpms').iterdir()} != set(inventory):
        raise ValueError('paired RPM inventory is incomplete or has extra files')
    cores = set()
    for name, entry in inventory.items():
        if Path(name).name != name or not name.endswith('.rpm'):
            raise ValueError('invalid paired RPM filename')
        path = regular(pair / 'rpms' / name)
        identity = entry.get('identity', [])
        wine_identity(identity, pin)
        if (path.stat().st_size != entry.get('size') or digest(path) != entry.get('sha256')):
            raise ValueError('paired RPM bytes or identity differ')
        if identity[0] == 'wine-core':
            cores.add(identity[3])
    if cores != {'i686', 'x86_64'}:
        raise ValueError('both traditional Wine core architectures are required')
    return record


def verify_signature(path, database):
    text = command('rpmkeys', '--dbpath', str(database), '--checksig', '--verbose', str(path))
    if not re.search(r'^\s+.*[Ss]ignature.*: OK$', text, re.M):
        raise ValueError(f'RPM has no verified cryptographic signature: {path.name}')


def sign(pair, pair_ref, pin, output):
    reference(pair_ref)
    record = pair_inventory(pair, pin)
    for name, entry in record['rpms'].items():
        path = pair / 'rpms' / name
        identity = command('rpm', '-qp', '--qf',
            '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{SOURCERPM}\t%{LICENSE}', str(path)).split('\t')
        if identity != entry['identity']:
            raise ValueError('actual RPM header differs from paired identity')
        command('rpmkeys', '--checksig', '--nosignature', str(path))
    if output.exists() or output.is_symlink():
        raise ValueError('signing output must not exist')
    if any(parent.is_symlink() for parent in output.parents):
        raise ValueError('signing output parents must not be symlinks')
    output.mkdir(mode=0o755)
    try:
        (output / 'rpms').mkdir()
        with tempfile.TemporaryDirectory(prefix='tilo-development-rpm-sign-') as temporary:
            home = Path(temporary)
            home.chmod(0o700)
            env = os.environ.copy()
            env['GNUPGHOME'] = str(home)
            try:
                command('gpg', '--batch', '--pinentry-mode', 'loopback', '--passphrase', '',
                        '--quick-generate-key', 'Tilo development Wine (not release signing)',
                        'rsa3072', 'sign', '0', env=env)
                fingerprints = [line.split(':')[9] for line in command('gpg', '--batch',
                    '--with-colons', '--list-secret-keys', env=env).splitlines() if line.startswith('fpr:')]
                if len(fingerprints) != 1 or not re.fullmatch(r'[A-F0-9]{40,64}', fingerprints[0]):
                    raise ValueError('expected one ephemeral development signing identity')
                fingerprint = fingerprints[0]
                (output / 'public.asc').write_text(command('gpg', '--batch', '--armor', '--export', fingerprint, env=env))
                database = home / 'rpmdb'
                database.mkdir()
                command('rpmkeys', '--dbpath', str(database), '--import', str(output / 'public.asc'))
                signed = {}
                for name, entry in record['rpms'].items():
                    target = output / 'rpms' / name
                    shutil.copyfile(pair / 'rpms' / name, target)
                    command('rpmsign', '--define', f'_gpg_path {home}', '--define', f'_gpg_name {fingerprint}',
                            '--define', '_openpgp_sign gpg', '--define', f'_openpgp_sign_id {fingerprint}',
                            '--addsign', str(target), env=env)
                    verify_signature(target, database)
                    signed[name] = {'sha256': digest(target), 'size': target.stat().st_size,
                                    'identity': entry['identity'], 'unsigned_sha256': entry['sha256']}
            finally:
                subprocess.run(['gpgconf', '--kill', 'all'], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        shutil.copyfile(pair / 'paired-contract.json', output / 'paired-contract.json')
        receipt = {'schema': 1, 'development_only': True, 'pair_reference': pair_ref,
                   'paired_contract_sha256': digest(output / 'paired-contract.json'),
                   'public_key_sha256': digest(output / 'public.asc'), 'fingerprint': fingerprint,
                   'rpms': signed}
        (output / 'development-wine.json').write_text(json.dumps(receipt, indent=2) + '\n')
        return receipt
    except BaseException:
        shutil.rmtree(output)
        raise


def metadata(directory, key_sha256):
    if not re.fullmatch(r'[0-9a-f]{64}', key_sha256):
        raise ValueError('independently pinned public key SHA256 required')
    key = regular(directory / 'public.asc')
    if digest(key) != key_sha256:
        raise ValueError('development Wine public key differs from independent pin')
    record = json.loads(regular(directory / 'development-wine.json').read_text())
    if (record.get('schema') != 1 or record.get('development_only') is not True
            or record.get('public_key_sha256') != key_sha256
            or digest(regular(directory / 'paired-contract.json')) != record.get('paired_contract_sha256')):
        raise ValueError('invalid development Wine trust record')
    reference(record['pair_reference'])
    pair = json.loads((directory / 'paired-contract.json').read_text())
    pin = json.loads(PIN.read_text())
    if pair.get('source') != pin or pair.get('execution_validated') is not False:
        raise ValueError('development Wine pair differs from the current reviewed source pin')
    if set(record.get('rpms', {})) != set(pair.get('rpms', {})) or not record['rpms']:
        raise ValueError('signed package inventory differs from original pair')
    for name, item in record['rpms'].items():
        original = pair['rpms'][name]
        wine_identity(item.get('identity', []), pin)
        if (Path(name).name != name or not name.endswith('.rpm')
                or item.get('identity') != original.get('identity')
                or item.get('unsigned_sha256') != original.get('sha256')):
            raise ValueError('signed package provenance differs from original pair')
    return record


def verify(directory, key_sha256, packages=None, fedora_key=None):
    """Separate trust databases prevent a development key authenticating Fedora RPMs."""
    record = metadata(directory, key_sha256)
    repository = packages or directory / 'rpms'
    files = list(repository.glob('*.rpm'))
    if not files:
        raise ValueError('repository has no RPMs')
    if packages is None and {p.name for p in files} != set(record['rpms']):
        raise ValueError('signed repository has missing or extra RPMs')
    if packages is not None and not set(record['rpms']).issubset({p.name for p in files}):
        raise ValueError('complete signed Wine pair is missing from frozen snapshot')
    cores = set()
    custom_names = {item['identity'][0] for item in record['rpms'].values()}
    with tempfile.TemporaryDirectory(prefix='tilo-rpm-verification-') as temporary:
        private = Path(temporary)
        devdb, fedoradb = private / 'development', private / 'fedora'
        devdb.mkdir(); fedoradb.mkdir()
        command('rpmkeys', '--dbpath', str(devdb), '--import', str(directory / 'public.asc'))
        if fedora_key:
            command('rpmkeys', '--dbpath', str(fedoradb), '--import', str(regular(fedora_key)))
        for path in files:
            regular(path)
            item = record['rpms'].get(path.name)
            if item:
                if digest(path) != item['sha256'] or path.stat().st_size != item['size']:
                    raise ValueError('signed Wine RPM bytes differ')
                verify_signature(path, devdb)
                if item['identity'][0] == 'wine-core':
                    cores.add(item['identity'][3])
            else:
                if not fedora_key:
                    raise ValueError('unexpected non-Wine RPM')
                name = command('rpm', '-qp', '--qf', '%{NAME}', str(path))
                if name in custom_names:
                    raise ValueError('Fedora package replaced a pinned traditional Wine package')
                verify_signature(path, fedoradb)
    if cores != {'i686', 'x86_64'}:
        raise ValueError('both signed Wine core architectures are required')
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('sign', 'verify'))
    parser.add_argument('--pair', type=Path)
    parser.add_argument('--pair-reference')
    parser.add_argument('--pin', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--directory', type=Path)
    parser.add_argument('--key-sha256')
    parser.add_argument('--packages', type=Path)
    parser.add_argument('--fedora-key', type=Path)
    args = parser.parse_args()
    if args.operation == 'sign':
        result = sign(args.pair, args.pair_reference, json.loads(args.pin.read_text()), args.output)
    else:
        result = verify(args.directory, args.key_sha256, args.packages, args.fedora_key)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
