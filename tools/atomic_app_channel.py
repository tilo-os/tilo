#!/usr/bin/env python3
"""Private Git channel CAS experiment; default prepares locally, never publishes.

Artifact bytes are tiny immutable Git fixture blobs, not installable packages.
Only an explicit apply flag can push the hardcoded private fixture channel ref.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

REPO = 'qos/ci-workflow-pilot-20260918-9245-repeat'
REMOTE = 'ssh://git@code.podesta.ai/' + REPO + '.git'
REF = 'refs/heads/probe-atomic-app-channel-20260919'
FILE = 'app-channel.json'
ARCHES = {'x86_64', 'aarch64'}


def check(condition, message):
    if not condition:
        raise ValueError(message)


def sha(value, size=40):
    check(isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(size) + '}', value), 'invalid immutable digest')
    return value


def encode(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


class Git:
    def __init__(self, directory):
        self.directory = Path(directory).resolve()
        self.fetched = set()
        for args in (('remote', 'get-url', '--all', 'origin'), ('remote', 'get-url', '--push', '--all', 'origin')):
            check(self.call(*args).decode().splitlines() == [REMOTE], 'private fixture remote required')

    def call(self, *args, data=None):
        result = subprocess.run(['git', '--git-dir=' + str(self.directory), *args], input=data,
                                capture_output=True, timeout=30)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors='replace').strip())
        return result.stdout

    def qualify(self):
        output = subprocess.check_output(['fgj', '--hostname', 'code.podesta.ai', 'api', 'repos/' + REPO], timeout=30)
        repository = json.loads(output)
        check(repository.get('full_name') == REPO and repository.get('private') is True, 'fixture privacy mismatch')

    def head(self):
        lines = self.call('ls-remote', 'origin', REF).decode().splitlines()
        check(len(lines) == 1 and lines[0].split()[1] == REF, 'fixture channel ref missing or ambiguous')
        return sha(lines[0].split()[0])

    def fetch(self, commit):
        sha(commit)
        if commit in self.fetched:
            return
        self.call('fetch', '--no-tags', 'origin', sha(commit))
        check(self.call('cat-file', '-t', commit).strip() == b'commit', 'expected commit')
        self.fetched.add(commit)

    def ancestor(self, older, newer):
        try:
            self.call('merge-base', '--is-ancestor', sha(older), sha(newer))
            return True
        except RuntimeError:
            return False

    def manifest(self, commit):
        names = self.call('ls-tree', '--name-only', commit).decode().splitlines()
        if FILE not in names:
            return None
        return json.loads(self.call('show', commit + ':' + FILE))

    def commit(self, parent, manifest):
        blob = self.call('hash-object', '-w', '--stdin', data=encode(manifest)).decode().strip()
        tree = self.call('mktree', data=f'100644 blob {blob}\t{FILE}\n'.encode()).decode().strip()
        return self.call('-c', 'user.name=CI Channel Pilot', '-c', 'user.email=ci-pilot@example.invalid',
                         'commit-tree', tree, '-p', parent, data=b'Atomic private app channel promotion\n').decode().strip()

    def push(self, candidate):
        # Deliberately ordinary non-force push: sibling candidates cannot both
        # fast-forward the same remote ref. Never rebase or retry this mutation.
        return self.call('push', '--no-follow-tags', '--porcelain', 'origin', sha(candidate) + ':' + REF).decode()


def verify_receipt(git, arch, receipt):
    check(arch in ARCHES and receipt.get('architecture') == arch, 'architecture mismatch')
    source = sha(receipt['source_sha'])
    artifact = sha(receipt['artifact_commit'])
    digest = sha(receipt['sha256'], 64)
    runtime = receipt['runtime']
    sha(runtime['root_hash'], 64)
    sha(runtime['publisher'], 64)
    check(bool(re.fullmatch('[A-Za-z0-9._+-]+', runtime['version'])), 'invalid runtime version')
    path = 'fixture-artifacts/' + arch + '.json'
    expected_url = f'https://code.podesta.ai/api/v1/repos/{REPO}/raw/{path}?ref={artifact}'
    check(receipt['url'] == expected_url, 'artifact URL must bind immutable private fixture commit')
    git.fetch(source)
    git.fetch(artifact)
    check(git.ancestor(source, artifact), 'artifact commit does not descend from declared source')
    content = git.call('show', artifact + ':' + path)
    check(len(content) <= 65536 and hashlib.sha256(content).hexdigest() == digest, 'artifact byte digest mismatch')
    expected = {'kind': 'nonpublishing-channel-fixture', 'architecture': arch, 'source_sha': source, 'runtime': runtime}
    check(json.loads(content) == expected, 'artifact receipt does not match immutable payload')


def assemble(git, expected, source, generation, updates):
    sha(expected)
    sha(source)
    check(type(generation) is int and generation > 0, 'positive integer generation required')
    check(updates and set(updates) <= ARCHES, 'known architecture update required')
    git.fetch(expected)
    git.fetch(source)
    previous = git.manifest(expected)
    if previous is None:
        check(generation == 1 and source == expected, 'initial manifest must start at seeded source, generation 1')
        entries = {}
    else:
        check(previous['schema'] == 1 and generation > previous['generation'], 'stale generation')
        git.fetch(previous['source_sha'])
        check(git.ancestor(previous['source_sha'], source), 'source ancestry regression')
        entries = dict(previous['architectures'])
    for arch, receipt in updates.items():
        check(receipt['source_sha'] == source, 'updated architecture must declare promotion source')
        entries[arch] = receipt
    # Verify preserved artifacts too, not just newly uploaded ones. Their source
    # identities remain untouched when this is a partial architecture promotion.
    for arch, receipt in entries.items():
        verify_receipt(git, arch, receipt)
        check(git.ancestor(receipt['source_sha'], source), 'architecture source is not an ancestor of channel source')
    return {'schema': 1, 'generation': generation, 'source_sha': source,
            'previous_channel_commit': expected, 'architectures': entries}


def prepare(git, expected, source, generation, updates):
    git.qualify()
    check(git.head() == expected, 'channel advanced; reconcile before preparing')
    manifest = assemble(git, expected, source, generation, updates)
    return {'expected_head': expected, 'candidate': git.commit(expected, manifest), 'manifest': manifest}


def promote(git, candidate, expected):
    git.qualify()
    parents = git.call('rev-list', '--parents', '-n', '1', sha(candidate)).decode().split()
    check(parents == [candidate, sha(expected)], 'candidate must have exactly expected old as sole parent')
    manifest = git.manifest(candidate)
    check(git.call('ls-tree', '--name-only', candidate).decode().splitlines() == [FILE], 'candidate tree must contain only channel manifest')
    check(manifest is not None and manifest['previous_channel_commit'] == expected, 'candidate parent receipt mismatch')
    previous = git.manifest(expected)
    old_entries = previous['architectures'] if previous else {}
    updates = {arch: receipt for arch, receipt in manifest['architectures'].items() if receipt != old_entries.get(arch)}
    rebuilt = assemble(git, expected, manifest['source_sha'], manifest['generation'], updates)
    check(manifest == rebuilt, 'candidate rewrites preserved identity or contains unexpected fields')
    check(git.head() == expected, 'channel advanced; candidate rejected without retry')
    error = None
    try:
        git.push(candidate)
    except Exception as failure:
        error = type(failure).__name__
    # An uncertain result is reconciled by a read, never by repeating a push.
    try:
        observed = git.head()
    except Exception:
        return {'outcome': 'uncertain', 'candidate': candidate, 'expected_head': expected, 'error': error}
    if observed == candidate:
        return {'outcome': 'promoted', 'candidate': candidate, 'observed_head': observed}
    return {'outcome': 'not-current', 'candidate': candidate, 'observed_head': observed, 'error': error}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--git-dir', type=Path, required=True)
    parser.add_argument('--expected-head', required=True)
    parser.add_argument('--apply-private-fixture', action='store_true')
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--promotion', type=Path, help='JSON source_sha, generation, architectures updates; prepare candidate locally')
    group.add_argument('--candidate', help='locally prepared candidate SHA')
    args = parser.parse_args()
    git = Git(args.git_dir)
    if args.promotion:
        proposal = json.loads(args.promotion.read_text())
        result = prepare(git, args.expected_head, proposal['source_sha'], proposal['generation'], proposal['architectures'])
        candidate = result['candidate']
    else:
        candidate = sha(args.candidate)
        result = {'candidate': candidate, 'expected_head': args.expected_head, 'dry_run': True}
    if args.apply_private_fixture:
        result = promote(git, candidate, args.expected_head)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == '__main__':
    main()
