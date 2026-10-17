"""Run signed runtime ELF helpers through their own bundled loader at package time.

The installed account consumer check (tilo-prepared-setup) applies the same
interpreter rules on the target image; this is the capture packaging side.
"""
import json
import platform
import struct
import subprocess


def require(condition, message):
    if not condition:
        raise ValueError(message)


def invoke(command, env):
    completed = subprocess.run(command, env=env, check=True, capture_output=True, timeout=60)
    require(len(completed.stdout) <= 4096 and len(completed.stderr) <= 4096,
            'capability command output exceeded its contract')
    return completed.stdout


def unique(items):
    result = {}
    for key, value in items:
        require(key not in result, 'duplicate capability response key')
        result[key] = value
    return result


def protocol(output):
    value = json.loads(output, object_pairs_hook=unique)
    require(isinstance(value, dict) and set(value) == {'schema', 'protected_grant_protocol'}
            and type(value['schema']) is int and value['schema'] == 1
            and type(value['protected_grant_protocol']) is int
            and value['protected_grant_protocol'] == 1, 'installed account consumer lacks protocol 1')


def interpreter(probe):
    """The one canonical ELF interpreter, as a path inside the runtime bundle."""
    with probe.open('rb') as source:
        header = source.read(64)
        require(len(header) == 64 and header[:6] == b'\x7fELF\x02\x01',
                'capability probe is not a little-endian ELF64 binary')
        machine = struct.unpack_from('<H', header, 18)[0]
        require(machine == {'x86_64': 62, 'aarch64': 183}.get(platform.machine()),
                'capability probe architecture differs from target image')
        offset = struct.unpack_from('<Q', header, 32)[0]
        size, count = struct.unpack_from('<HH', header, 54)
        require(size == 56 and 0 < count <= 128, 'invalid probe ELF program table')
        matches = []
        for index in range(count):
            source.seek(offset + index * size)
            entry = source.read(size)
            require(len(entry) == size, 'truncated probe ELF program table')
            if struct.unpack_from('<I', entry)[0] != 3:
                continue
            start = struct.unpack_from('<Q', entry, 8)[0]
            length = struct.unpack_from('<Q', entry, 32)[0]
            require(1 < length <= 4096, 'invalid probe ELF interpreter')
            source.seek(start)
            name = source.read(length)
            require(len(name) == length and name[-1:] == b'\0' and b'\0' not in name[:-1],
                    'invalid probe ELF interpreter name')
            matches.append(name[:-1].decode('ascii'))
        require(len(matches) == 1, 'probe must have one canonical ELF interpreter')
        name = matches[0]
        require(name.startswith('/') and all(part not in ('', '.', '..') for part in name[1:].split('/')),
                'unsafe probe ELF interpreter path')
        return 'lib/' + name[1:]
