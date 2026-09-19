"""Validate retained private-pilot landing observations, never authorize a merge."""
import json
from pathlib import Path

ROOT = Path(__file__).parent / 'results/trusted-landing-20260919'


def main():
    def read(name):
        return json.loads((ROOT / name).read_text(encoding='utf-8'))
    candidate, policy = read('candidate.json'), read('policy.json')
    bootstrap, pending, dry, landed = (read(name) for name in (
        'bootstrap-merge.json', 'pending-dry-run.json', 'ready-dry-run.json', 'candidate-merge.json'))
    assert candidate['repo'] == 'qos/ci-workflow-pilot-20260918-9245-repeat'
    assert bootstrap['state'] == 'merged_verified'
    assert bootstrap['readback']['merged_by'] == 'qos-integrator'
    assert candidate['base'] == bootstrap['head'] == policy['trusted_revision']
    assert policy['protected_paths'] == ['.forgejo']
    assert pending['state'] == 'rejected'
    assert dry['state'] == 'dry_run_ready' and dry['execute'] is False
    assert landed['state'] == 'merged_verified' and landed['execute'] is True
    assert landed['readback']['merged_by'] == 'qos-integrator'
    assert landed['head'] == landed['readback']['head'] == landed['readback']['merge_commit_sha'] == candidate['head']
    assert landed['readback']['main'] == candidate['head']
    evidence = landed['evidence']
    assert evidence['author'] != 'qos-integrator'
    assert any(r['state'] == 'APPROVED' and r['user'] == 'claude' and r['user'] != evidence['author']
               and r['commit_id'] == candidate['head'] and r['official'] and not r['dismissed']
               for r in evidence['reviews'])
    assert len(evidence['runs']) == 1
    run = evidence['runs'][0]
    assert run['run']['status'] == 'success'
    assert run['run']['commit_sha'] == candidate['head']
    assert run['run']['id'] == policy['runs'][0]['run_id']
    assert len(run['jobs']) == 1 and run['jobs'][0]['name'] == 'fixture-tests'
    assert run['jobs'][0]['status'] == 'success' and run['jobs'][0]['task_id'] > 0
    print('PASS: fresh pilot dry run and exact-head landing with independent approval and actual execution')


if __name__ == '__main__': main()
