"""Offline reproduction of explicitly approved cache-assisted reconstruction.

No registry, payload import, timestamp write, or compiler operation occurs here.
The caller reads each independent input through its exact-byte pinning reader.
"""
import json
import re
import types

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
PRODUCER = '9d7a6b259c9b75b6c5031472bfed05b6461c1893'


def reproduce_extensions(api, checkpoints, required, resume, material, inputs):
    require, sha, canonical = api.require, api.sha, api.canonical
    require(set(inputs) == {'partial_admission', 'partial_metadata', 'thin_admission',
                           'thin_receipt', 'source_metadata'}, 'independent extension inputs differ')
    for key in ('partial_admission', 'thin_admission', 'thin_receipt', 'source_metadata'):
        require(re.fullmatch('[0-9a-f]{64}', inputs[key][0]), 'independent extension pin required')
    partial_sha, partial = inputs['partial_admission']
    legacy = [c for c in checkpoints if c['receipt']['purpose'] == resume.PARTIAL]
    newer = [c for c in checkpoints if c['receipt']['purpose'] != resume.PARTIAL]
    require(legacy and newer and {c['receipt_sha256'] for c in newer} == set(partial['receipts'])
            == set(inputs['partial_metadata']), 'partial admission/metadata set differs')
    union = api.checkpoint_union(legacy, required)
    legacy_union = dict(union)
    native = types.SimpleNamespace(REQUIRED=required, tools=types.SimpleNamespace(
        sha=sha, canonical=canonical, strict_json=json.loads))
    class SourceBlob:
        # The producer reads UTF-8 Linux source. Reproduce that from the already
        # checked Git blobs without Windows locale/newline transformations.
        def __init__(self, path):
            self.path = str(path)
        def __truediv__(self, component):
            return SourceBlob(self.path.rstrip('/') + '/' + component)
        def read_text(self):
            suffix = self.path.removeprefix(str(material['source_root']).rstrip('/') + '/')
            require(suffix in ('os/Containerfile', 'os/dev/build-image.sh'), 'unexpected source blob access')
            return material['recipe' if suffix == 'os/Containerfile' else 'driver'].decode('utf-8')
    validator = types.FunctionType(resume.validate_reconstructed_metadata.__code__,
        dict(resume.validate_reconstructed_metadata.__globals__, Path=SourceBlob))
    for checkpoint in newer:
        receipt = checkpoint['receipt']
        require(re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-partial-native-aarch64@sha256:[0-9a-f]{64}',
                             receipt['reference']) and re.fullmatch('sha256:[0-9a-f]{64}', receipt['config_digest']),
                'partial immutable identity differs')
        # Reuse the independently pinned producer's depth-one source/recipe
        # derivation validator; actual payload verification remains the consumer.
        validator(native, receipt,
            inputs['partial_metadata'][checkpoint['receipt_sha256']], partial,
            checkpoint['receipt_sha256'], material['source_root'], legacy, legacy_union)
        for name, record in receipt['inventory'].items():
            require(name in set(required) | {'browser'} and set(record) == {'size', 'sha256'}
                    and type(record['size']) is int and 0 < record['size'] <= 4 * 1024**3
                    and re.fullmatch('[0-9a-f]{64}', record['sha256']), 'partial inventory differs')
            require(name not in union or union[name] == record, 'overlapping partial inventory differs')
            union[name] = record

    cache_sha, cache = inputs['thin_admission']
    _, receipt = inputs['thin_receipt']
    source_sha, source = inputs['source_metadata']
    require(cache['purpose'] == 'tilo-incomplete-thin-cache-admission'
            and cache['source_revision'] == SOURCE and cache['producer_helper_revision'] == PRODUCER
            and cache['restore_helper_sha256'] == sha(material['restore_helper'])
            and cache['source_metadata_sha256'] == source_sha
            and cache['cargo_revalidation_required'] is True and cache['incomplete_cache_reviewed'] is True,
            'cache admission identity differs')
    for prefix, repository in (('checkpoint', 'partial-thin-cache'), ('source_metadata', 'thin-cache-metadata')):
        require(re.fullmatch('code\\.podesta\\.ai/qos/tilo-development-' + repository +
                             '-aarch64@sha256:[0-9a-f]{64}', cache[prefix + '_reference'])
                and re.fullmatch('sha256:[0-9a-f]{64}', cache[prefix + '_config_digest']),
                'cache immutable identity differs')
    require(receipt['purpose'] == 'tilo-development-incomplete-thin-cargo-cache'
            and all(receipt[k] is False for k in ('resume_admitted', 'installable', 'complete'))
            and receipt['reference'] == cache['checkpoint_reference']
            and receipt['config_digest'] == cache['checkpoint_config_digest']
            and receipt['archive_sha256'] == cache['archive_sha256']
            and receipt['metadata_sha256'] == cache['metadata_sha256'], 'cache receipt differs')
    require(type(receipt['files']) is int and 0 < receipt['files'] <= 30000
            and type(receipt['retained_bytes']) is int and 0 < receipt['retained_bytes'] <= 10 * 1024**3,
            'cache receipt bounds differ')
    require(source['purpose'] == 'incomplete-thin-cargo-source-metadata'
            and source['source_revision'] == SOURCE and source['build_root'] == '/build'
            and 0 < len(source['tracked_files']) <= 20000
            and sha(('\n'.join(source['rpm_nevras']) + '\n').encode()) == source['rpm_nevras_sha256'],
            'source metadata identity differs')
    runtime = dict(purpose='tilo-incomplete-thin-cache-restored', approval_sha256=cache_sha,
        checkpoint_reference=cache['checkpoint_reference'], archive_sha256=cache['archive_sha256'],
        source_metadata_sha256=source_sha, metadata_sha256=cache['metadata_sha256'],
        restored_files=receipt['files'], restored_bytes=receipt['retained_bytes'],
        verified_source_timestamps_restored=len(source['tracked_files']), dependency_timestamps_restored=False,
        inner_tool_executables_verified=True, rpm_inventory_sha256=source['rpm_nevras_sha256'],
        cargo_revalidation_required=True, cargo_freshness_proven=False, complete=False, installable=False)
    # The reviewed restore helper writes this exact public JSON serialization.
    runtime_sha = sha((json.dumps(runtime, sort_keys=True, indent=2) + '\n').encode())
    thin = dict(approval_sha256=cache_sha, approval=cache, purpose='tilo-admitted-incomplete-thin-cache',
        cargo_revalidation_required=True, dependency_timestamps_restored=False, complete=False, installable=False,
        runtime_evidence=runtime, runtime_evidence_sha256=runtime_sha)
    extra = dict(reconstructed_partial_admission_sha256=partial_sha,
                 reconstructed_partial_admission=partial, thin_cache=thin)
    return union, extra, {'thin_cache_approval_sha256': cache_sha}
