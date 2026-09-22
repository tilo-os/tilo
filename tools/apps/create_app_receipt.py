#!/usr/bin/env python3
"""Generate an unpublished build receipt using already-built canonical binaries."""
import argparse
import json
from pathlib import Path

import app_channel_manifest as channel
from app_package_isa import POLICY
import canonical_app_verifier


def create(directory, pkg_binary, runtime_binary, signers_path, source, architecture):
    # This file comes from the signing invocation, not the downloaded collection.
    # It contains public keys only. Never substitute keys extracted from tapps.
    signers = channel.read_json(signers_path.read_bytes())
    channel.require(set(signers) == {'app_publisher', 'runtime_publisher'}, 'explicit signing invocation keys required')
    verifier_path = Path(canonical_app_verifier.__file__).resolve()
    policy = {'app_publishers': [channel.digest(signers['app_publisher'])],
              'runtime_publisher': channel.digest(signers['runtime_publisher']),
              'verifier_sha256': channel.sha(verifier_path),
              'canonical_binaries': {'tilo_pkg_sha256': channel.sha(pkg_binary),
                                     'tilo_runtime_bundle_sha256': channel.sha(runtime_binary)},
              'allowed_package_isa': {arch: [value[1]] for arch, value in POLICY.items()}}
    archive = directory / f'tilo-apps-{architecture}.tar.gz'
    checksum = (directory / (archive.name + '.sha256')).read_text().split()
    channel.require(len(checksum) == 2 and checksum[1] == archive.name, 'archive checksum sidecar identity mismatch')
    receipt = channel.create_receipt(archive, directory, checksum[0], source, architecture, policy,
                                     verifier_path, policy['verifier_sha256'], canonical_binaries=(pkg_binary, runtime_binary))
    (directory / 'verification-policy.json').write_text(json.dumps(policy, indent=2, sort_keys=True) + '\n')
    (directory / 'publication-receipt.json').write_text(json.dumps(receipt, indent=2, sort_keys=True) + '\n')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('collection', 'pkg-binary', 'runtime-binary', 'signers'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--source-sha', required=True)
    parser.add_argument('--architecture', choices=POLICY, required=True)
    args = parser.parse_args()
    result = create(args.collection, args.pkg_binary, args.runtime_binary, args.signers, args.source_sha, args.architecture)
    print(json.dumps({'architecture': result['architecture'], 'packages': len(result['packages']),
                      'publication': result['publication']['status'], 'backend': result['verification']['backend']}))


if __name__ == '__main__':
    main()
