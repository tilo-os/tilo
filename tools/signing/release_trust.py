#!/usr/bin/env python3
"""Release signing trust: the container policy installed systems enforce, and
the checks that keep CI signing within it.

    release_trust.py policy [--check]
        write os/etc/containers/policy.json from keys/ (or check it is current)
    release_trust.py mode
        print `chain` once keys/release/root.crt exists, else `key`
    release_trust.py signing-certs PURPOSE PUBLIC_KEY
        check the operational certificates CI signs PURPOSE with and print one
        `CERT EPOCH` line for each; nothing in key mode
    release_trust.py check-committed
        check every committed operational certificate against the public key
        of the KMS key it certifies (keys/cosign.pub, keys/extensions.pub)
    release_trust.py verify-args PURPOSE [EPOCH]
        the `cosign verify` arguments that check a signature's certificate chain

Two modes, chosen by what the ceremony (keys/README.md) has committed:

* key: the release key itself is the anchor. Signatures must verify against
  keys/cosign.pub and carry a public transparency-log (Rekor) entry.
* chain: the offline root in keys/release/root.crt is the anchor. Signatures
  must carry an operational certificate it issued for the purpose and current
  epoch, valid when Rekor logged the entry (containers-policy's `fulcio`
  requirement: the root stands in for a Fulcio CA, the certificate's e-mail and
  OIDC-issuer extensions are the identity). Rotating the operational key is a
  new certificate, not a new anchor.

The policy's default is `reject`. Every image reference an installed system
may pull is listed below with the reason it is verified, refused or accepted.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[2]
KEYS = REPO / 'keys'
TRUST_ENV = KEYS / 'release/trust.env'
ROOT = KEYS / 'release/root.crt'
OPERATIONAL = KEYS / 'operational'
POLICY = REPO / 'os/etc/containers/policy.json'

# Where the image installs the trust material the policy names.
INSTALLED_KEY = '/usr/share/tilo/cosign.pub'
INSTALLED_ROOT = '/usr/share/tilo/release/root.crt'
INSTALLED_REKOR = '/usr/share/tilo/release/rekor.pub'

PURPOSES = ('os-images', 'extension-manifests')
# The public half of each purpose's operational KMS key, which CI's signing
# checks compare with KMS before they sign.
OPERATIONAL_KEYS = {'os-images': KEYS / 'cosign.pub', 'extension-manifests': KEYS / 'extensions.pub'}

# Repositories installed systems pull and verify: bootc updates (the board
# images live in the forge's own registry) and the legacy VM guest
# (crates/runtime/legacy-vm).
SIGNED_REPOSITORIES = {
    'registry.tilo.so/tilo/tilo': 'os-images',
    'code.tilo.so/tilo/tilo': 'os-images',
    'registry.tilo.so/tilo/legacy-guest': 'os-images',
}
# Development and CI images Tilo publishes unsigned. Only a development
# image's own policy admits one; the default would refuse them too, these
# entries keep that explicit where tooling looks them up by name.
REJECTED_REPOSITORIES = tuple(f'{host}/tilo/{name}'
                              for host in ('code.tilo.so', 'registry.tilo.so')
                              for name in ('tilo-vm', 'tilo-vm-ref', 'tilo-ci', 'tilo-dev'))
# Public registries a person pulls from by choice: the Containers app, podman
# and tinker's base images. Their publishers do not sign for Tilo, and the
# choice of image is the person's, not the system's. A person adds another
# registry in ~/.config/containers/policy.json, which rootless podman reads
# instead of this file and the system's update path never does.
USER_CHOSEN_REGISTRIES = (
    'docker.io',
    'quay.io',
    'ghcr.io',
    'gcr.io',
    'mcr.microsoft.com',
    'public.ecr.aws',
    'registry.fedoraproject.org',
    'registry.access.redhat.com',
    'registry.gitlab.com',
)
# Local transports: nothing is fetched. containers-storage holds only what a
# policy admitted on pull or what was built on this machine; the archive and
# directory transports read files a person names. bootc's own installs from
# them run with an explicit policy of their own.
LOCAL_TRANSPORTS = ('containers-storage', 'dir', 'docker-archive', 'oci', 'oci-archive')

ACCEPT = [{'type': 'insecureAcceptAnything'}]
REJECT = [{'type': 'reject'}]


def trust() -> dict[str, str]:
    """keys/release/trust.env as a dict."""
    values = {}
    for line in TRUST_ENV.read_text(encoding='utf-8').splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        name, separator, value = line.partition('=')
        if not separator or not re.fullmatch(r'TILO_RELEASE_[A-Z_]+', name):
            raise ValueError(f'{TRUST_ENV}: unexpected line {line!r}')
        values[name] = value
    return values


def epoch() -> int:
    value = int(trust()['TILO_RELEASE_EPOCH'])
    if value < 1:
        raise ValueError('TILO_RELEASE_EPOCH must be positive')
    return value


def issuer(at_epoch: int | None = None) -> str:
    return trust()['TILO_RELEASE_ISSUER_PREFIX'] + str(epoch() if at_epoch is None else at_epoch)


def identity(purpose: str) -> str:
    if purpose not in PURPOSES:
        raise ValueError(f'unknown purpose {purpose!r}')
    return f"{purpose}@{trust()['TILO_RELEASE_IDENTITY_DOMAIN']}"


def mode() -> str:
    return 'chain' if ROOT.exists() else 'key'


def signed_requirement(purpose: str) -> dict:
    if mode() == 'chain':
        anchor = {'fulcio': {'caPath': INSTALLED_ROOT, 'oidcIssuer': issuer(),
                             'subjectEmail': identity(purpose)}}
    else:
        anchor = {'keyPath': INSTALLED_KEY}
    return {'type': 'sigstoreSigned', **anchor, 'rekorPublicKeyPath': INSTALLED_REKOR,
            'signedIdentity': {'type': 'matchRepository'}}


def policy() -> dict:
    docker = {repository: [signed_requirement(purpose)]
              for repository, purpose in SIGNED_REPOSITORIES.items()}
    docker.update({repository: REJECT for repository in REJECTED_REPOSITORIES})
    docker.update({registry: ACCEPT for registry in USER_CHOSEN_REGISTRIES})
    transports = {'docker': dict(sorted(docker.items()))}
    transports.update({transport: {'': ACCEPT} for transport in LOCAL_TRANSPORTS})
    return {'default': REJECT, 'transports': transports}


def render() -> str:
    return json.dumps(policy(), indent=2) + '\n'


def openssl(*args: str, data: bytes | None = None) -> bytes:
    return subprocess.run(['openssl', *args], input=data, check=True,
                          capture_output=True).stdout


def public_key_der(pem: bytes) -> bytes:
    return openssl('pkey', '-pubin', '-outform', 'DER', data=pem)


def check_certificate(path: Path, purpose: str, at_epoch: int, public_key: Path) -> None:
    """Fail unless PATH is a current operational certificate for PURPOSE,
    epoch AT_EPOCH and PUBLIC_KEY that chains to the root."""
    if subprocess.run(['openssl', 'verify', '-x509_strict', '-CAfile', str(ROOT), str(path)],
                      capture_output=True).returncode:
        raise ValueError(f'{path} does not chain to {ROOT}')
    text = openssl('x509', '-in', str(path), '-noout', '-text').decode()
    if f'email:{identity(purpose)}' not in text:
        raise ValueError(f'{path} does not name {identity(purpose)}')
    if issuer(at_epoch) not in text or 'CA:FALSE' not in text or 'Code Signing' not in text:
        raise ValueError(f'{path} is not an operational certificate of {issuer(at_epoch)}')
    certified = openssl('x509', '-in', str(path), '-noout', '-pubkey')
    if public_key_der(certified) != public_key_der(public_key.read_bytes()):
        raise ValueError(f'{path} certifies another key than the one CI signs with')
    days = int(trust()['TILO_RELEASE_OPERATIONAL_MIN_REMAINING_DAYS'])
    if subprocess.run(['openssl', 'x509', '-in', str(path), '-noout', '-checkend',
                       str(days * 86400)], capture_output=True).returncode:
        raise ValueError(f'{path} expires within {days} days; issue its successor '
                         '(keys/README.md, "Renew an operational certificate")')


def signing_certs(purpose: str, public_key: Path) -> list[tuple[Path, int]]:
    """The operational certificates to sign PURPOSE with: the current one and,
    after a revocation, the previous epoch's bridge."""
    if mode() == 'key':
        return []
    identity(purpose)
    current = OPERATIONAL / f'{purpose}.crt'
    if not current.exists():
        raise ValueError(f'{ROOT} exists but {current} does not: run the ceremony '
                         '(keys/README.md) before publishing')
    certs = [(current, epoch())]
    bridge = OPERATIONAL / f'{purpose}.bridge.crt'
    if bridge.exists():
        certs.append((bridge, epoch() - 1))
    for path, at_epoch in certs:
        check_certificate(path, purpose, at_epoch, public_key)
    return certs


def verify_args(purpose: str, at_epoch: int | None = None) -> list[str]:
    return ['--certificate-chain', str(ROOT), '--certificate-identity', identity(purpose),
            '--certificate-oidc-issuer', issuer(at_epoch), '--insecure-ignore-sct']


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest='command', required=True)
    write = commands.add_parser('policy')
    write.add_argument('--check', action='store_true')
    commands.add_parser('mode')
    certs = commands.add_parser('signing-certs')
    certs.add_argument('purpose', choices=PURPOSES)
    certs.add_argument('public_key', type=Path)
    commands.add_parser('check-committed')
    verify = commands.add_parser('verify-args')
    verify.add_argument('purpose', choices=PURPOSES)
    verify.add_argument('epoch', type=int, nargs='?')
    args = parser.parse_args(argv)
    try:
        if args.command == 'policy':
            if args.check:
                if POLICY.read_text(encoding='utf-8') != render():
                    print(f'{POLICY.relative_to(REPO)} is out of date: run '
                          'python3 tools/signing/release_trust.py policy', file=sys.stderr)
                    return 1
            else:
                with tempfile.NamedTemporaryFile('w', dir=POLICY.parent, delete=False,
                                                 encoding='utf-8') as stream:
                    stream.write(render())
                Path(stream.name).chmod(0o644)
                Path(stream.name).replace(POLICY)
        elif args.command == 'mode':
            print(mode())
        elif args.command == 'check-committed':
            for purpose, public_key in OPERATIONAL_KEYS.items():
                for path, at_epoch in signing_certs(purpose, public_key):
                    print(f'{path.relative_to(REPO)}: {identity(purpose)}, epoch {at_epoch}')
        elif args.command == 'signing-certs':
            for path, at_epoch in signing_certs(args.purpose, args.public_key):
                print(path, at_epoch)
        else:
            print('\n'.join(verify_args(args.purpose, args.epoch)))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'release trust: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
