#!/usr/bin/env python3
"""Nonpublishing final collection assembly from pinned verified baseline artifacts."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tomllib
import urllib.request
import zipfile

import app_channel_manifest as channel
import final_app_receipt as final

API = 'https://code.tilo.so/api/v1/repos/tilo/tilo'
MAX_BYTES = 2 * 1024**3
MAX_EXPANDED = 4 * 1024**3


def canonical_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def safe_name(name):
    path = PurePosixPath(name)
    channel.require(name and not path.is_absolute() and '\\' not in name and ':' not in name
                    and all(p not in ('', '.', '..') for p in name.rstrip('/').split('/')), 'unsafe archive path')
    return path


def extract_tar(archive, output):
    output.mkdir()
    count = total = 0
    names = set()
    with tarfile.open(archive, 'r:gz') as stream:
        for member in stream:
            name = member.name.rstrip('/')
            safe_name(name)
            count += 1
            total += member.size
            channel.require(count <= 4096 and 0 <= member.size and total <= MAX_EXPANDED and name not in names, 'archive resource/duplicate limit')
            names.add(name)
            channel.require(name in {'tapp', 'runtime', 'index.json', 'SHA256SUMS'} or name.startswith(('tapp/', 'runtime/')), 'unexpected collection path')
            channel.require(member.isdir() or (member.isfile() and not member.issparse()), 'archive links/sparse/special rejected')
            target = output / name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with stream.extractfile(member) as source, target.open('xb') as destination:
                    shutil.copyfileobj(source, destination)


@contextmanager
def download_destination(output):
    """Remove failed downloads without deleting a pre-existing destination."""
    if output is None:
        yield None
        return
    target = output.open('xb')
    try:
        with target:
            yield target
    except BaseException:
        output.unlink()
        raise


class ReadOnlyAPI:
    def __init__(self, token):
        self.token = token

    def get(self, path, output=None):
        # No redirect may carry the repository task credential to another origin.
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *args, **kwargs):
                return None
        request = urllib.request.Request(API + path, headers={'Authorization': 'Bearer ' + self.token})
        with urllib.request.build_opener(NoRedirect).open(request, timeout=60) as response:
            maximum = MAX_BYTES if output else 2 * 1024**2
            length = response.headers.get('Content-Length')
            channel.require(length is None or (length.isdigit() and int(length) <= maximum),
                            'API response exceeds limit')
            with download_destination(output) as target:
                if target is not None:
                    size = 0
                    while chunk := response.read(1024**2):
                        size += len(chunk)
                        channel.require(size <= maximum, 'artifact download exceeds limit')
                        target.write(chunk)
                    channel.require(length is None or size == int(length), 'API response truncated')
                    return
                raw = response.read(maximum + 1)
                channel.require(len(raw) <= maximum, 'API response exceeds limit')
                channel.require(length is None or len(raw) == int(length), 'API response truncated')
                return channel.read_json(raw)


def baseline_tree(api, proof, expected_hash, source, builder, collection, architecture, policy_sha, work, policy):
    channel.require(canonical_hash(proof) == channel.digest(expected_hash), 'baseline proof digest mismatch')
    channel.require(proof['schema'] == 'tilo.coordinator-verification/v1' and proof['backend'] == 'canonical-final-artifact-v1', 'baseline not independently verified')
    desired = proof['desired']
    channel.require(desired['source_sha'] == source and desired['builder_image'] == builder
                    and desired['collection'] == collection and desired['policy_sha256'] == policy_sha
                    and architecture in desired['architectures'], 'baseline scope mismatch')
    run_id = proof['run_id']
    channel.require(type(run_id) is int and run_id > 0, 'invalid baseline run')
    run = api.get(f'/actions/runs/{run_id}')
    channel.require(run['id'] == run_id and run['commit_sha'] == source and run['status'] == 'success'
                    and run['workflow_id'] == 'release-apps.yml'
                    and (run.get('trigger_event') or run.get('event')) == 'workflow_dispatch', 'baseline run provenance mismatch')
    listing = api.get(f'/actions/runs/{run_id}/artifacts?page=1&limit=50')
    channel.require(isinstance(listing, list), 'unexpected Forgejo artifact response')
    artifacts = listing
    channel.require(len(artifacts) < 50, 'artifact listing requires unbounded pagination')
    matches = [item for item in artifacts if item['name'] == f'final-app-collection-{architecture}']
    channel.require(len(matches) == 1 and not matches[0].get('expired', True), 'baseline artifact absent/ambiguous/expired')
    artifact_id = matches[0]['id']
    channel.require(type(artifact_id) is int and artifact_id > 0 and matches[0]['run_id'] == run_id, 'invalid artifact/run identity')
    bundle = work / 'baseline.zip'
    api.get(f'/actions/artifacts/{artifact_id}/zip', bundle)
    expected = {f'tilo-apps-{architecture}.tar.gz', 'final-publication-receipt.json', 'assembly-provenance.json'}
    with zipfile.ZipFile(bundle) as archive:
        entries = archive.infolist()
        channel.require(len(entries) == 3 and {entry.filename for entry in entries} == expected, 'unexpected baseline ZIP inventory')
        channel.require(sum(entry.file_size for entry in entries) <= MAX_BYTES, 'baseline ZIP exceeds limit')
        for entry in entries:
            safe_name(entry.filename)
            mode = entry.external_attr >> 16
            channel.require(not mode or (mode & 0o170000) in (0, 0o100000), 'ZIP nonregular entry')
            with archive.open(entry) as source_file, (work / entry.filename).open('xb') as target:
                shutil.copyfileobj(source_file, target)
    receipt = channel.pinned_json((work / 'final-publication-receipt.json').read_bytes(), proof['receipts'][architecture])
    channel.validate_receipt(receipt, architecture, policy, require_published=False)
    channel.require(receipt['source_sha'] == source, 'baseline receipt source mismatch')
    archive = work / f'tilo-apps-{architecture}.tar.gz'
    channel.require(archive.stat().st_size == receipt['collection']['bytes'] and channel.sha(archive) == receipt['collection']['sha256'], 'baseline archive digest mismatch')
    tree = work / 'baseline-tree'
    extract_tar(archive, tree)
    channel.archive_matches_tree(archive, tree)
    return tree


def runtime_root(tree):
    manifests = list((tree / 'runtime/tilo-runtime').glob('*/bundle.toml'))
    channel.require(len(manifests) == 1, 'one runtime required')
    return tomllib.loads(manifests[0].read_text())['root_hash']


def merge(plan, built, baseline, output, architecture, selected_apps):
    catalog = {app['name']: app for app in plan['catalog']}
    selected = set(selected_apps)
    channel.require(len(catalog) == len(plan['catalog']), 'duplicate catalog app')
    channel.require(len(selected) == len(selected_apps) and selected <= set(catalog),
                    'selected apps must be a unique subset of the collection')
    for app in catalog.values():
        channel.require(all(re.fullmatch('[A-Za-z0-9][A-Za-z0-9._+-]*', value)
                            for value in (app['name'], app['version'])), 'unsafe package identity')
    # A changed app cannot be replaced by an older same-version signed package.
    # Check the trusted selector output completely before copying any baseline.
    for name in selected:
        path = built / 'tapp' / f"{name}-{catalog[name]['version']}-{architecture}.tapp"
        channel.require(path.is_file() and not path.is_symlink(),
                        f'selected app missing from current build: {name}')
    output.mkdir()
    reused = []
    for app in plan['catalog']:
        name, version = app['name'], app['version']
        channel.require(all(re.fullmatch('[A-Za-z0-9][A-Za-z0-9._+-]*', value) for value in (name, version)), 'unsafe package identity')
        filename = f'{name}-{version}-{architecture}.tapp'
        path = built / 'tapp' / filename
        # Reuse is valid only for absent output, not a failed or malformed
        # current build (including a dangling symlink).
        if path.exists() or path.is_symlink():
            channel.require(path.is_file() and not path.is_symlink(), 'invalid current build package')
        else:
            channel.require(name not in selected, f'selected app missing from current build: {name}')
            channel.require(baseline is not None, 'missing package without verified baseline')
            path = baseline / 'tapp' / filename
            reused.append(filename)
        channel.require(path.is_file() and not path.is_symlink(), 'missing regular collection package')
        shutil.copyfile(path, output / filename)
    if reused:
        channel.require(runtime_root(built) == runtime_root(baseline), 'runtime changed: full rebuild required')
    return reused


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('built', 'plan', 'output', 'work', 'pkg-binary', 'runtime-binary', 'trust-policy'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('source-sha', 'builder-image', 'collection', 'architecture', 'trust-policy-sha256', 'coordinator-policy-sha256', 'intent', 'run-id', 'source-epoch'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--qualifier-attempt', type=int, required=True)
    parser.add_argument('--selected-apps', required=True, help='Exact space-separated needs.plan.outputs.apps')
    parser.add_argument('--baseline-proof', type=Path)
    parser.add_argument('--baseline-proof-sha256', default='')
    parser.add_argument('--baseline-source', default='')
    args = parser.parse_args()
    channel.require(args.qualifier_attempt > 0, 'positive qualifier attempt required')
    channel.digest(args.source_sha, 40)
    channel.require(re.fullmatch(r'registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}', args.builder_image), 'immutable builder required')
    policy, _ = final.preflight(args.pkg_binary, args.runtime_binary, args.trust_policy, args.trust_policy_sha256)
    args.work.mkdir(parents=True, exist_ok=False)
    baseline = None
    if args.baseline_source:
        channel.require(args.baseline_proof is not None, 'baseline proof required')
        proof = channel.read_json(args.baseline_proof.read_bytes())
        baseline = baseline_tree(ReadOnlyAPI(os.environ.get('FORGEJO_ARTIFACT_TOKEN', '')), proof,
            args.baseline_proof_sha256, args.baseline_source, args.builder_image, args.collection,
            args.architecture, args.coordinator_policy_sha256, args.work, policy)
    plan = channel.read_json(args.plan.read_bytes())
    selected = args.work / 'selected'
    reused = merge(plan, args.built, baseline, selected, args.architecture, args.selected_apps.split())
    environment = {'PATH': os.defpath, 'TILO_APP_COLLECTION_PLAN': str(args.plan.resolve()),
        'TILO_APP_ARCH': args.architecture, 'TILO_APP_TAPP_DIR': str(selected.resolve()),
        'TILO_APP_RUNTIME_DIR': str((args.built / 'runtime').resolve()),
        'TILO_APP_COLLECTION_OUT': str(args.output.resolve()), 'GITHUB_SHA': args.source_sha,
        'SOURCE_DATE_EPOCH': args.source_epoch}
    subprocess.run(['bash', 'dist/apps/assemble-collection.sh'], env=environment, check=True)
    receipt = final.verify(args.output, args.pkg_binary, args.runtime_binary, args.trust_policy,
                           args.trust_policy_sha256, args.source_sha, args.architecture)
    provenance = {'schema': 'tilo.app-assembly/v1', 'intent': args.intent, 'run_id': args.run_id,
        'source_sha': args.source_sha, 'builder_image': args.builder_image, 'architecture': args.architecture,
        'coordinator_policy_sha256': args.coordinator_policy_sha256, 'baseline_source': args.baseline_source,
        'baseline_proof_sha256': args.baseline_proof_sha256, 'reused_packages': reused,
        'final_receipt_sha256': channel.sha(args.output / 'final-publication-receipt.json')}
    (args.output / 'assembly-provenance.json').write_text(json.dumps(provenance, indent=2, sort_keys=True) + '\n')
    print(f"TILO_COORDINATOR_FINAL {args.intent} {args.qualifier_attempt} {args.architecture} {provenance['final_receipt_sha256']} {receipt['collection']['sha256']} {receipt['collection']['bytes']}")
    print(json.dumps({'architecture': args.architecture, 'packages': len(receipt['packages']), 'reused': len(reused), 'published': False}))


if __name__ == '__main__':
    main()
