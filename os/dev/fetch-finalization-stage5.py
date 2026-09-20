#!/usr/bin/env python3
"""Fetch one explicitly pinned prior stage-5 artifact from this repository."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import urllib.request
import zipfile

MAX_METADATA = 256 * 1024
MAX_ARCHIVE = 16 * 1024 * 1024
MAX_RECEIPT = 1024 * 1024
STAGES = ('rebind', 'loader', 'provisional', 'composefs', 'reseal')
REQUIRED = tuple(f'{number:02d}-{name}.json' for number, name in enumerate(STAGES, 1))
ALLOWED = set(REQUIRED) | {'06-prediction.json', '07-verified.json', '08-published.json',
    'result.json', 'status.json', 'signing-pending.json', 'worker-identity.json', 'cleanup.json'}
HEX = re.compile(r'[0-9a-f]{64}')
IMAGE = re.compile(r'sha256:[0-9a-f]{64}')


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate JSON field')
        result[key] = value
    return result


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def get_url(url, token, limit):
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            return None
    request = urllib.request.Request(url, headers={'Authorization': 'Bearer ' + token})
    with urllib.request.build_opener(NoRedirect).open(request, timeout=30) as response:
        raw = response.read(limit + 1)
    if len(raw) > limit:
        raise ValueError('artifact API response exceeds bound')
    return raw


def validate_receipts(files, request_raw, request_sha256):
    if hashlib.sha256(request_raw).hexdigest() != request_sha256 or not HEX.fullmatch(request_sha256):
        raise ValueError('current request digest differs')
    request = json.loads(request_raw, object_pairs_hook=unique)
    expected_binding = {'request_sha256': request_sha256, 'source_revision': request['source_revision'],
        'helpers_revision': request['helpers_revision'], 'architecture': request['architecture'],
        'new_target': request['new_target'], 'new_manifest_sha256': request['new_manifest_sha256']}
    parent = None
    receipts = []
    for number, name in enumerate(REQUIRED, 1):
        raw = files[name]
        if not 0 < len(raw) <= MAX_RECEIPT:
            raise ValueError('stage-5 receipt exceeds bound')
        receipt = json.loads(raw, object_pairs_hook=unique)
        if (not isinstance(receipt, dict) or receipt.get('schema') != 1
                or receipt.get('stage') != STAGES[number - 1]
                or receipt.get('request_sha256') != request_sha256
                or receipt.get('parent_receipt_sha256') != parent):
            raise ValueError('stage-5 receipt chain differs from current request')
        receipts.append(receipt)
        parent = hashlib.sha256(canonical(receipt)).hexdigest()
    checkpoint = receipts[-1]
    manifest = checkpoint.get('checkpoint_manifest_digest', '')
    config = checkpoint.get('checkpoint_config_digest', '')
    reference = checkpoint.get('checkpoint_reference')
    if (not IMAGE.fullmatch(manifest) or not IMAGE.fullmatch(config)
            or checkpoint.get('image_id') != config
            or reference != request['publish_repository'] + '@' + manifest
            or checkpoint.get('checkpoint_binding') != expected_binding
            or not HEX.fullmatch(checkpoint.get('producer_context_sha256', ''))):
        raise ValueError('stage-5 registry checkpoint binding differs')
    return parent


def archive_files(raw):
    result = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        for entry in archive.infolist():
            name = entry.filename
            mode = entry.external_attr >> 16
            kind = stat.S_IFMT(mode)
            if (name not in ALLOWED or name in result or entry.is_dir()
                    or kind not in (0, stat.S_IFREG) or entry.flag_bits & 1):
                raise ValueError('artifact contains an unexpected entry')
            if not 0 < entry.file_size <= MAX_RECEIPT:
                raise ValueError('artifact entry exceeds bound')
            data = archive.read(entry)
            if len(data) != entry.file_size:
                raise ValueError('artifact entry length differs')
            result[name] = data
    if not set(REQUIRED).issubset(result):
        raise ValueError('artifact lacks complete stage-5 receipts')
    return result


def retrieve(run_id, attempt, receipt_sha256, request_path, output, api, repository,
             token, current_run_id, getter=get_url):
    if (not str(run_id).isdecimal() or int(run_id) <= 0 or not str(attempt).isdecimal()
            or int(attempt) <= 0 or not HEX.fullmatch(receipt_sha256)
            or int(run_id) == int(current_run_id)):
        raise ValueError('explicit prior run, attempt and archive SHA256 required')
    base = api.rstrip('/') + '/repos/' + repository
    prior = json.loads(getter(base + f'/actions/runs/{int(run_id)}', token, MAX_METADATA),
                       object_pairs_hook=unique)
    if (prior.get('id') != int(run_id) or prior.get('workflow_id') != 'development-finalize.yml'
            or prior.get('status') not in ('failure', 'cancelled')):
        raise ValueError('prior finalization run identity or terminal state differs')
    jobs = json.loads(getter(base + f'/actions/runs/{int(run_id)}/jobs', token, MAX_METADATA),
                      object_pairs_hook=unique)
    matches = [job for job in jobs if job.get('name') == 'finalize'
               and job.get('attempt') == int(attempt)
               and job.get('status') in ('failure', 'cancelled')]
    if len(matches) != 1 or not isinstance(matches[0].get('task_id'), int) or matches[0]['task_id'] <= 0:
        raise ValueError('prior finalization attempt identity differs')
    artifacts = json.loads(getter(base + f'/actions/runs/{int(run_id)}/artifacts?page=1&limit=50',
                                  token, MAX_METADATA), object_pairs_hook=unique)
    name = f'finalization-{int(run_id)}-{int(attempt)}'
    matches = [item for item in artifacts if item.get('name') == name
               and item.get('run_id') == int(run_id) and not item.get('expired')]
    if len(matches) != 1 or not isinstance(matches[0].get('id'), int):
        raise ValueError('one unexpired prior finalization artifact required')
    raw = getter(base + f'/actions/artifacts/{matches[0]["id"]}/zip', token, MAX_ARCHIVE)
    files = archive_files(raw)
    request_raw = Path(request_path).read_bytes()
    accepted_receipt = validate_receipts(
        files, request_raw, hashlib.sha256(request_raw).hexdigest())
    if accepted_receipt != receipt_sha256:
        raise ValueError('prior stage-5 receipt SHA256 differs')
    output = Path(output)
    output.mkdir(mode=0o700)
    try:
        for name in REQUIRED:
            path = output / name
            with path.open('xb') as destination:
                destination.write(files[name])
                destination.flush()
                os.fsync(destination.fileno())
            path.chmod(0o600)
        descriptor = os.open(output, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    except BaseException:
        for path in output.iterdir():
            path.unlink()
        output.rmdir()
        raise
    return {'run_id': int(run_id), 'attempt': int(attempt), 'artifact_id': matches[0]['id'],
            'receipt_sha256': receipt_sha256, 'request_sha256': hashlib.sha256(request_raw).hexdigest(),
            'output': str(output)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-id', required=True)
    parser.add_argument('--attempt', required=True)
    parser.add_argument('--receipt-sha256', required=True)
    parser.add_argument('--request', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    api = os.environ.get('GITHUB_API_URL') or os.environ['GITHUB_SERVER_URL'].rstrip('/') + '/api/v1'
    result = retrieve(args.run_id, args.attempt, args.receipt_sha256, args.request, args.output,
        api, os.environ['GITHUB_REPOSITORY'], os.environ['TASK_TOKEN'], os.environ['GITHUB_RUN_ID'])
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError, zipfile.BadZipFile):
        raise SystemExit('stage-5 artifact retrieval rejected')
