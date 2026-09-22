"""Recheck downloaded x86 producer evidence; no network or executable invocation.

Run with python3 -I verify.py DOWNLOAD_DIRECTORY FRESH_OUTPUT_DIRECTORY.
The input directory contains artifact.json, run.json, jobs.json and artifact.zip.
"""
import hashlib
import gzip
import json
from pathlib import Path
import stat
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[4]
sys.path.append(str(ROOT / 'tools'))
import canonical_verifier_bundle as bundle

SOURCE = 'a987e76bac313e27f11c6f67d4c10779b2c3909f'
BUILDER = 'code.sandbox.example/qos/tilo-builder@sha256:027e43c051f1f34ce9ac1539172af0e711958243115e2efd08901ffa58729c5a'
ARCHIVE_SHA = '10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde'


def require(value, message):
    if not value:
        raise ValueError(message)


def verify(download, output):
    artifact, run, jobs = [json.loads((download / name).read_text())
                           for name in ('artifact.json', 'run.json', 'jobs.json')]
    require(artifact['id'] == 38736 and artifact['run_id'] == 23012
            and artifact['name'] == 'canonical-verifiers-x86_64-' + SOURCE
            and artifact['expired'] is False, 'wrong artifact identity')
    require(run['id'] == 23012 and run['commit_sha'] == SOURCE
            and run['workflow_id'] == 'canonical-verifier-bundle.yml'
            and run['repository']['full_name'] == 'qos/qos', 'wrong producing run')
    job = next(j for j in jobs if j['id'] == 84455)
    require(job['run_id'] == 23012 and job['repo_id'] == 23 and job['status'] == 'success'
            and job['attempt'] == 1 and job['task_id'] == 75605, 'x86 attempt not successful')
    require(not output.exists(), 'fresh output directory required')
    output.mkdir(parents=True)
    archive = download / 'artifact.zip'
    require(archive.stat().st_size < 16 * 1024**2, 'ZIP exceeds bound')
    with zipfile.ZipFile(archive) as zipped:
        members = zipped.infolist()
        require(len(members) == 2 and {m.filename for m in members} == {
            'x86_64.tar.gz', 'x86_64.tar.descriptor.json'}, 'unexpected or duplicate ZIP inventory')
        require(sum(m.file_size for m in members) == artifact['size_in_bytes'], 'artifact member size mismatch')
        for member in members:
            require(not member.is_dir() and not member.flag_bits & 1
                    and stat.S_IFMT(member.external_attr >> 16) in (0, stat.S_IFREG)
                    and member.file_size < 16 * 1024**2, 'unsafe ZIP member')
            (output / member.filename).write_bytes(zipped.read(member))
    descriptor = json.loads((output / 'x86_64.tar.descriptor.json').read_text())
    require(descriptor['source_sha'] == SOURCE and descriptor['compatible_builder'] == BUILDER
            and descriptor['sha256'] == ARCHIVE_SHA, 'source/builder/archive identity mismatch')
    require(descriptor['url'] == 'https://code.sandbox.example/api/packages/qos/generic/tilo-canonical-verifiers/'
            + ARCHIVE_SHA + '/x86_64.tar.gz', 'wrong intended content-addressed URL')
    frozen = subprocess.check_output(['git', '--work-tree=' + str(ROOT), '-C', str(ROOT),
                                      'show', SOURCE + ':tools/canonical_app_verifier.py'])
    require(hashlib.sha256(frozen).hexdigest() == descriptor['verifier_sha256'], 'adapter differs from committed source')
    require((ROOT / 'tools/canonical_app_verifier.py').read_bytes() == frozen, 'local repackager adapter differs')
    installed = bundle.materialize(output / 'x86_64.tar.gz', descriptor, output / 'materialized', BUILDER, 'x86_64')
    require((installed / 'canonical_app_verifier.py').read_bytes() == frozen, 'materialized adapter differs')
    repeated = bundle.produce(installed / 'tilo-app-package', installed / 'tilo-runtime-bundle',
                              output / 'repacked.tar.gz', BUILDER, SOURCE, 'x86_64')
    again = bundle.produce(installed / 'tilo-app-package', installed / 'tilo-runtime-bundle',
                           output / 'repacked-again.tar.gz', BUILDER, SOURCE, 'x86_64')
    repacked = (output / 'repacked.tar.gz').read_bytes()
    original = (output / 'x86_64.tar.gz').read_bytes()
    require(repeated == again and repacked == (output / 'repacked-again.tar.gz').read_bytes(),
            'local repeated bundle bytes differ')
    # Different zlib versions need not produce identical compressed bytes.
    # Preserve the downloaded immutable digest, compare the exact tar stream,
    # and report compression differences rather than silently changing identity.
    require(gzip.decompress(repacked) == gzip.decompress(original), 'repacked tar stream differs')
    receipt = {
        'artifact_id': 38736, 'api_run_id': 23012, 'ui_run_url': run['html_url'],
        'job_id': 84455, 'attempt': 1, 'task_id': 75605, 'source_sha': SOURCE,
        'architecture': 'x86_64', 'artifact_member_bytes': artifact['size_in_bytes'],
        'download_zip_bytes': archive.stat().st_size,
        'download_zip_sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
        'descriptor': descriptor, 'expires_at': artifact['expires_at'],
        'checks': ['exact ZIP inventory and sizes', 'archive digest and size',
                   'bounded consumer materialization', 'both ELF64 x86-64 identities',
                   'both canonical executable hashes', 'frozen adapter equals committed source',
                   'byte-identical independent tar stream', 'byte-identical repeated local compression'],
        'cross_platform_compressed_bytes_identical': repacked == original,
        'local_native_execution': False, 'arm_qualification': False,
        'registry_published': False, 'policy_activated': False,
    }
    (output / 'verification.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    verify(Path(sys.argv[1]), Path(sys.argv[2]))
