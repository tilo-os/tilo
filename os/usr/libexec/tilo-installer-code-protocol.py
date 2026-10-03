#!/usr/bin/env python3
"""Bounded, named-file protocol for disposable installer VM code refresh."""
import base64
import hashlib
import json
from pathlib import Path
import stat
import struct

SCHEMA = 'tilo.installer-code-refresh.v1'
LIMIT = 2 * 1024 * 1024
ALLOWLIST = frozenset({
    'usr/libexec/tilo-install',
    'usr/libexec/tilo-installer-offline-target.py',
    'usr/libexec/tilo-installer-dev-target-uki.py',
    'usr/libexec/tilo-install-dev-ssh.py',
    'usr/libexec/tilo-install-handoff-copy.py',
    'usr/libexec/tilo-boot-mount-contract',
})


def require(ok, message):
    if not ok:
        raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode()


def read_exact(stream, count):
    output = bytearray()
    while len(output) < count:
        data = stream.read(count - len(output))
        require(data, 'code refresh stream ended early')
        output.extend(data)
    return bytes(output)


def receive(stream):
    size = struct.unpack('!I', read_exact(stream, 4))[0]
    require(0 < size <= LIMIT, 'code refresh packet exceeds its bound')
    return validate(read_exact(stream, size))


def validate(raw):
    require(0 < len(raw) <= LIMIT, 'code refresh packet exceeds its bound')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate code refresh field')
            result[key] = value
        return result
    packet = json.loads(raw, object_pairs_hook=unique)
    require(type(packet) is dict and set(packet) == {'schema', 'files', 'source'}
            and packet['schema'] == SCHEMA and type(packet['source']) is dict
            and set(packet['source']) == {'revision', 'dirty'}
            and type(packet['source']['revision']) is str and len(packet['source']['revision']) == 40
            and all(char in '0123456789abcdef' for char in packet['source']['revision'])
            and type(packet['source']['dirty']) is bool,
            'invalid code refresh metadata')
    require(type(packet['files']) is list and 0 < len(packet['files']) <= len(ALLOWLIST),
            'invalid code refresh file inventory')
    files, manifest = {}, []
    for row in packet['files']:
        require(type(row) is dict and set(row) == {'path', 'sha256', 'data'}
                and all(type(row[key]) is str for key in row)
                and row['path'] in ALLOWLIST and row['path'] not in files,
                'code refresh path is not uniquely allowlisted')
        data = base64.b64decode(row['data'], validate=True)
        require(0 < len(data) <= 512 * 1024 and b'\0' not in data
                and hashlib.sha256(data).hexdigest() == row['sha256'],
                'code refresh content differs from its bounded hash')
        files[row['path']] = data
        manifest.append({'path': row['path'], 'sha256': row['sha256'], 'bytes': len(data)})
    # Canonical representation binds every selected file and source receipt;
    # insignificant JSON formatting never changes the evidence identity.
    digest = hashlib.sha256(canonical(packet)).hexdigest()
    return files, {'schema': SCHEMA, 'sha256': digest, 'files': manifest, 'source': packet['source']}


def bundle(root, revision, dirty):
    root = Path(root).resolve(strict=True) / 'os'
    rows = []
    for relative in sorted(ALLOWLIST):
        path = root
        for part in Path(relative).parts:
            path = path / part
            require(not path.is_symlink(), 'code refresh source contains a symlink')
        if not path.exists():
            continue
        require(stat.S_ISREG(path.stat().st_mode), 'code refresh source is not a regular file')
        data = path.read_bytes()
        require(b"\r" not in data, "installer helper must use LF line endings (configure Git checkout)")
        rows.append({'path': relative, 'sha256': hashlib.sha256(data).hexdigest(),
                     'data': base64.b64encode(data).decode()})
    raw = canonical({'schema': SCHEMA, 'files': rows,
                     'source': {'revision': revision, 'dirty': dirty}})
    validate(raw)
    return raw


def write_all(stream, data):
    remaining = memoryview(data)
    while remaining:
        count = stream.write(remaining)
        require(count is not None and count > 0, 'code refresh stream stopped accepting data')
        remaining = remaining[count:]
    stream.flush()


def send(stream, raw):
    validate(raw)
    write_all(stream, struct.pack('!I', len(raw)) + raw)
