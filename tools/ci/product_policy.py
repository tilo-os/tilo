#!/usr/bin/env python3
"""Derive exact routine PR check/lint policy from reviewed base inputs; GET-only.

Requires the separately audited integration_readiness and product_report
modules. No candidate checkout, permissions, statuses, merge or branch writes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

import integration_readiness as audit
import product_report as reports

# Reviewed feedback contract at product main c83e9b15c. Routine PRs check
# the affected closure on x86 and test owners/contracts; manual/release/nightly
# retain full dual-native qualification. Contract changes require fresh review.
CONTRACT = {
    '.forgejo/workflows/check.yml': 'd4b4cc2a9e8dacbf8612cc5dbd362648c2804998e66c0f3dd5374c78ee0b3be2',
    '.forgejo/workflows/lint.yml': '240042108e37d5d26a97cf805a5eb327fd04d430deec4949b9ab6c92070ad072',
    '.forgejo/workflows/nightly.yml': '7a3171ae272dc5e3af421796d9b8822b40e285e945b03f6834da087a5102b81e',
    'tools/ci/affected_packages.py': '8687a8a2181f782c69c9891777c687525c9b20e9c05019673ebcb587cccd987e',
}


def require(value, message):
    if not value:
        raise ValueError(message)


def routine(path, roots):
    if path.startswith('docs/designs/') and path.endswith('.md'):
        return True
    return (path.endswith('.rs') and Path(path).name != 'build.rs'
            and any(path.startswith(root + '/') for root in roots))


def changed_paths(root, base, head):
    raw = audit.git(root, 'diff', '--raw', '-z', '--no-renames', base, head).split(b'\0')
    changes = []
    for i in range(0, len(raw) - 1, 2):
        fields = raw[i].split()
        require(len(fields) == 5 and fields[0].startswith(b':'), 'unexpected raw Git diff')
        before, after, status = fields[0][1:], fields[1], fields[4]
        require(status in (b'A', b'M') and after in (b'100644', b'100755')
                and (before == b'000000' or before == after),
                'routine policy rejects deletion, rename, mode/type or symlink changes')
        path = raw[i + 1].decode('utf-8')
        require('\n' not in path and '\r' not in path and '\\' not in path
                and all(x not in ('', '.', '..') for x in path.split('/')), 'unsupported path')
        changes.append(path)
    require(changes, 'empty candidate diff')
    return changes


def protection_closure(paths, roots, build_scripts):
    """Protect every tracked non-routine input, compacted into whole subtrees."""
    tree = {}
    for path in paths:
        node = tree
        parts = path.split('/')
        for part in parts[:-1]:
            node = node.setdefault(part, {})
        node[parts[-1]] = not routine(path, roots) or path in build_scripts
    result = []
    def walk(node, prefix):
        if isinstance(node, bool):
            if node:
                result.append(prefix)
            return node
        old = len(result)
        protected = [walk(value, prefix + '/' + name if prefix else name) for name, value in sorted(node.items())]
        if prefix and all(protected):
            del result[old:]
            result.append(prefix)
        return all(protected)
    walk(tree, '')
    return result


def selector_plan(root, base, head, metadata):
    # Metadata is obtained from the reviewed base, not a PR artifact or CLI input.
    with tempfile.TemporaryDirectory(prefix='qos-trusted-metadata-') as directory:
        path = Path(directory) / 'metadata.json'
        path.write_text(json.dumps(metadata))
        result = subprocess.run([sys.executable, '-I', str(root / 'tools/ci/affected_packages.py'),
                                 '--base', base, '--head', head, '--shards', '2', '--metadata', str(path)],
                                cwd=root, check=True, capture_output=True, text=True, timeout=120)
    return json.loads(result.stdout)


def matrix_names(plan):
    rows = plan['matrix']['include']
    if plan['rust_required'] is False:
        require(plan['selected'] == [] and plan['selected_count'] == 0 and rows == [], 'inconsistent zero-Rust plan')
        return ['affected (runner)']
    require(plan['rust_required'] is True and rows and plan['selected_count'] == len(plan['selected']), 'invalid native plan')
    require(plan.get('qualification') == 'developer-feedback' and len(rows) == 1, 'unexpected feedback qualification')
    require(isinstance(plan.get('tested'), list) and set(plan['tested']) <= set(plan['selected'])
            and len(plan['tested']) == len(set(plan['tested'])), 'invalid test selection')
    names, assignments = [], {}
    for row in rows:
        require(set(row) == {'arch', 'estimated_cost', 'package_count', 'packages', 'runner', 'shard', 'test_packages'}, 'unknown matrix shape')
        require(row['arch'] == 'x86_64' and row['runner'] == 'runner', 'unexpected feedback runner')
        require(type(row['shard']) is int and row['shard'] == 1, 'unexpected shard')
        words = row['packages'].split()
        require(len(words) % 2 == 0 and words[::2] == ['-p'] * (len(words) // 2), 'invalid package arguments')
        packages = words[1::2]
        require(len(packages) == row['package_count'] and all(re.fullmatch('[A-Za-z0-9_-]+', p) for p in packages), 'invalid shard packages')
        tests = row['test_packages'].split()
        require(len(tests) % 2 == 0 and tests[::2] == ['-p'] * (len(tests) // 2)
                and sorted(tests[1::2]) == sorted(plan['tested']), 'matrix test scope differs from trusted selection')
        key = (row['arch'], row['shard'])
        require(key not in assignments, 'duplicate native shard')
        assignments[key] = packages
        # Installed Forgejo's implicit matrix display sorts keys and caps names
        # at 255 UTF-8 bytes. Cargo identifiers/these fields are ASCII. Fail on
        # collisions; never recover names from observed candidate job records.
        name = 'affected (' + ', '.join(str(row[k]) for k in sorted(row)) + ')'
        require(name.isascii(), 'unsupported matrix display encoding')
        names.append(name if len(name) <= 255 else name[:252] + '…')
    for arch in ('x86_64',):
        packages = [p for (a, _), ps in assignments.items() if a == arch for p in ps]
        require(sorted(packages) == sorted(plan['selected']), 'native matrix omits or duplicates selected packages')
    require(len(names) == len(set(names)), 'truncated matrix names collide')
    return names


def newest_pr_runs(runs, head):
    result = {}
    for workflow in ('check.yml', 'lint.yml'):
        ids = [r['id'] for r in runs if r.get('workflow_id') == workflow
               and r.get('event') == 'pull_request' and r.get('commit_sha') == head]
        require(ids and all(type(x) is int and x > 0 for x in ids), 'missing exact-head PR workflow: ' + workflow)
        result[workflow] = max(ids)
    return result


def derive(root, base, head, reviewers, roots, runs):
    root = Path(root).resolve()
    require(audit.SHA.fullmatch(base) and audit.SHA.fullmatch(head), 'full base/head required')
    require(reviewers and all(re.fullmatch('[A-Za-z0-9_.-]+', r) for r in reviewers), 'trusted reviewer list required')
    require(all(re.fullmatch(r'apps/[A-Za-z0-9_-]+/src', p) for p in roots), 'routine roots must be explicit app src directories')
    try:
        audit.git(root, 'merge-base', '--is-ancestor', base, head)
    except subprocess.CalledProcessError:
        raise ValueError('candidate does not contain reviewed main, or required Git objects are unavailable') from None
    require(audit.git(root, 'rev-parse', 'HEAD').decode().strip() == base, 'checkout must equal reviewed current base')
    require(not audit.git(root, 'status', '--porcelain', '--untracked-files=all').strip(), 'trusted checkout must be clean')
    require(audit.git(root, 'rev-parse', '--is-shallow-repository').strip() == b'false', 'complete Git history required')
    for path, expected in CONTRACT.items():
        raw = audit.git(root, 'show', base + ':' + path)
        require(hashlib.sha256(raw).hexdigest() == expected, 'unreviewed workflow/selector contract: ' + path)
        require((root / path).read_bytes() == raw, 'checkout differs from committed trusted bytes: ' + path)
    changes = changed_paths(root, base, head)
    require(all(routine(p, roots) for p in changes), 'candidate changes harness, policy or unknown routine input')
    metadata = json.loads(subprocess.run(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'],
                          cwd=root, check=True, capture_output=True, text=True, timeout=120).stdout)
    package_roots = {Path(p['manifest_path']).resolve().parent for p in metadata['packages']}
    require(all((root / p).parent.resolve() in package_roots for p in roots), 'routine root is not a reviewed Cargo package')
    build_packages = {Path(p['manifest_path']).resolve().parent for p in metadata['packages']
                      if any('custom-build' in t.get('kind', []) for t in p.get('targets', []))}
    require(not {(root / p).parent.resolve() for p in roots} & build_packages,
            'routine app with custom build script requires separate trusted policy review')
    build_scripts = {Path(t['src_path']).resolve().relative_to(root).as_posix()
                     for p in metadata['packages'] for t in p.get('targets', []) if 'custom-build' in t.get('kind', [])}
    require(not set(changes) & build_scripts, 'candidate changes executable build policy')
    tracked = audit.git(root, 'ls-tree', '-r', '--name-only', '-z', base).decode().strip('\0').split('\0')
    closure = protection_closure(tracked, roots, build_scripts)
    plan = selector_plan(root, base, head, metadata)
    require(sorted(plan['changed_files']) == sorted(changes), 'selector diff does not match exact candidate')
    native = matrix_names(plan)
    ids = newest_pr_runs(runs, head)
    skips = ['report-check-result'] + (['affected (runner)'] if plan['rust_required'] is False else [])
    policy = {'trusted_revision': base, 'protected_paths': closure, 'reviewers': reviewers,
              'runs': [{'workflow': 'check.yml', 'run_id': ids['check.yml'], 'events': ['pull_request'],
                        'jobs': ['builder', 'select', 'security', 'checks', 'report-check-result'] + native, 'allowed_skips': skips},
                       {'workflow': 'lint.yml', 'run_id': ids['lint.yml'], 'events': ['pull_request'], 'jobs': ['quality']}]}
    if plan['rust_required'] is False:
        policy['zero_rust_selection'] = {'base': base, 'head': head, 'trusted_revision': base,
                                        'selector_sha256': CONTRACT['tools/ci/affected_packages.py'], 'plan': plan}
    errors = audit.policy_errors(policy) + audit.provenance_errors(root, base, head, policy)
    require(not errors, '; '.join(errors))
    return policy, plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pr', type=int, required=True)
    parser.add_argument('--reviewed-base', required=True)
    parser.add_argument('--git-root', type=Path, required=True)
    parser.add_argument('--reviewer', action='append', required=True)
    parser.add_argument('--routine-root', action='append', default=[])
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    require(args.pr > 0 and not args.out.exists(), 'positive PR and new output path required')
    read = audit.api
    report = {'repo': reports.REPO, 'pr': args.pr, 'reviewed_base': args.reviewed_base,
              'merge_authorized': False, 'audit': {'observed_ready': False, 'errors': []}}
    try:
        base = read(reports.HOST, reports.REPO, '/branches/main')['commit']['id']
        report['base'] = base
        require(base == args.reviewed_base, 'current main differs from reviewed base')
        pr = read(reports.HOST, reports.REPO, f'/pulls/{args.pr}')
        head = pr['head']['sha']
        report['head'] = head
        runs = reports.pages(read, '/actions/runs?head_sha=' + head, 'workflow_runs')
        require(all(r.get('commit_sha') == head for r in runs), 'exact-head run inventory mismatch')
        policy, plan = derive(args.git_root, base, head, args.reviewer, args.routine_root, runs)
        result = reports.audit_candidate(args.pr, policy, args.git_root, read, runs)
        # Recheck run inventory too: a new independent run invalidates old green.
        latest = reports.pages(read, '/actions/runs?head_sha=' + head, 'workflow_runs')
        require(newest_pr_runs(latest, head) == newest_pr_runs(runs, head), 'new run created during audit; recollect')
        require(read(reports.HOST, reports.REPO, '/branches/main')['commit']['id'] == base, 'main advanced during audit')
        report.update(plan=plan, policy=policy, audit=result)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        report['audit'] = {'observed_ready': False, 'errors': [str(error) if isinstance(error, ValueError)
                                                           else 'incomplete observation: ' + type(error).__name__]}
    with args.out.open('x', encoding='utf-8') as stream:
        json.dump(report, stream, indent=2)
        stream.write('\n')
    print(json.dumps({'observed_ready': report['audit']['observed_ready'], 'merge_authorized': False}))
    return 0 if report['audit']['observed_ready'] else 2


if __name__ == '__main__':
    raise SystemExit(main())
