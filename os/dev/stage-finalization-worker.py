#!/usr/bin/python3
"""Stage bounded public finalization inputs; release the pinned worker only last.

No signing authorization is inferred from this handshake. The retained artifact
is authenticated by the finalizer before its first signing operation.
"""
import argparse
import base64
import ctypes
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import signing_spool as spool


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


driver = module('stage_driver', 'finalize-development-rootfs.py')
transport = module('stage_transport', 'transfer-signing-spool.py')
trust = module('stage_trust', 'development-trust.py')
importer = module('stage_importer', 'import-development-rootfs.py')
LIMIT = 1024 * 1024
BRIDGE = Path('/var/lib/tilo-signing-bridge')
PUBLIC = (*trust.FILES, 'manifest.json')
CODE = ('remote-development-signer.py', 'signing_spool.py', 'development-signer.py',
        'signing-spool-endpoint.py')


def parse(raw):
    return json.loads(raw, object_pairs_hook=spool.unique)


def checked_bundle(files, expected):
    if set(files) != set(PUBLIC) or any(not 0 < len(v) <= 65536 for v in files.values()):
        raise ValueError('public bundle file admission failed')
    manifest = parse(files['manifest.json'])
    if (spool.digest(files['manifest.json']) != expected or
            manifest != {'schema': 1, 'purpose': 'tilo-development',
                         'sha256': {n: spool.digest(files[n]) for n in trust.FILES}}):
        raise ValueError('public manifest binding differs')
    return trust.identities({n: files[n] for n in trust.FILES})


def validate(packet, request_raw):
    if set(packet) != {'schema', 'request_sha256', 'context', 'files'} or packet['schema'] != 1:
        raise ValueError('staging packet schema differs')
    if spool.digest(request_raw) != packet['request_sha256']:
        raise ValueError('worker request bytes differ')
    request = driver.validate_request(parse(request_raw))
    fixed = {'source_dir': '/work/source', 'receipt': '/work/inputs/retained-rootfs.json',
             'public_bundle': '/work/inputs/original-public',
             'new_public_bundle': '/work/inputs/new-public',
             'signer': str(BRIDGE / 'remote-development-signer.py')}
    if any(request[k] != v for k, v in fixed.items()):
        raise ValueError('staging requires the fixed worker paths')
    context = packet['context']
    spool.context(context)
    for key, expected in {'revision': request['source_revision'], 'architecture': request['architecture'],
                          'manifest_sha256': request['new_manifest_sha256'], 'target': request['new_target']}.items():
        if context[key] != expected:
            raise ValueError('bridge context differs from finalization request')
    expected_names = {'retained-rootfs.json'} | {f'{d}/{n}' for d in ('original-public', 'new-public') for n in PUBLIC}
    if set(packet['files']) != expected_names:
        raise ValueError('unexpected staging files')
    files = {name: base64.b64decode(data, validate=True) for name, data in packet['files'].items()}
    if any(not 0 < len(data) <= 65536 for data in files.values()):
        raise ValueError('staging file exceeds admission')
    if spool.digest(files['retained-rootfs.json']) != request['receipt_sha256']:
        raise ValueError('retained receipt differs')
    importer.retained_identity(parse(files['retained-rootfs.json']), SimpleNamespace(
        revision=request['source_revision'], architecture=request['architecture'],
        manifest_sha256=request['manifest_sha256'], target=request['target'],
        generation=request['generation'], source_date_epoch=request['source_date_epoch']))
    for directory, digest in [('original-public', request['manifest_sha256']), ('new-public', request['new_manifest_sha256'])]:
        identities = checked_bundle({n: files[f'{directory}/{n}'] for n in PUBLIC}, digest)
        if directory == 'new-public' and identities['registry-owner.pub'] != request['new_owner']:
            raise ValueError('new owner differs from public bundle')
    return request, files


def bound_context(context, binding):
    transport.validate_binding(binding)
    if 'docker' not in binding or binding['endpoint'] != str(BRIDGE / 'signing-spool-endpoint.py'):
        raise ValueError('finalization requires the pinned nested worker')
    if (context['pod_uid'] != binding['pod_uid'] or context['container_id'] != binding['docker']['id'] or
            context['image_id'] != binding['docker']['image_id'] or spool.context(context) != binding['context_sha256']):
        raise ValueError('staging context differs from pinned transport identity')


def remote_call(client, operation, data):
    client.check()
    argv = client.exec_prefix() + ['docker', 'exec', '-i', client.binding['docker']['id'],
        '/usr/bin/python3', '/work/helpers/os/dev/stage-finalization-worker.py', operation]
    with tempfile.TemporaryFile() as source:
        source.write(data)
        source.seek(0)
        output = io.BytesIO()
        client.runner(argv, output, spool.MAX_JSON, source=source, timeout=90)
    client.check()
    return parse(output.getvalue())


def stage(client, packet, request_raw):
    """Use an existing authenticated Transport, including its API runner adapter."""
    bound_context(packet['context'], client.binding)
    validate(packet, request_raw)
    data = spool.canonical(packet)
    if len(data) > LIMIT:
        raise ValueError('staging packet exceeds admission')
    expected = {'schema': 1, 'request_sha256': packet['request_sha256'],
                'context_sha256': client.binding['context_sha256'],
                'packet_sha256': spool.digest(data), 'ready': False}
    if remote_call(client, 'receive', data) != expected:
        raise ValueError('remote staging receipt differs')
    result = remote_call(client, 'release', spool.canonical(expected))
    if result != {**expected, 'ready': True}:
        raise ValueError('operator gate response differs')
    return result


def receive(packet, work=Path('/work'), bridge=BRIDGE):
    spool.signer.trusted(work, directory=True)
    request, files = validate(packet, spool.snapshot(work / 'finalization-request.json', spool.MAX_JSON))
    helpers = work / 'helpers'
    if spool.document(helpers / '.finalization-helpers.json') != {'revision': request['helpers_revision']}:
        raise ValueError('helper revision differs')
    if spool.document(work / 'source/.finalization-source.json') != {'revision': request['source_revision']}:
        raise ValueError('source revision differs')
    # Exclusive directory reservation makes partial staging fail closed. Never
    # repair or adopt a partial previous handshake automatically.
    staging = work / '.operator-staging'
    staging.mkdir(mode=0o700)
    inputs = work / 'inputs'
    inputs.mkdir(mode=0o700)
    for name in ('original-public', 'new-public'):
        (inputs / name).mkdir(mode=0o700)
    for name, data in files.items():
        spool.write_new(inputs / name, data)
    spool.signer.trusted(bridge.parent, directory=True)
    bridge.mkdir(mode=0o700)
    (bridge / 'requests').mkdir(mode=0o700)
    for name in CODE:
        source = spool.signer.trusted(helpers / 'os/dev' / name)
        spool.write_new(bridge / name, spool.snapshot(source, LIMIT))
        os.chmod(bridge / name, 0o700)
    cert = files['new-public/efi-signing.crt']
    spool.write_new(bridge / 'efi-signing.crt', cert)
    config = {'schema': 1, 'context': packet['context'], 'spool': str(bridge / 'requests'),
              'public_certificate': str(bridge / 'efi-signing.crt'),
              'certificate_sha256': spool.digest(cert), 'timeout_seconds': 1800}
    spool.write_new(bridge / 'bridge.json', spool.canonical(config))
    receipt = {'schema': 1, 'request_sha256': packet['request_sha256'],
               'context_sha256': spool.context(packet['context']), 'packet_sha256': spool.digest(spool.canonical(packet)),
               'ready': False}
    spool.publish(staging / 'receipt.json', spool.canonical(receipt))
    return receipt


def release(receipt, work=Path('/work')):
    staging = spool.private_directory(work / '.operator-staging')
    if spool.document(staging / 'receipt.json') != receipt or receipt['ready'] is not False:
        raise ValueError('staging receipt differs')
    if spool.digest(spool.snapshot(work / 'finalization-request.json', spool.MAX_JSON)) != receipt['request_sha256']:
        raise ValueError('worker request changed')
    spool.write_new(staging / 'ready', receipt['request_sha256'].encode())
    # Atomic no-overwrite publication from a private directory onto trusted /work.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.renameat2(-100, os.fsencode(staging / 'ready'), -100, os.fsencode(work / 'operator-ready'), 1):
        raise OSError(ctypes.get_errno(), 'operator gate publication rejected')
    spool.sync_directory(work)
    return {**receipt, 'ready': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('stage', 'receive', 'release'))
    for name in ('context', 'binding', 'request', 'signing-context', 'receipt', 'original-public', 'new-public'):
        parser.add_argument('--' + name)
    args = parser.parse_args()
    if args.operation != 'stage':
        raw = sys.stdin.buffer.read(LIMIT + 1)
        if len(raw) > LIMIT:
            raise ValueError('staging stream exceeds admission')
        result = (receive if args.operation == 'receive' else release)(parse(raw))
    else:
        request_raw = spool.snapshot(spool.signer.trusted(Path(args.request)), spool.MAX_JSON)
        context = spool.document(spool.signer.trusted(Path(args.signing_context)))
        binding = spool.document(spool.signer.trusted(Path(args.binding)))
        bound_context(context, binding)
        files = {'retained-rootfs.json': spool.snapshot(spool.signer.trusted(Path(args.receipt)), spool.MAX_JSON)}
        for directory, original in [('original-public', args.original_public), ('new-public', args.new_public)]:
            for name in PUBLIC:
                files[f'{directory}/{name}'] = spool.snapshot(spool.signer.trusted(Path(original) / name), 65536)
        packet = {'schema': 1, 'request_sha256': spool.digest(request_raw), 'context': context,
                  'files': {name: base64.b64encode(data).decode() for name, data in files.items()}}
        client = transport.Transport(args.context, binding)
        result = stage(client, packet, request_raw)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, TypeError, KeyError):
        sys.exit('operator staging rejected; no signing authorization or automatic retry')
