#!/usr/bin/env python3
"""GET-only product integration inventory and optional trusted-policy audits.

Never grants permissions, posts statuses, updates branches or merges. A report is
a snapshot, not authorization. Expected jobs come only from operator policies.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path

import integration_readiness as audit

HOST, REPO = 'code.podesta.ai', 'qos/qos'


def pages(read, path, field=None):
    result, seen = [], set()
    for page in range(1, 101):
        response = read(HOST, REPO, path + ('&' if '?' in path else '?') + f'limit=50&page={page}')
        rows = response[field] if field else response
        if not isinstance(rows, list):
            raise ValueError('unexpected list response')
        for row in rows:
            key = row['id']
            if key in seen:
                raise ValueError('pagination repeated a record; completeness unavailable')
            seen.add(key)
            result.append(row)
        if field and isinstance(response.get('total_count'), int):
            if len(result) == response['total_count']:
                return result
            if len(result) > response['total_count'] or not rows:
                raise ValueError('run count inconsistent')
        elif len(rows) < 50:
            return result
    raise ValueError('pagination bound exceeded')


def audit_candidate(number, policy, root, read, available_runs):
    errors = audit.policy_errors(policy)
    if {r.get('workflow') for r in policy.get('runs', [])} != {'check.yml', 'lint.yml'}:
        errors.append('operator policy must explicitly cover check.yml and lint.yml')
    for required in policy.get('runs', []):
        choices = [r['id'] for r in available_runs if r.get('workflow_id') == required.get('workflow')
                   and r.get('event') == 'pull_request']
        if (required.get('events') != ['pull_request'] or not choices
                or required.get('run_id') != max(choices)):
            errors.append('policy must select newest observed exact-head PR run for each workflow')
    if errors:
        return {'observed_ready': False, 'errors': sorted(set(errors))}
    first = audit.collect(HOST, REPO, number, 'main', policy, read=read)
    errors += audit.observation_errors(first, REPO, number, 'main', policy)
    head, base = first['pr']['head']['sha'], first['base']['commit']['id']
    errors += audit.provenance_errors(root, base, head, policy)
    second = audit.collect(HOST, REPO, number, 'main', policy, read=read)
    errors += audit.observation_errors(second, REPO, number, 'main', policy)
    if audit.fingerprint(first) != audit.fingerprint(second):
        errors.append('candidate evidence changed during audit')
    return {'observed_ready': not errors, 'errors': sorted(set(errors)),
            'policy': policy, 'evidence': audit.fingerprint(second)}


def collect(read=audit.api, policies=None, root=None):
    policies = policies or {}
    repository = read(HOST, REPO, '')
    if repository.get('id') != 23 or repository.get('full_name') != REPO:
        raise ValueError('product repository identity mismatch')
    base = read(HOST, REPO, '/branches/main')['commit']['id']
    protections = read(HOST, REPO, '/branch_protections')
    grants = {}
    for name in ('qos-integrator', 'claude'):
        value = read(HOST, REPO, f'/collaborators/{name}/permission')
        user = value.get('user') or {}
        grants[name] = {'permission': value.get('permission'),
                        'id': user.get('id'), 'login': user.get('login'),
                        'is_admin': user.get('is_admin'), 'active': user.get('active')}
    report = {'schema': 1, 'collected_at': datetime.now(timezone.utc).isoformat(),
              'repo': REPO, 'main': base, 'merge_authorized': False,
              'repository': {k: repository.get(k) for k in ('id', 'private', 'default_branch',
                  'allow_fast_forward_only_merge', 'default_merge_style', 'has_actions')},
              'branch_protections': protections, 'grants': grants, 'pull_requests': []}
    for pr in pages(read, '/pulls?state=open'):
        head, number = pr['head']['sha'], pr['number']
        if not audit.SHA.fullmatch(head):
            raise ValueError('missing exact PR head')
        runs = pages(read, '/actions/runs?head_sha=' + head, 'workflow_runs')
        if any(r.get('commit_sha') != head for r in runs):
            raise ValueError('server did not honor exact-head filter')
        reviews = pages(read, f'/pulls/{number}/reviews')
        item = {'number': number, 'title': pr['title'], 'head': head,
                'base_branch': pr['base']['ref'], 'draft': pr.get('draft'),
                'author': pr['user']['login'], 'mergeable': pr.get('mergeable'),
                'runs': [{k: r.get(k) for k in ('id', 'workflow_id', 'event', 'status', 'commit_sha')}
                         for r in runs],
                'reviews': [{k: r.get(k) for k in ('id', 'state', 'commit_id', 'official', 'dismissed')}
                            | {'reviewer': (r.get('user') or {}).get('login')} for r in reviews]}
        policy = policies.get(str(number))
        if policy is None:
            item['audit'] = {'observed_ready': False, 'errors': [
                'no explicit operator policy: matrix completeness and trusted harness unqualified']}
        else:
            if root is None:
                raise ValueError('trusted Git checkout required with policies')
            item['audit'] = audit_candidate(number, policy, root, read, runs)
        fresh = read(HOST, REPO, f'/pulls/{number}')
        if fresh.get('head', {}).get('sha') != head or fresh.get('state') != 'open':
            item['audit']['observed_ready'] = False
            item['audit']['errors'].append('PR changed during report')
        report['pull_requests'].append(item)
    report['main_after'] = read(HOST, REPO, '/branches/main')['commit']['id']
    report['stable_main'] = report['main_after'] == base
    if not report['stable_main']:
        for item in report['pull_requests']:
            item['audit']['observed_ready'] = False
            item['audit']['errors'].append('main advanced during report; collect again')
    return report


def markdown(report):
    lines = ['# Product integration snapshot', '',
             f"Collected {report['collected_at']}; main `{report['main']}`.",
             'Read-only evidence. Merge authorization is always false.', '',
             '| PR | Head | check.yml runs | lint.yml runs | Audit |',
             '|---|---|---|---|---|']
    for pr in report['pull_requests']:
        def status(workflow):
            return ', '.join(f"{r['id']}:{r['status']}" for r in pr['runs'] if r['workflow_id'] == workflow) or 'missing'
        state = 'observed ready (not authorized)' if pr['audit']['observed_ready'] else 'blocked'
        lines.append(f"| {pr['number']} | {pr['head'][:12]} | {status('check.yml')} | {status('lint.yml')} | {state} |")
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True, help='new directory for report.json and report.md')
    parser.add_argument('--policies', type=Path, help='operator-owned JSON object keyed by PR number')
    parser.add_argument('--git-root', type=Path, help='clean reviewed current-base checkout; never candidate checkout')
    args = parser.parse_args()
    if args.out.exists():
        parser.error('output directory already exists')
    policies = json.loads(args.policies.read_text()) if args.policies else None
    result = collect(policies=policies, root=args.git_root)
    args.out.mkdir(parents=True)
    (args.out / 'report.json').write_text(json.dumps(result, indent=2) + '\n')
    (args.out / 'report.md').write_text(markdown(result))
    print(markdown(result))


if __name__ == '__main__':
    main()
