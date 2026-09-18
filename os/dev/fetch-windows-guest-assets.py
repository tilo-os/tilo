#!/usr/bin/env python3
"""Fetch only the pinned DXVK archive/license needed for guest assembly."""
import argparse
import hashlib
import importlib.util
from pathlib import Path
import re
import os
import tempfile
import urllib.parse
import urllib.request

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('dxvk_installer', HERE / 'install-windows-guest-dxvk.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
NAMES = (installer.ARCHIVE, installer.LICENSE)
MANIFEST = HERE.parent / 'external-assets/github-assets.tsv'
LIMIT = 128 * 1024 * 1024


def pins(manifest):
    rows = [line.split('\t') for line in manifest.read_text().splitlines()
            if line and not line.startswith('#')]
    result = []
    for name in NAMES:
        matches = [row for row in rows if len(row) >= 2 and row[1] == name]
        if len(matches) != 1 or len(matches[0]) != 3:
            raise ValueError(f'expected exactly one three-field asset pin: {name}')
        checksum, _, url = matches[0]
        parsed = urllib.parse.urlsplit(url)
        if (not re.fullmatch(r'[0-9a-f]{64}', checksum) or parsed.scheme != 'https'
                or not parsed.hostname or parsed.username is not None or parsed.password is not None):
            raise ValueError(f'invalid asset pin: {name}')
        result.append((checksum, name, url))
    return result


def verified(path, checksum):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > LIMIT:
        return False
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest() == checksum


def fetch(output, manifest=MANIFEST):
    selected = pins(manifest)
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    for checksum, name, url in selected:
        target = output / name
        if verified(target, checksum):
            continue
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=output, prefix=f'.{name}.', delete=False) as stream:
                temporary = Path(stream.name)
                digest = hashlib.sha256()
                size = 0
                with urllib.request.urlopen(url, timeout=60) as response:
                    while chunk := response.read(1024 * 1024):
                        size += len(chunk)
                        if size > LIMIT:
                            raise ValueError(f'asset exceeds size limit: {name}')
                        digest.update(chunk)
                        stream.write(chunk)
                if digest.hexdigest() != checksum:
                    raise ValueError(f'asset checksum mismatch: {name}')
                stream.flush()
                os.fsync(stream.fileno())
            temporary.chmod(0o644)
            os.replace(temporary, target)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    try:
        fetch(args.output)
    except (OSError, ValueError) as error:
        parser.exit(1, f'guest asset preparation failed: {error}\n')
    print('Verified guest assets: ' + ', '.join(NAMES))


if __name__ == '__main__':
    main()
