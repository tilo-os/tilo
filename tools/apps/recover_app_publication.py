#!/usr/bin/env python3
"""Read-only preparation and verification of a pinned, already-built release.

No rerun, build, signing, registry mutation, policy generation or stage deletion.
Archive SHA pins are reviewed source, not fields accepted from uploaded policy.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import tarfile
import tomllib
import urllib.request
import zipfile

import app_channel_manifest as channel
import canonical_verifier_bundle as bundle
import final_app_receipt as final

API = 'https://code.tilo.so/api/v1/repos/tilo/tilo'
MAX_METADATA = 2 * 1024**2
MAX_EXPANDED = 2 * 1024**3


def validate_run(run, jobs, manifest):
    channel.require(run['id'] == manifest['run_id']
                    and run['commit_sha'] == manifest['source']
                    and run['workflow_id'] == 'release-apps.yml'
                    and (run.get('event') or run.get('trigger_event')) == 'push'
                    and run['status'] == 'failure'
                    and run['prettyref'] == 'main', 'original main release identity mismatch')
    by_id = {job['id']: job for job in jobs}
    channel.require(len(by_id) == len(jobs), 'duplicate job identity')
    for job_id in manifest['successful_jobs']:
        job = by_id.get(job_id, {})
        channel.require(job.get('status') == 'success' and job.get('attempt') == 1,
                        'original build/authorization no longer qualified')
    job = by_id.get(manifest['publish_job'], {})
    channel.require(job.get('name') == 'publish' and job.get('status') == 'failure'
                    and job.get('attempt') == 1, 'publication already retried or changed')


def validate_artifacts(items, manifest):
    channel.require(isinstance(items, list) and len(items) == len(manifest['artifacts']),
                    'unexpected original artifact inventory')
    by_id = {item['id']: item for item in items}
    channel.require(len(by_id) == len(items), 'duplicate artifact identity')
    for expected in manifest['artifacts']:
        item = by_id.get(expected['id'], {})
        channel.require(item.get('name') == expected['name']
                        and item.get('run_id') == manifest['run_id']
                        and item.get('size_in_bytes') == expected['bytes']
                        and item.get('expired') is False, 'artifact absent/changed/expired')


class ReadOnlyAPI:
    def __init__(self, token):
        channel.require(bool(token), 'existing repository read credential required')
        self.token = token

    def get(self, path, output=None, expected_bytes=None):
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *args, **kwargs):
                return None
        request = urllib.request.Request(API + path,
                                         headers={'Authorization': 'Bearer ' + self.token})
        with urllib.request.build_opener(NoRedirect).open(request, timeout=60) as response:
            if output is None:
                raw = response.read(MAX_METADATA + 1)
                channel.require(len(raw) <= MAX_METADATA, 'API metadata exceeds limit')
                return channel.read_json(raw)
            channel.require(type(expected_bytes) is int and 0 < expected_bytes <= 1024**3,
                            'invalid bounded archive size')
            total = 0
            with output.open('xb') as target:
                while chunk := response.read(1024**2):
                    total += len(chunk)
                    channel.require(total <= expected_bytes, 'artifact download exceeds pin')
                    target.write(chunk)
                target.flush()
                os.fsync(target.fileno())
            channel.require(total == expected_bytes, 'truncated artifact archive')


def inventory(archive):
    entries = archive.infolist()
    channel.require(len(entries) <= 4096 and sum(e.file_size for e in entries) <= MAX_EXPANDED,
                    'ZIP resource limit')
    names = set()
    for entry in entries:
        name = entry.filename
        path = PurePosixPath(name)
        channel.require(name and not path.is_absolute() and '\\' not in name and ':' not in name
                        and all(part not in ('', '.', '..') for part in name.rstrip('/').split('/')),
                        'unsafe ZIP path')
        channel.require(name not in names, 'duplicate ZIP path')
        names.add(name)
        mode = entry.external_attr >> 16
        channel.require(not entry.flag_bits & 1 and
                        (stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR)),
                        'ZIP encrypted/link/special entry')
        channel.require(name.rstrip('/') in {'tapp', 'runtime', 'runtime-package', 'index.json',
                                            'SHA256SUMS', 'publication-receipt.json',
                                            'verification-policy.json', 'tilo-app-package'}
                        or name.startswith(('tapp/', 'runtime/', 'runtime-package/')),
                        'unexpected ZIP inventory')
    return entries


def extract(archive_path, expected, source, output):
    channel.require(archive_path.stat().st_size == expected['bytes']
                    and channel.sha(archive_path) == expected['sha256'], 'ZIP digest mismatch')
    # Validate every name and pinned metadata before creating any extracted file.
    with zipfile.ZipFile(archive_path) as archive:
        entries = inventory(archive)
        for name in ('index.json', 'publication-receipt.json'):
            channel.require(archive.getinfo(name).file_size <= MAX_METADATA, 'oversized metadata')
            raw = archive.read(name)
            channel.require(hashlib.sha256(raw).hexdigest() == expected['metadata_sha256'][name],
                            'original metadata digest mismatch')
            value = channel.read_json(raw)
            field = 'source' if name == 'index.json' else 'source_sha'
            channel.require(value[field] == source and value['architecture'] == expected['architecture'],
                            'original metadata source/architecture mismatch')
        output.mkdir()
        for entry in entries:
            target = output / entry.filename
            if entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(entry) as incoming, target.open('xb') as outgoing:
                    shutil.copyfileobj(incoming, outgoing, 1024**2)
                # Keep only the original regular permission bits. No suid/sgid.
                target.chmod((entry.external_attr >> 16) & 0o777 or 0o644)


def prepare(api, manifest, preserved, release):
    run = api.get(f'/actions/runs/{manifest["run_id"]}')
    jobs = api.get(f'/actions/runs/{manifest["run_id"]}/jobs?page=1&limit=50')
    channel.require(isinstance(jobs, list) and len(jobs) < 50,
                    'unbounded original job listing')
    validate_run(run, jobs, manifest)
    validate_artifacts(api.get(f'/actions/runs/{manifest["run_id"]}/artifacts'), manifest)
    preserved.mkdir(mode=0o700, parents=True, exist_ok=True)
    channel.require(preserved.is_dir() and not preserved.is_symlink()
                    and preserved.stat().st_mode & 0o077 == 0, 'private preservation directory required')
    release.mkdir(parents=True, exist_ok=True)
    for artifact in manifest['artifacts']:
        archive = preserved / artifact['file']
        if not archive.exists():
            partial = archive.with_suffix('.partial')
            try:
                api.get(f'/actions/artifacts/{artifact["id"]}/zip', partial, artifact['bytes'])
                channel.require(channel.sha(partial) == artifact['sha256'], 'downloaded ZIP digest mismatch')
                partial.chmod(0o600)
                partial.rename(archive)
            finally:
                partial.unlink(missing_ok=True)
        destination = release / artifact['name']
        channel.require(not destination.exists(), 'extracted destination already exists')
        extract(archive, artifact, manifest['source'], destination)
    (preserved / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


def pinned_verifier(tools, policy_path, policy_sha):
    policy, adapter = final.preflight(tools / 'tilo-app-package', tools / 'tilo-runtime-bundle',
                                      policy_path, policy_sha)
    # Use the independently pinned bundle adapter, not the candidate checkout's
    # same-named Python module. Preflight has already verified its exact digest.
    channel.require(channel.sha(adapter) == policy['verifier_sha256'], 'pinned adapter changed')
    spec = importlib.util.spec_from_file_location('recovery_pinned_verifier', adapter)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return policy, module.CanonicalVerifier(tools / 'tilo-app-package',
                                            tools / 'tilo-runtime-bundle', policy)


def verify(release, manifest, policy_path, policy_sha, tools):
    # Independent provisioned policy is the sole trust authority. In particular,
    # never load verification-policy.json from either downloaded archive.
    bundle.fetch(policy_path, policy_sha, tools, manifest['builder'],
                 os.environ.get('VERIFIER_BUNDLE_READ_TOKEN', ''))
    policy, verifier = pinned_verifier(tools, policy_path, policy_sha)
    proofs = []
    for artifact in manifest['artifacts']:
        tree = release / artifact['name']
        for name, digest in artifact['metadata_sha256'].items():
            channel.require(channel.sha(tree / name) == digest, 'prepared metadata changed')
        index = channel.read_json((tree / 'index.json').read_bytes())
        original = channel.read_json((tree / 'publication-receipt.json').read_bytes())
        channel.require(index['source'] == original['source_sha'] == manifest['source']
                        and index['architecture'] == original['architecture'] == artifact['architecture'],
                        'verified source mismatch')
        runtime = verifier.verify_runtime(tree / 'runtime', artifact['architecture'])
        channel.require(all(runtime[key] == original['runtime'][key] for key in runtime),
                        'original runtime identity mismatch')
        by_file = {app['file']: app for app in original['packages']}
        channel.require(len(by_file) == len(original['packages']) == len(index['apps']) == 59,
                        'original package inventory mismatch')
        observed = []
        for app in index['apps']:
            path = tree / 'tapp' / app['file']
            expected = by_file.get(app['file'], {})
            channel.require(channel.sha(path) == app['sha256'] == expected.get('sha256'),
                            'original package digest mismatch')
            identity = verifier.verify_tapp(path, app)
            channel.require(identity['package_id'] == expected['package_id']
                            and identity['publisher'] == expected['publisher']
                            and app['version'] == expected['version']
                            and app['id'] == expected['app_id'], 'original signed package identity mismatch')
            with tarfile.open(path, 'r:') as tapp:
                signed = tomllib.loads(tapp.extractfile('manifest.toml').read().decode())
            channel.require(signed['package']['isa'] == {artifact['architecture']: expected['isa']}
                            and expected['isa'] in policy['allowed_package_isa'][artifact['architecture']],
                            'original signed ISA is not allowed')
            observed.append({'file': app['file'], 'sha256': app['sha256'], **identity})
        channel.require(set(by_file) == {p.name for p in (tree / 'tapp').iterdir()},
                        'unexpected package payload')
        proofs.append({'architecture': artifact['architecture'], 'runtime': runtime, 'packages': observed})
    (release / 'recovery-verification.json').write_text(json.dumps({
        'source': manifest['source'], 'run_id': manifest['run_id'], 'policy_sha256': policy_sha,
        'archives': manifest['artifacts'], 'verified': proofs}, indent=2) + '\n')
    # Normal publication executes the host-native package CLI. Replace only
    # this tool (never a signed package) with the independently verified binary;
    # uploaded executable bytes are not a verification or execution authority.
    native = release / f'tilo-apps-{bundle.native_arch()}' / 'tilo-app-package'
    shutil.copyfile(tools / 'tilo-app-package', native)
    native.chmod(0o755)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['prepare', 'verify'])
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--release', type=Path, required=True)
    parser.add_argument('--preserved', type=Path)
    parser.add_argument('--policy', type=Path)
    parser.add_argument('--policy-sha256')
    parser.add_argument('--tools', type=Path)
    args = parser.parse_args()
    manifest = channel.read_json(args.manifest.read_bytes())
    channel.require(re.fullmatch('[0-9a-f]{40}', manifest['source']), 'invalid source identity')
    if args.command == 'prepare':
        channel.require(args.preserved is not None, 'preservation path required')
        prepare(ReadOnlyAPI(os.environ.get('FORGEJO_ARTIFACT_TOKEN', '')),
                manifest, args.preserved, args.release)
    else:
        channel.require(args.policy is not None and args.tools is not None
                        and args.policy_sha256 is not None, 'independent verification inputs required')
        verify(args.release, manifest, args.policy, args.policy_sha256, args.tools)


if __name__ == '__main__':
    main()
