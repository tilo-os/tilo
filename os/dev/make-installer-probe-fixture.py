#!/usr/bin/env python3
"""Pack original CI boot inputs for read-only VM helper diagnostics, never admission."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile


def create(root, output, digest, promotion=None):
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
    if promotion is not None:
        if type(promotion) is not dict or not re.fullmatch('[0-9a-f]{128}', promotion.get('composefs_digest', '')) or not all(
                type(promotion.get(key)) is str and 0 < len(promotion[key]) <= 16384
                for key in ('origin', 'loader', 'update_origin')):
            raise ValueError('invalid promotion fixture')
        manifest['promotion']=promotion
    raw=json.dumps(manifest,sort_keys=True).encode()
    if len(raw)>16384: raise ValueError('fixture manifest exceeds metadata bound')
    with output.open('xb') as stream, tarfile.open(fileobj=stream,mode='w') as archive:
        entry=tarfile.TarInfo('fixture.json'); entry.size=len(raw)
        archive.addfile(entry,io.BytesIO(raw))
        for name in files: archive.add(root/name,arcname=name,recursive=False)
    return manifest


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-digest',required=True)
    parser.add_argument('--promotion-origin',type=Path)
    parser.add_argument('--promotion-loader',type=Path)
    parser.add_argument('--composefs-digest')
    parser.add_argument('--update-origin')
    args=parser.parse_args()
    values=(args.promotion_origin,args.promotion_loader,args.composefs_digest,args.update_origin)
    promotion=None
    if any(values):
        if not all(values) or not re.fullmatch('[0-9a-f]{128}',args.composefs_digest):
            parser.error('promotion requires origin, loader, composefs digest and update origin')
        if any(path.stat().st_size>16384 or path.is_symlink() for path in values[:2]):
            parser.error('unsafe promotion fixture metadata')
        promotion={'origin':args.promotion_origin.read_text(),'loader':args.promotion_loader.read_text(),
                   'composefs_digest':args.composefs_digest,'update_origin':args.update_origin}
    print(json.dumps(create(args.root,args.output,args.target_digest,promotion),sort_keys=True))
