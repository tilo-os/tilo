#!/usr/bin/env python3
"""Export and inspect the schema-2 internal-installer bundle.

    internal-bundle.py export --image DISK --arch ARCH --release ID --output DIR
    internal-bundle.py inspect --manifest DIR/MANIFEST.json

`export` turns the live installer disk that os/dev/build-custom-installer.sh
produces (raw, or qcow2 converted with qemu-img) into the two staging
components the Imager writes onto an internal disk, plus a manifest the Rust
validator (apps/imager/src/setup/internal_bundle.rs) accepts byte-for-byte:

  staging-esp   the live ESP, with its one live UKI specialised to boot the
                staging root (`tilo.root=` + 37 zeros becomes
                `root=UUID=<staging root fsid>` + one space, so no PE offset
                moves) and that same UKI installed as the removable-media
                fallback loader /EFI/BOOT/BOOT{X64,AA64}.EFI.
  staging-root  the live Btrfs root, shrunk to its minimum plus 1 GiB of slack
                and truncated. Its partition keeps the live root size (>= 40
                GiB); the live installer grows the filesystem at boot.

Export needs root on Linux: it loop-attaches and mounts PRIVATE COPIES of both
filesystems (never the source image) to specialise the ESP and to run
`btrfs filesystem resize`. It refuses up front, before creating any output,
when it cannot.

What export proves structurally, offline: the GPT, both filesystem
identities, the Btrfs superblock after shrinking (and `btrfs check`), the
exact specialised UKI bytes read back from the remounted ESP at both paths,
and a manifest that validates and whose component bytes hash as recorded.
What it cannot prove, and what the VM round trip
(os/dev/test-internal-install-cycle.sh) must: that firmware loading the
fallback path runs systemd-stub, that the initrd mounts root=UUID from the
staging partition, and that the grown root boots the installer. Schema
validation establishes consistency, never release authenticity.
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
import shutil
import stat
import struct
import subprocess
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
# The live installer disk's root partition type (os/dev/disk-image.sh). It is
# accepted only as an export source; the staging root is always ROOT_GUID.
TILO_ROOT_GUID = '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab'
GUID_PATTERN = r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
GROW_FILESYSTEM = 1 << 59
MIB = 1 << 20
GIB = 1 << 30

SCHEMA = 2
PURPOSE = 'tilo-internal-installer'
# build-custom-installer.sh never sizes the live root below this floor; the
# staging root partition is at least this large even though its image is not.
LIVE_ROOT_FLOOR = 40 * GIB
ROOT_SLACK = GIB
FALLBACK_LOADERS = {'x86_64': '/EFI/BOOT/BOOTX64.EFI', 'aarch64': '/EFI/BOOT/BOOTAA64.EFI'}
# Length-stable live placeholder and its binding: 47 bytes each.
ROOT_PLACEHOLDER = 'tilo.root=' + '0' * 37
# The specialised live command line: flags and exact values. tilo.generation
# (dense positive integer) and at most two known consoles are checked apart.
LIVE_FLAGS = frozenset({'rw', 'quiet', 'splash', 'slab_nomerge', 'plymouth.ignore-serial-consoles',
                        'tilo.install'})
LIVE_VALUES = {'tilo.mode': 'normal', 'tilo.luks': '0' * 45, 'tilo.unlock': '0' * 75,
               'init_on_alloc': '1', 'init_on_free': '1', 'lockdown': 'integrity',
               'randomize_kstack_offset': 'on', 'page_alloc.shuffle': '1', 'vsyscall': 'none',
               'preempt': 'full'}


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
    require(type(manifest['schema']) is int and manifest['schema'] == SCHEMA, 'unsupported schema')
    require(manifest['purpose'] == PURPOSE, 'wrong bundle purpose')
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
        # The staging root is the minimised filesystem; its partition keeps the
        # live root floor, and the live installer grows the filesystem at boot.
        floor = max(size, LIVE_ROOT_FLOOR) if role == 'staging-root' else size
        integer(component['minimum_partition_bytes'], floor)
        text(component['sha256'], r'[0-9a-f]{64}', 64)
        part_type = guid(component['partition_type_guid'])
        if role == 'staging-esp':
            require(part_type == ESP_GUID and component['filesystem_type'] == 'vfat', 'unsupported ESP')
            text(component['filesystem_uuid'], r'[0-9A-F]{4}-[0-9A-F]{4}', 9)
            require(component['filesystem_uuid'] != '0000-0000', 'zero FAT identity')
        else:
            # Deliberately the generic Linux type, never the Tilo root type:
            # installed-target discovery must keep seeing exactly one Tilo root.
            require(part_type == ROOT_GUID and component['filesystem_type'] == 'btrfs',
                    'unsupported root partition')
            guid(component['filesystem_uuid'])
    disjoint([(c['source_offset'], c['byte_size']) for c in components], image_size)
    boot = manifest['boot_binding']
    keys(boot, 'efi_path uki_architecture uki_sha256 uki_cmdline root_locator composefs_digest')
    require(boot['uki_architecture'] == arch, 'UKI architecture mismatch')
    require(boot['efi_path'] == FALLBACK_LOADERS[arch], 'EFI path must be the removable-media fallback loader')
    text(boot['uki_sha256'], r'[0-9a-f]{64}', 64)
    text(boot['composefs_digest'], r'[0-9a-f]{128}', 128)
    expected = 'UUID=' + by_role['staging-root']['filesystem_uuid']
    require(boot['root_locator'] == expected, 'root UUID binding mismatch')
    validate_cmdline(boot['uki_cmdline'], expected, boot['composefs_digest'])


def validate_cmdline(cmdline, root_locator, digest):
    """Accept exactly the specialised live installer command line.

    The allow-list is the token set os/Containerfile.installer leaves on the
    live UKI after projecting /usr/lib/bootc/kargs.d, with the root slot bound.
    os/tests/internal_bundle_test.py derives that set from those files, so a
    kargs change that this profile does not know fails there, not in the field.
    Expansion requires consumer evidence, not ignored arguments.
    """
    text(cmdline, r'[\x20-\x7e]+', 4096)
    require(not any(c in cmdline for c in '\\"\''), 'quoted or escaped command line unsupported')
    tokens = cmdline.split()
    require(len(tokens) <= 64, 'too many kernel arguments')
    require(not any(token.startswith('qos.') for token in tokens), 'legacy qos.* kernel argument')
    names = [token.split('=', 1)[0] for token in tokens]
    require(all(names.count(name) == 1 for name in names if name != 'console'), 'duplicate kernel argument')
    consoles = [token for token in tokens if token.startswith('console=')]
    require(len(set(consoles)) == len(consoles) and len(consoles) <= 2, 'duplicate console argument')
    require('root=' + root_locator in tokens and 'composefs=' + digest in tokens
            and 'tilo.install' in tokens, 'missing or mismatched measured boot binding')
    allowed_values = dict(LIVE_VALUES, root=root_locator, composefs=digest)
    for token in tokens:
        if token in LIVE_FLAGS:
            continue
        name, separator, value = token.partition('=')
        if name == 'tilo.generation' and re.fullmatch(r'[1-9][0-9]{0,9}', value):
            continue
        if name == 'console' and value in ('tty0', 'ttyS0,115200'):
            continue
        require(separator and name in allowed_values and value == allowed_values[name],
                'unrecognized command-line binding')
    require('rw' in tokens, 'composefs physical root requires rw')


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
        # systemd-repart's GrowFileSystem=yes sets bit 59 on the live Tilo root.
        growable = (*ROOT_GUIDS.values(), TILO_ROOT_GUID)
        allowed_attributes = (0, GROW_FILESYSTEM) if kind in growable else (0,)
        require(attrs in allowed_attributes, 'unsupported partition attributes for type')
        role = 'staging-esp' if kind == ESP_GUID else 'staging-root'
        require(kind in (ESP_GUID, ROOT_GUID, TILO_ROOT_GUID, *ROOT_GUIDS.values()),
                'unsupported partition type')
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


def uki_tool():
    spec = importlib.util.spec_from_file_location('tilo_uki_cmdline', Path(__file__).with_name('uki-cmdline.py'))
    uki = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(uki)
    return uki


def root_binding(filesystem_uuid):
    """The exact bytes that replace ROOT_PLACEHOLDER: same length, one trailing space."""
    guid(filesystem_uuid)
    replacement = 'root=UUID=' + filesystem_uuid + ' '
    require(len(replacement) == len(ROOT_PLACEHOLDER), 'root binding is not length-stable')
    return replacement


def specialize_uki(data, filesystem_uuid):
    """Prepare bytes only; not signed, boot-qualified, or a published bundle.

    Use the repository's equal-width UKI tool on an owned ordinary-file copy.
    No source artifact is modified. Reject signed/PCR-signature inputs rather
    than pretending their authentication survives command-line specialization.
    """
    replacement = root_binding(filesystem_uuid)
    info = inspect_uki(data)
    uki = uki_tool()
    require(not any(name in ('.pcrsig', '.pcrpkey', '.profile') for name, *_ in uki.sections(data)),
            'PCR signature-bearing or multi-profile specialization unsupported')
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    directories = struct.unpack_from('<I', data, pe+24+108)[0]
    if directories > 4:
        require(struct.unpack_from('<H', data, pe+20)[0] >= 152, 'truncated PE security directory')
        require(struct.unpack_from('<II', data, pe+24+112+4*8) == (0, 0),
                'signed UKI specialization unsupported')
    tokens = info['uki_cmdline'].split()
    require(tokens.count(ROOT_PLACEHOLDER) == 1 and not any(t.startswith('root=') for t in tokens),
            'one exact unbound root slot required')
    digests = [t.partition('=')[2] for t in tokens if t.startswith('composefs=')]
    require(len(digests) == 1, 'one composefs identity required')
    text(digests[0], r'[0-9a-f]{128}', 128)
    locator = 'UUID=' + filesystem_uuid
    wanted_cmdline = info['uki_cmdline'].replace(ROOT_PLACEHOLDER, replacement)
    validate_cmdline(wanted_cmdline, locator, digests[0])
    with tempfile.TemporaryDirectory(prefix='tilo-uki-copy-') as temporary:
        path = Path(temporary) / 'prepared.efi'
        path.write_bytes(data)
        with redirect_stdout(io.StringIO()):
            uki.main(['uki-cmdline.py', 'replace', ROOT_PLACEHOLDER, replacement, str(path)])
        prepared = path.read_bytes()
    offset, size = uki.find_cmdline(data)
    expected = (data[:offset] +
                data[offset:offset+size].replace(ROOT_PLACEHOLDER.encode(), replacement.encode()) +
                data[offset+size:])
    require(len(prepared) == len(data) and prepared == expected,
            'specialization changed bytes outside root slot')
    require(list(uki.section_table(prepared)) == list(uki.section_table(data)),
            'specialization moved a PE section')
    require(inspect_uki(prepared)['uki_cmdline'] == wanted_cmdline, 'specialization readback mismatch')
    return prepared


def strip_authenticode(data):
    """Return an unsigned copy of a UKI whose only signature is a trailing table.

    Specialising the command line invalidates any Authenticode signature, and
    the internal flow requires Secure Boot to be disabled. Dropping the table
    explicitly keeps the staged loader honest: it is unsigned, not "signed"
    with a signature that no longer verifies. Only the canonical layout is
    accepted: certificate table at the very end, after every section, made of
    well-formed WIN_CERTIFICATE PKCS#7 entries. An unsigned input is returned
    unchanged.
    """
    info = inspect_uki(data)
    del info
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    if struct.unpack_from('<I', data, pe+24+108)[0] <= 4:
        return data
    require(struct.unpack_from('<H', data, pe+20)[0] >= 152, 'truncated PE security directory')
    directory = pe+24+112+4*8
    offset, size = struct.unpack_from('<II', data, directory)
    if (offset, size) == (0, 0):
        return data
    sections_end = max(raw + rsz for _n, raw, rsz, _v in uki_tool().sections(data))
    require(offset % 8 == 0 and offset >= sections_end and offset + size == len(data),
            'certificate table is not the trailing file region')
    cursor = offset
    while cursor < len(data):
        require(cursor + 8 <= len(data), 'truncated certificate entry')
        length, revision, kind = struct.unpack_from('<IHH', data, cursor)
        require(length >= 8 and cursor + length <= len(data) and revision == 0x0200 and kind == 0x0002,
                'unsupported certificate entry')
        cursor += (length + 7) & ~7
    require(cursor == len(data), 'certificate table has trailing bytes')
    unsigned = bytearray(data[:offset])
    struct.pack_into('<II', unsigned, directory, 0, 0)
    unsigned = bytes(unsigned)
    require(inspect_uki(unsigned)['uki_cmdline'] == inspect_uki(data)['uki_cmdline'],
            'signature removal changed the command line')
    return unsigned


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


def bundle_names(arch):
    """Release asset names. The Imager fetches each component next to the manifest."""
    require(arch in FALLBACK_LOADERS, 'unsupported architecture')
    stem = 'tilo-internal-installer-' + arch
    return {'manifest': stem + '.json', 'staging-esp': stem + '-esp.img', 'staging-root': stem + '-root.img'}


def align_up(value, alignment):
    return (value + alignment - 1) // alignment * alignment


def fat_serial(boot):
    """The FAT volume serial as blkid names it (`XXXX-XXXX`), from a boot sector."""
    require(type(boot) is bytes and len(boot) >= 512 and boot[510:512] == b'\x55\xaa',
            'missing FAT boot sector signature')
    require(struct.unpack_from('<H', boot, 11)[0] in (512, 1024, 2048, 4096), 'unsupported FAT sector size')
    # FAT32 has no 16-bit FAT size; its extended BPB sits 28 bytes later.
    extended = 0x42 if struct.unpack_from('<H', boot, 22)[0] == 0 else 0x26
    require(boot[extended] == 0x29, 'FAT volume serial is absent')
    serial = struct.unpack_from('<I', boot, extended + 1)[0]
    require(serial != 0, 'zero FAT identity')
    return '%04X-%04X' % (serial >> 16, serial & 0xffff)


def _crc32c_table():
    table = []
    for byte in range(256):
        crc = byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82f63b78 if crc & 1 else 0)
        table.append(crc)
    return table


CRC32C_TABLE = _crc32c_table()


def crc32c(data):
    crc = 0xffffffff
    for byte in data:
        crc = CRC32C_TABLE[(crc ^ byte) & 0xff] ^ (crc >> 8)
    return crc ^ 0xffffffff


BTRFS_SUPER_OFFSET = 0x10000
BTRFS_SUPER_SIZE = 4096
BTRFS_DEV_ITEM = 0xc9


def btrfs_superblock(block):
    """Checked facts from the primary Btrfs superblock (4 KiB at 64 KiB)."""
    require(type(block) is bytes and len(block) == BTRFS_SUPER_SIZE, 'truncated Btrfs superblock')
    require(block[0x40:0x48] == b'_BHRfS_M', 'not a Btrfs filesystem')
    require(struct.unpack_from('<Q', block, 0x30)[0] == BTRFS_SUPER_OFFSET, 'not the primary superblock')
    # crc32c is the mkfs.btrfs default and the only checksum verified here.
    require(struct.unpack_from('<H', block, 0xc4)[0] == 0, 'unsupported Btrfs checksum type')
    require(block[4:0x20] == bytes(28) and struct.unpack_from('<I', block, 0)[0] == crc32c(block[0x20:]),
            'Btrfs superblock checksum mismatch')
    fsid = str(uuid.UUID(bytes=block[0x20:0x30]))
    guid(fsid)
    total, _used, _root_dir, devices = struct.unpack_from('<QQQQ', block, 0x70)
    require(devices == 1, 'multi-device Btrfs is unsupported')
    sectorsize = struct.unpack_from('<I', block, 0x90)[0]
    require(sectorsize in (4096, 16384, 65536), 'unsupported Btrfs sector size')
    device_total = struct.unpack_from('<Q', block, BTRFS_DEV_ITEM + 8)[0]
    return {'fsid': fsid, 'total_bytes': total, 'device_total_bytes': device_total,
            'sectorsize': sectorsize}


def read_btrfs_superblock(path):
    with regular_file(path) as stream:
        return btrfs_superblock(exact(stream, BTRFS_SUPER_OFFSET, BTRFS_SUPER_SIZE))


def plan_root_size(minimum, partition_size):
    """Minimum device size plus slack, 1 MiB aligned, never beyond the partition."""
    integer(minimum)
    integer(partition_size)
    return min(align_up(minimum + ROOT_SLACK, MIB), partition_size)


def sha256_file(path):
    digest = hashlib.sha256()
    with regular_file(path) as stream:
        while data := stream.read(MIB):
            digest.update(data)
    return digest.hexdigest()


def copy_sparse(stream, destination, offset, size, *, directory_fd):
    """copy_extent for a mostly empty filesystem: all-zero MiBs become holes."""
    stream.seek(offset)
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=directory_fd)
    zero = bytes(MIB)
    with os.fdopen(descriptor, 'wb') as output:
        remaining = size
        while remaining:
            data = stream.read(min(MIB, remaining))
            require(data, 'truncated source during export')
            if data == zero[:len(data)]:
                output.seek(len(data), os.SEEK_CUR)
            else:
                output.write(data)
            remaining -= len(data)
        output.truncate(size)
        output.flush()
        os.fsync(output.fileno())


class System:
    """The privileged Linux operations export needs, applied to private copies only.

    Tests substitute a fake; nothing here is reached without require_privileges.
    """

    TOOLS = ('losetup', 'mount', 'umount', 'btrfs')

    def require_privileges(self, qcow2=False):
        if not sys.platform.startswith('linux'):
            raise PermissionError('internal-bundle export needs Linux (loop devices, vfat and Btrfs mounts)')
        if os.geteuid() != 0:
            raise PermissionError('internal-bundle export needs root: it loop-mounts private copies of the '
                                  'ESP and root and shrinks the root with `btrfs filesystem resize`; '
                                  'rerun it with sudo')
        missing = [tool for tool in self.TOOLS + (('qemu-img',) if qcow2 else ()) if not shutil.which(tool)]
        if missing:
            raise PermissionError('internal-bundle export is missing required tools: ' + ' '.join(missing))

    def run(self, argv):
        result = subprocess.run([str(part) for part in argv], stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, check=False)
        if result.returncode != 0:
            raise OSError(' '.join(str(part) for part in argv) + ' failed: ' +
                          (result.stderr or result.stdout).strip())
        return result.stdout

    def convert_qcow2(self, source, destination):
        self.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', source, destination])

    def attach(self, path):
        device = self.run(['losetup', '--find', '--show', '--nooverlap', path]).strip()
        require(re.fullmatch(r'/dev/loop[0-9]+', device), 'unexpected loop device name')
        return device

    def detach(self, device):
        self.run(['losetup', '--detach', device])

    def mount(self, device, target, fstype, options):
        self.run(['mount', '-t', fstype, '-o', options, device, target])

    def umount(self, target):
        self.run(['umount', target])

    def btrfs_min_dev_size(self, mountpoint):
        output = self.run(['btrfs', 'inspect-internal', 'min-dev-size', '--id', '1', mountpoint])
        match = re.match(r'\s*([0-9]+) bytes', output)
        require(match, 'unrecognized btrfs min-dev-size output')
        return int(match.group(1))

    def btrfs_resize(self, mountpoint, size):
        self.run(['btrfs', 'filesystem', 'resize', '1:%d' % size, mountpoint])
        self.run(['btrfs', 'filesystem', 'sync', mountpoint])

    def btrfs_check(self, image):
        self.run(['btrfs', 'check', '--readonly', image])


@contextmanager
def mounted(system, image, mountpoint, fstype, options):
    """Loop-mount a private image file; always unmount and detach, reporting failures."""
    mountpoint.mkdir(mode=0o700)
    device = system.attach(image)
    mounted_ok = False
    try:
        system.mount(device, mountpoint, fstype, options)
        mounted_ok = True
        yield mountpoint
    finally:
        errors = []
        if mounted_ok:
            try:
                system.umount(mountpoint)
            except OSError as error:
                errors.append(str(error))
        try:
            system.detach(device)
        except OSError as error:
            errors.append(str(error))
        if errors:
            raise OSError('cleanup of ' + str(mountpoint) + ' failed: ' + '; '.join(errors))
        mountpoint.rmdir()


def _regular(path):
    return stat.S_ISREG(os.lstat(path).st_mode)


def live_uki_path(esp):
    """The one live UKI under /EFI/Linux (bootc nests it one directory deeper)."""
    linux = esp / 'EFI' / 'Linux'
    require(linux.is_dir() and not linux.is_symlink(), 'live ESP has no /EFI/Linux')
    found = []
    for directory, subdirectories, files in os.walk(linux, followlinks=False):
        subdirectories.sort()
        for name in sorted(files):
            if name.lower().endswith('.efi'):
                path = Path(directory) / name
                require(_regular(path), 'live UKI is not a regular file')
                found.append(path)
    require(len(found) == 1, 'expected exactly one live UKI under /EFI/Linux, found %d' % len(found))
    return found[0]


def _replace_contents(path, data):
    with open(path, 'r+b') as output:
        output.truncate(0)
        output.write(data)
        output.flush()
        os.fsync(output.fileno())


def specialize_esp_tree(esp, filesystem_uuid, arch):
    """Specialise the live UKI in a mounted ESP and install it as the fallback loader.

    Firmware boots `\\EFI\\BOOT\\BOOT{X64,AA64}.EFI` from the owned Boot####
    entry. Making that file the specialised UKI itself, rather than trusting
    systemd-boot's loader.conf to pick it, leaves one boot path to prove:
    systemd-stub runs, sets LoaderDevicePartUUID and mounts root=UUID. The
    UKI under /EFI/Linux gets the same bytes so a systemd-boot started by any
    other route still reaches the staging root, never the unbound placeholder.
    """
    uki = live_uki_path(esp)
    fallback = esp / FALLBACK_LOADERS[arch].lstrip('/')
    require(fallback.exists() and _regular(fallback), 'live ESP has no removable-media fallback loader')
    unsigned = strip_authenticode(uki.read_bytes())
    require(inspect_uki(unsigned)['uki_architecture'] == arch, 'live UKI architecture mismatch')
    prepared = specialize_uki(unsigned, filesystem_uuid)
    _replace_contents(uki, prepared)
    _replace_contents(fallback, prepared)
    return uki.relative_to(esp), prepared


def verify_esp_tree(esp, uki, arch, prepared):
    """Read both loader paths back and require the exact specialised bytes."""
    for path in (esp / uki, esp / FALLBACK_LOADERS[arch].lstrip('/')):
        require(_regular(path) and path.read_bytes() == prepared,
                'specialised loader readback mismatch: ' + str(path.relative_to(esp)))
    return inspect_uki(prepared)


def minimise_root(system, image, work):
    """Shrink a private Btrfs copy to its minimum + slack; truncate; check it."""
    size = image.stat().st_size
    before = read_btrfs_superblock(image)
    require(before['total_bytes'] == before['device_total_bytes'] <= size,
            'Btrfs size disagrees with its partition')
    with mounted(system, image, work / 'root', 'btrfs', 'rw,noatime,nodev,nosuid') as mountpoint:
        target = plan_root_size(system.btrfs_min_dev_size(mountpoint), before['total_bytes'])
        if target < before['total_bytes']:
            system.btrfs_resize(mountpoint, target)
    after = read_btrfs_superblock(image)
    require(after['fsid'] == before['fsid'], 'Btrfs identity changed while shrinking')
    require(after['total_bytes'] == after['device_total_bytes'] == target,
            'Btrfs did not shrink to the planned size')
    os.truncate(image, target)
    system.btrfs_check(image)
    return target, after['fsid']


def _cleanup_work(work, created):
    """Remove only what export created in its private work directory."""
    for path in created:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
    try:
        work.rmdir()
    except OSError as error:
        # A mount that failed to detach keeps its mountpoint; never recurse into it.
        print('internal-bundle: work directory retained at ' + str(work) + ': ' + str(error),
              file=sys.stderr)


def export_image(image, output, release, arch, *, work_dir=None, system=None):
    """Produce the schema-2 bundle for one architecture into a new directory.

    Output (0600 files in a new 0700 directory): the manifest and the two
    component images named by bundle_names(arch). The source image is only
    read. On failure the partial output is retained and named in the error;
    no manifest is written unless every check has passed.
    """
    text(release, r'[A-Za-z0-9][A-Za-z0-9._-]*', 128)
    names = bundle_names(arch)
    output = Path(output)
    if os.path.lexists(output):
        raise FileExistsError('output must be a new directory')
    system = system or System()
    with regular_file(image) as stream:
        header = stream.read(104)
    qcow2 = header[:4] == b'QFI\xfb'
    if qcow2:
        qcow_virtual_size(header)
    # Refuse before creating anything: no work directory, no output.
    system.require_privileges(qcow2=qcow2)
    work = Path(tempfile.mkdtemp(prefix='.tilo-bundle-work-', dir=work_dir or output.parent))
    created = []
    try:
        raw = image
        if qcow2:
            raw = work / 'source.raw'
            created.append(raw)
            system.convert_qcow2(image, raw)
        output.mkdir(mode=0o700)
        directory_fd = os.open(output, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            with regular_file(raw) as stream:
                before = fingerprint(stream)
                source_bytes = before[2]
                partitions = {p['role']: p for p in _read_gpt(stream)}
                esp, root = partitions['staging-esp'], partitions['staging-root']
                require(root['partition_type_guid'] == TILO_ROOT_GUID, 'live image root is not a Tilo root')
                require(root['size'] >= LIVE_ROOT_FLOOR, 'live root is below the 40 GiB floor')
                copy_extent(stream, names['staging-esp'], esp['offset'], esp['size'],
                            directory_fd=directory_fd)
                copy_sparse(stream, names['staging-root'], root['offset'], root['size'],
                            directory_fd=directory_fd)
                require(before == fingerprint(stream), 'source changed during export')
        finally:
            os.close(directory_fd)
        esp_image, root_image = output / names['staging-esp'], output / names['staging-root']
        filesystem_uuid = read_btrfs_superblock(root_image)['fsid']
        with mounted(system, esp_image, work / 'esp', 'vfat', 'rw,nodev,nosuid,noexec,flush') as tree:
            uki, prepared = specialize_esp_tree(tree, filesystem_uuid, arch)
        with mounted(system, esp_image, work / 'esp', 'vfat', 'ro,nodev,nosuid,noexec') as tree:
            info = verify_esp_tree(tree, uki, arch, prepared)
        root_bytes, fsid = minimise_root(system, root_image, work)
        require(fsid == filesystem_uuid, 'root identity changed after specialisation')
        with regular_file(esp_image) as stream:
            serial = fat_serial(exact(stream, 0, SECTOR))
        digest = [t.partition('=')[2] for t in info['uki_cmdline'].split() if t.startswith('composefs=')]
        require(len(digest) == 1, 'one composefs identity required')
        manifest = {
            'schema': SCHEMA, 'purpose': PURPOSE, 'release_id': release, 'architecture': arch,
            'firmware': 'uefi', 'secure_boot': 'disabled', 'installer_protocol': 1,
            'source_image_bytes': source_bytes,
            'components': [
                {'role': 'staging-esp', 'filename': names['staging-esp'], 'byte_size': esp['size'],
                 'sha256': sha256_file(esp_image), 'partition_type_guid': ESP_GUID,
                 'filesystem_type': 'vfat', 'filesystem_uuid': serial,
                 'minimum_partition_bytes': esp['size'], 'source_offset': esp['offset']},
                {'role': 'staging-root', 'filename': names['staging-root'], 'byte_size': root_bytes,
                 'sha256': sha256_file(root_image), 'partition_type_guid': ROOT_GUID,
                 'filesystem_type': 'btrfs', 'filesystem_uuid': filesystem_uuid,
                 'minimum_partition_bytes': root['size'], 'source_offset': root['offset']}],
            'boot_binding': {
                'efi_path': FALLBACK_LOADERS[arch], 'uki_architecture': info['uki_architecture'],
                'uki_sha256': info['uki_sha256'], 'uki_cmdline': info['uki_cmdline'],
                'root_locator': 'UUID=' + filesystem_uuid, 'composefs_digest': digest[0]}}
        validate_manifest(manifest)
        encoded = (json.dumps(manifest, indent=2, sort_keys=True) + '\n').encode()
        require(parse_manifest(encoded) == manifest, 'manifest encoding round trip mismatch')
        descriptor = os.open(output / names['manifest'],
                             os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        require(inspect_bundle(output / names['manifest']) == manifest, 'bundle readback mismatch')
        return manifest
    except BaseException as error:
        raise OSError('internal bundle export failed; partial output retained at ' + str(output) +
                      '; reason: ' + str(error)) from error
    finally:
        _cleanup_work(work, created)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    export = commands.add_parser('export', help='export the schema-2 bundle from a live installer disk (root)')
    export.add_argument('--image', required=True, type=Path, help='raw or qcow2 live installer disk')
    export.add_argument('--arch', required=True, choices=sorted(FALLBACK_LOADERS))
    export.add_argument('--release', required=True, help='release identity recorded in the manifest')
    export.add_argument('--output', required=True, type=Path, help='new directory for the bundle')
    export.add_argument('--work-dir', type=Path, help='scratch parent (default: beside --output)')
    inspect = commands.add_parser('inspect', help='verify manifest syntax and component bytes')
    inspect.add_argument('--manifest', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == 'export':
            manifest = export_image(args.image, args.output, args.release, args.arch, work_dir=args.work_dir)
            path = args.output / bundle_names(args.arch)['manifest']
            print(json.dumps({'status': 'exported', 'qualification': 'bootability_not_established',
                              'manifest': path.name, 'manifest_sha256': sha256_file(path),
                              'manifest_size': path.stat().st_size,
                              'release_id': manifest['release_id'], 'arch': manifest['architecture'],
                              'components': [c['filename'] for c in manifest['components']]}, indent=2))
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
