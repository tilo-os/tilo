#!/usr/bin/env python3
"""Verify a final merged collection using independently pinned policy and tools."""
import argparse
import json
from pathlib import Path
import app_channel_manifest as channel


def load_policy(path, expected):
    policy = channel.pinned_json(path.read_bytes(), expected)
    channel.require(set(policy.get('canonical_binaries', {})) ==
                    {'tilo_pkg_sha256', 'tilo_runtime_bundle_sha256'},
                    'independently pinned canonical executable identities required')
    return policy


def preflight(pkg, runtime, policy_path, policy_sha):
    policy = load_policy(policy_path, policy_sha)
    verifier = Path(__file__).resolve().with_name('canonical_app_verifier.py')
    channel.require(channel.sha(verifier) == policy['verifier_sha256'], 'trusted verifier digest mismatch')
    # Hash first: importing Python is execution, even before constructing a verifier.
    import importlib.util
    spec = importlib.util.spec_from_file_location('pinned_canonical_verifier', verifier)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.CanonicalVerifier(pkg, runtime, policy)
    return policy, verifier


def verify(tree, pkg, runtime, policy_path, policy_sha, source, architecture):
    policy, verifier = preflight(pkg, runtime, policy_path, policy_sha)
    archive = tree / f'tilo-apps-{architecture}.tar.gz'
    # The complete final archive is verified anew. Never reuse the selected-app
    # build receipt or accept its observed verification policy as authority.
    receipt = channel.create_receipt(archive, tree, channel.sha(archive), source, architecture,
                                     policy, verifier, policy['verifier_sha256'],
                                     canonical_binaries=(pkg, runtime))
    (tree / 'final-publication-receipt.json').write_text(json.dumps(receipt, indent=2, sort_keys=True) + '\n')
    return receipt


def bind(receipt_path, receipt_sha, downloaded, url, policy_path, policy_sha, output):
    policy = load_policy(policy_path, policy_sha)
    receipt = channel.pinned_json(receipt_path.read_bytes(), receipt_sha)
    channel.validate_receipt(receipt, receipt['architecture'], policy, require_published=False)
    channel.require(receipt['publication'] == {'status': 'unpublished'}, 'expected unpublished final receipt')
    channel.require(downloaded.stat().st_size == receipt['collection']['bytes'] and
                    channel.sha(downloaded) == receipt['collection']['sha256'], 'uploaded archive readback mismatch')
    upload = {'schema': 'tilo.app-upload/v1', 'source_sha': receipt['source_sha'],
              'architecture': receipt['architecture'], 'sha256': receipt['collection']['sha256'],
              'bytes': receipt['collection']['bytes'], 'url': url}
    raw = (json.dumps(upload, indent=2, sort_keys=True) + '\n').encode()
    import hashlib
    result = channel.attach_publication(receipt, raw, hashlib.sha256(raw).hexdigest(), policy)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    output.with_suffix('.upload.json').write_bytes(raw)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--trust-policy', type=Path, required=True)
    parser.add_argument('--trust-policy-sha256', required=True)
    commands = parser.add_subparsers(dest='command', required=True)
    verify_parser = commands.add_parser('verify')
    for name in ('collection', 'pkg-binary', 'runtime-binary'):
        verify_parser.add_argument('--' + name, type=Path, required=True)
    verify_parser.add_argument('--source-sha', required=True)
    verify_parser.add_argument('--architecture', choices=sorted(channel.ARCHES), required=True)
    binding = commands.add_parser('bind-readback')
    for name in ('receipt', 'downloaded', 'output'):
        binding.add_argument('--' + name, type=Path, required=True)
    binding.add_argument('--receipt-sha256', required=True)
    binding.add_argument('--url', required=True)
    args = parser.parse_args()
    if args.command == 'verify':
        result = verify(args.collection, args.pkg_binary, args.runtime_binary, args.trust_policy,
                        args.trust_policy_sha256, args.source_sha, args.architecture)
    else:
        result = bind(args.receipt, args.receipt_sha256, args.downloaded, args.url,
                      args.trust_policy, args.trust_policy_sha256, args.output)
    print(json.dumps({'architecture': result['architecture'], 'publication': result['publication']['status']}))


if __name__ == '__main__':
    main()
