#!/usr/bin/env python3
"""Trusted, read-only final-artifact acceptance callback; never auto-installed.

Install this module and its dependencies outside candidate checkouts. Construct it
with platform-owned policy/API clients, then inject it as ProductForgejo.verifier.
"""
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
import zipfile

import app_channel_manifest as channel
from app_packaging_product import dispatch_inputs, run_inputs_match, validate_evidence
from assemble_verified_apps import MAX_BYTES, extract_tar, extraction_destination
import canonical_verifier_bundle as bundle
import final_app_receipt as final


JOB_KEYS = ('id', 'repo_id', 'run_id', 'status', 'attempt', 'task_id', 'handle')
RUN_KEYS = ('id', 'status', 'workflow_id', 'trigger_event', 'commit_sha', 'event_payload')


def validate_run(api, intent, run):
    desired = intent['desired']
    payload = channel.read_json((run.get('event_payload') or '{}').encode())
    channel.require(api.policy_sha == desired['policy_sha256'] and run.get('status') == 'success'
                    and run.get('repository', {}).get('id') == 23
                    and run.get('repository', {}).get('full_name') == 'tilo/tilo'
                    and run.get('workflow_id') == 'release-apps.yml'
                    and run.get('trigger_event') == 'workflow_dispatch'
                    and run.get('commit_sha') == desired['source_sha']
                    and payload.get('ref') == desired['source_sha']
                    and run_inputs_match(intent, payload.get('inputs'))
                    and type(run.get('id')) is int and run['id'] > 0,
                    'final run does not match trusted dispatch')


def qualifier(jobs, run_id):
    channel.require(isinstance(jobs, list), 'invalid jobs response')
    matches = [job for job in jobs if job.get('name') == 'qualify-collection']
    channel.require(len(matches) == 1, 'one current qualifier job required')
    job = matches[0]
    channel.require(job.get('repo_id') == 23 and job.get('run_id') == run_id and job.get('status') == 'success'
                    and all(type(job.get(k)) is int and job[k] > 0 for k in ('id', 'attempt', 'task_id'))
                    and isinstance(job.get('handle'), str) and bool(job['handle']), 'qualifier is not a current successful attempt')
    return job


def markers(raw, intent, attempt, architectures):
    log = raw.decode('utf-8')
    rows = re.findall(r'(?m)^(?:\d{4}-\d{2}-\d{2}T[0-9:.]+Z )?TILO_COORDINATOR_FINAL '
                      + re.escape(intent) + ' ' + str(attempt) + r' (x86_64|aarch64) ([0-9a-f]{64}) ([0-9a-f]{64}) ([1-9][0-9]*)\r?$', log)
    channel.require(len(rows) == len(architectures) and {row[0] for row in rows} == set(architectures),
                    'current qualifier log lacks exact architecture digest markers')
    return {arch: (receipt, archive, int(size)) for arch, receipt, archive, size in rows}


def extract_artifact(archive, output, architecture, marker):
    expected = {f'tilo-apps-{architecture}.tar.gz', 'final-publication-receipt.json', 'assembly-provenance.json'}
    channel.require(archive.stat().st_size <= MAX_BYTES, 'final artifact download exceeds bound')
    with extraction_destination(output):
        with zipfile.ZipFile(archive) as zipper:
            entries = zipper.infolist()
            channel.require(len(entries) == 3 and {entry.filename for entry in entries} == expected
                            and sum(entry.file_size for entry in entries) <= MAX_BYTES,
                            'unexpected final artifact inventory/size')
            for entry in entries:
                mode = entry.external_attr >> 16
                channel.require(not mode or (mode & 0o170000) in (0, 0o100000), 'nonregular final artifact member')
                if entry.filename.endswith('.json'):
                    channel.require(entry.file_size <= 2 * 1024**2, 'oversized receipt metadata')
                with zipper.open(entry) as source, (output / entry.filename).open('xb') as target:
                    import shutil
                    shutil.copyfileobj(source, target)
        receipt_path = output / 'final-publication-receipt.json'
        receipt = channel.pinned_json(receipt_path.read_bytes(), marker[0])
        tar = output / f'tilo-apps-{architecture}.tar.gz'
        channel.require(tar.stat().st_size == marker[2] and channel.sha(tar) == marker[1]
                        and receipt['collection']['sha256'] == marker[1] and receipt['collection']['bytes'] == marker[2],
                        'artifact bytes differ from trusted qualifier marker')
        return receipt, tar


class FinalArtifactAcceptance:
    def __init__(self, product_api, read_api, policy_path, policy_sha256, bundle_token='', work_parent=None):
        # All configuration is injected by the trusted platform, never by the run.
        self.api = product_api
        self.read = read_api
        self.policy_path = Path(policy_path)
        self.policy_sha = policy_sha256
        self.bundle_token = bundle_token
        self.work_parent = work_parent

    def __call__(self, intent, matched_run):
        intent = copy.deepcopy(intent)
        channel.require(self.api.policy.get('final_receipt_policy_sha256') == self.policy_sha,
                        'final verifier policy is not bound to trusted coordinator policy')
        self.api.qualify_receipt(intent['desired'])
        run = self.api.run(matched_run['id'])
        validate_run(self.api, intent, run)
        channel.require(all(run.get(k) == matched_run.get(k) for k in RUN_KEYS), 'matched run changed before verification')
        run_id = run['id']
        jobs_path = f'repos/tilo/tilo/actions/runs/{run_id}/jobs'
        jobs = self.api.call(jobs_path)
        job = qualifier(jobs, run_id)
        log = self.api.raw_api(f'repos/tilo/tilo/actions/jobs/{job["id"]}/logs', 8 * 1024**2)
        bound = markers(log, intent['id'], job['attempt'], intent['desired']['architectures'])
        artifacts_path = f'repos/tilo/tilo/actions/runs/{run_id}/artifacts?page=1&limit=50'
        artifacts = self.api.call(artifacts_path)
        channel.require(isinstance(artifacts, list) and len(artifacts) < 50, 'incomplete artifact listing')
        root_policy = channel.pinned_json(self.policy_path.read_bytes(), self.policy_sha)
        receipts, artifact_ids = {}, {}
        with tempfile.TemporaryDirectory(prefix='final-app-accept-', dir=self.work_parent) as temporary:
            work = Path(temporary)
            tools = work / 'verifiers'
            # Native replay may use a different approved tool architecture from
            # the producing job; compare verified content, not tool binary identity.
            bundle.fetch(self.policy_path, self.policy_sha, tools, intent['desired']['builder_image'], self.bundle_token)
            for architecture in intent['desired']['architectures']:
                matches = [a for a in artifacts if a.get('name') == f'final-app-collection-{architecture}']
                channel.require(len(matches) == 1, 'missing/ambiguous final architecture artifact')
                artifact = matches[0]
                channel.require(type(artifact.get('id')) is int and artifact['id'] > 0 and artifact.get('run_id') == run_id
                                and artifact.get('expired') is False and type(artifact.get('size_in_bytes')) is int
                                and 0 < artifact['size_in_bytes'] <= MAX_BYTES, 'invalid final artifact metadata')
                # Retain only one architecture's large payload at a time. Tool
                # bundles and accepted digests survive; payload scratch does not.
                with tempfile.TemporaryDirectory(prefix=architecture + '-', dir=work) as scratch:
                    area = Path(scratch)
                    downloaded = area / 'artifact.zip'
                    self.read.get(f'/actions/artifacts/{artifact["id"]}/zip', downloaded)
                    original, archive = extract_artifact(downloaded, area / 'payload', architecture, bound[architecture])
                    channel.validate_receipt(original, architecture, root_policy, require_published=False)
                    channel.require(original['source_sha'] == intent['desired']['source_sha']
                                    and original['publication'] == {'status': 'unpublished'}, 'final source/publication mismatch')
                    tree = area / 'tree'
                    extract_tar(archive, tree)
                    # Both paths are in this private temporary filesystem. Give
                    # final.verify its expected pathname without another 2GiB copy.
                    os.link(archive, tree / archive.name)
                    replay = final.verify(tree, tools / 'tilo-app-package', tools / 'tilo-runtime-bundle', self.policy_path,
                                          self.policy_sha, intent['desired']['source_sha'], architecture)
                    for key in ('schema', 'architecture', 'source_sha', 'collection', 'runtime', 'packages', 'publication'):
                        channel.require(replay[key] == original[key], 'canonical replay differs from qualified receipt: ' + key)
                    receipts[architecture] = bound[architecture][0]
                    artifact_ids[architecture] = artifact['id']
        # No baseline can advance if a rerun changed the qualifier attempt/task,
        # run inputs or artifact identities during download/canonical execution.
        self.api.qualify_receipt(intent['desired'])
        channel.require(self.api.policy.get('final_receipt_policy_sha256') == self.policy_sha,
                        'final verifier policy binding changed during replay')
        current = self.api.run(run_id)
        validate_run(self.api, intent, current)
        channel.require(all(current.get(k) == run.get(k) for k in RUN_KEYS)
                        and self.api.call(jobs_path) == jobs and self.api.call(artifacts_path) == artifacts,
                        'run, jobs or artifacts changed during final verification')
        channel.require(channel.sha(self.policy_path) == self.policy_sha, 'verification policy changed during replay')
        return validate_evidence({'schema': 'tilo.coordinator-verification/v1', 'backend': 'canonical-final-artifact-v1',
            'run_id': run_id, 'desired': intent['desired'], 'receipts': receipts,
            'qualifier': {key: job[key] for key in JOB_KEYS}, 'artifacts': artifact_ids,
            'final_policy_sha256': self.policy_sha}, intent['desired'], run_id)
