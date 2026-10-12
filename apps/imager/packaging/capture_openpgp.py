"""Closed signed helper table from the canonical trusted-builder GPGME probe."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

ROLES = {'openpgp', 'gpgconf', 'agent', 'keyboxd', 'ostree'}
LIMIT = 64 * 1024 * 1024


def require(value, message):
    if not value:
        raise ValueError(message)


def parse_probe(raw):
    require(len(raw) <= 32 * 1024, 'GPGME probe exceeds bound')
    result = {}
    for line in raw.decode('utf-8', 'strict').splitlines():
        role, value = line.split('\t')
        require(role in ROLES | {'version', 'libexecdir'} and role not in result
                and value and len(value.encode()) <= 4096
                and all(ord(c) >= 32 and ord(c) != 127 for c in value),
                'Invalid or duplicate GPGME engine field')
        result[role] = value
    require({'openpgp', 'gpgconf', 'agent', 'version', 'libexecdir'} <= result.keys(),
            'GPGME lacks its configured OpenPGP helpers')
    return result


def helper(role, target):
    require(role in ROLES, 'Unrecognized helper role')
    path = Path(target)
    require(path.is_absolute() and str(path) == target
            and all(part not in ('.', '..') for part in path.parts),
            'Helper path must be canonical absolute')
    # Builder aliases are resolved explicitly, never projected as ambient links.
    source = path.resolve(strict=True)
    info = source.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_mode & 0o111
            and not info.st_mode & 0o022 and 0 < info.st_size <= LIMIT,
            'Helper must be a bounded controlled executable')
    data = source.read_bytes()
    require(len(data) == info.st_size and data[:4] == b'\x7fELF', 'Helper is not a native ELF')
    after = source.lstat()
    require((info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
            == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
            'Builder helper changed during discovery')
    return {'role': role, 'target': target, 'source': str(source),
            'bundle': 'bin/' + source.name, 'sha256': hashlib.sha256(data).hexdigest()}


def describe(raw):
    values = parse_probe(raw)
    require(len(values['version'].encode()) <= 128
            and values['libexecdir'].startswith('/')
            and str(Path(values['libexecdir'])) == values['libexecdir']
            and all(part not in ('.', '..') for part in Path(values['libexecdir']).parts),
            'Unsupported configured engine version or libexec namespace')
    for role in ROLES - {'ostree'}:
        if role in values:
            require(values[role].startswith(('/usr/bin/', '/usr/libexec/')),
                    'Configured GPGME helper is outside supported signed target namespaces')
    entries = [helper('ostree', '/usr/bin/ostree')]
    entries.extend(helper(role, values[role]) for role in sorted(ROLES - {'ostree'}) if role in values)
    names = {}
    for entry in entries:
        prior = names.setdefault(entry['bundle'], entry['sha256'])
        require(prior == entry['sha256'], 'Different helpers collide in signed bundle')
    require(len({item['target'] for item in entries}) == len(entries), 'Helper target collision')
    # Source paths are build input only; runtime admission sees no ambient source.
    manifest = {'schema': 1, 'openpgp_version': values['version'],
                'libexecdir': values['libexecdir'],
                'helpers': [{k: v for k, v in item.items() if k not in {'source', 'sha256'}} for item in entries]}
    return manifest, sorted({entry['source'] for entry in entries})


def main():
    probe, output, sources = map(Path, sys.argv[1:])
    completed = subprocess.run([str(probe)], env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'},
                               capture_output=True, timeout=30, check=True)
    require(len(completed.stderr) <= 32 * 1024, 'GPGME probe diagnostic exceeds bound')
    manifest, paths = describe(completed.stdout)
    output.write_text(json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n')
    sources.write_text('\n'.join(paths) + '\n')


if __name__ == '__main__':
    main()
