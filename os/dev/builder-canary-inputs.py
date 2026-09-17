#!/usr/bin/env python3
"""Bind native canaries to actual image digests and successful cold receipts."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import uuid

MODULE = importlib.util.spec_from_file_location('native_index', Path(__file__).with_name('promote-native-index.py'))
native = importlib.util.module_from_spec(MODULE)
MODULE.loader.exec_module(native)
REPOSITORY = 'code.podesta.ai/qos/tilo-builder'
FIELDS = {'schema', 'architecture', 'image', 'revision', 'runId', 'coldAttempt', 'namespace'}


def matrix(directory, revision):
    candidates = {}
    for path in sorted(directory.rglob('child.json')):
        match = re.fullmatch(r'builder-native-(x86_64|aarch64)-([1-9][0-9]*)', path.parent.name)
        if not match or path.is_symlink() or path.stat().st_size > 16384:
            raise ValueError('invalid native descriptor artifact')
        arch, attempt = match[1], int(match[2])
        if (arch, attempt) in candidates:
            raise ValueError('duplicate native descriptor attempt')
        candidates[arch, attempt] = path
    with tempfile.TemporaryDirectory(prefix='tilo-native-selection-') as temporary:
        selected = Path(temporary)
        for arch in native.ARCHITECTURES:
            attempts = [attempt for candidate_arch, attempt in candidates if candidate_arch == arch]
            if not attempts:
                raise ValueError('missing successful native descriptor')
            path = candidates[arch, max(attempts)]
            if json.loads(path.read_text()).get('architecture') != arch:
                raise ValueError('native descriptor artifact architecture mismatch')
            shutil.copyfile(path, selected / f'{arch}.json')
        children = native.load_children(selected, revision)
    return {'include': [dict(arch=arch, runner='runner-arm64' if arch == 'aarch64' else 'runner',
                             image=f'{REPOSITORY}@{digest}')
                        for arch, digest in sorted(children.items())]}


def write_descriptors(directory, selected, revision):
    directory.mkdir(parents=True, exist_ok=False)
    if not isinstance(selected, dict) or set(selected) != {'include'} or len(selected['include']) != 2:
        raise ValueError('invalid selected native matrix')
    for item in selected['include']:
        if set(item) != {'arch', 'runner', 'image'}:
            raise ValueError('invalid selected native matrix fields')
        identity(item['arch'], item['image'], revision, '1')
        with (directory / f'{item["arch"]}.json').open('x') as output:
            json.dump(dict(architecture=item['arch'], digest=item['image'].split('@')[1],
                           sourceRevision=revision), output)
    native.load_children(directory, revision)


def identity(arch, image, revision, run_id):
    if arch not in native.ARCHITECTURES:
        raise ValueError('invalid canary architecture')
    if not re.fullmatch(re.escape(REPOSITORY) + r'@sha256:[0-9a-f]{64}', image):
        raise ValueError('canary requires an immutable native image')
    if not re.fullmatch(r'[0-9a-f]{40}', revision) or not re.fullmatch(r'[1-9][0-9]*', run_id):
        raise ValueError('invalid canary revision/run identity')


def new_receipt(arch, image, revision, run_id, attempt):
    identity(arch, image, revision, run_id)
    if not re.fullmatch(r'[1-9][0-9]*', attempt):
        raise ValueError('invalid cold attempt')
    return dict(schema=1, architecture=arch, image=image, revision=revision,
                runId=run_id, coldAttempt=int(attempt),
                namespace=f'tilo/canary/{run_id}/{arch}/{uuid.uuid4().hex}')


def select_receipt(directory, arch, image, revision, run_id):
    identity(arch, image, revision, run_id)
    attempts = {}
    for path in sorted(directory.rglob('receipt.json')):
        if not path.parent.name.startswith('builder-cache-cold-'):
            continue
        if path.is_symlink() or path.stat().st_size > 16384:
            raise ValueError('invalid cold receipt file')
        receipt = json.loads(path.read_text())
        if not isinstance(receipt, dict) or set(receipt) != FIELDS or receipt['schema'] != 1:
            raise ValueError('invalid cold receipt fields')
        identity(receipt['architecture'], receipt['image'], receipt['revision'], receipt['runId'])
        if receipt['revision'] != revision or receipt['runId'] != run_id:
            raise ValueError('cold receipt belongs to another revision/run')
        attempt = receipt['coldAttempt']
        if type(attempt) is not int or attempt <= 0:
            raise ValueError('invalid cold receipt attempt')
        expected_name = f'builder-cache-cold-{receipt["architecture"]}-{attempt}'
        if path.parent.name != expected_name:
            raise ValueError('cold receipt artifact/attempt mismatch')
        prefix = f'tilo/canary/{run_id}/{receipt["architecture"]}/'
        if not re.fullmatch(re.escape(prefix) + r'[0-9a-f]{32}', receipt['namespace']):
            raise ValueError('invalid cold cache namespace')
        if receipt['architecture'] != arch:
            continue
        if attempt in attempts:
            raise ValueError('duplicate successful cold attempt')
        attempts[attempt] = receipt
    if not attempts:
        raise ValueError('missing successful cold receipt')
    # Upload happens only after cold acceptance. Per-job attempts, unlike
    # timestamps or a consumer's run_attempt, order successful producer runs.
    selected = attempts[max(attempts)]
    if selected['image'] != image:
        raise ValueError('latest cold receipt used a different native image')
    return selected


def verify_acceptance(directory, selected, revision, run_id):
    for item in selected['include']:
        arch, image = item['arch'], item['image']
        cold = select_receipt(directory, arch, image, revision, run_id)
        attempts = {}
        for path in directory.rglob('warm.json'):
            match = re.fullmatch(r'builder-cache-warm-(x86_64|aarch64)-([1-9][0-9]*)', path.parent.name)
            if not match or path.is_symlink() or path.stat().st_size > 16384:
                raise ValueError('invalid warm acceptance artifact')
            if match[1] != arch:
                continue
            attempt = int(match[2])
            value = json.loads(path.read_text())
            if (not isinstance(value, dict) or set(value) != {'schema', 'warmAttempt', 'cold'}
                    or value['schema'] != 1 or type(value['warmAttempt']) is not int
                    or value['warmAttempt'] != attempt):
                raise ValueError('invalid warm acceptance fields')
            if attempt in attempts:
                raise ValueError('duplicate warm acceptance attempt')
            attempts[attempt] = value['cold']
        if not attempts or attempts[max(attempts)] != cold:
            raise ValueError('warm acceptance does not match latest successful cold image/namespace')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('matrix', 'cold', 'warm', 'descriptors', 'verify'))
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    revision = os.environ['GITHUB_SHA']
    if args.mode == 'verify':
        verify_acceptance(args.directory, json.loads(os.environ['CANDIDATE_MATRIX']),
                          revision, os.environ['GITHUB_RUN_ID'])
        return
    if args.mode == 'descriptors':
        write_descriptors(args.directory, json.loads(os.environ['CANDIDATE_MATRIX']), revision)
        return
    if args.mode == 'matrix':
        with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
            output.write('matrix=' + json.dumps(matrix(args.directory, revision), separators=(',', ':')) + '\n')
        return
    values = (os.environ['CANDIDATE_ARCH'], os.environ['CANDIDATE_IMAGE'],
              revision, os.environ['GITHUB_RUN_ID'])
    if args.mode == 'cold':
        receipt = new_receipt(*values, os.environ['GITHUB_RUN_ATTEMPT'])
        args.directory.mkdir(parents=True, exist_ok=False)
        (args.directory / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    else:
        receipt = select_receipt(args.directory, *values)
        attempt = os.environ['GITHUB_RUN_ATTEMPT']
        if not re.fullmatch(r'[1-9][0-9]*', attempt):
            raise ValueError('invalid warm attempt')
        Path('warm-receipt').mkdir(exist_ok=False)
        Path('warm-receipt/warm.json').write_text(json.dumps({
            'schema': 1, 'warmAttempt': int(attempt), 'cold': receipt}, indent=2) + '\n')
    with Path(os.environ['GITHUB_ENV']).open('a') as output:
        output.write('TILO_SCCACHE_KEY_PREFIX=' + receipt['namespace'] + '\n')


if __name__ == '__main__':
    main()
