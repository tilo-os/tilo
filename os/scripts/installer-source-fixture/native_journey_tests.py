#!/usr/bin/env python3
"""Run positive installer journeys against an owned GPT and genuine helper.

Invoke with sudo on a dedicated native builder. NBD provisioning is an external
prerequisite. Inputs are the actual root-owned artifacts and unchanged Cargo
JSON; no receipt or installer facts are synthesized. No desktop is started.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

from bounded_native import bounded, assert_children_retired
from owned_gpt import owned_source, _LIVE_ATTACHMENTS, checked

TESTS = (
    'setup::journey::tests::same_computer_permission_accepts_semantic_grant_and_persists_review_before_advancing',
    'setup::journey::main_story_tests::guided_new_user_migrates_recognized_home_then_reviews_shared_files_storage',
    'setup::journey::main_story_tests::guided_existing_backup_migrates_this_home_without_creating_another_identity',
    'setup::journey::source_change_tests::changed_document_has_published_recovery_and_fresh_review_preserves_identity_and_accounts',
    'setup::journey::prepared_media_native::recovery_tests::prepared_stage_rechecks_recovery_custody_at_actual_write_admission',
)


def admitted(path):
    path = Path(path).resolve(strict=True)
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid or info.st_nlink != 1 or info.st_mode & 0o022:
        raise RuntimeError('root-owned immutable regular input required: ' + str(path))
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for part in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(part)
    return path, digest.hexdigest()


def children():
    return {int(pid) for pid in Path(f'/proc/self/task/{os.getpid()}/children').read_text().split()}


def inner(args):
    if os.getpid() != 1:
        raise RuntimeError('fresh owned PID namespace init required')
    evidence = Path(args.evidence).resolve(strict=True)
    info = evidence.stat()
    if info.st_uid or info.st_mode & 0o077:
        raise RuntimeError('root-private evidence directory required')
    helper, helper_hash = admitted(args.helper)
    compiler, compiler_hash = admitted(args.compiler_json)
    tests, tests_hash = admitted(args.tests)
    tests_compiler, tests_compiler_hash = admitted(args.tests_compiler_json)
    helper_records = [json.loads(line) for line in compiler.read_text().splitlines()]
    test_records = [json.loads(line) for line in tests_compiler.read_text().splitlines()]
    helper_records = [record for record in helper_records
                      if record.get('reason') == 'compiler-artifact'
                      and record['target']['name'] == 'imager'
                      and record['target']['kind'] == ['bin']]
    test_records = [record for record in test_records
                    if record.get('reason') == 'compiler-artifact'
                    and record['target']['name'] == 'imager'
                    and record['profile']['test'] and record.get('executable')]
    if (len(helper_records) != 1 or len(test_records) != 1
            or helper_records[0]['manifest_path'] != test_records[0]['manifest_path']
            or helper_records[0]['package_id'] != test_records[0]['package_id']):
        raise RuntimeError('actual matched helper/test compiler manifests required')
    (evidence / 'input-hashes.json').write_text(json.dumps({
        str(helper): helper_hash, str(compiler): compiler_hash, str(tests): tests_hash,
        str(tests_compiler): tests_compiler_hash,
    }, indent=2) + '\n')
    results = []
    for index, selector in enumerate(TESTS):
        work = evidence / str(index)
        work.mkdir(mode=0o700)
        def retired():
            assert_children_retired()
            # During source setup, the exact owned NBD may already exist even
            # if connect() has not yielded yet. It is retired by owned_source.
            attachments = [item for item in _LIVE_ATTACHMENTS.values()
                           if item.scratch == work / 'physical']
            expected = {item.child.pid for item in attachments if item.child is not None}
            if children() != expected:
                raise RuntimeError('actual journey descendant retirement unobserved')

        with owned_source(work / 'physical', args.namespace_fd, retired) as source:
            source.verify()
            environment = {
                'TILO_ORIGINAL_SOURCE_GPT_STORE': str(source.mount),
                'TILO_NATIVE_IMAGER_HELPER': str(helper),
                'TILO_NATIVE_IMAGER_HELPER_SHA256': helper_hash,
                'TILO_NATIVE_IMAGER_HELPER_COMPILER_JSON': str(compiler),
                'TILO_NATIVE_IMAGER_HELPER_COMPILER_SHA256': compiler_hash,
            }
            # Production stores its protected source record on the original Home
            # volume. Give the private namespace that real Ext4-backed location,
            # not the builder's unrelated /var/lib filesystem.
            checked(['/usr/bin/mount', '-t', 'tmpfs', '-o', 'mode=0755,size=1M',
                     'tmpfs', '/var/lib'])
            target = Path('/var/lib/tilo-imager/home-capture')
            try:
                target.mkdir(parents=True, mode=0o700)
                source.system_store.mkdir(parents=True, mode=0o700)
                checked(['/usr/bin/mount', '--bind', str(source.system_store), str(target)])
                try:
                    status, out, err = bounded(
                        ['/usr/bin/env', *(f'{key}={value}' for key, value in environment.items()),
                         str(tests), '--exact', selector, '--include-ignored', '--nocapture', '--test-threads=1'],
                        timeout=180, limit=1024 * 1024,
                    )
                finally:
                    checked(['/usr/bin/umount', str(target)])
            finally:
                checked(['/usr/bin/umount', '/var/lib'])
            (work / 'stdout.log').write_bytes(out)
            (work / 'stderr.log').write_bytes(err)
            executed = b'test result: ok. 1 passed; 0 failed' in out
            results.append({'test': selector, 'status': status, 'passed': status == 0 and executed})
            print(json.dumps(results[-1]), flush=True)
            if status != 0 or not executed:
                print(out.decode(errors='replace'), err.decode(errors='replace'), flush=True)
            source.verify()
            retired()
        if children():
            raise RuntimeError('source NBD retirement unobserved')
        if not (work / 'physical/release-complete').is_file():
            raise RuntimeError('physical owned source teardown incomplete')
    (evidence / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    return int(not all(result['passed'] for result in results))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', required=True)
    parser.add_argument('--compiler-json', required=True)
    parser.add_argument('--tests', required=True)
    parser.add_argument('--tests-compiler-json', required=True)
    parser.add_argument('--evidence', required=True)
    parser.add_argument('--namespace-fd', type=int)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise RuntimeError('dedicated native root coordinator required')
    if args.namespace_fd is not None:
        return inner(args)
    # Hold the real initial PID namespace before entering the child namespace.
    witness = os.open('/proc/self/ns/pid', os.O_RDONLY | os.O_CLOEXEC)
    try:
        status, out, err = bounded([
            '/usr/bin/unshare', '--mount', '--pid', '--fork', '--kill-child=KILL',
            '--net', '--mount-proc', '--propagation', 'private',
            '/usr/bin/python3', str(Path(__file__).resolve()),
            *sys.argv[1:], '--namespace-fd', str(witness),
        ], timeout=900, limit=16 * 1024 * 1024, pass_fds=(witness,))
        sys.stdout.buffer.write(out)
        sys.stderr.buffer.write(err)
        return status
    finally:
        os.close(witness)


if __name__ == '__main__':
    sys.exit(main())
