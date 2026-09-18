#!/usr/bin/env python3
"""Independently reproduce a native reconstruction plan offline, without an engine.

Receipt bytes and original checkpoints are independently pinned inputs. This
approves recipe derivation, not registry contents, inventory or installation.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import stat
import sys
import types

sys.dont_write_bytecode = True
SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
HEX = re.compile(r'[0-9a-f]{64}')
DIGEST = re.compile(r'sha256:[0-9a-f]{64}')


def require(value, message):
    if not value: raise ValueError(message)


def sha(data): return hashlib.sha256(data).hexdigest()
def canonical(value): return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate receipt property')
        result[key] = value
    return result


def read_pinned(path, expected):
    require(HEX.fullmatch(expected or ''), 'independent exact receipt SHA256 required')
    path = Path(path)
    require(not any(p.is_symlink() or getattr(p, 'is_junction', lambda: False)()
                    for p in (path, *path.parents)), 'redirected receipt path')
    # Windows has no O_NOFOLLOW; the descriptor's regular-file check and exact
    # independently supplied content hash still bind every accepted byte.
    descriptor = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
    with os.fdopen(descriptor, 'rb') as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= 1024 * 1024, 'bounded receipt file required')
        raw = stream.read(1024 * 1024 + 1)
        after = os.fstat(stream.fileno())
        require((before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'receipt changed while reading')
    require(len(raw) <= 1024 * 1024 and sha(raw) == expected, 'receipt bytes differ from independent pin')
    return json.loads(raw, object_pairs_hook=unique)


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.DEVNULL, timeout=30)


def blob(repo, revision, path):
    value = git(repo, 'show', revision + ':' + path)
    require(len(value) <= 2 * 1024 * 1024, 'trusted source blob exceeds bound')
    return value


def trusted_inputs(source, helper_repo, helper_revision, rpms, wpe, wpe_sha):
    require(re.fullmatch(r'[0-9a-f]{40}', helper_revision), 'independently pinned helper commit required')
    require(git(helper_repo, 'cat-file', '-t', helper_revision).strip() == b'commit', 'helper identity must be a commit')
    source = Path(source).resolve()
    require(git(source, 'rev-parse', 'HEAD').decode().strip() == SOURCE, 'application source must be frozen4583')
    require(not git(source, 'status', '--porcelain', '--untracked-files=all').strip(), 'application source must be clean')
    # No modified or newline-transformed working helper bytes are executed.
    for name in ('os/dev/native_builder_artifact.py', 'os/dev/wpe-stage-artifact.py', 'os/Containerfile'):
        require((source / name).read_bytes() == blob(source, SOURCE, name), 'application working bytes differ from Git blob')
    spec = importlib.util.spec_from_file_location('approval_original_native', source / 'os/dev/native_builder_artifact.py')
    native = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(native)
    # The source function's only filesystem hash is the recipe. Supply its
    # independently read Git blob instead of requiring Linux O_NOFOLLOW merely
    # to hash an already byte-compared trusted file on an offline Windows host.
    def source_blob_hash(path, limit):
        require(Path(path) == source / 'os/Containerfile', 'unexpected original input hashing request')
        data = blob(source, SOURCE, 'os/Containerfile')
        require(len(data) <= limit, 'original recipe exceeds source bound')
        return sha(data)
    native.tools.file_hash = source_blob_hash
    pin = blob(source, SOURCE, 'os/base-snapshot.env').decode()
    matched = re.search(r'^BASE_REF=[\"\x27]?([^\s\"\x27]+)', pin, re.M)
    require(matched is not None, 'frozen base pin missing')
    epoch = int(git(source, 'show', '-s', '--format=%ct', SOURCE))
    inputs = native.expected_inputs(source, SOURCE, 'aarch64', matched[1], rpms, wpe, wpe_sha, epoch)
    helper = blob(helper_repo, helper_revision, 'os/dev/native_resume.py')
    module = types.ModuleType('approval_pinned_native_resume')
    # Code is from the independently supplied trusted commit, never a receipt.
    exec(compile(helper, 'pinned-native-resume.py', 'exec'), module.__dict__)
    material = {'recipe': blob(source, SOURCE, 'os/Containerfile'),
                'driver': blob(source, SOURCE, 'os/dev/build-image.sh'), 'helper': helper,
                'producer': blob(helper_repo, helper_revision, 'os/dev/resume-native-payload.py')}
    return native.REQUIRED, inputs, module, material


def checkpoint_union(checkpoints, required):
    union = {}
    for item in checkpoints:
        require(set(item) == {'receipt_sha256', 'receipt'} and HEX.fullmatch(item['receipt_sha256']), 'checkpoint binding differs')
        receipt = item['receipt']
        require(set(receipt) == {'purpose', 'source_revision', 'reference', 'config_digest', 'metadata_sha256',
            'inventory', 'installable', 'complete_native_builder', 'build_success'}, 'checkpoint schema differs')
        require(receipt['purpose'] == 'tilo-development-partial-native-checkpoint' and receipt['source_revision'] == SOURCE
                and all(receipt[k] is False for k in ('installable', 'complete_native_builder', 'build_success')),
                'checkpoint source or completion claim differs')
        require(re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-partial-native-aarch64@sha256:[0-9a-f]{64}', receipt['reference'])
                and DIGEST.fullmatch(receipt['config_digest']) and HEX.fullmatch(receipt['metadata_sha256']),
                'checkpoint immutable identity differs')
        require(isinstance(receipt['inventory'], dict) and receipt['inventory'], 'empty checkpoint inventory')
        for name, record in receipt['inventory'].items():
            require(name in set(required) | {'browser'} and set(record) == {'size', 'sha256'}
                and type(record['size']) is int and 0 < record['size'] <= 4 * 1024**3
                and HEX.fullmatch(record['sha256']), 'checkpoint binary record differs')
            require(name not in union or union[name] == record, 'overlapping checkpoint records disagree')
            union[name] = record
    return union


def approve(receipt, receipt_sha, checkpoints, required, inputs, resume, material,
            helper_revision, run_id, attempt):
    require(HEX.fullmatch(receipt_sha) and run_id.isdecimal() and attempt.isdecimal(), 'exact receipt and run pins required')
    require(1 <= len(checkpoints) <= 4, 'bounded independent checkpoints required')
    expected_fields = {'schema', 'purpose', 'inputs', 'reference', 'config_digest', 'metadata_sha256',
        'reconstruction', 'reconstruction_sha256', 'complete_native_builder', 'production_signed', 'installable',
        'consumer_adoption_required'}
    require(set(receipt) == expected_fields and type(receipt['schema']) is int and receipt['schema'] == 1
            and receipt['purpose'] == 'tilo-development-reconstructed-native-builder'
            and canonical(receipt['inputs']) == canonical(inputs), 'reconstructed receipt source/input identity differs')
    require(receipt['complete_native_builder'] is True and receipt['production_signed'] is False
            and receipt['installable'] is False and receipt['consumer_adoption_required'] is True,
            'reconstructed completion claims differ')
    require(re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-reconstructed-native-aarch64@sha256:[0-9a-f]{64}', receipt['reference'])
            and DIGEST.fullmatch(receipt['config_digest']) and HEX.fullmatch(receipt['metadata_sha256']),
            'reconstructed immutable identity differs')
    union = checkpoint_union(checkpoints, required)
    recipe, missing = resume.derive_recipe(material['recipe'].decode(), union, required)
    parameters = {key: receipt['reconstruction'][key] for key in ('source_path', 'derived_recipe_path', 'seed_image_id')}
    for key in ('source_path', 'derived_recipe_path'):
        path = parameters[key]
        require(isinstance(path, str) and 0 < len(path) <= 4096 and PurePosixPath(path).is_absolute()
                and str(PurePosixPath(path)) == path and '..' not in PurePosixPath(path).parts,
                'noncanonical recorded driver parameter')
    driver = resume.derive_driver(material['driver'].decode(), **parameters)
    expected = {'schema': 1, 'purpose': 'tilo-native-reconstruction', 'source_revision': SOURCE,
        'original_recipe_sha256': sha(material['recipe']), 'derived_recipe_sha256': sha(recipe.encode()),
        'original_driver_sha256': sha(material['driver']), 'derived_driver_sha256': sha(driver.encode()),
        **parameters, 'resume_helper_sha256': sha(material['helper']), 'producer_sha256': sha(material['producer']),
        'checkpoints': checkpoints, 'reused_inventory': union, 'missing_targets': missing,
        'run_id': run_id, 'attempt': attempt,
        'remaining_phases': ['thin-app-shared-graph', 'elf-hardening', 'runtime-bundle', 'app-store'],
        'original_recipe_executed_verbatim': False}
    require(canonical(receipt['reconstruction']) == canonical(expected), 'independent reconstruction reproduction differs')
    approved = sha(canonical(expected))
    require(receipt['reconstruction_sha256'] == approved, 'remote reconstruction digest differs from independent result')
    return {'schema': 1, 'purpose': 'tilo-native-reconstruction-offline-approval',
        'receipt_sha256': receipt_sha, 'approved_reconstruction_sha256': approved,
        'source_revision': SOURCE, 'helper_revision': helper_revision, 'run_id': run_id, 'attempt': attempt,
        'reference': receipt['reference'], 'config_digest': receipt['config_digest'],
        'checkpoint_receipt_sha256': [item['receipt_sha256'] for item in checkpoints],
        'registry_verified': False, 'complete_inventory_verified': False, 'seed_payload_verified': False,
        'consumer_import_required': True, 'installable': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'helper-repo', 'helper-revision', 'receipt', 'receipt-sha256', 'rpms', 'wpe-reference',
                 'wpe-receipt-sha256', 'run-id', 'attempt', 'output'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--checkpoint', nargs=2, action='append', required=True, metavar=('PATH', 'SHA256'))
    args = parser.parse_args()
    receipt = read_pinned(args.receipt, args.receipt_sha256)
    require(1 <= len(args.checkpoint) <= 4, 'bounded checkpoints required')
    checkpoints = [{'receipt_sha256': digest, 'receipt': read_pinned(path, digest)} for path, digest in args.checkpoint]
    required, inputs, resume, material = trusted_inputs(args.source, args.helper_repo, args.helper_revision,
        args.rpms, args.wpe_reference, args.wpe_receipt_sha256)
    result = approve(receipt, args.receipt_sha256, checkpoints, required, inputs, resume, material,
        args.helper_revision, args.run_id, args.attempt)
    with Path(args.output).open('xb') as stream:
        stream.write(canonical(result) + b'\n')
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try: main()
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError):
        sys.exit('independent reconstruction approval rejected; no build or registry operation performed')
