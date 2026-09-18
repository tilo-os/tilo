"""Explicit frozen-native reuse after reviewed provider/rootfs-only corrections."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

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


CACHE_BASE = '344257f56275919abc63181d8954e838f9b5303e'
CACHE_TRUST = 'e5241775387c6afb39d2e6bf8350a59a8aa478b3'
CACHE_API = 'f6881005fc62dd54cba7dbe7c241149151adfd74'
CACHE_VIEWER = 'a89fe484328ff4206c35f842c07c3824b9df18ef'
CACHE_SOURCE = '317eb82f7178b14623350f933088093cd3cdfd23'


def verify(repo, revision, path, digest, native_receipt_sha256, reconstruction_sha256):
    approval = read_approval(path, digest)
    if approval.get('purpose') != 'tilo-reviewed-cache-source-compatibility':
        return _verify_base(repo, revision, path, digest, native_receipt_sha256, reconstruction_sha256)
    require(set(approval) == {'schema', 'purpose', 'assembly_source_revision',
        'previous_approval_json', 'previous_approval_sha256', 'wpe_bridge'}, 'cache admission schema differs')
    require(type(approval['schema']) is int and approval['schema'] == 2
        and approval['assembly_source_revision'] == revision == CACHE_SOURCE,
        'cache admission requires exact reviewed source')
    require(git(repo, 'show', '-s', '--format=%P', CACHE_SOURCE).decode().strip() == CACHE_VIEWER
        and git(repo, 'show', '-s', '--format=%P', CACHE_VIEWER).decode().strip() == CACHE_API
        and git(repo, 'show', '-s', '--format=%P', CACHE_API).decode().strip() == CACHE_TRUST
        and git(repo, 'show', '-s', '--format=%P', CACHE_TRUST).decode().strip() == CACHE_BASE,
        'reviewed cache correction ancestry differs')
    for parent, child, name in ((CACHE_BASE, CACHE_TRUST, 'os/Containerfile'),
                                 (CACHE_TRUST, CACHE_API, 'os/dev/build-image.sh'),
                                 (CACHE_API, CACHE_VIEWER, 'os/Containerfile'),
                                 (CACHE_VIEWER, CACHE_SOURCE, 'os/Containerfile')):
        require(git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', parent, child)
                == name.encode() + b'\0', 'cache correction path differs')
    # Exact commit pins bind the reviewed relocation, guarded API, SSH dependency and build-only trust parser bytes;
    # they are not an extensible allowlist of build-script changes.
    previous = approval['previous_approval_json'].encode()
    require(sha(previous) == approval['previous_approval_sha256'], 'previous admission digest differs')
    with tempfile.TemporaryDirectory(prefix='tilo-cache-admission-') as temporary:
        prior = Path(temporary) / 'previous.json'
        prior.write_bytes(previous)
        result = _verify_base(repo, CACHE_BASE, prior, approval['previous_approval_sha256'],
                              native_receipt_sha256, reconstruction_sha256, checkout_revision=revision)
    changes = dict(result['changes'])
    for name in ('os/Containerfile', 'os/dev/build-image.sh'):
        blobs = [git(repo, 'rev-parse', commit + ':' + name).decode().strip() for commit in (SOURCE, revision)]
        changes[name] = dict(old_blob=blobs[0], new_blob=blobs[1],
            old_sha256=sha(git(repo, 'show', SOURCE + ':' + name)),
            new_sha256=sha(git(repo, 'show', revision + ':' + name)))
    verify_cache_recipe(git(repo, 'show', SOURCE + ':os/Containerfile'), git(repo, 'show', revision + ':os/Containerfile'))
    verify_policy_placement(git(repo, 'show', revision + ':os/Containerfile'))
    verify_wpe_bridge(approval['wpe_bridge'], changes['os/Containerfile'], sha(git(repo, 'show', SOURCE + ':os/dev/build-wpe.sh')))
    result.update(approval=approval, approval_sha256=digest, assembly_source_revision=revision,
        assembly_epoch=int(git(repo, 'show', '-s', '--format=%ct', revision)), changes=changes,
        layer_epoch_anchor=CACHE_BASE, layer_epoch=int(git(repo, 'show', '-s', '--format=%ct', CACHE_BASE)))
    return result


TRUST_TOOL_STAGE = (b'# Public trust parsing is a build-only operation. The pinned base supplies the\n'
                    b'# OpenSSL CLI while its ABI libraries remain part of the runtime package policy.\n'
                    b'FROM ${BASE_REF} AS public-trust-tools\n\n')


def verify_cache_recipe(original, actual):
    # The exact source pin permits only this inert alias to an already pinned
    # base. No native compilation instruction, dependency or payload changes.
    require(actual.count(TRUST_TOOL_STAGE) == 1, 'exact build-only trust stage required')
    require(actual.index(TRUST_TOOL_STAGE) < actual.index(b'FROM ${BASE_REF} AS rootfs\n'),
            'trust tools stage must precede rootfs')
    verify_rootfs(original, actual.replace(TRUST_TOOL_STAGE, b'', 1))


def _verify_base(repo, revision, path, digest, native_receipt_sha256, reconstruction_sha256, checkout_revision=None):
    """Compare complete committed trees before any source compatibility adoption."""
    approval = read_approval(path, digest)
    rootfs = approval.get('purpose') == 'tilo-rootfs-native-source-compatibility'
    policy = rootfs and 'rootfs_revision' in approval
    followup = policy and 'policy_revision' in approval
    extra = {'provider_revision', 'wpe_bridge'} if rootfs else set()
    if policy:
        extra.add('rootfs_revision')
    if followup:
        extra.add('policy_revision')
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
    require(git(repo, 'rev-parse', 'HEAD').decode().strip() == (checkout_revision or revision)
        and not git(repo, 'status', '--porcelain', '--untracked-files=all').strip(),
        'compatible assembly source must be clean and exact')
    # Authenticated checkout includes the explicit two/three/four/five-commit chain.
    # No hidden ancestors, rename heuristics, or unrecorded changes are permitted.
    recipe_revision = approval['rootfs_revision'] if policy else revision
    policy_revision = approval['policy_revision'] if followup else revision
    if followup:
        require(re.fullmatch('[0-9a-f]{40}', policy_revision or '')
            and git(repo, 'show', '-s', '--format=%P', revision).decode().strip() == policy_revision,
            'rootfs follow-up ancestry differs')
        require(git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', policy_revision, revision) == b'os/Containerfile\0',
            'rootfs follow-up must change only Containerfile')
        verify_rootfs(git(repo, 'show', policy_revision + ':os/Containerfile'),
            git(repo, 'show', revision + ':os/Containerfile'))
    if policy:
        require(re.fullmatch('[0-9a-f]{40}', recipe_revision or '')
            and git(repo, 'show', '-s', '--format=%P', policy_revision).decode().strip() == recipe_revision, 'policy correction ancestry differs')
        require(git(repo, 'diff-tree', '--no-commit-id', '--name-only', '--no-renames', '-r', '-z', recipe_revision, policy_revision) .split(b'\0')[:-1] == [p.encode() for p in POLICY_PATHS], 'policy correction must change only reviewed rootfs policy files')
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
