#!/usr/bin/env python3
"""Offline internal-installer bundle primitives and conservative inspection.

Schema validation establishes consistency, never release authenticity or bootability.
The current qOS Btrfs/composefs producer/consumer relocation contract is not yet
qualified: export never publishes a manifest until that interface is established.
`export_ranges` is a file-level primitive, not a successful installer export.
"""
import argparse
import importlib.util
from contextlib import contextmanager, redirect_stdout
import hashlib
import json
import io
import os
from pathlib import Path
import re
import stat
import struct
import sys
import tempfile
import uuid
import zlib

MAX_JSON = 65536
MAX_IMAGE = 1 << 40
SECTOR = 512
ESP_GUID = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
ROOT_GUID = '0fc63daf-8483-4772-8e79-3d69d8477de4'
ROOT_GUIDS = {'x86_64': '4f68bce3-e8cd-4db1-96e7-fbcaf984b709',
              'aarch64': 'b921b045-1df0-41c3-af44-4c6f280d3fae'}
GUID_PATTERN = r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def keys(value, expected):
    require(type(value) is dict and set(value) == set(expected.split()),
            'missing or unknown object fields')


def integer(value, minimum=1, maximum=MAX_IMAGE):
    require(type(value) is int and minimum <= value <= maximum, 'integer outside bounds')
    return value


def text(value, pattern, limit):
    require(type(value) is str and len(value) <= limit and re.fullmatch(pattern, value),
            'invalid or unbounded text')
    return value


def guid(value):
    text(value, GUID_PATTERN, 36)
    require(uuid.UUID(value).int != 0, 'zero UUID')
    return value


def disjoint(ranges, limit):
    last = 0
    for offset, size in sorted(ranges):
        integer(offset, 0)
        integer(size)
        require(offset >= last and offset + size <= limit, 'overlapping or out-of-bounds extents')
        last = offset + size


def validate_manifest(manifest):
    keys(manifest, 'schema purpose release_id architecture firmware secure_boot installer_protocol '
                   'source_image_bytes components boot_binding')
    require(type(manifest['schema']) is int and manifest['schema'] == 1, 'unsupported schema')
    require(manifest['purpose'] == 'qos-internal-installer', 'wrong bundle purpose')
    text(manifest['release_id'], r'[A-Za-z0-9][A-Za-z0-9._-]*', 128)
    arch = manifest['architecture']
    require(arch in ('x86_64', 'aarch64'), 'unsupported architecture')
    require(manifest['firmware'] == 'uefi' and manifest['secure_boot'] == 'disabled',
            'unsupported firmware/security mode')
    require(type(manifest['installer_protocol']) is int and manifest['installer_protocol'] == 1,
            'unsupported installer protocol')
    image_size = integer(manifest['source_image_bytes'])
    components = manifest['components']
    require(type(components) is list and len(components) == 2, 'exactly two components required')
    by_role = {}
    filenames = set()
    for component in components:
        keys(component, 'role filename byte_size sha256 partition_type_guid filesystem_type '
                        'filesystem_uuid minimum_partition_bytes source_offset')
        role = component['role']
        require(type(role) is str and role in ('staging-root', 'staging-esp') and role not in by_role,
                'unsupported or duplicate role')
        by_role[role] = component
        filename = text(component['filename'], r'[A-Za-z0-9][A-Za-z0-9._-]*\.img', 128)
        require(filename not in filenames, 'duplicate component filename')
        filenames.add(filename)
        size = integer(component['byte_size'])
        offset = integer(component['source_offset'])
        require(size % SECTOR == 0 and offset % SECTOR == 0, 'unaligned extent')
        integer(component['minimum_partition_bytes'], size)
        text(component['sha256'], r'[0-9a-f]{64}', 64)
        part_type = guid(component['partition_type_guid'])
        if role == 'staging-esp':
            require(part_type == ESP_GUID and component['filesystem_type'] == 'vfat', 'unsupported ESP')
            text(component['filesystem_uuid'], r'[0-9A-F]{4}-[0-9A-F]{4}', 9)
            require(component['filesystem_uuid'] != '0000-0000', 'zero FAT identity')
        else:
            require(part_type in (ROOT_GUID, ROOT_GUIDS[arch]) and component['filesystem_type'] == 'btrfs',
                    'unsupported root partition')
            guid(component['filesystem_uuid'])
    disjoint([(c['source_offset'], c['byte_size']) for c in components], image_size)
    boot = manifest['boot_binding']
    keys(boot, 'efi_path uki_architecture uki_sha256 uki_cmdline root_locator composefs_digest')
    require(boot['uki_architecture'] == arch, 'UKI architecture mismatch')
    text(boot['efi_path'], r'/EFI/(?:[A-Za-z0-9_-]+/)*[A-Za-z0-9_-][A-Za-z0-9._-]*\.efi', 240)
    text(boot['uki_sha256'], r'[0-9a-f]{64}', 64)
    text(boot['composefs_digest'], r'[0-9a-f]{128}', 128)
    expected = 'UUID=' + by_role['staging-root']['filesystem_uuid']
    require(boot['root_locator'] == expected, 'root UUID binding mismatch')
    validate_cmdline(boot['uki_cmdline'], expected, boot['composefs_digest'])


def validate_cmdline(cmdline, root_locator, digest):
    text(cmdline, r'[\x20-\x7e]+', 4096)
    require(not any(c in cmdline for c in '\\"\''), 'quoted or escaped command line unsupported')
    tokens = cmdline.split()
    require(len(tokens) <= 64, 'too many kernel arguments')
    names = [token.split('=', 1)[0] for token in tokens]
    require(all(names.count(name) == 1 for name in names if name != 'console'), 'duplicate kernel argument')
    consoles = [token for token in tokens if token.startswith('console=')]
    require(len(set(consoles)) == len(consoles) and len(consoles) <= 2, 'duplicate console argument')
    require('root=' + root_locator in tokens and 'composefs=' + digest in tokens
            and 'qos.install' in tokens, 'missing or mismatched measured boot binding')
    # Narrow schema profile. Expansion requires consumer evidence, not ignored arguments.
    allowed_flags = {'ro', 'rw', 'quiet', 'splash', 'qos.install', 'slab_nomerge',
                     'plymouth.ignore-serial-consoles'}
    allowed_values = {'root': root_locator, 'composefs': digest, 'qos.mode': 'normal',
                      'qos.luks': '0'*43, 'qos.unlock': '0'*74,
                      'init_on_alloc': '1', 'init_on_free': '1', 'lockdown': 'integrity',
                      'randomize_kstack_offset': 'on', 'page_alloc.shuffle': '1', 'vsyscall': 'none'}
    for token in tokens:
        if token in allowed_flags:
            continue
        name, separator, value = token.partition('=')
        if name == 'qos.generation' and re.fullmatch(r'[1-9][0-9]{0,9}', value):
            continue
        if name == 'console' and value in ('tty0', 'ttyS0,115200'):
            continue
        require(separator and name in allowed_values and value == allowed_values[name],
                'unrecognized command-line binding')
    require('rw' in tokens and 'ro' not in tokens, 'composefs physical root requires rw')


def parse_manifest(raw):
    require(type(raw) is bytes and 0 < len(raw) <= MAX_JSON, 'manifest exceeds size limit')
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    def invalid_constant(value):
        raise ValueError('non-finite JSON number')
    try:
        result = json.loads(raw.decode('utf-8'), object_pairs_hook=pairs,
                            parse_constant=invalid_constant)
    except (UnicodeError, RecursionError) as error:
        raise ValueError('invalid JSON encoding/depth') from error
    validate_manifest(result)
    return result


@contextmanager
def regular_file(path):
    """Pin one non-symlink regular file; never open a FIFO or device for I/O."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        require(stat.S_ISREG(os.fstat(fd).st_mode), 'not a regular file')
        with os.fdopen(fd, 'rb', closefd=False) as stream:
            yield stream
    finally:
        os.close(fd)


def fingerprint(stream):
    info = os.fstat(stream.fileno())
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns


def exact(stream, offset, size):
    stream.seek(offset)
    data = stream.read(size)
    require(len(data) == size, 'truncated image')
    return data


def _read_gpt(stream):
    size = integer(os.fstat(stream.fileno()).st_size, 68 * SECTOR)
    require(size % SECTOR == 0, 'unsupported sector geometry')
    sectors = size // SECTOR
    mbr = exact(stream, 0, SECTOR)
    require(mbr[510:] == b'\x55\xaa', 'missing protective MBR')
    require(mbr[446] == 0 and mbr[450] == 0xee and
            struct.unpack_from('<II', mbr, 454) == (1, min(sectors-1, 0xffffffff)) and
            mbr[462:510] == bytes(48), 'hybrid or malformed protective MBR')

    def header(lba):
        data = bytearray(exact(stream, lba*SECTOR, SECTOR))
        fields = struct.unpack_from('<8sIIIIQQQQ16sQIII', data)
        signature, revision, length, crc, reserved, current, backup, first, last, disk, table, count, width, table_crc = fields
        require(signature == b'EFI PART' and revision == 0x10000 and length == 92 and reserved == 0,
                'unsupported GPT header')
        struct.pack_into('<I', data, 16, 0)
        require(zlib.crc32(data[:length]) == crc, 'GPT header CRC mismatch')
        require(current == lba and backup == (sectors-1 if lba == 1 else 1), 'GPT header locations mismatch')
        require(uuid.UUID(bytes_le=disk).int != 0, 'zero disk identity')
        require(1 <= count <= 128 and width == 128, 'unsupported GPT table size')
        table_bytes = count * width
        table_sectors = (table_bytes + SECTOR-1) // SECTOR
        require(2 + table_sectors <= first <= last < sectors - 1 - table_sectors,
                'GPT usable region overlaps metadata')
        require(table == (2 if lba == 1 else sectors-1-table_sectors), 'noncanonical GPT table placement')
        entries = exact(stream, table*SECTOR, table_bytes)
        require(zlib.crc32(entries) == table_crc, 'GPT table CRC mismatch')
        return (first, last, disk, count, width, entries)

    primary = header(1)
    require(primary == header(sectors-1), 'primary/backup GPT disagree')
    first, last, _disk, count, width, entries = primary
    partitions, identities = [], set()
    for number in range(count):
        entry = entries[number*width:(number+1)*width]
        if entry[:16] == bytes(16):
            require(entry == bytes(width), 'nonzero unused GPT entry')
            continue
        kind = str(uuid.UUID(bytes_le=entry[:16]))
        identity = str(uuid.UUID(bytes_le=entry[16:32]))
        guid(identity)
        require(identity not in identities, 'duplicate partition identity')
        identities.add(identity)
        start, end, attrs = struct.unpack_from('<QQQ', entry, 32)
        require(first <= start <= end <= last, 'unsupported partition extent')
        allowed_attributes = (0, 1 << 59) if kind in ROOT_GUIDS.values() else (0,)
        require(attrs in allowed_attributes, 'unsupported partition attributes for type')
        role = 'staging-esp' if kind == ESP_GUID else 'staging-root'
        require(kind in (ESP_GUID, ROOT_GUID, *ROOT_GUIDS.values()), 'unsupported partition type')
        partitions.append({'number': number+1, 'role': role, 'partition_type_guid': kind,
                           'partition_uuid': identity, 'attributes': attrs, 'offset': start*SECTOR, 'size': (end-start+1)*SECTOR})
    require(len(partitions) == 2 and {p['role'] for p in partitions} == {'staging-root', 'staging-esp'},
            'require exactly one ESP and one root')
    disjoint([(p['offset'], p['size']) for p in partitions], size)
    return partitions


def read_gpt(path):
    with regular_file(path) as stream:
        before = fingerprint(stream)
        result = _read_gpt(stream)
        require(before == fingerprint(stream), 'source changed during inspection')
        return result


def copy_extent(stream, destination, offset, size, *, directory_fd=None, created=None):
    digest = hashlib.sha256()
    stream.seek(offset)
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=directory_fd)
    with os.fdopen(descriptor, 'wb') as output:
        if created is not None:
            created(destination, descriptor)
        remaining = size
        while remaining:
            data = stream.read(min(1024*1024, remaining))
            require(data, 'truncated source during export')
            output.write(data)
            digest.update(data)
            remaining -= len(data)
        output.flush()
        os.fsync(output.fileno())
    return digest.hexdigest()


def export_ranges(image, output):
    """Copy actual GPT ranges into a new directory. Never emit a manifest.

    Returned descriptors intentionally lack filesystem/UKI facts. Only inspection
    of actual artifacts can provide those; callers must not infer them from GPT.
    Source attributes are observations, never target/staging growth policy.
    Failed exports retain partial output: portable pathname cleanup cannot prove
    exclusive ownership against concurrent substitution. Destination identity,
    privacy and emptiness checks detect changes but do not confine a hostile
    same-UID writer between checks.
    """
    output = Path(output)
    with regular_file(image) as stream:
        before = fingerprint(stream)
        partitions = _read_gpt(stream)
        # mkdir/O_EXCL reserve names; they do not grant cleanup authority over
        # later pathname replacements. On failure retain all partial artifacts.
        output.mkdir(mode=0o700)
        directory_fd = None
        created_files = {}
        try:
            expected = output.stat(follow_symlinks=False)
            directory_fd = os.open(output, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            identity = os.fstat(directory_fd)
            require((expected.st_dev, expected.st_ino) == (identity.st_dev, identity.st_ino),
                    'output directory substituted before pinning')
            require(identity.st_uid == os.getuid() and stat.S_IMODE(identity.st_mode) == 0o700,
                    'output directory must be private and owned by current user')
            with os.scandir(directory_fd) as entries:
                require(next(entries, None) is None, 'output directory unexpectedly contains files')

            def record_creation(filename, descriptor):
                # Called only after successful O_EXCL; partial writes are tracked
                # too. This identity is used for readback, never pathname deletion.
                created_files[filename] = None
                # Retain the object itself so inode reuse cannot defeat readback.
                created_files[filename] = os.dup(descriptor)

            def check_directory():
                current = output.stat(follow_symlinks=False)
                require((current.st_dev, current.st_ino) == (identity.st_dev, identity.st_ino),
                        'output directory replaced during export')

            components = []
            for partition in partitions:
                check_directory()
                filename = partition['role'] + '.img'
                digest = copy_extent(stream, filename, partition['offset'], partition['size'],
                                     directory_fd=directory_fd, created=record_creation)
                components.append({'role': partition['role'], 'filename': filename,
                                   'byte_size': partition['size'], 'sha256': digest,
                                   'partition_type_guid': partition['partition_type_guid'],
                                   'minimum_partition_bytes': partition['size'],
                                   'source_offset': partition['offset'],
                                   'source_attributes': partition['attributes']})
            require(before == fingerprint(stream), 'source changed during export')
            check_directory()
            for component in components:
                current = os.stat(component['filename'], dir_fd=directory_fd, follow_symlinks=False)
                pinned = os.fstat(created_files[component['filename']])
                require((current.st_dev, current.st_ino) == (pinned.st_dev, pinned.st_ino)
                        and stat.S_ISREG(current.st_mode) and current.st_size == component['byte_size'],
                        'exported component replaced or resized')
            return components
        except BaseException as error:
            # A stat→unlink/rmdir check is not atomic. With portable stdlib APIs,
            # retaining bounded artifacts is preferable to deleting another
            # writer's same-name file/directory. No pathname cleanup occurs.
            raise OSError('partition export failed; partial output retained at ' + str(output) +
                          '; exclusively created component count=' + str(len(created_files)) +
                          '; reason: ' + str(error)) from error
        finally:
            for descriptor in created_files.values():
                if descriptor is not None:
                    os.close(descriptor)
            if directory_fd is not None:
                os.close(directory_fd)


def inspect_bundle(path):
    """Check schema, actual regular-file sizes and bytes. Not an authenticity check."""
    path = Path(path)
    with regular_file(path) as stream:
        before = fingerprint(stream)
        manifest = parse_manifest(stream.read(MAX_JSON+1))
        require(before == fingerprint(stream), 'manifest changed during inspection')
    for component in manifest['components']:
        try:
            with regular_file(path.parent / component['filename']) as stream:
                before = fingerprint(stream)
                require(before[2] == component['byte_size'], 'component size mismatch')
                digest = hashlib.sha256()
                while data := stream.read(1024*1024):
                    digest.update(data)
                require(before == fingerprint(stream), 'component changed during inspection')
                require(digest.hexdigest() == component['sha256'], 'component digest mismatch')
        except OSError as error:
            raise ValueError('component is missing or not a regular file') from error
    return manifest


def inspect_uki(data):
    """Read bounded PE sections, reusing the existing UKI reader after validation."""
    require(type(data) is bytes and 512 <= len(data) <= 256*1024*1024, 'UKI size outside bounds')
    require(data[:2] == b'MZ', 'not a DOS/PE executable')
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    require(64 <= pe <= len(data)-24 and data[pe:pe+4] == b'PE\0\0', 'invalid PE signature')
    machine, count = struct.unpack_from('<HH', data, pe+4)
    architecture = {0x8664: 'x86_64', 0xaa64: 'aarch64'}.get(machine)
    require(architecture is not None and 4 <= count <= 32, 'unsupported PE machine/section count')
    optional_size = struct.unpack_from('<H', data, pe+20)[0]
    base = pe+24+optional_size
    require(optional_size >= 112 and base+40*count <= len(data), 'truncated PE headers')
    require(struct.unpack_from('<H', data, pe+24)[0] == 0x20b and
            struct.unpack_from('<H', data, pe+24+68)[0] == 10, 'not a PE32+ EFI application')
    spec = importlib.util.spec_from_file_location('qos_uki_cmdline', Path(__file__).with_name('uki-cmdline.py'))
    uki = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(uki)
    sections, ranges, virtual = {}, [], []
    for _header, name, address, size, raw_size, offset in uki.section_table(data):
        require(name not in sections, 'duplicate PE section')
        require(size > 0 and raw_size >= size and offset >= base+40*count, 'invalid PE section size')
        sections[name] = (offset, size)
        ranges.append((offset, raw_size))
        virtual.append((address, max(size, raw_size)))
    disjoint(ranges, len(data))
    disjoint(virtual, 1 << 32)
    require({'.linux', '.initrd', '.osrel', '.cmdline'} <= sections.keys(), 'required UKI section missing')
    offset, size = sections['.cmdline']
    require(size <= 4097, 'UKI command line exceeds limit')
    offset_existing, size_existing = uki.find_cmdline(data)
    require((offset, size) == (offset_existing, size_existing), 'UKI reader section disagreement')
    sealed = data[offset:offset+size].rstrip(b'\0')
    try:
        cmdline = sealed.decode('ascii')
    except UnicodeError as error:
        raise ValueError('non-ASCII UKI command line') from error
    text(cmdline, r'[\x20-\x7e]+', 4096)
    return {'uki_architecture': architecture, 'uki_cmdline': cmdline,
            'uki_sha256': hashlib.sha256(data).hexdigest()}


def specialize_uki(data, filesystem_uuid):
    """Prepare bytes only; not signed, boot-qualified, or a published bundle.

    Use the repository's equal-width UKI tool on an owned ordinary-file copy.
    No source artifact is modified. Reject signed/PCR-signature inputs rather
    than pretending their authentication survives command-line specialization.
    """
    guid(filesystem_uuid)
    info = inspect_uki(data)
    spec = importlib.util.spec_from_file_location('qos_uki_cmdline', Path(__file__).with_name('uki-cmdline.py'))
    uki = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(uki)
    require(not any(name in ('.pcrsig', '.pcrpkey', '.profile') for name, *_ in uki.sections(data)),
            'PCR signature-bearing or multi-profile specialization unsupported')
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    directories = struct.unpack_from('<I', data, pe+24+108)[0]
    if directories > 4:
        require(struct.unpack_from('<H', data, pe+20)[0] >= 152, 'truncated PE security directory')
        require(struct.unpack_from('<II', data, pe+24+112+4*8) == (0, 0),
                'signed UKI specialization unsupported')
    placeholder = 'qos.root=' + '0'*37
    tokens = info['uki_cmdline'].split()
    require(tokens.count(placeholder) == 1 and not any(t.startswith('root=') for t in tokens),
            'one exact unbound root slot required')
    digests = [t.partition('=')[2] for t in tokens if t.startswith('composefs=')]
    require(len(digests) == 1, 'one composefs identity required')
    text(digests[0], r'[0-9a-f]{128}', 128)
    locator = 'UUID=' + filesystem_uuid
    replacement = 'root=' + locator
    wanted_cmdline = info['uki_cmdline'].replace(placeholder, replacement)
    validate_cmdline(wanted_cmdline, locator, digests[0])
    with tempfile.TemporaryDirectory(prefix='qos-uki-copy-') as temporary:
        path = Path(temporary) / 'prepared.efi'
        path.write_bytes(data)
        with redirect_stdout(io.StringIO()):
            uki.main(['uki-cmdline.py', 'replace', placeholder, replacement, str(path)])
        prepared = path.read_bytes()
    offset, size = uki.find_cmdline(data)
    expected = data[:offset] + data[offset:offset+size].replace(placeholder.encode(), replacement.encode()) + data[offset+size:]
    require(prepared == expected, 'specialization changed bytes outside root slot')
    require(inspect_uki(prepared)['uki_cmdline'] == wanted_cmdline, 'specialization readback mismatch')
    return prepared


def qcow_virtual_size(header):
    """Header triage only; no conversion or backing-file resolution is performed."""
    require(type(header) is bytes and len(header) >= 104, 'truncated qcow2 header')
    magic, version = struct.unpack_from('>II', header)
    require(magic == 0x514649fb and version == 3, 'only qcow2 v3 supported')
    backing, backing_size, cluster_bits, size, crypt = struct.unpack_from('>QIIQI', header, 8)
    require(backing == 0 and backing_size == 0, 'external backing files unsupported')
    require(9 <= cluster_bits <= 21 and crypt == 0, 'qcow2 cluster/encryption unsupported')
    require(struct.unpack_from('>I', header, 60)[0] == 0 and struct.unpack_from('>Q', header, 64)[0] == 0,
            'qcow2 snapshots unsupported')
    incompatible, compatible, autoclear, refcount_order, header_length = struct.unpack_from('>QQQII', header, 72)
    require(incompatible == compatible == autoclear == 0 and refcount_order == 4 and header_length == 104,
            'qcow2 feature/extension profile unsupported (including external data files)')
    integer(size, 68*SECTOR)
    require(size % SECTOR == 0, 'unaligned virtual size')
    return size


def export_image(image, output, release):
    """Intermediate checkpoint: refuse before creating output or native resources.

    TODO: implement the reviewed artifact-bound profile, verified filesystem and
    initrd readers, exclusive raw conversion, owned read-only Linux lifecycle,
    private-copy specialization, and final publication together. Header triage
    and root-slot replacement alone must never reach successful publication.
    """
    text(release, r'[A-Za-z0-9][A-Za-z0-9._-]*', 128)
    if os.path.lexists(output):
        raise FileExistsError('output must be a new directory')
    with regular_file(image) as stream:
        qcow_virtual_size(stream.read(104))
    raise ValueError('qOS export unavailable: artifact-bound consumer/initrd version, Btrfs '
                     'default-subvolume and composefs/fs-verity closure, and ESP dependency '
                     'verification are not implemented; no conversion or output was created')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    export = commands.add_parser('export', help='report unqualified qOS export gates')
    export.add_argument('--image', required=True, type=Path)
    export.add_argument('--output', required=True, type=Path)
    export.add_argument('--release', required=True)
    inspect = commands.add_parser('inspect', help='verify manifest syntax and component bytes')
    inspect.add_argument('--manifest', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == 'export':
            export_image(args.image, args.output, args.release)
        else:
            manifest = inspect_bundle(args.manifest)
            print(json.dumps({'status': 'schema_and_component_bytes_verified',
                              'qualification': 'bootability_not_established',
                              'authenticity': 'not_established', 'manifest': manifest}, indent=2))
        return 0
    except (ValueError, OSError) as error:
        print('internal-bundle: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
