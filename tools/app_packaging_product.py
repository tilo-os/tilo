#!/usr/bin/env python3
"""Disabled-by-default, nonpublishing adapter for the shared packaging coordinator.

No scheduler, credentials or service is installed. Read-only unless explicitly
enabled both by a trusted local policy and --apply-qualification. Successful
runs remain active until an independent canonical-artifact verifier is attached.
"""
import argparse
import base64
import hashlib
import json
import io
from pathlib import Path
import re
import subprocess
import threading
import time
import zipfile

import app_packaging_coordinator as core
from resolve_ci_builder import resolve

REPO = 'qos/qos'
WORKFLOW = 'release-apps.yml'
REQUIRED_PATHS = {'.forgejo/workflows/' + WORKFLOW,
                  'tools/ci_python.py',
                  'tools/app_packaging_product.py', 'tools/app_packaging_coordinator.py',
                  'tools/app_packaging_qualification.py', 'tools/app_release.py',
                  'tools/app_packaging_plan.py', 'tools/ci_select.py',
                  'dist/apps/app-collections.toml',
                  'tools/resolve_ci_builder.py', 'dist/apps/package-apps.sh',
                  'os/scripts/build-thin-apps.sh', 'tools/ci_select.py',
                  'tools/app_package_isa.py', 'tools/create_app_receipt.py',
                  'tools/assemble_verified_apps.py', 'tools/final_app_receipt.py',
                  'tools/app_channel_manifest.py', 'tools/canonical_app_verifier.py',
                  'dist/apps/assemble-collection.sh', 'tools/assemble_app_collection.py',
                  'tools/rustc-thin-app-wrapper.sh', 'os/scripts/bake-runtime-bundle.sh',
                  'os/scripts/verify-elf-hardening.sh', 'tools/ci-sccache.sh',
                  '.forgejo/actions/restore-cargo-downloads/action.yml'}


def require(value, message):
    if not value:
        raise ValueError(message)


def bounded_output(command, maximum, timeout=30):
    """Bound captured CLI stdout and kill/reap on overflow or timeout.

    This bounds this process's capture, not the CLI's internal HTTP buffers.
    Stderr is discarded rather than retained or included in error diagnostics.
    """
    require(type(maximum) is int and maximum >= 0 and timeout > 0, 'invalid capture limit')
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + timeout
    output, errors = bytearray(), []
    finished = threading.Event()

    def read():
        try:
            with process.stdout:
                while len(output) <= maximum:
                    chunk = process.stdout.read1(min(65536, maximum + 1 - len(output)))
                    if not chunk:
                        break
                    output.extend(chunk)
        except Exception as error:
            errors.append(error)
        finally:
            finished.set()

    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    try:
        if not finished.wait(max(0, deadline - time.monotonic())):
            raise TimeoutError('plan API response timed out')
        require(len(output) <= maximum, 'plan API response exceeds byte bound')
        require(not errors, 'plan API response could not be read')
        try:
            status = process.wait(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            raise TimeoutError('plan API response timed out') from None
        require(status == 0, 'plan API request failed')
        return bytes(output)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        # The child is reaped even if an unrelated descendant inherited stdout.
        # Never wait indefinitely for that descendant to close its pipe.
        reader.join(timeout=1)


def digest(value, size=64):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(size) + '}', value), 'invalid digest')
    return value


def validate_desired(receipt):
    require(isinstance(receipt, dict) and set(receipt) == {
        'source_sha', 'builder_image', 'collection', 'architectures', 'policy_sha256'}, 'invalid desired identity')
    digest(receipt['source_sha'], 40)
    require(bool(receipt['builder_image']), 'explicit immutable builder required')
    resolve(receipt['builder_image'])
    digest(receipt['policy_sha256'])
    require(re.fullmatch('[a-z][a-z0-9-]*', receipt['collection']), 'invalid collection')
    require(receipt['architectures'] in [['x86_64'], ['aarch64'], ['x86_64', 'aarch64']], 'invalid architecture scope')
    return receipt


def dispatch_inputs(intent):
    desired = validate_desired(intent['desired'])
    baseline = intent['baseline']
    evidence = intent.get('baseline_evidence')
    # A builder/policy/scope change invalidates all previous packaging, even if
    # no app source changed. Never pass an incremental base across that boundary.
    compatible = desired['collection'] == 'default' and baseline is not None and all(baseline.get(key) == desired[key]
        for key in ('builder_image', 'collection', 'architectures', 'policy_sha256'))
    if baseline is not None:
        require(isinstance(evidence, dict), 'baseline lacks independent verification')
        validate_evidence(evidence, baseline, evidence.get('run_id'))
    return {'apps': '', 'collection': desired['collection'],
            'architectures': ','.join(desired['architectures']), 'publish': 'false',
            'coordinator_intent': intent['id'], 'builder_image': desired['builder_image'],
            'coordinator_policy_sha256': desired['policy_sha256'],
            'baseline_sha': baseline['source_sha'] if compatible else '',
            'baseline_run_id': str(evidence['run_id']) if compatible else '',
            'baseline_evidence': core.canonical(evidence) if compatible else '',
            'baseline_receipt_sha256': core.receipt_key(evidence) if compatible else ''}


def validate_evidence(proof, desired, run_id):
    require(isinstance(proof, dict) and proof.get('schema') == 'tilo.coordinator-verification/v1', 'invalid verification proof')
    require(type(run_id) is int and run_id > 0 and proof.get('run_id') == run_id, 'verification run mismatch')
    require(proof.get('desired') == desired, 'verification source/builder/scope mismatch')
    require(proof.get('backend') == 'canonical-final-artifact-v1', 'canonical artifact replay required')
    receipts = proof.get('receipts')
    require(isinstance(receipts, dict) and set(receipts) == set(desired['architectures']), 'incomplete architecture verification')
    for value in receipts.values():
        digest(value)
    return proof


class ProductForgejo(core.Forgejo):
    def __init__(self, policy, verifier=None):
        self.policy = policy
        # Trusted code dependency injection, not a command/plugin named by source
        # or a boolean in a downloaded JSON file. No provider is enabled by CLI.
        self.verifier = verifier

    @property
    def policy_sha(self):
        return core.receipt_key(self.policy)

    def qualify_receipt(self, receipt):
        validate_desired(receipt)
        require(self.policy.get('enabled') is True and self.policy.get('qualification_only') is True,
                'production qualification is disabled')
        require(receipt['policy_sha256'] == self.policy_sha, 'trusted policy identity changed')
        paths = self.policy.get('trusted_files', {})
        require(isinstance(paths, dict) and REQUIRED_PATHS <= set(paths), 'incomplete trusted workflow policy')
        repository = self.call('repos/' + REPO)
        require(repository.get('id') == 23 and repository.get('full_name') == REPO, 'repository identity mismatch')
        for path, expected in paths.items():
            require(isinstance(path, str) and re.fullmatch('[A-Za-z0-9_./-]+', path) and
                    '..' not in path.split('/') and not path.startswith('/'), 'unsafe policy path')
            digest(expected)
            blob = self.call(f'repos/{REPO}/contents/{path}?ref={receipt["source_sha"]}')
            raw = base64.b64decode(blob['content'], validate=True)
            require(hashlib.sha256(raw).hexdigest() == expected, 'workflow/helper differs from trusted policy: ' + path)

    def dispatch(self, intent):
        return self.call(f'repos/{REPO}/actions/workflows/{WORKFLOW}/dispatches', {
            'ref': intent['desired']['source_sha'], 'return_run_info': True,
            'inputs': dispatch_inputs(intent)})

    def run(self, run_id):
        return self.call(f'repos/{REPO}/actions/runs/{run_id}')

    def raw_api(self, path, maximum):
        # The server metadata is checked before requesting this small artifact.
        # Keep bytes raw: the ordinary call() method expects JSON API responses.
        return bounded_output(['fgj', '--hostname', 'code.podesta.ai', 'api', path], maximum)

    def artifact_bytes(self, artifact_id):
        return self.raw_api(f'repos/{REPO}/actions/artifacts/{artifact_id}/zip', 256 * 1024)

    def no_build_evidence(self, intent, run):
        """Verify the trusted plan, never infer no-build from skipped jobs."""
        # Check trusted executable provenance again when accepting artifacts,
        # including reconciliation after a worker restart.
        self.qualify_receipt(intent['desired'])
        jobs_path = f'repos/{REPO}/actions/runs/{run["id"]}/jobs'
        jobs = self.call(jobs_path)
        require(isinstance(jobs, list), 'invalid jobs response')
        plans = [job for job in jobs if job.get('name') == 'plan']
        require(len(plans) == 1, 'exactly one trusted plan job required')
        plan_job = plans[0]
        require(type(plan_job.get('id')) is int and plan_job['id'] > 0 and
                plan_job.get('repo_id') == 23 and plan_job.get('run_id') == run['id'] and plan_job.get('status') == 'success' and
                type(plan_job.get('attempt')) is int and plan_job['attempt'] > 0 and
                type(plan_job.get('task_id')) is int and plan_job['task_id'] > 0 and
                isinstance(plan_job.get('handle'), str) and bool(plan_job['handle']), 'plan job is not a current success')
        path = f'repos/{REPO}/actions/runs/{run["id"]}/artifacts?page=1&limit=50'
        artifacts = self.call(path)
        require(isinstance(artifacts, list) and len(artifacts) < 50, 'ambiguous/incomplete plan artifact listing')
        name = f'coordinator-plan-{plan_job["attempt"]}'
        matches = [item for item in artifacts if item.get('name') == name]
        require(len(matches) == 1, 'current plan artifact missing or ambiguous')
        artifact = matches[0]
        require(type(artifact.get('id')) is int and artifact['id'] > 0 and artifact.get('run_id') == run['id'] and
                artifact.get('expired') is False and type(artifact.get('size_in_bytes')) is int and
                0 < artifact['size_in_bytes'] <= 256 * 1024, 'invalid plan artifact metadata')
        raw = self.artifact_bytes(artifact['id'])
        require(len(raw) <= 256 * 1024, 'plan artifact exceeds byte bound')
        with zipfile.ZipFile(io.BytesIO(raw)) as archive:
            entries = archive.infolist()
            require(len(entries) == 1 and entries[0].filename == 'coordinator-plan.json' and
                    0 < entries[0].file_size <= 128 * 1024, 'invalid plan artifact inventory')
            content = archive.read(entries[0])
        log = self.raw_api(f'repos/{REPO}/actions/jobs/{plan_job["id"]}/logs', 8 * 1024 * 1024).decode('utf-8')
        markers = re.findall(r'(?m)^(?:\d{4}-\d{2}-\d{2}T[0-9:.]+Z )?TILO_COORDINATOR_PLAN '
                             + re.escape(intent['id']) + r' ([0-9a-f]{64})\r?$', log)
        require(markers == [hashlib.sha256(content).hexdigest()], 'plan artifact not bound by trusted current job log')
        document = json.loads(content)
        require(document.get('schema') == 'tilo.coordinator-plan/v1' and type(document.get('run_id')) is int and
                document['run_id'] == run['id'] and type(document.get('plan_attempt')) is int and
                document['plan_attempt'] == plan_job['attempt'] and
                document.get('source_sha') == intent['desired']['source_sha'] and
                document.get('builder_image') == intent['desired']['builder_image'] and
                document.get('inputs') == dispatch_inputs(intent), 'plan provenance does not match intent')
        plan = document.get('plan')
        require(isinstance(plan, dict) and type(plan.get('count')) is int and plan['count'] >= 0, 'invalid trusted plan')
        for key in ('apps', 'packages', 'catalog'):
            require(isinstance(plan.get(key), list) and len(plan[key]) == plan['count'], 'inconsistent trusted plan count')
        # A rerun replacing this successful plan while downloading must not be
        # accepted through an older artifact. No stale-attempt fallback exists.
        current = self.run(run['id'])
        keys = ('id', 'status', 'workflow_id', 'trigger_event', 'commit_sha', 'event_payload')
        stable_run = (all(current.get(key) == run.get(key) for key in keys) and
                      current.get('repository', {}).get('id') == 23 and
                      current.get('repository', {}).get('full_name') == REPO)
        require(self.call(jobs_path) == jobs and stable_run,
                'run or current jobs changed during plan verification')
        if plan['count'] != 0:
            return None
        require(plan.get('features') == [], 'zero-app plan has build features')
        return core.NoBuildAcceptance({'schema': 'tilo.coordinator-no-build/v1',
            'run_id': run['id'], 'desired': intent['desired'], 'inputs': dispatch_inputs(intent),
            'plan_job_id': plan_job['id'], 'plan_attempt': plan_job['attempt'],
            'plan_task_id': plan_job['task_id'], 'plan_handle': plan_job['handle'],
            'artifact_id': artifact['id'], 'plan_sha256': hashlib.sha256(content).hexdigest()})

    def validate_run(self, intent, run):
        require(self.policy.get('enabled') is True and self.policy.get('qualification_only') is True and
                intent['desired']['policy_sha256'] == self.policy_sha, 'active trusted policy changed or disabled')
        payload = json.loads(run.get('event_payload') or '{}')
        require(run.get('repository', {}).get('id') == 23 and
                run.get('repository', {}).get('full_name') == REPO and
                run.get('workflow_id') == WORKFLOW and
                run.get('trigger_event') == 'workflow_dispatch' and
                run.get('commit_sha') == intent['desired']['source_sha'] and
                payload.get('ref') == intent['desired']['source_sha'] and
                payload.get('inputs') == dispatch_inputs(intent) and
                type(run.get('id')) is int and run['id'] > 0,
                'run does not match immutable nonpublishing intent')
        if run['status'] == 'success':
            no_build = self.no_build_evidence(intent, run)
            if no_build is not None:
                return no_build
            if self.verifier is None:
                raise core.VerificationRequired('independent final artifact verification is not installed')
            return validate_evidence(self.verifier(intent, run), intent['desired'], run['id'])


def reconcile(store, api, run_id):
    active = store.read()['active']
    require(active is not None, 'no uncertain intent to reconcile')
    try:
        core.accept_run(store, active['id'], api.run(run_id), api.validate_run)
    except core.VerificationRequired:
        with store.transaction() as state:
            current = state['active']
            require(current is not None and current['id'] == active['id'], 'intent changed')
            require(current.get('run_id') in (None, run_id), 'run ID conflicts with concurrent reconciliation')
            current.update(run_id=run_id, status='awaiting-verification')
            store.record(current)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state', type=Path, required=True)
    parser.add_argument('--policy', type=Path, required=True)
    parser.add_argument('--apply-qualification', action='store_true')
    commands = parser.add_subparsers(dest='command', required=True)
    observe = commands.add_parser('observe')
    observe.add_argument('--sequence', type=int, required=True)
    observe.add_argument('--receipt', type=Path, required=True)
    commands.add_parser('status')
    commands.add_parser('tick')
    reconcile_parser = commands.add_parser('reconcile')
    reconcile_parser.add_argument('--run-id', type=int, required=True)
    args = parser.parse_args()
    policy = json.loads(args.policy.read_text())
    api = ProductForgejo(policy)
    apply = args.apply_qualification and args.command != 'status'
    if apply:
        require(policy.get('enabled') is True and policy.get('qualification_only') is True,
                'trusted policy has not enabled qualification')
    store = core.Store(args.state, writable=apply)
    try:
        if args.command == 'observe':
            receipt = validate_desired(json.loads(args.receipt.read_text()))
            require(receipt['policy_sha256'] == api.policy_sha, 'trusted policy identity mismatch')
            if apply:
                store.observe(args.sequence, receipt)
        elif apply and args.command == 'tick':
            core.tick(store, api)
        elif apply and args.command == 'reconcile':
            reconcile(store, api, args.run_id)
        state = store.read()
        print(json.dumps({'dry_run': not apply, 'publication_enabled': False,
                          'next_action': core.next_action(state), 'state': state}, indent=2))
    finally:
        if store.db:
            store.db.close()


if __name__ == '__main__':
    main()
