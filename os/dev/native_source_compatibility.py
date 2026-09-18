"""Explicit frozen-native reuse after one reviewed Python validator correction."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
PATHS = ('os/dev/windows_provider.py', 'os/tests/test_windows_provider.py')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.DEVNULL, timeout=30)


def read_approval(path, digest):
    path = Path(path)
    require(re.fullmatch('[0-9a-f]{64}', digest or '') and path.is_file()
            and not path.is_symlink() and path.stat().st_size <= 65536, 'bounded pinned source admission required')
    raw = path.read_bytes()
    require(len(raw) <= 65536 and sha(raw) == digest, 'source admission bytes differ')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate source admission property')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def verify(repo, revision, path, digest, native_receipt_sha256, reconstruction_sha256):
    """Compare complete committed trees before any source compatibility adoption."""
    approval = read_approval(path, digest)
    require(set(approval) == {'schema', 'purpose', 'native_source_revision', 'assembly_source_revision',
        'native_receipt_sha256', 'approved_reconstruction_sha256', 'changes'}, 'source admission schema differs')
    require(type(approval['schema']) is int and approval['schema'] == 1
        and approval['purpose'] == 'tilo-windows-provider-native-source-compatibility'
        and approval['native_source_revision'] == SOURCE
        and approval['assembly_source_revision'] == revision and revision != SOURCE
        and re.fullmatch('[0-9a-f]{40}', revision)
        and approval['native_receipt_sha256'] == native_receipt_sha256
        and approval['approved_reconstruction_sha256'] == reconstruction_sha256,
        'source admission identity differs')
    require(git(repo, 'rev-parse', 'HEAD').decode().strip() == revision
        and not git(repo, 'status', '--porcelain', '--untracked-files=all').strip(),
        'compatible assembly source must be clean and exact')
    # Two-deep authenticated checkout contains both commits. No hidden ancestor
    # chain, rename heuristics, or unrecorded changes are permitted.
    require(git(repo, 'show', '-s', '--format=%P', revision).decode().strip() == SOURCE,
        'assembly correction must directly descend from frozen native source')
    changed = git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', SOURCE, revision)
    require(changed.split(b'\0')[:-1] == [name.encode() for name in PATHS]
            and set(approval['changes']) == set(PATHS), 'complete tree diff exceeds reviewed Python correction')
    changes = {}
    for name in PATHS:
        records = []
        for commit in (SOURCE, revision):
            entry = git(repo, 'ls-tree', commit, '--', name).decode().strip().split()
            require(len(entry) == 4 and entry[0] in ('100644', '100755') and entry[1] == 'blob'
                    and entry[3] == name, 'source correction must be a regular tracked file')
            records.append((entry[0], entry[2], sha(git(repo, 'show', commit + ':' + name))))
        require(records[0][0] == records[1][0], 'source correction file mode changed')
        expected = dict(old_blob=records[0][1], new_blob=records[1][1],
                        old_sha256=records[0][2], new_sha256=records[1][2])
        require(approval['changes'][name] == expected, 'source correction blob differs: ' + name)
        changes[name] = expected
    return dict(schema=1, purpose='tilo-native-source-compatibility-verified', approval_sha256=digest,
        approval=approval, native_source_revision=SOURCE, assembly_source_revision=revision,
        native_epoch=int(git(repo, 'show', '-s', '--format=%ct', SOURCE)),
        assembly_epoch=int(git(repo, 'show', '-s', '--format=%ct', revision)),
        changes=changes, remaining_tracked_tree_identical=True, complete_native_inventory_required=True,
        installable=False)


def original_native_inputs(assembly_inputs, admission):
    """Reproduce original inputs after proving all other tracked source unchanged."""
    require(assembly_inputs['revision'] == admission['assembly_source_revision']
            and assembly_inputs['epoch'] == admission['assembly_epoch'], 'assembly input identity differs')
    return dict(assembly_inputs, revision=SOURCE, epoch=admission['native_epoch'])
