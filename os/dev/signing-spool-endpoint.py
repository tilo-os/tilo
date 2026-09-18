#!/usr/bin/python3
"""Fixed-file public signing-spool endpoint; no credentials or shell execution."""
import argparse
import json
from pathlib import Path
import re
import sys
import signing_spool as spool


def response_for(request, raw):
    value = json.loads(raw, object_pairs_hook=spool.unique)
    expected = {'schema', 'request_id', 'context_sha256', 'operation', 'input_sha256', 'output_sha256', 'output_bytes'}
    if (not isinstance(value, dict) or set(value) != expected
            or any(value[k] != request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256'))
            or not spool.HEX.fullmatch(value['output_sha256'])
            or type(value['output_bytes']) is not int or not 0 < value['output_bytes'] <= spool.MAX_INPUT + 65536):
        raise ValueError('response does not match exact remote request')
    return value


def execute(config_path, operation, nonce, response, input_stream, output_stream):
    config = spool.document(spool.signer.trusted(Path(config_path)))
    binding = spool.context(config['context'])
    root = spool.private_directory(config['spool'])
    if operation == 'list':
        entries = []
        for directory in sorted(root.iterdir()):
            if not re.fullmatch(r'[0-9a-f]{32}', directory.name):
                raise ValueError('unexpected spool directory')
            spool.private_directory(directory)
            if not (directory / 'request.json').exists() or (directory / 'consumed.json').exists():
                continue
            request = spool.request_metadata(spool.document(directory / 'request.json'))
            if request['context_sha256'] != binding or request['request_id'] != directory.name:
                raise ValueError('spool request context differs')
            entries.append(directory.name)
            if len(entries) > 64:
                raise ValueError('pending signing request count exceeds bound')
        output_stream.write(spool.canonical({'requests': entries}))
        return
    if not re.fullmatch(r'[0-9a-f]{32}', nonce or ''):
        raise ValueError('invalid request identifier')
    directory = spool.private_directory(root / nonce)
    request = spool.request_metadata(spool.document(directory / 'request.json'))
    if request['context_sha256'] != binding or request['request_id'] != nonce:
        raise ValueError('remote request context differs')
    if operation == 'request':
        output_stream.write(spool.canonical(request))
    elif operation == 'input':
        data = spool.snapshot(directory / 'input', spool.MAX_INPUT)
        if len(data) != request['input_bytes'] or spool.digest(data) != request['input_sha256']:
            raise ValueError('remote request input changed')
        output_stream.write(data)
    elif operation in ('put-output', 'publish-response'):
        if not response or len(response.encode()) > spool.MAX_JSON:
            raise ValueError('bounded response metadata required')
        value = response_for(request, response)
        if operation == 'put-output':
            data = input_stream.read(value['output_bytes'] + 1)
            if len(data) != value['output_bytes'] or spool.digest(data) != value['output_sha256']:
                raise ValueError('response stream differs from admitted bytes')
            spool.write_new(directory / 'output', data)
        else:
            data = spool.snapshot(directory / 'output', spool.MAX_INPUT + 65536)
            if len(data) != value['output_bytes'] or spool.digest(data) != value['output_sha256']:
                raise ValueError('remote response output changed')
            spool.publish(directory / 'response.json', spool.canonical(value))
        output_stream.write(spool.canonical({'request_id': nonce, 'operation': operation, 'accepted': True}))
    else:
        raise ValueError('unknown spool transfer operation')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('list', 'request', 'input', 'put-output', 'publish-response'))
    parser.add_argument('--request-id')
    parser.add_argument('--response')
    args = parser.parse_args()
    try:
        executable = spool.signer.trusted(Path(__file__).absolute())
        execute(executable.with_name('bridge.json'), args.operation, args.request_id,
                args.response, sys.stdin.buffer, sys.stdout.buffer)
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'signing spool transfer rejected\n')
