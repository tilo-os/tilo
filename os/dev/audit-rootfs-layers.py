#!/usr/bin/env python3
"""Bounded Docker-archive layer audit, with no archive or extracted files on disk.

This detects provided exact private material and complete private PEM envelopes,
excluding pinned public fixtures. It does not establish universal secret absence
or authorize publication/installation.
"""
import argparse
import base64
import binascii
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
PEM = re.compile(rb'-----BEGIN ((?:[A-Z0-9]+ )*PRIVATE KEY)-----')
MAX_PEM = 65536
PEM_SPACE = b' \t\r\n\v\f'
PEM_HEADERS = re.compile(rb'Proc-Type:[ \t]*4,ENCRYPTED\n'
                         rb'DEK-Info:[ \t]*[A-Z0-9-]+,[0-9A-Fa-f]+\n\n')
# Public key vectors from independently authenticated Fedora packages. These
# are exact decoded DER hashes, not file, package, architecture, or PEM-label
# exemptions. Explicit private match inputs
# always reject independently, including a supplied copy of a public vector.
# RPM: https://kojipkgs.fedoraproject.org/packages/gnutls/3.8.13/1.fc44/data/signed/6d9f90a6/aarch64/gnutls-3.8.13-1.fc44.aarch64.rpm
# RPM SHA256: 45ba27b248c7901dcab3a6b56bd3b11bd71fd48f83de4f1488d1ab594bb524ac
# Fedora signer: 36F612DCF27F7D1A48A835E4DBFCF71C6D9F90A6
# libgnutls.so.30.42.0 SHA256: 9be5a78ad24f4e012862db3ea0563b641a3bc6253becaa7e9e8fd44e4372932b
# RPM: https://kojipkgs.fedoraproject.org/packages/gnutls/3.8.13/1.fc44/data/signed/6d9f90a6/aarch64/gnutls-utils-3.8.13-1.fc44.aarch64.rpm
# RPM SHA256: 4aff6d39cea2c9f1bdb8cf02b0b021685d7942cc2c9ce18e0aabbdd67daf2933
# gnutls-cli SHA256: dc2cb3d6639a8daa93e74381593f2cf36f9d4bd0c3d2ad1481f75b2d20d5755d
PUBLIC_PEM_DER_SHA256 = frozenset({
    '458022ddcb5598b39253bc027eb02c4f3dba00e2bdbaef239a35bc91c9bd4f3a',
    '995b8d5b08ac126a74f39d812fc8cce270d026093d810f366de923bb0fe47f60',
    '60c323abe4854b03743005874156ef6e0343bc9c05c09a2a43335cdda19342b3',
    '2859ff03b44c8f0e4f96604b8cced325ef4d01ca935422b2041d88d8809f8fa1',
    '09f1c1a4680df80b9e8e6e587d3a787de7402a919e366426ad807a1c0f8c98c1',
    '0294b4406902e33bb02f89eaf4c691328790d7a9cd01b873d68edac994f310a1',
    'd245e52c341a76645e1b0e56c58dc964cd4f3ecaea64a9c6d6db9897d4948a8a',
    '87e288dc7dbc29a24241b596ec8c84f949d9f09f34fc0cf69242481fb2274770',
    '323a1e2a6917c0615aa5e0144ba9ea0b18553c671284f2b75ca5dde1308c477b',
    '803fc9d7eb40ea13dc82723b2d99780ac1e0335ba0b65d660e66994c613ed422',
    '6f0c3130a081db0cd783ea43ab6992680f37b16d789b39df5bf0e7437b04c6ff',
    'd609f09d5bc2b9038bff653e31ce4d594ea32f8647fe8414d094aacb69e9cd9b',
    'aafe40f40e5e2c77bd51c7293152896478f75a80d01e7ed820f6c97c616c70e5',
    '82bf1f671fb9c8a3258e558e139ac450d787bb128c202e895b16a519691b0afc',
    # Public simulate_proxy.pl example from perl-IO-Socket-SSL.
    # RPM: https://kojipkgs.fedoraproject.org/packages/perl-IO-Socket-SSL/2.098/2.fc44/data/signed/6d9f90a6/noarch/perl-IO-Socket-SSL-2.098-2.fc44.noarch.rpm
    # RPM SHA256: c611b0adbabd7df6828a8fbdb573435f034ac2242407024e8987a54aa4959399
    # File SHA256: bf6fcca47a4b4245420813689304921ebd89e630a8b2a701e6c42f5e04a73e27
    '8c3458123cf70946e0399c08ca73aca997b11001dae82b9057ebc30d9f4089f4',
})
# Legacy encrypted examples bind their label and LF-normalized encryption
# headers as well as decoded ciphertext. A changed cipher or IV never inherits
# a public-vector exception merely because the ciphertext is unchanged.
# Public server_key.pem example; same independently verified Fedora signer.
# RPM: https://kojipkgs.fedoraproject.org/packages/perl-Net-SSLeay/1.94/12.fc44/data/signed/6d9f90a6/x86_64/perl-Net-SSLeay-1.94-12.fc44.x86_64.rpm
# RPM SHA256: 21ac409aaa96d6fcd8a05d138291308d53a238c45fe2079573ec4f510762d38f
# File SHA256: 0e13f274a94db44c94ced16b2332fdbc9ff770eae88bdec9c0347695c7c99ee2
PUBLIC_ENCRYPTED_PEM_SHA256 = frozenset({
    '952702af054ee00ff725de0f2f949ae002884d43a87474a0ed113cc62a02ac91',
})
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
    'private PEM envelope exceeds size limit': 'private-pem-size-limit',
}


class AuditError(ValueError):
    pass


class PrivatePEM:
    """Recognize complete envelopes, including ones split across read calls.

    Crypto parsers contain BEGIN/END strings as ordinary program data. Require
    a matching footer and nonempty valid base64 between them. A bounded pending
    candidate is separate from the exact-private-byte matcher and never joins
    markers across intervening binary bytes, quotes, or other invalid payload.
    """
    def __init__(self):
        self.pending = b''

    @staticmethod
    def normalized(raw, partial=False):
        # JSON metadata can store a complete PEM with escaped line breaks.
        # Decode only these separators inside a candidate, never arbitrary JSON
        # escapes or adjacent string fields. Quotes/NUL still reject it.
        if partial and raw.endswith(b'\\'):
            raw = raw[:-1]
        raw = raw.replace(b'\\r\\n', b'\n').replace(b'\\n', b'\n').replace(b'\\r', b'\n')
        return raw.replace(b'\r\n', b'\n').replace(b'\r', b'\n').lstrip(PEM_SPACE)

    @classmethod
    def body(cls, raw, partial):
        raw = cls.normalized(raw, partial)
        if raw.startswith(b'Proc-Type:') or (partial and b'Proc-Type:'.startswith(raw)):
            headers = PEM_HEADERS.match(raw)
            if headers is None:
                return partial and len(raw) <= 512 and all(
                    byte in b'\t\n' or 32 <= byte <= 126 and byte not in b'\"\'\\'
                    for byte in raw)
            raw = raw[headers.end():]
        payload = raw.translate(None, PEM_SPACE)
        if not re.fullmatch(rb'[A-Za-z0-9+/]*={0,2}', payload):
            return False
        if partial:
            return True
        try:
            return payload and base64.b64decode(payload, validate=True)
        except binascii.Error:
            return False

    @classmethod
    def public_vector(cls, label, raw, decoded):
        headers = PEM_HEADERS.match(cls.normalized(raw))
        if headers:
            identity = label + b'\0' + headers[0] + b'\0' + decoded
            return hashlib.sha256(identity).hexdigest() in PUBLIC_ENCRYPTED_PEM_SHA256
        return hashlib.sha256(decoded).hexdigest() in PUBLIC_PEM_DER_SHA256

    def feed(self, data):
        window = self.pending + data
        self.pending = window[-256:]
        for begin in PEM.finditer(window):
            footer = b'-----END ' + begin[1] + b'-----'
            rest = window[begin.end():]
            end = rest.find(footer)
            if end >= 0:
                decoded = self.body(rest[:end], partial=False)
                if decoded:
                    if end + len(footer) + begin.end() - begin.start() > MAX_PEM:
                        raise AuditError('private PEM envelope exceeds size limit')
                    if not self.public_vector(begin[1], rest[:end], decoded):
                        return True
                continue
            # A footer itself may straddle the next chunk. Remove only an exact
            # prefix of this candidate's footer while checking body plausibility.
            body = rest
            for size in range(min(len(rest), len(footer) - 1), 0, -1):
                if rest.endswith(footer[:size]):
                    body = rest[:-size]
                    break
            if self.body(body, partial=True):
                candidate = window[begin.start():]
                if len(candidate) > MAX_PEM:
                    raise AuditError('private PEM envelope exceeds size limit')
                self.pending = candidate
        return False


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
        self.pem = PrivatePEM()
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
            if 'private-pem-envelope' not in self.findings and self.pem.feed(data):
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


def metadata_values(raw):
    """Scan JSON strings separately after decoding, including duplicate values.

    Raw scanning still covers non-JSON metadata and literal escape sequences in
    arbitrary files. Decoding prevents valid JSON escapes (e.g. slash or Unicode
    newline) from hiding a key, without joining distinct values into a fake key.
    """
    yield raw
    try:
        values = [json.loads(raw, object_pairs_hook=list)]
    except (UnicodeDecodeError, json.JSONDecodeError):
        return
    except (ValueError, RecursionError):
        raise AuditError('invalid Docker archive metadata') from None
    while values:
        value = values.pop()
        if isinstance(value, str):
            yield value.encode('utf-8', errors='surrogatepass')
        elif isinstance(value, (list, tuple)):
            values.extend(value)


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
            for decoded in metadata_values(value):
                if any(secret in decoded for secret in private_values):
                    metadata_findings.add('provided-private-material')
                if PrivatePEM().feed(decoded):
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
            'scope': 'all uncompressed layer bytes and archive metadata file contents: provided exact private bytes and complete private PEM envelopes excluding pinned public fixtures',
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
