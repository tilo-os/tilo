#!/usr/bin/env python3
"""Bounded Docker-archive layer audit, with no archive or extracted files on disk.

This detects provided exact private material and private PEM envelopes. It does
not establish universal secret absence or authorize publication/installation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import stat
import subprocess
import threading

CHUNK = 1024 * 1024
MAX_BYTES = 256 * 1024**3
MAX_METADATA = 8 * CHUNK
MAX_LAYERS = 1024
PEM = re.compile(rb'-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----')
PUBLIC_FAILURE_REASONS = {
    'truncated archive': 'truncated-archive',
    'unbounded archive read requested': 'invalid-read-bound',
    'negative tar field': 'negative-tar-field',
    'invalid tar field': 'invalid-tar-field',
    'invalid archive name': 'invalid-archive-name',
    'unsafe archive name': 'unsafe-archive-name',
    'invalid PAX metadata': 'invalid-pax-metadata',
    'invalid tar terminator': 'invalid-tar-terminator',
    'nonzero bytes after tar terminator': 'trailing-tar-data',
    'invalid or compressed layer tar header': 'invalid-tar-header',
    'oversized tar metadata': 'oversized-tar-metadata',
    'global path/size metadata unsupported': 'unsupported-global-pax-metadata',
    'invalid PAX size': 'invalid-pax-size',
    'tar entry exceeds byte limit': 'tar-entry-byte-limit',
    'unsupported tar entry type': 'unsupported-tar-entry-type',
    'truncated tar entry': 'truncated-tar-entry',
    'archive entry count limit exceeded': 'archive-entry-limit',
    'duplicate JSON key': 'duplicate-json-key',
    'immutable image ID required': 'invalid-image-id',
    'private match inputs must be bounded nonempty values': 'invalid-private-match-inputs',
    'duplicate archive entry': 'duplicate-archive-entry',
    'invalid archive directory': 'invalid-archive-directory',
    'invalid legacy layer alias': 'invalid-layer-alias',
    'unknown legacy layer alias': 'unknown-layer-alias',
    'unexpected Docker archive entry type': 'unexpected-archive-entry-type',
    'too many layers': 'layer-count-limit',
    'too many layer occurrences': 'layer-occurrence-limit',
    'truncated layer': 'truncated-layer',
    'layer filename differs from content digest': 'layer-digest-mismatch',
    'Docker metadata exceeds size limit': 'archive-metadata-size-limit',
    'archive byte limit exceeded': 'archive-byte-limit',
    'one Docker image manifest required': 'invalid-manifest-count',
    'unexpected Docker manifest schema': 'invalid-manifest-schema',
    'archive configuration differs from requested image ID': 'image-config-mismatch',
    'invalid manifest layer list': 'invalid-manifest-layers',
    'manifest/configuration/layer identity mismatch': 'archive-identity-mismatch',
    'invalid Docker archive metadata': 'invalid-archive-metadata',
    'private comparison input must be a bounded owner-only regular file': 'invalid-private-file',
    'invalid immutable image identity or timeout': 'invalid-audit-inputs',
    'layer export failed or exceeded timeout': 'layer-export-failed',
}


class AuditError(ValueError):
    pass


def exact(stream, size):
    result = bytearray()
    while len(result) < size:
        part = stream.read(min(CHUNK, size - len(result)))
        if not part:
            raise AuditError('truncated archive')
        result.extend(part)
    return bytes(result)


class Reader:
    def __init__(self, stream, limit, patterns=(), hashing=False):
        self.stream, self.remaining = stream, limit
        self.patterns = patterns
        self.digest = hashlib.sha256() if hashing else None
        self.tail = b''
        self.overlap = max([256, *(len(value) - 1 for value in patterns)])
        self.bytes_read = 0
        self.findings = set()

    def read(self, size):
        if size < 0 or size > CHUNK:
            raise AuditError('unbounded archive read requested')
        if not self.remaining:
            return b''
        data = self.stream.read(min(size, self.remaining))
        self.remaining -= len(data)
        self.bytes_read += len(data)
        if self.digest:
            self.digest.update(data)
        if self.patterns or self.digest:
            window = self.tail + data
            if any(value in window for value in self.patterns):
                self.findings.add('provided-private-material')
            if PEM.search(window):
                self.findings.add('private-pem-envelope')
            self.tail = window[-self.overlap:]
        return data


def consume(stream):
    while stream.read(CHUNK):
        pass


def number(value):
    if value[0] & 0x80:
        if value[0] & 0x40:
            raise AuditError('negative tar field')
        return int.from_bytes(bytes([value[0] & 0x7f]) + value[1:], 'big')
    try:
        return int(value.rstrip(b'\0 ').lstrip(b' ') or b'0', 8)
    except ValueError:
        raise AuditError('invalid tar field') from None


def safe_name(value):
    try:
        name = value.decode('utf-8')
    except UnicodeError:
        raise AuditError('invalid archive name') from None
    path = PurePosixPath(name)
    if not name or len(value) > 4096 or path.is_absolute() or '..' in path.parts or '\x00' in name:
        raise AuditError('unsafe archive name')
    return name.rstrip('/')


def pax_fields(data):
    fields = {}
    while data:
        try:
            length = int(data.split(b' ', 1)[0])
            record = data[:length]
            if length <= 0 or len(record) != length or not record.endswith(b'\n'):
                raise ValueError()
            key, value = record.split(b' ', 1)[1][:-1].split(b'=', 1)
        except (ValueError, IndexError):
            raise AuditError('invalid PAX metadata') from None
        if key in (b'path', b'size', b'linkpath'):
            fields[key] = value
        data = data[length:]
    return fields


def walk_tar(stream, visit, max_entries):
    pending = {}
    for _ in range(max_entries):
        header = exact(stream, 512)
        if header == bytes(512):
            if exact(stream, 512) != bytes(512):
                raise AuditError('invalid tar terminator')
            while data := stream.read(CHUNK):
                if any(data):
                    raise AuditError('nonzero bytes after tar terminator')
            return
        if number(header[148:156]) != sum(header[:148]) + 256 + sum(header[156:]):
            raise AuditError('invalid or compressed layer tar header')
        size = number(header[124:136])
        kind = header[156:157]
        name = header[:100].split(b'\0', 1)[0]
        link = header[157:257].split(b'\0', 1)[0]
        if header[257:263] in (b'ustar\x00', b'ustar '):
            prefix = header[345:500].split(b'\0', 1)[0]
            if prefix:
                name = prefix + b'/' + name
        if kind in (b'x', b'g', b'L', b'K'):
            if size > 65536:
                raise AuditError('oversized tar metadata')
            data = exact(stream, size)
            if kind == b'x':
                pending.update(pax_fields(data))
            elif kind == b'g':
                if pax_fields(data):
                    raise AuditError('global path/size metadata unsupported')
            elif kind == b'L':
                pending[b'path'] = data.rstrip(b'\0')
            elif kind == b'K':
                pending[b'linkpath'] = data.rstrip(b'\0')
        else:
            if b'size' in pending:
                try:
                    size = int(pending[b'size'])
                except ValueError:
                    raise AuditError('invalid PAX size') from None
            if not 0 <= size <= MAX_BYTES:
                raise AuditError('tar entry exceeds byte limit')
            name = safe_name(pending.get(b'path', name))
            link = pending.get(b'linkpath', link)
            pending = {}
            if kind not in (b'\0', b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7'):
                raise AuditError('unsupported tar entry type')
            data = Reader(stream, size)
            visit(name, kind, size, data, link)
            consume(data)
            if data.remaining:
                raise AuditError('truncated tar entry')
        if padding := (-size) % 512:
            exact(stream, padding)
    raise AuditError('archive entry count limit exceeded')


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise AuditError('duplicate JSON key')
        result[key] = value
    return result


def audit_stream(stream, image_id, private_values=(), max_bytes=MAX_BYTES):
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise AuditError('immutable image ID required')
    if len(private_values) > 32 or any(not 16 <= len(value) <= 65536 for value in private_values):
        raise AuditError('private match inputs must be bounded nonempty values')
    # Shell command substitution removes trailing newlines from credential
    # files. Match the original and a CR/LF-trimmed variant, without implying
    # coverage of encodings or arbitrary secret transformations.
    private_values = tuple(dict.fromkeys(
        variant for value in private_values for variant in (value, value.rstrip(b'\r\n'))
        if len(variant) >= 16))
    layers, metadata, names, aliases = {}, {}, set(), {}
    metadata_findings = set()
    metadata_bytes = 0
    total = Reader(stream, max_bytes)

    def visit(name, kind, size, content, link):
        nonlocal metadata_bytes
        if name in names:
            raise AuditError('duplicate archive entry')
        names.add(name)
        if kind == b'5':
            if size:
                raise AuditError('invalid archive directory')
            return
        if kind == b'2':
            # containers/image emits legacy layer.tar aliases alongside the
            # canonical digest.tar payload. Validate metadata only; never follow
            # or extract links, and never permit alias chains or forward links.
            if (size or not re.fullmatch(r'[0-9a-f]{64}/layer[.]tar', name)
                    or not re.fullmatch(rb'[.][.]/[0-9a-f]{64}[.]tar', link)):
                raise AuditError('invalid legacy layer alias')
            target = link[3:].decode('ascii')
            if target not in layers:
                raise AuditError('unknown legacy layer alias')
            aliases[name] = target
            return
        if kind not in (b'0', b'\0') or link:
            raise AuditError('unexpected Docker archive entry type')
        canonical_layer = re.fullmatch(r'[0-9a-f]{64}[.]tar', name)
        if name.endswith('/layer.tar') or canonical_layer:
            if len(layers) >= MAX_LAYERS:
                raise AuditError('too many layers')
            layer = Reader(content, size, tuple(private_values), hashing=True)
            walk_tar(layer, lambda *args: None, 1000000)
            if layer.remaining:
                raise AuditError('truncated layer')
            if canonical_layer and layer.digest.hexdigest() != name[:-4]:
                raise AuditError('layer filename differs from content digest')
            layers[name] = {'diff_id': 'sha256:' + layer.digest.hexdigest(),
                            'bytes_scanned': layer.bytes_read, 'findings': sorted(layer.findings)}
        else:
            metadata_bytes += size
            if size > CHUNK or metadata_bytes > MAX_METADATA:
                raise AuditError('Docker metadata exceeds size limit')
            value = exact(content, size)
            metadata[name] = value
            if any(secret in value for secret in private_values):
                metadata_findings.add('provided-private-material')
            if PEM.search(value):
                metadata_findings.add('private-pem-envelope')

    walk_tar(total, visit, 4096)
    # A budget boundary must not masquerade as end-of-file.
    if not total.remaining and stream.read(1):
        raise AuditError('archive byte limit exceeded')
    try:
        manifests = json.loads(metadata['manifest.json'], object_pairs_hook=unique)
        if not isinstance(manifests, list) or len(manifests) != 1:
            raise AuditError('one Docker image manifest required')
        manifest = manifests[0]
        if set(manifest) - {'Config', 'RepoTags', 'Layers', 'LayerSources'}:
            raise AuditError('unexpected Docker manifest schema')
        config_bytes = metadata[manifest['Config']]
        if hashlib.sha256(config_bytes).hexdigest() != image_id[7:]:
            raise AuditError('archive configuration differs from requested image ID')
        config = json.loads(config_bytes, object_pairs_hook=unique)
        ordered = manifest['Layers']
        if not isinstance(ordered, list) or not all(isinstance(name, str) for name in ordered):
            raise AuditError('invalid manifest layer list')
        if len(ordered) > MAX_LAYERS:
            raise AuditError('too many layer occurrences')
        ordered = [aliases.get(name, name) for name in ordered]
        # Repeated filesystem deltas are valid. Podman exports each distinct
        # payload once, with different legacy aliases for its chain positions.
        # Preserve every ordered occurrence when checking the config identity.
        if (not ordered or set(ordered) != set(layers) or config['rootfs']['type'] != 'layers'
                or config['rootfs']['diff_ids'] != [layers[name]['diff_id'] for name in ordered]):
            raise AuditError('manifest/configuration/layer identity mismatch')
    except (KeyError, TypeError, json.JSONDecodeError):
        raise AuditError('invalid Docker archive metadata') from None
    rows = [layers[name] for name in ordered]
    return {'schema': 1, 'image_id': image_id, 'scanner': 'docker-layer-private-material-v1',
            'status': 'rejected-private-material' if metadata_findings or any(row['findings'] for row in rows) else 'passed-scoped-scan',
            'scope': 'all uncompressed layer bytes and archive metadata file contents: provided exact private bytes and private PEM envelopes only',
            'universal_secret_absence_proven': False, 'installable': False,
            'archive_bytes_read': total.bytes_read, 'unique_layers_scanned': len(layers),
            'metadata_findings': sorted(metadata_findings), 'layers': rows}


def load_private(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
                or info.st_mode & 0o077 or not 16 <= info.st_size <= 65536):
            raise AuditError('private comparison input must be a bounded owner-only regular file')
        return exact(stream, info.st_size)


def audit_image(image_id, private_values=(), timeout=1800):
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id) or not 1 <= timeout <= 1800:
        raise AuditError('invalid immutable image identity or timeout')
    child = subprocess.Popen(['podman', '--remote=false', 'save', '--format=docker-archive', image_id],
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
    expired = threading.Event()
    def stop():
        if child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    def expire():
        expired.set()
        stop()
    timer = threading.Timer(timeout, expire)
    timer.start()
    try:
        result = audit_stream(child.stdout, image_id, private_values)
        code = child.wait(timeout=10)
        if expired.is_set() or code:
            raise AuditError('layer export failed or exceeded timeout')
        return result
    finally:
        timer.cancel()
        stop()
        child.wait(timeout=10)
        child.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image-id', required=True)
    parser.add_argument('--private-file', action='append', default=[], type=Path)
    args = parser.parse_args()
    stage = 'private-comparison-input'
    try:
        private_values = [load_private(path) for path in args.private_file]
        stage = 'layer-export-and-scan'
        result = audit_image(args.image_id, private_values)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0 if result['status'] == 'passed-scoped-scan' else 1
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        # Export/parser failures must remain observable without copying exception
        # diagnostics, archive paths, command output or private comparison bytes.
        reason = 'unclassified-audit-error'
        if isinstance(error, AuditError):
            reason = PUBLIC_FAILURE_REASONS.get(str(error), reason)
        elif isinstance(error, OSError):
            reason = 'io-error'
        elif isinstance(error, subprocess.SubprocessError):
            reason = 'subprocess-error'
        print(json.dumps({'schema': 1,
            'image_id': args.image_id if re.fullmatch(r'sha256:[0-9a-f]{64}', args.image_id) else None,
            'scanner': 'docker-layer-private-material-v1', 'status': 'failed-scoped-scan',
            'failed_stage': stage, 'failure_reason': reason,
            'universal_secret_absence_proven': False, 'installable': False}, sort_keys=True))
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
