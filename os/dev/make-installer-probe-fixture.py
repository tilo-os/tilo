#!/usr/bin/env python3
"""Pack original CI boot inputs for read-only VM helper diagnostics, never admission."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile


def create(root, output, digest):
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', digest):
        raise ValueError('a pinned OCI digest is required')
    files = {}
    for source in sorted(root.rglob('*')):
        if source.is_symlink(): raise ValueError('fixture must not contain symlinks')
        if source.is_file():
            if source.relative_to(root).as_posix() == 'fixture.json': raise ValueError('reserved fixture manifest path')
            if not 0 < source.stat().st_size <= 256*1024*1024: raise ValueError('fixture file outside size bound')
            files[source.relative_to(root).as_posix()] = hashlib.sha256(source.read_bytes()).hexdigest()
    if not 1 <= len(files) <= 15 or sum((root/name).stat().st_size for name in files) > 512*1024*1024:
        raise ValueError('fixture exceeds file/total size bound')
    manifest = {'schema':'tilo.installer-code-boot-fixture.v1','physical_admission':False,
                'source_kind':'operator-supplied-original-ci-inputs','target_digest':digest,'files':files}
    with output.open('xb') as stream, tarfile.open(fileobj=stream,mode='w') as archive:
        raw=json.dumps(manifest,sort_keys=True).encode(); entry=tarfile.TarInfo('fixture.json'); entry.size=len(raw)
        archive.addfile(entry,io.BytesIO(raw))
        for name in files: archive.add(root/name,arcname=name,recursive=False)
    return manifest


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-digest',required=True)
    args=parser.parse_args()
    print(json.dumps(create(args.root,args.output,args.target_digest),sort_keys=True))
