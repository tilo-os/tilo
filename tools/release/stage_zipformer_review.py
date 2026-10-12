#!/usr/bin/env python3
"""Explicit remote public-weight review staging; no system install or promotion.

Run only after reviewing the model card and all wheel license expressions.
Retain distribution licenses and verify every pinned public file before extraction.
"""
import hashlib
import json
from pathlib import Path, PurePosixPath
import stat
import urllib.request
import zipfile

ROOT = Path('/tmp/tilo-dictation/target/streaming-alternative-review-v1')
REVISION = 'd42f2d9f7ca24806fb667456a18a9f1b60f70d16'
MODEL_URL = 'https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-20M-2023-02-17/resolve/'+REVISION+'/'
WEIGHTS = [
    ('encoder-epoch-99-avg-1.int8.onnx', 42845182, '3810755ce7c3ab26b42a8bcf39d191308fa27fb0f53358823ba46141d03b7eb3'),
    ('decoder-epoch-99-avg-1.onnx', 2092272, '45a7f940ecfb53d89fa270ad11b88b961e53a317203eb24b1c8e95ed208b0f30'),
    ('joiner-epoch-99-avg-1.int8.onnx', 259572, 'e085d73b593cf9b0707f370dbd656d58327d3fe36d80d849202ef81df02cb01e'),
]
WHEELS = [
    ('sherpa-onnx', '1.13.8', 'sherpa_onnx-1.13.8-cp314-cp314-manylinux2014_x86_64.manylinux_2_17_x86_64.whl', 4403669, '97309e4b5d9850490085da61e35b0fca002a39ec9da93cd9e21d2d0dcc1cc62a', 'Apache licensed, as found in the LICENSE file'),
    ('sherpa-onnx-core', '1.13.8', 'sherpa_onnx_core-1.13.8-py3-none-manylinux2014_x86_64.whl', 10642497, '4da90acf435373d7b2ba9cc0be806e7e274d28f63f5780880b5dea6721626e36', 'Apache-2.0'),
    ('numpy', '2.4.2', 'numpy-2.4.2-cp314-cp314-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl', 16611738, 'bba37bc29d4d85761deed3954a1bc62be7cf462b9510b51d367b769a8c8df325', 'BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0'),
]


def fetch(url, size):
    with urllib.request.urlopen(url, timeout=60) as stream:
        data = stream.read(size+1)
    if len(data) != size:
        raise ValueError('bounded public size receipt mismatch')
    return data


def main():
    ROOT.mkdir(mode=0o700, exist_ok=True)
    card = (ROOT/'model-card.md').read_bytes()
    if (len(card) != 304 or hashlib.sha1(b'blob 304\0'+card).hexdigest() != 'ecd11453303959cac85223da2859373c8005aff6'
            or b'license: apache-2.0' not in card):
        raise ValueError('reviewed Apache model card required')
    model = ROOT/'model'; model.mkdir(exist_ok=True)
    site = ROOT/'site'; site.mkdir(exist_ok=True)
    receipts = {'model_revision': REVISION, 'language_coverage': ['en'], 'model_license': 'Apache-2.0', 'files': {}}
    for name, size, digest in WEIGHTS:
        data = fetch(MODEL_URL+name, size)
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError('public weight hash mismatch')
        (model/name).write_bytes(data); receipts['files'][name] = digest
    tokens = fetch(MODEL_URL+'tokens.txt', 5048)
    if hashlib.sha1(b'blob '+str(len(tokens)).encode()+b'\0'+tokens).hexdigest() != 'b4d1bf82e68de1354f2a3f5fd51094d61d65c77c':
        raise ValueError('pinned token git blob mismatch')
    (model/'tokens.txt').write_bytes(tokens)
    receipts['files']['tokens.txt'] = hashlib.sha256(tokens).hexdigest()
    for package, version, name, size, digest, license_expression in WHEELS:
        with urllib.request.urlopen('https://pypi.org/pypi/'+package+'/'+version+'/json', timeout=30) as stream:
            metadata = json.load(stream)
        if (metadata['info'].get('license_expression') or metadata['info']['license']) != license_expression:
            raise ValueError('reviewed public wheel license changed')
        match = [f for f in metadata['urls'] if f['filename'] == name and f['digests']['sha256'] == digest and f['size'] == size and not f['yanked']]
        if len(match) != 1:
            raise ValueError('exact reviewed wheel required')
        wheels = ROOT/'wheels'; wheels.mkdir(exist_ok=True)
        data = fetch(match[0]['url'], size)
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError('wheel hash mismatch')
        path = wheels/name; path.write_bytes(data)
        with zipfile.ZipFile(path) as archive:
            for entry in archive.infolist():
                parts = PurePosixPath(entry.filename)
                if parts.is_absolute() or '..' in parts.parts or stat.S_ISLNK(entry.external_attr >> 16):
                    raise ValueError('unsafe public wheel path')
            archive.extractall(site)
        receipts['files'][name] = digest
        (ROOT/(package+'-metadata.json')).write_text(json.dumps(metadata, indent=2)+'\n')
    (ROOT/'stage-receipts.json').write_text(json.dumps(receipts, indent=2)+'\n')
    print('ZIPFORMER_STAGE '+json.dumps(receipts), flush=True)


if __name__ == '__main__':
    main()
