"""Explicit frozen-native reuse after reviewed provider/rootfs-only corrections."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
POLICY_PATHS = ('docs/security/boot-chain.md', 'os/scripts/verify-runtime-crypto-packages.sh',
                'os/usr/libexec/tilo-security-audit')
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
    rootfs = approval.get('purpose') == 'tilo-rootfs-native-source-compatibility'
    policy = rootfs and 'rootfs_revision' in approval
    extra = {'provider_revision', 'wpe_bridge'} if rootfs else set()
    if policy:
        extra.add('rootfs_revision')
    require(set(approval) == {'schema', 'purpose', 'native_source_revision', 'assembly_source_revision',
        'native_receipt_sha256', 'approved_reconstruction_sha256', 'changes'} | extra, 'source admission schema differs')
    require(type(approval['schema']) is int and approval['schema'] == 1
        and approval['purpose'] in ('tilo-windows-provider-native-source-compatibility', 'tilo-rootfs-native-source-compatibility')
        and approval['native_source_revision'] == SOURCE
        and approval['assembly_source_revision'] == revision and revision != SOURCE
        and re.fullmatch('[0-9a-f]{40}', revision)
        and approval['native_receipt_sha256'] == native_receipt_sha256
        and approval['approved_reconstruction_sha256'] == reconstruction_sha256,
        'source admission identity differs')
    require(git(repo, 'rev-parse', 'HEAD').decode().strip() == revision
        and not git(repo, 'status', '--porcelain', '--untracked-files=all').strip(),
        'compatible assembly source must be clean and exact')
    # Authenticated checkout includes the explicit two/three/four-commit chain.
    # No hidden ancestors, rename heuristics, or unrecorded changes are permitted.
    recipe_revision = approval['rootfs_revision'] if policy else revision
    if policy:
        require(re.fullmatch('[0-9a-f]{40}', recipe_revision or '')
            and git(repo, 'show', '-s', '--format=%P', revision).decode().strip() == recipe_revision, 'policy correction ancestry differs')
        require(git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', recipe_revision, revision) .split(b'\0')[:-1] == [p.encode() for p in POLICY_PATHS], 'policy correction must change only reviewed rootfs policy files')
    parent = approval['provider_revision'] if rootfs else SOURCE
    if rootfs:
        require(re.fullmatch('[0-9a-f]{40}', parent or '') and
            git(repo, 'show', '-s', '--format=%P', parent).decode().strip() == SOURCE, 'provider correction ancestry differs')
        require(git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', parent, recipe_revision) == b'os/Containerfile\0', 'rootfs correction must change only Containerfile')
    require(git(repo, 'show', '-s', '--format=%P', recipe_revision).decode().strip() == parent,
        'assembly correction must directly descend from frozen native source')
    changed = git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', SOURCE, revision)
    paths = tuple(sorted((*PATHS, 'os/Containerfile', *(POLICY_PATHS if policy else ())))) if rootfs else PATHS
    require(changed.split(b'\0')[:-1] == [name.encode() for name in paths]
            and set(approval['changes']) == set(paths), 'complete tree diff exceeds reviewed Python correction')
    changes = {}
    for name in paths:
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
    if rootfs:
        recipe = git(repo, 'show', revision + ':os/Containerfile')
        verify_rootfs(git(repo, 'show', SOURCE + ':os/Containerfile'), recipe)
        if policy:
            verify_policy_placement(recipe)
        verify_wpe_bridge(approval['wpe_bridge'], changes['os/Containerfile'],
            sha(git(repo, 'show', SOURCE + ':os/dev/build-wpe.sh')))
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
    result = dict(assembly_inputs, revision=SOURCE, epoch=admission['native_epoch'])
    if 'wpe_bridge' in admission['approval']:
        bridge = admission['approval']['wpe_bridge']
        recipe = admission['changes']['os/Containerfile']
        require(result['containerfile_sha256'] == recipe['new_sha256']
            and result['wpe_receipt_sha256'] == bridge['assembly_receipt_sha256']
            and result['wpe_reference'] == json.loads(bridge['original_receipt_json'])['reference'], 'assembly recipe/WPE input differs')
        result.update(containerfile_sha256=recipe['old_sha256'], wpe_receipt_sha256=bridge['original_receipt_sha256'])
    return result


def verify_rootfs(old, new):
    """Only the body of the existing rootfs stage may differ."""
    start = b'FROM ${BASE_REF} AS rootfs\n'
    end = b'FROM rootfs AS split\n'
    for raw in (old, new):
        require(raw.count(start) == 1 and raw.count(end) == 1, 'unique rootfs boundaries required')
    a, b = old.index(start) + len(start), old.index(end)
    c, d = new.index(start) + len(start), new.index(end)
    require(a < b and c < d and old[:a] == new[:c] and old[b:] == new[d:], 'non-rootfs recipe bytes changed')
    directives = lambda raw: re.findall(rb'^\s*FROM[ \t]+[^\r\n]+', raw, re.M | re.I)
    require(directives(old) == directives(new), 'recipe stage directives changed')


def verify_wpe_bridge(bridge, recipe, build_wpe_sha):
    require(set(bridge) == {'original_receipt_json', 'original_receipt_sha256', 'assembly_receipt_json', 'assembly_receipt_sha256'}, 'WPE bridge schema differs')
    receipts = []
    for prefix in ('original', 'assembly'):
        raw = bridge[prefix + '_receipt_json'].encode('utf-8')
        require(sha(raw) == bridge[prefix + '_receipt_sha256'], 'WPE bridge receipt bytes differ')
        receipts.append(json.loads(raw))
    old, new = receipts
    require(old['audit']['compatible_recipes'] == {'os/Containerfile': recipe['old_sha256'], 'os/dev/build-wpe.sh': build_wpe_sha}, 'original WPE recipe identity differs')
    expected = json.loads(json.dumps(old))
    expected['audit']['compatible_recipes']['os/Containerfile'] = recipe['new_sha256']
    expected['audit_sha256'] = new['audit_sha256']
    require(re.fullmatch('[0-9a-f]{64}', new['audit_sha256']) and new == expected, 'WPE bridge changes more than consumer recipe/audit pin')
    # The new audit is encoded canonically for an independently reproducible pin.
    require(new['audit_sha256'] == sha((json.dumps(new['audit'], sort_keys=True, separators=(',', ':')) + '\n').encode()), 'new WPE audit hash differs')


def verify_selected_wpe(admission, raw, digest):
    bridge = admission['approval'].get('wpe_bridge')
    if bridge:
        require(digest == bridge['assembly_receipt_sha256'] and sha(raw) == digest
            and raw.decode('utf-8') == bridge['assembly_receipt_json'], 'selected WPE receipt differs from source admission')


def verify_policy_placement(recipe):
    """The changed policy is a rootfs-only input, never a native build input."""
    begin = recipe.index(b'FROM ${BASE_REF} AS rootfs\n')
    end = recipe.index(b'FROM rootfs AS split\n')
    for path in POLICY_PATHS:
        name = path.encode()
        require(name not in recipe[:begin] and name not in recipe[end:]
            and name in recipe[begin:end], 'policy inputs must be consumed only in rootfs')
    require(b'RUN bash /usr/local/bin/verify-runtime-crypto-packages.sh' in recipe[begin:end],
        'runtime crypto gate invocation required')
