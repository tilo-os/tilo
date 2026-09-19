#!/usr/bin/env python3
"""Fail closed on disabled or publishing coordinator qualification requests."""
import os
import re
import subprocess
import uuid

import app_packaging_product as product
from resolve_ci_builder import resolve


def validate(env, ancestor=None):
    intent = env.get('COORDINATOR_INTENT', '')
    extra = [env.get(name, '') for name in ('REQUESTED_BUILDER', 'BASELINE_SHA', 'BASELINE_RECEIPT_SHA256',
                                          'BASELINE_RUN_ID', 'COORDINATOR_POLICY_SHA256', 'BASELINE_EVIDENCE')]
    if not intent:
        if any(extra):
            raise ValueError('coordinator inputs require a qualification intent')
        return False
    if env.get('COORDINATOR_ENABLED') != 'true':
        raise ValueError('coordinator qualification is disabled')
    if str(uuid.UUID(intent)) != intent:
        raise ValueError('invalid coordinator intent')
    source = env.get('GITHUB_SHA', '')
    if (env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch' or
            not re.fullmatch('[0-9a-f]{40}', source) or env.get('GITHUB_REF') != source or
            env.get('REQUESTED_PUBLISH') != 'false' or env.get('REQUESTED_APPS')):
        raise ValueError('qualification requires immutable SHA dispatch, publish=false and collection scope')
    if not env.get('REQUESTED_BUILDER'):
        raise ValueError('qualification requires a pinned builder')
    resolve(env['REQUESTED_BUILDER'])
    baseline, evidence, run_id, policy_sha, raw_proof = extra[1:]
    if not re.fullmatch('[0-9a-f]{64}', policy_sha):
        raise ValueError('qualification requires trusted coordinator policy identity')
    if baseline or evidence or run_id or raw_proof:
        if (not re.fullmatch('[0-9a-f]{40}', baseline) or not re.fullmatch('[0-9a-f]{64}', evidence)
                or not re.fullmatch('[1-9][0-9]*', run_id)):
            raise ValueError('baseline requires source and verified-receipt identities')
        if env.get('REQUESTED_COLLECTION') != 'default':
            raise ValueError('incremental qualification supports only the default collection')
        if not raw_proof or len(raw_proof) > 16384:
            raise ValueError('bounded verified baseline evidence required')
        proof = product.json.loads(raw_proof)
        if product.core.canonical(proof) != raw_proof or product.core.receipt_key(proof) != evidence:
            raise ValueError('baseline verification proof hash mismatch')
        expected = {'source_sha': baseline, 'builder_image': env['REQUESTED_BUILDER'],
                    'collection': env['REQUESTED_COLLECTION'],
                    'architectures': env.get('REQUESTED_ARCHITECTURES', '').split(','),
                    'policy_sha256': policy_sha}
        product.validate_evidence(proof, product.validate_desired(expected), int(run_id))
        if ancestor is None:
            subprocess.run(['git', 'merge-base', '--is-ancestor', baseline, source], check=True)
        elif not ancestor(baseline, source):
            raise ValueError('baseline is not an ancestor of source')
    return True


if __name__ == '__main__':
    validate(os.environ)
