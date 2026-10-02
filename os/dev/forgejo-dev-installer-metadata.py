"""Read CI installer source/checksums without downloading its disk payload."""
import hashlib
import json
import struct
import subprocess
import sys
import urllib.request
import zlib


def fgj(*args):
    return subprocess.run(['fgj', '--hostname', 'code.tilo.so', *args],
                          capture_output=True, check=True, timeout=60).stdout


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError('Artifact endpoint redirected; refusing to forward credentials')


def metadata(prefix):
    expected = {'source.txt', 'tilo-dev-installer-aarch64.qcow2.sha256',
                'tilo-dev-installer-aarch64.qcow2.zst.sha256'}
    result = {}
    offset = 0
    for _ in range(3):
        header = struct.unpack_from('<I5H3I2H', prefix, offset)
        signature, version, flags, method, _, _, crc, packed, size, name_size, extra_size = header
        if signature != 0x04034b50 or version > 20 or flags not in (0, 8) or method != 8:
            raise ValueError('Unexpected metadata ZIP header')
        name = prefix[offset + 30:offset + 30 + name_size].decode('utf-8')
        if name not in expected or name in result or name_size > 128 or extra_size > 4096:
            raise ValueError('Unexpected metadata entry')
        start = offset + 30 + name_size + extra_size
        decoder = zlib.decompressobj(-15)
        data = decoder.decompress(prefix[start:], 65537)
        if not decoder.eof or len(data) > 65536:
            raise ValueError('Metadata entry is truncated or oversized')
        consumed = len(prefix[start:]) - len(decoder.unused_data)
        offset = start + consumed
        if flags == 8:
            if prefix[offset:offset + 4] == b'PK\x07\x08':
                offset += 4
            crc, packed, size = struct.unpack_from('<III', prefix, offset)
            offset += 12
        if packed != consumed or size != len(data) or crc != zlib.crc32(data):
            raise ValueError('Metadata ZIP CRC or length mismatch')
        result[name] = data
    return result


def verify_medium(run_id):
    if type(run_id) is not int or run_id <= 0:
        raise ValueError('CI run ID must be positive')
    run = json.loads(fgj('api', f'repos/tilo/tilo/actions/runs/{run_id}'))
    if (run.get('id') != run_id or run.get('workflow_id') != 'os-dev-installer.yml'
            or run.get('status') != 'success' or run.get('is_fork_pull_request') is not False):
        raise SystemExit('Installer CI run is not a successful repository build')
    artifacts = json.loads(fgj('api', f'repos/tilo/tilo/actions/runs/{run_id}/artifacts'))
    artifacts = [a for a in artifacts if a.get('name') == f'tilo-dev-installer-aarch64-{run_id}'
                 and a.get('run_id') == run_id and a.get('expired') is False]
    if (len(artifacts) != 1 or type(artifacts[0].get('id')) is not int
            or artifacts[0]['id'] <= 0):
        raise SystemExit('CI medium has no unique live artifact')
    artifact_id = artifacts[0]['id']
    token = fgj('auth', 'token').decode().strip()
    request = urllib.request.Request(
        f'https://code.tilo.so/api/v1/repos/tilo/tilo/actions/artifacts/{artifact_id}/zip',
        headers={'Authorization': 'token ' + token, 'Accept': 'application/zip'})
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=60) as response:
        if response.status != 200:
            raise SystemExit('CI artifact transport failed')
        prefix = response.read(65536)
    values = metadata(prefix)
    source = dict(line.split('=', 1) for line in values['source.txt'].decode().splitlines())
    if source.get('build_source') != run.get('commit_sha'):
        raise SystemExit('CI artifact assembly source differs from its live run')
    result = {'schema': 'tilo.ci-dev-installer-metadata.v1', 'run_id': run_id,
              'artifact_id': artifact_id, 'assembly_source': run['commit_sha'],
              'source': source, 'source_sha256': hashlib.sha256(values['source.txt']).hexdigest(),
              'metadata_download_bytes': len(prefix)}
    for name in ('tilo-dev-installer-aarch64.qcow2.sha256', 'tilo-dev-installer-aarch64.qcow2.zst.sha256'):
        checksum = values[name].decode().split()
        if len(checksum) != 2 or len(checksum[0]) != 64 or checksum[1] != name.removesuffix('.sha256'):
            raise SystemExit('Invalid CI medium checksum metadata')
        int(checksum[0], 16)
        result[name] = checksum[0]
    return result


def main():
    if len(sys.argv) != 2 or not sys.argv[1].isdigit() or int(sys.argv[1]) <= 0:
        raise SystemExit('usage: forgejo-dev-installer-metadata.py INTERNAL_RUN_ID')
    try:
        print(json.dumps(verify_medium(int(sys.argv[1])), sort_keys=True))
    except (ValueError, OSError, KeyError, TypeError, struct.error,
            subprocess.SubprocessError) as error:
        raise SystemExit(f'CI installer metadata: {error}') from None


if __name__ == '__main__':
    main()
