"""Stream a verified CI installer ZIP through zstd without caching its archive."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import struct
import subprocess
import threading
import urllib.request
import zlib

SPEC = importlib.util.spec_from_file_location(
    'ci_medium', Path(__file__).with_name('forgejo-dev-installer-metadata.py'))
medium = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(medium)
NAME = 'tilo-dev-installer-aarch64.qcow2'
CHUNK = 1024 * 1024
MAX_DISK = 100 * 1024**3


def require(ok, message):
    if not ok:
        raise ValueError(message)


def payload(response, prefix, offset):
    """Bounded extraction of the final ZIP entry, including its CRC/lengths."""
    header = struct.unpack_from('<I5H3I2H', prefix, offset)
    signature, version, flags, method, _, _, crc, packed, size, name_size, extra_size = header
    require(signature == 0x04034b50 and version in (20, 45)
            and flags in (0, 8) and method == 8, 'Unexpected payload ZIP header')
    start = offset + 30
    require(name_size <= 128 and extra_size <= 4096
            and start + name_size + extra_size <= len(prefix), 'Invalid payload header bounds')
    require(prefix[start:start + name_size].decode() == NAME + '.zst', 'Unexpected payload name')
    extra = prefix[start + name_size:start + name_size + extra_size]
    if not flags and (packed == 0xffffffff or size == 0xffffffff):
        at = 0
        found = False
        while at + 4 <= len(extra):
            kind, length = struct.unpack_from('<HH', extra, at)
            require(at + 4 + length <= len(extra), 'Truncated ZIP extra field')
            if kind == 1:
                at += 4
                if size == 0xffffffff:
                    size = struct.unpack_from('<Q', extra, at)[0]
                    at += 8
                if packed == 0xffffffff:
                    packed = struct.unpack_from('<Q', extra, at)[0]
                found = True
                break
            at += 4 + length
        require(found, 'Missing ZIP64 sizes')
    decoder = zlib.decompressobj(-15)
    data = prefix[start + name_size + extra_size:]
    actual_packed = actual_size = actual_crc = 0
    while not decoder.eof:
        if not data:
            data = response.read(CHUNK)
            require(data, 'Truncated compressed CI payload')
        output = decoder.decompress(data, CHUNK)
        rest = decoder.unused_data if decoder.eof else decoder.unconsumed_tail
        actual_packed += len(data) - len(rest)
        data = rest
        actual_size += len(output)
        require(actual_size <= MAX_DISK, 'CI payload exceeds download bound')
        actual_crc = zlib.crc32(output, actual_crc)
        if output:
            yield output
    if flags == 8:
        while len(data) < 24:
            chunk = response.read(24 - len(data))
            require(chunk, 'Truncated ZIP data descriptor')
            data += chunk
        if data.startswith(b'PK\x07\x08'):
            data = data[4:]
        descriptor = '<IQQ' if version == 45 else '<III'
        crc, packed, size = struct.unpack_from(descriptor, data)
    require((crc, packed, size) == (actual_crc, actual_packed, actual_size),
            'CI payload ZIP CRC or length mismatch')


def extract(response, prefix, output, authority, reserve_bytes=8 * 1024**3):
    values, offset = medium.metadata(prefix, with_offset=True)
    require(hashlib.sha256(values['source.txt']).hexdigest() == authority['source_sha256'],
            'Downloaded metadata differs from live CI authority')
    expected = {}
    for suffix in ('.sha256', '.zst.sha256'):
        key = NAME + suffix
        digest = values[key].decode().split()[0]
        require(digest == authority[key], 'Downloaded checksum differs from live CI authority')
        expected[suffix] = digest
    require(output.parent.is_dir() and not output.exists() and not output.is_symlink(),
            'Download output must be a new file in an existing directory')
    require(shutil.disk_usage(output.parent).free >= reserve_bytes,
            'Insufficient free space before download')
    compressed = hashlib.sha256()
    decoded = hashlib.sha256()
    errors = []
    process = subprocess.Popen(['zstd', '-d', '-q', '-c'], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    def feed():
        try:
            for chunk in payload(response, prefix, offset):
                compressed.update(chunk)
                process.stdin.write(chunk)
        except Exception as error:
            errors.append(error)
            if process.poll() is None:
                process.terminate()
        finally:
            try:
                process.stdin.close()
            except BrokenPipeError:
                pass
    worker = threading.Thread(target=feed)
    worker.start()
    written = 0
    try:
        with output.open('xb') as disk:
            while chunk := process.stdout.read(CHUNK):
                require(shutil.disk_usage(output.parent).free >= reserve_bytes,
                        'Stopped to preserve host free space; partial output is unverified')
                written += len(chunk)
                require(written <= MAX_DISK, 'Decoded disk exceeds download bound')
                decoded.update(chunk)
                disk.write(chunk)
        worker.join()
        require(process.wait() == 0 and not errors, 'CI extraction failed: ' + str(errors[:1]))
        require(compressed.hexdigest() == expected['.zst.sha256']
                and decoded.hexdigest() == expected['.sha256'], 'Full CI disk checksum mismatch')
        return {'schema': 'tilo.ci-dev-installer-download.v1', 'authority': authority,
                'disk': str(output.resolve()), 'bytes': written,
                'sha256': decoded.hexdigest(), 'compressed_sha256': compressed.hexdigest(),
                'archive_cached': False}
    finally:
        if process.poll() is None:
            process.terminate()
        response.close()
        worker.join(timeout=35)
        process.wait()
        process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run_id', type=int, help='internal Forgejo installer run ID')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    authority = medium.verify_medium(args.run_id)
    token = medium.fgj('auth', 'token').decode().strip()
    request = urllib.request.Request(
        f'https://code.tilo.so/api/v1/repos/tilo/tilo/actions/artifacts/{authority["artifact_id"]}/zip',
        headers={'Authorization': 'token ' + token, 'Accept': 'application/zip'})
    with urllib.request.build_opener(medium.NoRedirect()).open(request, timeout=30) as response:
        require(response.status == 200, 'CI artifact transport failed')
        result = extract(response, response.read(65536), args.output, authority)
    args.output.with_suffix(args.output.suffix + '.verified.json').write_text(
        json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
