#!/usr/bin/env python3
"""Record verified native WPE candidate inputs; this receipt is not a signature."""
import argparse
import hashlib
import json
from pathlib import Path
import re

REPOSITORY = 'code.podesta.ai/qos/tilo-wpe-buildroot'
ARCHITECTURES = {'x86_64': 'amd64', 'aarch64': 'arm64'}


def receipt(args):
    if args.architecture not in ARCHITECTURES or not re.fullmatch('[0-9a-f]{40}', args.revision):
        raise ValueError('invalid native architecture or source revision')
    if not all(re.fullmatch('[1-9][0-9]*', value) for value in (args.run_id, args.attempt)):
        raise ValueError('invalid run identity')
    expected_tag = f'{REPOSITORY}:candidate-{args.revision}-{args.run_id}-{args.attempt}-{args.architecture}'
    if args.tag != expected_tag or not re.fullmatch(re.escape(REPOSITORY) + r'@sha256:[0-9a-f]{64}', args.reference):
        raise ValueError('candidate repository or unique tag differs')
    if not re.fullmatch(r'code\.podesta\.ai/qos/fedora-bootc@sha256:[0-9a-f]{64}', args.base):
        raise ValueError('base must be the pinned Fedora mirror')
    images = json.loads(args.inspection.read_text())
    if not isinstance(images, list) or len(images) != 1:
        raise ValueError('expected one candidate image')
    image = images[0]
    labels = image.get('Config', {}).get('Labels') or {}
    if (image.get('Architecture') != ARCHITECTURES[args.architecture] or image.get('Os') != 'linux'
            or args.reference not in image.get('RepoDigests', [])
            or labels.get('org.opencontainers.image.revision') != args.revision
            or labels.get('ai.tilo.wpe.base') != args.base
            or labels.get('ai.tilo.wpe.purpose') != 'development-candidate'):
        raise ValueError('candidate architecture, digest or provenance differs')
    packages = args.packages.read_bytes()
    lines = packages.decode().splitlines()
    if not lines or any(not line for line in lines) or lines != sorted(lines):
        raise ValueError('package inventory must be nonempty and sorted')
    return {'schema': 1, 'purpose': 'tilo-development-wpe-candidate', 'production_signed': False,
            'reference': args.reference, 'tag': args.tag, 'architecture': args.architecture,
            'base_reference': args.base, 'revision': args.revision,
            'run_id': args.run_id, 'attempt': args.attempt,
            'recipe_sha256': hashlib.sha256(args.recipe.read_bytes()).hexdigest(),
            'package_inventory_sha256': hashlib.sha256(packages).hexdigest(), 'packages': len(lines)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('architecture', 'revision', 'run-id', 'attempt', 'tag', 'reference', 'base'):
        parser.add_argument('--' + name, required=True)
    for name in ('inspection', 'packages', 'recipe'):
        parser.add_argument('--' + name, type=Path, required=True)
    try:
        print(json.dumps(receipt(parser.parse_args()), indent=2, sort_keys=True))
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f'WPE candidate receipt: {error}\n')


if __name__ == '__main__':
    main()
