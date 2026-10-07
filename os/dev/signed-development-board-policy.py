#!/usr/bin/env python3
"""Require the existing OS signer for the opt-in development board repository."""
import json
from pathlib import Path
import sys

REPOSITORY = 'registry.tilo.so/tilo/tilo-vm'
REQUIREMENT = [{'type': 'sigstoreSigned', 'keyPath': '/usr/share/tilo/cosign.pub',
                'signedIdentity': {'type': 'matchRepository'}}]


def strengthen(root: Path) -> None:
    if (root / 'usr/lib/tilo/image-flavour').read_text().strip() != 'dev':
        raise ValueError('signed board policy requires the existing development flavour')
    key = root / 'usr/share/tilo/cosign.pub'
    if key.is_symlink() or not key.is_file():
        raise ValueError('existing OS public key is missing or redirected')
    path = root / 'etc/containers/policy.json'
    if path.is_symlink() or not path.is_file():
        raise ValueError('development policy is missing or redirected')
    policy = json.loads(path.read_text())
    docker = policy['transports']['docker']
    if docker.get(REPOSITORY) != [{'type': 'insecureAcceptAnything'}]:
        raise ValueError('expected the ordinary unsigned development scope')
    docker[REPOSITORY] = REQUIREMENT
    path.write_text(json.dumps(policy, indent=2) + '\n')


if __name__ == '__main__':
    strengthen(Path(sys.argv[1]))
