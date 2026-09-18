#!/usr/bin/env python3
"""Verify retained coordinator state against both server records and job logs."""
import json
from pathlib import Path

ROOT = Path(__file__).parent / 'results/packaging-coordinator-20260919'


def read(name):
    return json.loads((ROOT / name).read_text())


def main():
    setup = read('setup.json')
    pending = read('pending-C-active-A.json')
    final = read('final-state.json')
    attempts = read('attempts.json')
    assert len(attempts) == 2
    assert [a['desired']['source_sha'] for a in attempts] == [setup['source_A'], setup['source_C']]
    assert pending['active']['desired']['source_sha'] == setup['source_A']
    assert pending['desired']['source_sha'] == setup['source_C'] and pending['baseline'] is None
    assert final['active'] is None and final['desired'] == final['baseline'] == attempts[1]['desired']
    assert attempts[0]['baseline'] is None
    assert attempts[1]['baseline'] == attempts[0]['desired']
    for generation, attempt in zip(('A', 'C'), attempts):
        run = read(f'run-{generation}.json')
        assert run['id'] == attempt['run_id'] and run['status'] == attempt['status'] == 'success'
        assert run['commit_sha'] == setup['source_' + generation]
        assert run['trigger_event'] == 'workflow_dispatch' and run['workflow_id'] == 'packaging-coordinator-probe.yml'
        inputs = run['dispatch_payload']['inputs']
        baseline = 'none' if generation == 'A' else setup['source_A']
        assert inputs == {'coordinator_intent': attempt['id'], 'baseline_sha': baseline}
        assert run['dispatch_payload']['ref'] == setup['source_' + generation]
        lines = (ROOT / f'job-{generation}-receipt.log').read_text().splitlines()
        assert len(lines) == 1
        receipt = json.loads(lines[0].split('COORDINATOR_RECEIPT=', 1)[1])
        assert receipt == {'SOURCE_SHA': setup['source_' + generation], 'INTENT': attempt['id'], 'BASELINE_SHA': baseline}
    print('PASS: A then latest C, successful A baseline, two exact immutable dispatch/job receipts; no B dispatch')


if __name__ == '__main__':
    main()
