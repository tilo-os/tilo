#!/usr/bin/env python3
"""Small provenance envelope from the existing guarded workflow plan job."""
import json
import hashlib
import os
from pathlib import Path

import app_packaging_qualification as qualification

INPUT_ENV = {'apps': 'REQUESTED_APPS', 'collection': 'REQUESTED_COLLECTION',
             'architectures': 'REQUESTED_ARCHITECTURES', 'publish': 'REQUESTED_PUBLISH',
             'coordinator_intent': 'COORDINATOR_INTENT', 'builder_image': 'REQUESTED_BUILDER',
             'coordinator_policy_sha256': 'COORDINATOR_POLICY_SHA256',
             'baseline_sha': 'BASELINE_SHA', 'baseline_run_id': 'BASELINE_RUN_ID',
             'baseline_evidence': 'BASELINE_EVIDENCE', 'baseline_receipt_sha256': 'BASELINE_RECEIPT_SHA256'}


def create(env, plan, ancestor=None):
    if not qualification.validate(env, ancestor):
        raise ValueError('coordinator plan requires guarded qualification')
    run_id, attempt = int(env['GITHUB_RUN_ID']), int(env['GITHUB_RUN_ATTEMPT'])
    if run_id <= 0 or attempt <= 0 or env['RESOLVED_BUILDER'] != env['REQUESTED_BUILDER']:
        raise ValueError('plan run/attempt/resolved builder mismatch')
    if not isinstance(plan, dict) or type(plan.get('count')) is not int or plan['count'] < 0:
        raise ValueError('invalid plan count')
    for key in ('apps', 'packages', 'catalog'):
        if not isinstance(plan.get(key), list) or len(plan[key]) != plan['count']:
            raise ValueError('plan count disagrees with ' + key)
    return {'schema': 'tilo.coordinator-plan/v1', 'run_id': run_id, 'plan_attempt': attempt,
            'source_sha': env['GITHUB_SHA'], 'builder_image': env['RESOLVED_BUILDER'],
            'inputs': {name: env.get(variable, '') for name, variable in INPUT_ENV.items()}, 'plan': plan}


if __name__ == '__main__':
    result = create(os.environ, json.loads(Path('app-release-plan.json').read_text()))
    raw = (json.dumps(result, sort_keys=True) + '\n').encode()
    Path('coordinator-plan.json').write_bytes(raw)
    print('TILO_COORDINATOR_PLAN', os.environ['COORDINATOR_INTENT'],
          hashlib.sha256(raw).hexdigest())
