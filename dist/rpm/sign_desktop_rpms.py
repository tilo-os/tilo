#!/usr/bin/env python3
"""Sign a desktop release's tested Fedora RPMs with the RPM key in KMS, and prove the result.

    sign_desktop_rpms.py sign --certificate CERT --key KEY --version V --arch ARCH \\
        [--tested SOURCE_JSON] LANE OUT
    sign_desktop_rpms.py verify --certificate CERT --version V --arch ARCH RELEASE

`sign` takes the three RPMs a Fedora package lane built and install-tested (LANE, its
dist/ci-output/fedora-44-<arch>), and with --tested refuses any whose SHA-256 is not the one
host-session-e2e recorded for the packages it logged into. It signs copies with `rpmsign
--addsign`, whose `%__gpg_sign_cmd` is openpgp_kms.py beside this file: rpm hands it the bytes,
KMS signs their digest, the key never leaves KMS. Each copy must then verify with `rpmkeys
--checksig` against CERT alone, and keep the tested package's lead, immutable header and payload
byte for byte: only the signature header may differ. Only then are the signed copies written to
OUT, with tilo-rpm-signing-receipt-<arch>.json naming, for each, the tested and signed SHA-256
and the immutable bytes' SHA-256.

`verify` checks a release directory the same way before it is published: each RPM signed by
CERT's key, its bytes the ones the receipt names, its immutable bytes the tested ones.
"""
from __future__ import annotations

import argparse
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

sys.path.insert(0, str(Path(__file__).resolve().parent))
import openpgp_kms  # noqa: E402

NAMES = ('tilo-session', 'tilo-session-apps', 'tilo-session-devel')
SUFFIX = {'amd64': 'x86_64', 'arm64': 'aarch64'}
LEAD_MAGIC = bytes.fromhex('edabeedb')
HEADER_MAGIC = bytes.fromhex('8eade801')
RECEIPT_KEYS = {'schema', 'arch', 'version', 'commit', 'ref', 'fingerprint', 'rpms'}
RPM_KEYS = {'name', 'tested_sha256', 'immutable_sha256', 'signed_sha256'}
# rpm runs %__gpg_sign_cmd split on whitespace, without a shell.
PLAIN_ARGUMENT = re.compile(r'[A-Za-z0-9/._:+=-]+')


class Refused(Exception):
    """A package, receipt or signature this release must not ship."""


def regular(path: Path) -> None:
    if not stat.S_ISREG(path.lstat().st_mode):
        raise Refused(f'not a regular file: {path.name}')


def sha256(path: Path) -> str:
    regular(path)
    digest = hashlib.sha256()
    with path.open('rb') as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def immutable_digest(path: Path) -> str:
    """SHA-256 of the lead, the immutable header and the payload: everything but the aligned
    signature header, which is all signing may change."""
    regular(path)
    with path.open('rb') as source:
        lead = source.read(96)
        header = source.read(16)
        if len(lead) != 96 or lead[:4] != LEAD_MAGIC:
            raise Refused(f'{path.name}: not an RPM lead')
        if len(header) != 16 or header[:4] != HEADER_MAGIC:
            raise Refused(f'{path.name}: no RPM signature header')
        count, size = struct.unpack('>II', header[8:])
        if count > 4096 or size > 16 * 1024 * 1024:
            raise Refused(f'{path.name}: unbounded RPM signature header')
        offset = 96 + ((16 + count * 16 + size + 7) // 8) * 8
        source.seek(offset)
        if source.read(4) != HEADER_MAGIC:
            raise Refused(f'{path.name}: no RPM immutable header')
        source.seek(offset)
        digest = hashlib.sha256(lead)
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def expected_names(version: str, arch: str) -> set[str]:
    if arch not in SUFFIX:
        raise Refused(f'no desktop release architecture {arch}')
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', version):
        raise Refused(f'not a desktop release version: {version}')
    return {f'{name}-{version}-1.fc44.{SUFFIX[arch]}.rpm' for name in NAMES}


def tested_rpms(lane: Path, version: str, arch: str) -> list[Path]:
    """The lane's three RPMs, top level only: a failed lane keeps its RPMs in failed-rpms/."""
    rpms = sorted(lane.glob('*.rpm'))
    if {path.name for path in rpms} != expected_names(version, arch):
        raise Refused(f'{lane} holds {sorted(path.name for path in rpms)}, '
                      f'not the three {arch} RPMs of {version}')
    for path in rpms:
        regular(path)
    return rpms


def check_tested(rpms: list[Path], source: Path) -> None:
    """Refuse RPMs other than those host-session-e2e logged into (its source.json)."""
    report = json.loads(source.read_text())
    artifacts = report.get('artifacts', {})
    for path in rpms:
        recorded = artifacts.get(path.name, {}).get('sha256')
        if recorded != sha256(path):
            raise Refused(f'{path.name} is not the package host-session-e2e tested '
                          f'(recorded {recorded}, lane {sha256(path)})')


def plain(value: str) -> str:
    if not PLAIN_ARGUMENT.fullmatch(value):
        raise Refused(f'not usable in an rpm signing command: {value!r}')
    return value


def rpmsign(certificate: Path, key: str, fingerprint: str, files: list[Path]) -> None:
    """Sign FILES in place with rpmsign, the signature made by openpgp_kms.py."""
    python = plain(str(Path(sys.executable).resolve()))
    signer = plain(str(Path(__file__).resolve().with_name('openpgp_kms.py')))
    # rpm execs the first word with the rest as argv (argv[0] second), and feeds the bytes to
    # sign through __plaintext_filename where it defines it, else on stdin.
    command = (f'{python} python3 -I {signer} sign --certificate {plain(str(certificate.resolve()))} '
               f'--key {plain(key)} --output %{{__signature_filename}} %{{?__plaintext_filename}}')
    subprocess.run(['rpmsign', '--define', '_openpgp_sign gpg',
                    '--define', f'_openpgp_sign_id {fingerprint}', '--define', f'_gpg_name {fingerprint}',
                    '--define', f'__gpg_sign_cmd {command}', '--addsign', *map(str, files)],
                   check=True, stdin=subprocess.DEVNULL)


def signature_verified(output: str, fingerprint: str) -> bool:
    """Whether `rpmkeys --checksig --verbose` reports a good signature by FINGERPRINT's key
    (rpm 4 names its key ID, rpm 6 its fingerprint) and nothing that failed."""
    if re.search(r'NOT OK|NOKEY|NOTTRUSTED|NOTFOUND|BAD', output):
        return False
    key_ids = re.findall(r'signature, key ID ([0-9a-f]+): OK', output, re.IGNORECASE)
    fingerprints = re.findall(r'signature, key fingerprint: ([0-9a-f]+): OK', output, re.IGNORECASE)
    if not key_ids and not fingerprints:
        return False
    fingerprint = fingerprint.lower()
    return (all(len(key_id) >= 8 and fingerprint.endswith(key_id.lower()) for key_id in key_ids)
            and all(found.lower() == fingerprint for found in fingerprints))


def verify_signatures(certificate: Path, fingerprint: str, files: list[Path]) -> None:
    """Each of FILES verifies with rpmkeys against a keyring holding CERTIFICATE alone."""
    with tempfile.TemporaryDirectory(prefix='rpm-keyring-') as keyring:
        subprocess.run(['rpmkeys', '--dbpath', keyring, '--import', str(certificate.resolve())],
                       check=True, capture_output=True, text=True)
        for path in files:
            checked = subprocess.run(['rpmkeys', '--dbpath', keyring, '--checksig', '--verbose', str(path)],
                                     capture_output=True, text=True)
            output = checked.stdout + checked.stderr
            if checked.returncode != 0 or not signature_verified(output, fingerprint):
                raise Refused(f'{path.name} does not verify with {fingerprint}:\n{output}')


def sign_lane(certificate: Path, key: str, lane: Path, version: str, arch: str, out: Path,
              tested: Path | None = None, environ=os.environ) -> dict:
    fingerprint = openpgp_kms.Certificate.load(certificate).fingerprint.hex().upper()
    rpms = tested_rpms(lane, version, arch)
    if tested is not None:
        check_tested(rpms, tested)
    if out.exists():
        raise Refused(f'{out} exists')
    manifest = {path.name: {'name': path.name, 'tested_sha256': sha256(path),
                            'immutable_sha256': immutable_digest(path)} for path in rpms}
    with tempfile.TemporaryDirectory(prefix='desktop-rpms-') as temporary:
        copies = []
        for path in rpms:
            copy = Path(temporary) / path.name
            shutil.copyfile(path, copy)
            copies.append(copy)
        rpmsign(certificate, key, fingerprint, copies)
        verify_signatures(certificate, fingerprint, copies)
        for copy in copies:
            entry = manifest[copy.name]
            if immutable_digest(copy) != entry['immutable_sha256']:
                raise Refused(f'signing changed the header or payload of {copy.name}')
            entry['signed_sha256'] = sha256(copy)
            if entry['signed_sha256'] == entry['tested_sha256']:
                raise Refused(f'{copy.name} came back unsigned')
        # Nothing is written until every copy qualified.
        out.mkdir(parents=True)
        for copy in copies:
            shutil.copyfile(copy, out / copy.name)
    receipt = {'schema': 2, 'arch': arch, 'version': version,
               'commit': environ.get('T_COMMIT', ''), 'ref': environ.get('T_REF', ''),
               'fingerprint': fingerprint, 'rpms': [manifest[name] for name in sorted(manifest)]}
    (out / f'tilo-rpm-signing-receipt-{arch}.json').write_text(json.dumps(receipt, indent=1) + '\n')
    return receipt


def verify_release(certificate: Path, release: Path, version: str, arch: str) -> dict:
    fingerprint = openpgp_kms.Certificate.load(certificate).fingerprint.hex().upper()
    path = release / f'tilo-rpm-signing-receipt-{arch}.json'
    regular(path)
    if path.stat().st_size > 65536:
        raise Refused(f'{path.name} is unbounded')
    receipt = json.loads(path.read_text())
    if set(receipt) != RECEIPT_KEYS or receipt['schema'] != 2:
        raise Refused(f'{path.name} is not a schema 2 signing receipt')
    if (receipt['arch'], receipt['version'], receipt['fingerprint']) != (arch, version, fingerprint):
        raise Refused(f'{path.name} is for {receipt["arch"]} {receipt["version"]} signed by '
                      f'{receipt["fingerprint"]}, not {arch} {version} signed by {fingerprint}')
    entries = receipt['rpms']
    if (not all(isinstance(entry, dict) and set(entry) == RPM_KEYS for entry in entries)
            or {entry['name'] for entry in entries} != expected_names(version, arch)
            or len(entries) != len(NAMES)):
        raise Refused(f'{path.name} does not name the three {arch} RPMs of {version}')
    files = []
    for entry in entries:
        rpm = release / entry['name']
        if sha256(rpm) != entry['signed_sha256']:
            raise Refused(f'{rpm.name} is not the signed package the receipt names')
        if immutable_digest(rpm) != entry['immutable_sha256']:
            raise Refused(f'{rpm.name} differs from the tested package in its header or payload')
        files.append(rpm)
    verify_signatures(certificate, fingerprint, files)
    return receipt


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n', 1)[0])
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('sign', 'verify'):
        command = commands.add_parser(name)
        command.add_argument('--certificate', type=Path, required=True)
        command.add_argument('--version', required=True)
        command.add_argument('--arch', required=True, choices=sorted(SUFFIX))
        if name == 'sign':
            command.add_argument('--key', required=True)
            command.add_argument('--tested', type=Path)
            command.add_argument('lane', type=Path)
            command.add_argument('out', type=Path)
        else:
            command.add_argument('release', type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == 'sign':
            receipt = sign_lane(args.certificate, args.key, args.lane, args.version, args.arch,
                                args.out, args.tested)
            print(f'signed and verified the {args.arch} RPMs of {args.version} with {receipt["fingerprint"]}')
        else:
            verify_release(args.certificate, args.release, args.version, args.arch)
            print(f'verified the signed {args.arch} RPMs of {args.version}')
    except (Refused, openpgp_kms.Refused, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f'sign_desktop_rpms: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
