#!/usr/bin/python3
"""Live-installer side of USB-free installation (design spec §6).

Tilo Imager stages a live installer on the same disk as the retained OS and
leaves `/EFI/tilo-staging/handoff.json` on the staging ESP. That file is a
*hint, never authority*: every subcommand here re-derives the facts it acts on
from firmware variables, the partition table and the mount table, and treats
the handoff only as the list of identities those facts must equal.

Subcommands (all run as root from the live medium):

  detect --out FILE --error FILE
      Before greetd: if the firmware loader ran from a staging ESP whose handoff
      names that same ESP, publish a validated copy for the unprivileged UI.
  check-install --handoff FILE --disk DEV --mode guided|erase|prepared [...]
      tilo-install's replacement for the live-disk refusal. Prints KEY=VALUE
      facts on success; any mismatch is a refusal (exit 1).
  check-mounts --handoff FILE --disk DEV
      Only the two staging partitions may be mounted from the target disk.
  write-reclaim-record --handoff FILE --target-var DIR [...]
      Record the identities `tilo-staging-reclaim` needs on the installed system.
  firmware-finalize --handoff FILE --target-esp-partuuid UUID [...]
      After a successful install: delete the owned staging Boot#### when its
      bytes still point at the staging ESP, then set BootNext to the installed
      target. BootOrder is never written except to undo efibootmgr's own
      insertion when it had to create the target entry.

Test seams (never set on a real system): TILO_STAGING_EFIVARS,
TILO_STAGING_ESP_DIR, TILO_STAGING_LSBLK_JSON, TILO_STAGING_EFIBOOTMGR.
"""
import json
import os
import re
import stat
import struct
import subprocess
import sys
import tempfile
import uuid

HANDOFF_MAX_BYTES = 64 * 1024
HANDOFF_RELATIVE = ('EFI', 'tilo-staging', 'handoff.json')
EFI_GLOBAL = '8be4df61-93ca-11d2-aa0d-00e098032b8c'
LOADER_VENDOR = '4a67b082-0a4c-41cf-b6c7-440b29bb8c4f'
ESP_TYPE = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
# The Tilo staging root type; only the live installer UKI resolves it
# (os/usr/libexec/tilo-root-discover).
STAGING_ROOT_TYPE = 'd4c4daf6-f72c-43ee-b8e3-5ba32550311d'
GUIDED_EFI_BYTES = 2 * 1024 * 1024 * 1024
GUIDED_BOOT_BYTES = 1024 * 1024 * 1024
MIN_GUIDED_ROOT_BYTES = 32 * 1024 * 1024 * 1024
MIB = 1024 * 1024
MAX_LBA = 1 << 62
GUID_RE = re.compile(r'^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$')
BOOT_ID_RE = re.compile(r'^[0-9A-F]{4}$')
PROFILE_KEYS = {'schema', 'hostname', 'username', 'full_name', 'locale', 'keyboard',
                'timezone', 'filesystem', 'prepared_setup', 'device_label', 'purpose'}
HANDOFF_KEYS = {'schema', 'operation_id', 'host_os', 'disk_guid', 'disk_size_bytes',
                'staging_esp', 'staging_root', 'target_extent', 'owned_boot_entry',
                'original_boot_order', 'profile'}
LIVE_MOUNT_PREFIXES = ('/sysroot', '/run/initramfs/live', '/run/rootfsbase')


class Refusal(Exception):
    """A fact did not match; the caller must leave everything untouched."""


def efivars_dir():
    return os.environ.get('TILO_STAGING_EFIVARS', '/sys/firmware/efi/efivars')


def efibootmgr():
    return os.environ.get('TILO_STAGING_EFIBOOTMGR', 'efibootmgr')


# --- strict handoff parsing ---------------------------------------------------

def _reject_constant(name):
    raise Refusal('handoff contains a non-finite number: ' + name)


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise Refusal('handoff repeats the field ' + repr(key))
        result[key] = value
    return result


def _int(value, label, minimum=0):
    if type(value) is not int or value < minimum or value >= MAX_LBA:
        raise Refusal('handoff field %s is not a bounded integer' % label)
    return value


def _guid(value, label):
    if not isinstance(value, str) or not GUID_RE.match(value):
        raise Refusal('handoff field %s is not a GUID' % label)
    return value.lower()


def _extent(value, label, with_partuuid):
    keys = {'first_lba', 'last_lba'} | ({'partuuid'} if with_partuuid else set())
    if not isinstance(value, dict) or set(value) != keys:
        raise Refusal('handoff field %s has unexpected shape' % label)
    first = _int(value['first_lba'], label + '.first_lba', 1)
    last = _int(value['last_lba'], label + '.last_lba', 1)
    if last < first:
        raise Refusal('handoff field %s ends before it starts' % label)
    extent = {'first_lba': first, 'last_lba': last}
    if with_partuuid:
        extent['partuuid'] = _guid(value['partuuid'], label + '.partuuid')
    return extent


def overlaps(a, b):
    return a['first_lba'] <= b['last_lba'] and b['first_lba'] <= a['last_lba']


def validate_flatpak_inventory(raw, operation, handoff=None):
    def fields(value, expected):
        if not isinstance(value, dict) or set(value) != set(expected):
            raise Refusal('Flatpak metadata fields differ from schema 1')
    def bounded_list(value, limit):
        if not isinstance(value, list) or len(value) > limit:
            raise Refusal('Flatpak metadata list exceeds its bound')
        return value
    def digest(value):
        if not isinstance(value, list) or len(value) != 32 or any(type(v) is not int or not 0 <= v <= 255 for v in value):
            raise Refusal('Flatpak trust digest is malformed')
    def scope(value):
        if value == 'User':
            return
        fields(value, ('System',))
        if not isinstance(value['System'], str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', value['System']):
            raise Refusal('Flatpak installation scope is malformed')
    def strings(value, names):
        for name in names:
            if not isinstance(value[name], str) or len(value[name]) > 4096 or any(ord(c) < 32 for c in value[name]):
                raise Refusal('Flatpak metadata string is malformed')
    def installed(value):
        fields(value, ('reference', 'origin', 'installation', 'name'))
        scope(value['installation'])
        strings(value, ('reference', 'origin', 'name'))
    def remote(value):
        fields(value, ('installation', 'name', 'url', 'options'))
        scope(value['installation'])
        strings(value, ('name', 'url', 'options'))
        from urllib.parse import urlsplit
        url = urlsplit(value['url'])
        if url.scheme != 'https' or not url.hostname or url.username or url.password or url.query or url.fragment:
            raise Refusal('Flatpak remote contains unsupported trust or account data')
    fields(raw, ('schema', 'inventory', 'selected', 'installations'))
    if type(raw['schema']) is not int or raw['schema'] != 1:
        raise Refusal('Flatpak inventory version is unsupported')
    fields(raw['inventory'], ('refs', 'remotes'))
    for value in bounded_list(raw['inventory']['refs'], 100000):
        installed(value)
    for value in bounded_list(raw['selected'], 100000):
        installed(value)
    for value in bounded_list(raw['inventory']['remotes'], 4096):
        remote(value)
    for source in bounded_list(raw['installations'], 64):
        fields(source, ('installation', 'source_plan', 'repositories', 'deployments', 'config_sha256'))
        scope(source['installation'])
        digest(source['config_sha256'])
        encoded = bounded_list(source['source_plan'], HANDOFF_MAX_BYTES)
        if any(type(byte) is not int or not 0 <= byte <= 255 for byte in encoded):
            raise Refusal('Flatpak stable source plan is malformed')
        try:
            plan = json.loads(bytes(encoded))
        except (ValueError, UnicodeError) as error:
            raise Refusal('Flatpak source plan is malformed') from error
        if not isinstance(plan, dict) or 'flatpak_inventory' in plan or plan.get('operation_id') != operation:
            raise Refusal('Flatpak source plan is recursive or belongs to another operation')
        validate_migration(plan, handoff)
        for trust in bounded_list(source['repositories'], 4096):
            fields(trust, ('remote', 'definition_sha256'))
            remote(trust['remote'])
            digest(trust['definition_sha256'])
        for deploy in bounded_list(source['deployments'], 100000):
            fields(deploy, ('reference', 'commit', 'deploy_sha256'))
            strings(deploy, ('reference', 'commit'))
            if not re.fullmatch('[0-9a-f]{64}', deploy['commit']):
                raise Refusal('Flatpak deployment commit is malformed')
            digest(deploy['deploy_sha256'])
    return raw


def validate_migration(raw, handoff=None):
    """Secret-free consent and source binding, never a mount/copy authority."""
    required = {'schema', 'operation_id', 'categories', 'home'}
    if not isinstance(raw, dict) or not required <= set(raw) or set(raw) - required - {'flatpak_inventory', 'home_policy', 'pending_computer_applications_scope', 'deferred_handoff'}:
        raise Refusal('migration plan fields differ from schema 1')
    if (type(raw['schema']) is not int or raw['schema'] != 1
            or (handoff is not None and raw['operation_id'] != handoff['operation_id'])):
        raise Refusal('migration plan operation differs from the installation')
    _guid(raw['operation_id'], 'migration operation_id')
    categories = raw['categories']
    if (not isinstance(categories, dict) or set(categories) != {'home', 'flatpak', 'accounts'}
            or any(type(value) is not bool for value in categories.values()) or not any(categories.values())):
        raise Refusal('migration categories are malformed or empty')
    policy = raw.get('home_policy', 'copy')
    if (policy not in ('copy', 'access-read-only', 'access-read-write')
            or (policy != 'copy' and not categories['home'])):
        raise Refusal('migration home policy is malformed or lacks home consent')
    home = raw['home']
    if not isinstance(home, dict) or set(home) != {'volume', 'relative', 'inode', 'original_uid', 'subvolume'}:
        raise Refusal('migration home fields are malformed')
    inode = _int(home['inode'], 'migration home inode', 1)
    uid = _int(home['original_uid'], 'migration source uid')
    if uid > 0xffffffff:
        raise Refusal('migration source uid exceeds its range')
    deferred = raw.get('deferred_handoff')
    if deferred is not None:
        if (not isinstance(deferred, dict)
                or set(deferred) != {'schema', 'original_uid', 'journal_sha256'}
                or type(deferred['schema']) is not int or deferred['schema'] != 1
                or type(deferred['original_uid']) is not int
                or deferred['original_uid'] != 1000
                or deferred['original_uid'] != uid
                or not isinstance(deferred['journal_sha256'], list)
                or len(deferred['journal_sha256']) != 32
                or any(type(byte) is not int or not 0 <= byte <= 255
                       for byte in deferred['journal_sha256'])
                or not any(deferred['journal_sha256'])):
            raise Refusal('deferred source handoff is malformed')
    locator = home['relative']
    if not isinstance(locator, dict) or set(locator) != {'platform', 'units'} or locator['platform'] != 'Unix':
        raise Refusal('migration locator must be a native Linux relative path')
    units = locator['units']
    if (not isinstance(units, list) or not 0 < len(units) <= 4096
            or any(type(unit) is not int or not 0 < unit <= 255 for unit in units)):
        raise Refusal('migration locator bytes are malformed')
    components = bytes(units).split(b'/')
    if len(components) > 128 or any(part in (b'', b'.', b'..') for part in components):
        raise Refusal('migration locator must not traverse outside its filesystem')
    volume = home['volume']
    keys = {'disk_guid', 'disk_bytes', 'sector_bytes', 'partition_guid', 'first_lba', 'last_lba', 'filesystem_uuid', 'filesystem'}
    if not isinstance(volume, dict) or set(volume) != keys:
        raise Refusal('migration volume fields are malformed')
    for key in ('disk_guid', 'partition_guid', 'filesystem_uuid'):
        value = _guid(volume[key], 'migration ' + key)
        if value == '00000000-0000-0000-0000-000000000000':
            raise Refusal('migration volume has a nil identity')
    first = _int(volume['first_lba'], 'migration first LBA', 1)
    last = _int(volume['last_lba'], 'migration last LBA', first)
    sector = _int(volume['sector_bytes'], 'migration sector size', 1)
    size = _int(volume['disk_bytes'], 'migration disk size', 1)
    if (sector not in (512, 4096) or (last + 1) * sector > size
            or volume['filesystem'] not in ('ext4', 'xfs', 'btrfs')
            or (handoff is not None and (handoff['host_os'] != 'linux'
                or volume['disk_guid'].lower() != handoff['disk_guid']
                or size != handoff['disk_size_bytes']))):
        raise Refusal('migration volume differs from the retained Linux source')
    subvolume = home['subvolume']
    if volume['filesystem'] == 'btrfs':
        if (not isinstance(subvolume, dict) or set(subvolume) != {'volume', 'inode'}
                or type(subvolume['inode']) is not int or subvolume['inode'] != inode
                or not isinstance(subvolume['volume'], list) or len(subvolume['volume']) != 41
                or any(type(unit) is not int or not 0 <= unit <= 255 for unit in subvolume['volume'])
                or subvolume['volume'][0] != 3):
            raise Refusal('migration Btrfs identity is malformed')
    elif subvolume is not None:
        raise Refusal('migration filesystem has an unexpected subvolume identity')
    for staged in (() if handoff is None else (handoff['staging_esp'], handoff['staging_root'], handoff['target_extent'])):
        if overlaps({'first_lba': first, 'last_lba': last}, staged):
            raise Refusal('migration source overlaps installation space')
        if volume['partition_guid'].lower() == staged.get('partuuid'):
            raise Refusal('migration source names a staging partition')
    if 'flatpak_inventory' in raw:
        if not categories['flatpak']:
            raise Refusal('Flatpak inventory lacks application migration consent')
        validate_flatpak_inventory(raw['flatpak_inventory'], raw['operation_id'], handoff)
    scope = raw.get('pending_computer_applications_scope')
    if scope is not None:
        _pending_computer_scope(dict(retained_home=raw, identity_public_key=scope.get('identity_public_key') if isinstance(scope, dict) else None,
            setup_binding=scope.get('setup_binding') if isinstance(scope, dict) else None,
            pending_computer_applications_scope=scope))
    return raw


def _pending_computer_scope(receipt):
    # Validate the pending scope through the native closed parser, without
    # claiming a completed archive or opening any source.
    metadata = {key: receipt[key] for key in (
        'identity_public_key', 'setup_binding', 'retained_home',
        'pending_computer_applications_scope') if key in receipt}
    metadata['schema'] = 1
    _prepared_transport('preflight-authenticated', '--source-dir', '/nonexistent',
                        receipt=metadata)
    return receipt.get('pending_computer_applications_scope')


def _prepared_transport(*args, receipt=None):
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          'tilo-installer-prepared-setup-extract')
    try:
        result = subprocess.run([script, *args],
                                input=None if receipt is None else canonical(receipt),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=120, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Refusal('prepared setup transport is unavailable') from error
    if result.returncode != 0 or len(result.stdout) > 80 * 1024:
        raise Refusal('prepared setup transport validation failed')
    return result.stdout


def validate_handoff(raw):
    """Return the normalized handoff (lowercase GUIDs) or raise Refusal."""
    if not isinstance(raw, dict) or set(raw) not in (HANDOFF_KEYS, HANDOFF_KEYS | {'migration'}):
        raise Refusal('handoff fields differ from schema 1')
    if type(raw['schema']) is not int or raw['schema'] != 1:
        raise Refusal('unsupported handoff schema')
    if raw['host_os'] not in ('windows', 'linux'):
        raise Refusal('handoff names an unsupported host OS')
    entry = raw['owned_boot_entry']
    if not isinstance(entry, str) or not BOOT_ID_RE.match(entry):
        raise Refusal('handoff owned_boot_entry is not four uppercase hex digits')
    order = raw['original_boot_order']
    if (not isinstance(order, list) or len(order) > 128
            or not all(isinstance(item, str) and BOOT_ID_RE.match(item) for item in order)):
        raise Refusal('handoff original_boot_order is malformed')
    if entry in order:
        raise Refusal('the owned staging entry was part of the original BootOrder')
    profile = raw['profile']
    if not isinstance(profile, dict) or not set(profile) <= PROFILE_KEYS:
        raise Refusal('handoff profile carries fields outside the installer profile')
    for key, value in profile.items():
        if key == 'schema':
            if type(value) is not int:
                raise Refusal('handoff profile schema is not an integer')
        elif key == 'purpose':
            if value not in ('computer', 'server'):
                raise Refusal('handoff purpose is malformed')
        elif key == 'device_label':
            if (not isinstance(value, str) or (value and not value.strip())
                    or len(value.encode('utf-8')) > 64
                    or any(ord(char) < 32 or 127 <= ord(char) <= 159 for char in value)):
                raise Refusal('handoff device label is malformed')
        elif key == 'prepared_setup':
            if (not isinstance(value, dict) or set(value) != {'schema', 'sealed_bytes', 'ciphertext_sha256'}
                    or type(value['schema']) is not int or value['schema'] != 1
                    or type(value['sealed_bytes']) is not int or not 48 <= value['sealed_bytes'] <= 32 * 1024 * 1024 + 16384
                    or not isinstance(value['ciphertext_sha256'], str)
                    or not re.fullmatch(r'[0-9a-f]{64}', value['ciphertext_sha256'])):
                raise Refusal('handoff prepared setup expectation is malformed')
        elif not isinstance(value, str) or len(value) > 256:
            raise Refusal('handoff profile field %s is not a bounded string' % key)
    handoff = {
        'schema': 1,
        'operation_id': _guid(raw['operation_id'], 'operation_id'),
        'host_os': raw['host_os'],
        'disk_guid': _guid(raw['disk_guid'], 'disk_guid'),
        'disk_size_bytes': _int(raw['disk_size_bytes'], 'disk_size_bytes', 1),
        'staging_esp': _extent(raw['staging_esp'], 'staging_esp', True),
        'staging_root': _extent(raw['staging_root'], 'staging_root', True),
        'target_extent': _extent(raw['target_extent'], 'target_extent', False),
        'owned_boot_entry': entry,
        'original_boot_order': list(order),
        'profile': dict(profile),
    }
    esp, root, target = handoff['staging_esp'], handoff['staging_root'], handoff['target_extent']
    if esp['partuuid'] == root['partuuid'] or handoff['disk_guid'] in (esp['partuuid'], root['partuuid']):
        raise Refusal('handoff GUIDs are not distinct')
    for a, b, label in ((esp, root, 'staging partitions'), (target, esp, 'target and staging ESP'),
                        (target, root, 'target and staging root')):
        if overlaps(a, b):
            raise Refusal('handoff %s overlap' % label)
    if 'migration' in raw:
        handoff['migration'] = validate_migration(raw['migration'], handoff)
    return handoff


def parse_handoff(data):
    if len(data) > HANDOFF_MAX_BYTES:
        raise Refusal('handoff is larger than %d bytes' % HANDOFF_MAX_BYTES)
    try:
        text = data.decode('utf-8')
        raw = json.loads(text, object_pairs_hook=_unique_object, parse_constant=_reject_constant)
    except (UnicodeDecodeError, ValueError) as error:
        raise Refusal('handoff is not valid JSON: %s' % error)
    return validate_handoff(raw)


def read_bounded(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | getattr(os, 'O_CLOEXEC', 0))
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise Refusal('%s is not a regular file' % path)
        chunks, total = [], 0
        while True:
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            total += len(chunk)
            if total > HANDOFF_MAX_BYTES:
                raise Refusal('%s is larger than %d bytes' % (path, HANDOFF_MAX_BYTES))
            chunks.append(chunk)
        return b''.join(chunks)
    finally:
        os.close(fd)


def load_handoff_file(path):
    return parse_handoff(read_bounded(path))


# --- firmware variables ------------------------------------------------------

def read_efivar(name, vendor):
    path = os.path.join(efivars_dir(), '%s-%s' % (name, vendor))
    try:
        data = read_bounded(path)
    except FileNotFoundError:
        return None
    if len(data) < 4:
        raise Refusal('firmware variable %s is truncated' % name)
    return data[4:]


def loader_device_partuuid():
    value = read_efivar('LoaderDevicePartUUID', LOADER_VENDOR)
    if value is None:
        return None
    try:
        text = value.decode('utf-16-le').rstrip('\0').strip()
    except UnicodeDecodeError:
        raise Refusal('LoaderDevicePartUUID is not UTF-16')
    if not GUID_RE.match(text):
        raise Refusal('LoaderDevicePartUUID is not a GUID')
    return text.lower()


def boot_ids(name):
    value = read_efivar(name, EFI_GLOBAL)
    if value is None:
        return []
    if len(value) % 2:
        raise Refusal('%s has an odd length' % name)
    return ['%04X' % item for (item,) in struct.iter_unpack('<H', value)]


def decode_load_option(data):
    """Decode an EFI_LOAD_OPTION (without efivarfs attributes) into its single
    GPT hard-drive node. Anything else — MBR, several HD nodes, a truncated
    path — is not a staging entry and returns None."""
    if len(data) < 6:
        return None
    path_length = struct.unpack_from('<H', data, 4)[0]
    offset = 6
    while True:
        if offset + 2 > len(data):
            return None
        if data[offset:offset + 2] == b'\0\0':
            offset += 2
            break
        offset += 2
    description = data[6:offset - 2]
    if len(description) % 2:
        return None
    path = data[offset:offset + path_length]
    if len(path) != path_length:
        return None
    hd, file_path, cursor = [], None, 0
    while cursor + 4 <= len(path):
        kind, subtype, length = struct.unpack_from('<BBH', path, cursor)
        if length < 4 or cursor + length > len(path):
            return None
        body = path[cursor + 4:cursor + length]
        if kind == 0x7F:
            break
        if kind == 0x04 and subtype == 0x01:
            if length != 42:
                return None
            number, start, size = struct.unpack_from('<IQQ', body, 0)
            signature, mbr_type, signature_type = body[20:36], body[36], body[37]
            if mbr_type != 0x02 or signature_type != 0x02:
                return None
            hd.append({'partition_number': number, 'start_lba': start, 'size_lba': size,
                       'partuuid': str(uuid.UUID(bytes_le=bytes(signature)))})
        elif kind == 0x04 and subtype == 0x04:
            try:
                file_path = body.decode('utf-16-le').rstrip('\0')
            except UnicodeDecodeError:
                return None
        cursor += length
    if len(hd) != 1:
        return None
    result = dict(hd[0])
    result['path'] = file_path
    return result


def read_boot_entry(entry_id):
    data = read_efivar('Boot' + entry_id, EFI_GLOBAL)
    return None if data is None else decode_load_option(data)


def all_boot_entries():
    entries = {}
    pattern = re.compile(r'^Boot([0-9A-Fa-f]{4})-' + EFI_GLOBAL + '$')
    try:
        names = sorted(os.listdir(efivars_dir()))
    except FileNotFoundError:
        return entries
    for name in names:
        match = pattern.match(name)
        if match:
            entries[match.group(1).upper()] = read_boot_entry(match.group(1))
    return entries


# --- ESP access --------------------------------------------------------------

def run(argv, check=True):
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            universal_newlines=True)
    if check and result.returncode != 0:
        raise Refusal('%s failed: %s' % (argv[0], result.stderr.strip()))
    return result


def with_boot_esp(esp_partuuid, read):
    """Read the handoff bytes from the ESP the loader actually ran from."""
    fixture = os.environ.get('TILO_STAGING_ESP_DIR')
    if fixture:
        return read(fixture)
    device = '/dev/disk/by-partuuid/' + esp_partuuid
    if not os.path.exists(device):
        raise FileNotFoundError(device)
    device = os.path.realpath(device)
    if not stat.S_ISBLK(os.stat(device).st_mode):
        raise Refusal('staging ESP %s is not a block device' % device)
    part_type = run(['lsblk', '-dno', 'PARTTYPE', device]).stdout.strip().lower()
    if part_type != ESP_TYPE:
        raise Refusal('loader partition %s is not an EFI System Partition' % device)
    mounted = run(['findmnt', '-rn', '-o', 'TARGET', '-S', device], check=False).stdout.split('\n')[0]
    if mounted:
        return read(mounted)
    # The detect unit may write only below /run/tilo-installer; the mount stays
    # inside that unit's private namespace either way.
    parent = '/run/tilo-installer' if os.path.isdir('/run/tilo-installer') else '/run'
    directory = tempfile.mkdtemp(prefix='esp.', dir=parent)
    try:
        run(['mount', '-t', 'vfat', '-o', 'ro,nosuid,nodev,noexec', device, directory])
        try:
            return read(directory)
        finally:
            run(['umount', directory], check=False)
    finally:
        os.rmdir(directory)


def read_esp_handoff(esp_partuuid):
    return with_boot_esp(esp_partuuid, lambda root: read_bounded(os.path.join(root, *HANDOFF_RELATIVE)))


def deliver_staged_preparation(handoff, output):
    expected = handoff['profile'].get('prepared_setup')
    if expected is None:
        return
    # Reuse the root image's canonical bounded ciphertext-only transport. The
    # boot-selected ESP is the sole source, never an attached-media scan.
    def preflight(directory):
        _prepared_transport('preflight', '--source-dir', directory,
                            '--receipt', json.dumps(expected))
    def deliver(root):
        # Verify the mounted ESP's ciphertext before publishing to the selected
        # generation. Existing output still has to match that same receipt.
        with tempfile.TemporaryDirectory(prefix='tilo-prepared-', dir=output) as staging:
            _prepared_transport('--source-root', root, '--out-dir', staging)
            preflight(staging)
        if os.path.lexists(os.path.join(output, 'prepared-setup.sealed')):
            preflight(output)
        else:
            _prepared_transport('--source-root', root, '--out-dir', output)
            preflight(output)
    try:
        with_boot_esp(handoff['staging_esp']['partuuid'], deliver)
    except (Refusal, ValueError, OSError) as error:
        raise Refusal('prepared encrypted setup delivery failed: %s' % error) from error
    write_atomic(os.path.join(output, 'profile.json'), canonical(handoff['profile']), 0o644)


def verified_live_handoff(copy_path):
    """The UI's copy must equal what the firmware-booted staging ESP holds."""
    copy = load_handoff_file(copy_path)
    loader = loader_device_partuuid()
    if loader is None:
        raise Refusal('firmware did not report the ESP the installer booted from')
    if loader != copy['staging_esp']['partuuid']:
        raise Refusal('the handoff names a different ESP than the one this installer booted from')
    try:
        original = parse_handoff(read_esp_handoff(loader))
    except FileNotFoundError:
        raise Refusal('the booted staging ESP no longer holds a handoff')
    if original != copy:
        raise Refusal('the handoff copy differs from the staging ESP')
    return copy


# --- partition facts ---------------------------------------------------------

def lsblk_disk(disk):
    fixture = os.environ.get('TILO_STAGING_LSBLK_JSON')
    if fixture:
        with open(fixture, encoding='utf-8') as handle:
            data = json.load(handle)
    else:
        data = json.loads(run(['lsblk', '-J', '-b', '-p', '-o',
                               'NAME,TYPE,START,SIZE,PARTUUID,PARTTYPE,PTTYPE,PTUUID,LOG-SEC,MOUNTPOINTS,UUID,FSTYPE',
                               disk]).stdout)
    devices = [item for item in data.get('blockdevices', []) if item.get('name') == disk]
    if len(devices) != 1 or devices[0].get('type') != 'disk':
        raise Refusal('%s is not exactly one whole disk' % disk)
    return with_gpt_facts(devices[0], disk)


def with_gpt_facts(node, disk):
    """Fill GPT identities lsblk left empty (no udev database) from sfdisk.

    lsblk takes PTTYPE/PTUUID/PARTUUID/PARTTYPE from udev; without a udev
    database (early boot, containers) they come back empty. sfdisk reads the
    same facts from the GPT itself. Only missing fields are filled; a field
    both tools report must agree, or the disk is refused.
    """
    children = node.get('children') or []
    wanted = [node.get('pttype'), node.get('ptuuid')]
    wanted += [c.get(k) for c in children for k in ('partuuid', 'parttype')]
    if all(wanted):
        return node
    table = json.loads(run(['sfdisk', '--json', disk]).stdout).get('partitiontable', {})

    def fill(target, key, value):
        value = str(value or '').lower()
        have = str(target.get(key) or '').lower()
        if have and value and have != value:
            raise Refusal('lsblk and the GPT disagree about %s of %s' % (key, target.get('name')))
        if not have:
            target[key] = value

    fill(node, 'pttype', table.get('label'))
    fill(node, 'ptuuid', table.get('id'))
    by_node = {part.get('node'): part for part in table.get('partitions', [])}
    for child in children:
        part = by_node.get(child.get('name'))
        if part is None:
            raise Refusal('the GPT of %s does not list %s' % (disk, child.get('name')))
        fill(child, 'partuuid', part.get('uuid'))
        fill(child, 'parttype', part.get('type'))
    return node


def number(node, key):
    value = node.get(key)
    if isinstance(value, str) and value.isdigit():
        value = int(value)
    if type(value) is not int or value < 0:
        raise Refusal('lsblk reported no %s for %s' % (key, node.get('name')))
    return value


def mounts(node):
    points = node.get('mountpoints')
    if points is None:
        points = [node.get('mountpoint')]
    return [point for point in points if point]


def subtree_mounts(node):
    found = list(mounts(node))
    for child in node.get('children') or []:
        found.extend(subtree_mounts(child))
    return found


def is_live_mount(point):
    return point in ('/', '/boot') or point.startswith('/boot/') or any(
        point == prefix or point.startswith(prefix + '/') for prefix in LIVE_MOUNT_PREFIXES)


def disk_facts(disk, handoff):
    """Validate the target disk against the handoff; return its partitions."""
    node = lsblk_disk(disk)
    if str(node.get('pttype', '')).lower() != 'gpt':
        raise Refusal('%s is not a GPT disk' % disk)
    if str(node.get('ptuuid', '')).lower() != handoff['disk_guid']:
        raise Refusal('disk GUID differs from the staged plan')
    if number(node, 'size') != handoff['disk_size_bytes']:
        raise Refusal('disk size differs from the staged plan')
    sector = number(node, 'log-sec')
    if sector not in (512, 4096):
        raise Refusal('unsupported logical sector size %d' % sector)
    if mounts(node):
        raise Refusal('the whole disk %s is mounted' % disk)
    partitions = []
    for child in node.get('children') or []:
        if child.get('type') != 'part':
            raise Refusal('unexpected %s node below %s' % (child.get('type'), disk))
        start_bytes = number(child, 'start') * 512
        size = number(child, 'size')
        if start_bytes % sector or size % sector or size == 0:
            raise Refusal('partition %s is not sector aligned' % child.get('name'))
        first = start_bytes // sector
        partitions.append({
            'name': child.get('name'),
            'partuuid': str(child.get('partuuid') or '').lower(),
            'type': str(child.get('parttype') or '').lower(),
            'first_lba': first,
            'last_lba': first + size // sector - 1,
            'mounts': subtree_mounts(child),
            'filesystem_uuid': str(child.get('uuid') or '').lower(),
            'filesystem': str(child.get('fstype') or '').lower(),
        })
    if 'migration' in handoff:
        source = handoff['migration']['home']['volume']
        matches = [part for part in partitions if part['partuuid'] == source['partition_guid'].lower()]
        if (len(matches) != 1 or source['sector_bytes'] != sector
                or matches[0]['first_lba'] != source['first_lba'] or matches[0]['last_lba'] != source['last_lba']
                or matches[0]['filesystem_uuid'] != source['filesystem_uuid'].lower()
                or matches[0]['filesystem'] != source['filesystem']):
            raise Refusal('retained migration source identity or geometry changed')
    return sector, partitions


def staged_partition(partitions, expected, part_type, label):
    matches = [part for part in partitions if part['partuuid'] == expected['partuuid']]
    if len(matches) != 1:
        raise Refusal('%s %s is not present exactly once' % (label, expected['partuuid']))
    part = matches[0]
    if part['type'] != part_type:
        raise Refusal('%s has an unexpected partition type' % label)
    if (part['first_lba'], part['last_lba']) != (expected['first_lba'], expected['last_lba']):
        raise Refusal('%s extent differs from the staged plan' % label)
    return part


def check_mounts(handoff, partitions, staging_root):
    staged = {handoff['staging_esp']['partuuid'], handoff['staging_root']['partuuid']}
    for part in partitions:
        if part['mounts'] and part['partuuid'] not in staged:
            raise Refusal('%s is mounted; only the staged installer may be in use' % part['name'])
    if not any(is_live_mount(point) for point in staging_root['mounts']):
        raise Refusal('the running installer root is not the staging root partition')


def check_install(args):
    if args.mode != 'guided':
        raise Refusal('the disk holding the running installer accepts only guided alongside; '
                      'whole-disk erase and prepared partitions are refused')
    handoff = verified_live_handoff(args.handoff)
    sector, partitions = disk_facts(args.disk, handoff)
    if args.sector_size != sector:
        raise Refusal('reviewed logical sector size differs from the disk')
    esp = staged_partition(partitions, handoff['staging_esp'], ESP_TYPE, 'staging ESP')
    root = staged_partition(partitions, handoff['staging_root'], STAGING_ROOT_TYPE, 'staging root')
    if esp['partuuid'] != loader_device_partuuid():
        raise Refusal('the installer did not boot from the staging ESP')
    check_mounts(handoff, partitions, root)
    target = handoff['target_extent']
    free_start = target['first_lba'] * sector
    free_size = (target['last_lba'] - target['first_lba'] + 1) * sector
    if (args.free_start_bytes, args.free_size_bytes) != (free_start, free_size):
        raise Refusal('the guided extent differs from the staged target extent')
    if free_start % MIB or free_size % MIB:
        raise Refusal('the staged target extent is not MiB aligned')
    if free_start + free_size > handoff['disk_size_bytes'] - MIB:
        raise Refusal('the staged target extent reaches the backup GPT')
    for part in partitions:
        if overlaps(part, target):
            raise Refusal('the staged target extent is no longer unallocated (%s)' % part['name'])
    if args.efi_start_bytes != free_start or args.efi_size_bytes != GUIDED_EFI_BYTES:
        raise Refusal('the Tilo ESP must be 2 GiB at the start of the target extent')
    if args.boot_start_bytes != free_start + GUIDED_EFI_BYTES or args.boot_size_bytes != GUIDED_BOOT_BYTES:
        raise Refusal('the Tilo boot must be 1 GiB immediately after the Tilo ESP')
    if args.root_start_bytes != args.boot_start_bytes + GUIDED_BOOT_BYTES:
        raise Refusal('the Tilo root must immediately follow the Tilo boot')
    if args.root_start_bytes + args.root_size_bytes != free_start + free_size:
        raise Refusal('the Tilo root must end exactly where the staged installer begins')
    if args.root_size_bytes < MIN_GUIDED_ROOT_BYTES:
        raise Refusal('the staged target extent is smaller than the guided minimum')
    facts = {
        'INTERNAL_DISK_GUID': handoff['disk_guid'],
        'INTERNAL_HOST_OS': handoff['host_os'],
        'INTERNAL_STAGING_ESP_PARTUUID': esp['partuuid'],
        'INTERNAL_STAGING_ROOT_PARTUUID': root['partuuid'],
        'INTERNAL_OWNED_BOOT_ENTRY': handoff['owned_boot_entry'],
    }
    for key in sorted(facts):
        print('%s=%s' % (key, facts[key]))


def check_mounts_command(args):
    handoff = load_handoff_file(args.handoff)
    _, partitions = disk_facts(args.disk, handoff)
    root = staged_partition(partitions, handoff['staging_root'], STAGING_ROOT_TYPE, 'staging root')
    staged_partition(partitions, handoff['staging_esp'], ESP_TYPE, 'staging ESP')
    check_mounts(handoff, partitions, root)


# --- publishing --------------------------------------------------------------

def write_atomic(path, data, mode):
    directory = os.path.dirname(path) or '.'
    fd, temporary = tempfile.mkstemp(prefix='.tilo-staging.', dir=directory)
    try:
        os.fchmod(fd, mode)
        os.write(fd, data)
        os.fsync(fd)
        os.close(fd)
        fd = -1
        os.rename(temporary, path)
        directory_fd = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except BaseException:
        if fd >= 0:
            os.close(fd)
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise


def canonical(document):
    return (json.dumps(document, sort_keys=True, separators=(',', ':')) + '\n').encode()


def detect(args):
    for stale in (args.out, args.error):
        try:
            os.unlink(stale)
        except FileNotFoundError:
            pass
    os.makedirs(os.path.dirname(args.out), mode=0o755, exist_ok=True)
    if not os.path.isdir(efivars_dir()):
        return 0
    try:
        loader = loader_device_partuuid()
        if loader is None:
            return 0
        try:
            handoff = parse_handoff(read_esp_handoff(loader))
        except FileNotFoundError:
            return 0
        if handoff['staging_esp']['partuuid'] != loader:
            raise Refusal('the staging handoff names a different ESP than the one this installer booted from')
        deliver_staged_preparation(handoff, os.path.dirname(args.out))
    except Refusal as error:
        write_atomic(args.error, ('Staged installation was not recognised: %s\n' % error).encode(), 0o644)
        print('tilo-staging-handoff: %s' % error, file=sys.stderr)
        return 0
    write_atomic(args.out, canonical(handoff), 0o644)
    print('tilo-staging-handoff: staged installation for disk %s recognised' % handoff['disk_guid'])
    return 0


def write_reclaim_record(args):
    handoff = load_handoff_file(args.handoff)
    if not GUID_RE.match(args.target_root_partuuid) or not GUID_RE.match(args.target_esp_partuuid):
        raise Refusal('target partition GUIDs are malformed')
    sector = args.sector_size
    if sector not in (512, 4096) or args.target_root_start_bytes % sector or args.target_root_size_bytes % sector:
        raise Refusal('target root geometry is not sector aligned')
    first = args.target_root_start_bytes // sector
    last = first + args.target_root_size_bytes // sector - 1
    if last != handoff['target_extent']['last_lba'] or first < handoff['target_extent']['first_lba']:
        raise Refusal('target root does not end at the staged target extent')
    record = {
        'schema': 1,
        'operation_id': handoff['operation_id'],
        'disk_guid': handoff['disk_guid'],
        'disk_size_bytes': handoff['disk_size_bytes'],
        'sector_bytes': sector,
        'staging_esp': handoff['staging_esp'],
        'staging_root': handoff['staging_root'],
        'target_root': {'partuuid': args.target_root_partuuid.lower(), 'first_lba': first, 'last_lba': last},
        'target_esp_partuuid': args.target_esp_partuuid.lower(),
    }
    directory = os.path.join(args.target_var, 'lib', 'tilo')
    os.makedirs(directory, mode=0o755, exist_ok=True)
    write_atomic(os.path.join(directory, 'staging-reclaim.json'), canonical(record), 0o600)
    if 'migration' in handoff:
        write_atomic(os.path.join(directory, 'migration-plan.json'), canonical(handoff['migration']), 0o600)
    print('recorded staging reclaim for the installed system')


def _open_directory_chain(path):
    """Pinned directory descriptors; no symlink component is ever followed."""
    if not os.path.isabs(path):
        raise Refusal('metadata delivery directory is not absolute')
    parts = path.split('/')[1:]
    if any(part in ('', '.', '..') for part in parts):
        raise Refusal('metadata delivery directory is not normalized')
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        for part in parts:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def _read_plan_at(directory, name, owner):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK, dir_fd=directory)
    try:
        facts = os.fstat(fd)
        if (not stat.S_ISREG(facts.st_mode) or facts.st_uid != owner or facts.st_mode & 0o077
                or facts.st_size > HANDOFF_MAX_BYTES):
            raise Refusal('migration metadata is not a bounded private owner-controlled file')
        data = bytearray()
        while len(data) <= HANDOFF_MAX_BYTES:
            chunk = os.read(fd, min(65536, HANDOFF_MAX_BYTES + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > HANDOFF_MAX_BYTES:
            raise Refusal('migration metadata exceeds its size bound')
        try:
            raw = json.loads(data.decode('utf-8'), object_pairs_hook=_unique_object, parse_constant=_reject_constant)
        except (UnicodeDecodeError, ValueError) as error:
            raise Refusal('migration metadata is not valid JSON: %s' % error)
        return validate_migration(raw)
    finally:
        os.close(fd)


def authenticated_retained_plan(stream):
    data = stream.buffer.read(HANDOFF_MAX_BYTES + 32769)
    if len(data) > HANDOFF_MAX_BYTES + 32768:
        raise Refusal('authenticated prepared choices exceed their bound')
    try:
        receipt = json.loads(data.decode('utf-8'), object_pairs_hook=_unique_object, parse_constant=_reject_constant)
    except (UnicodeDecodeError, ValueError) as error:
        raise Refusal('authenticated prepared choices are malformed') from error
    if (not isinstance(receipt, dict) or type(receipt.get('schema')) is not int or receipt.get('schema') != 1
            or set(receipt) - {'schema', 'identity_public_key', 'setup_binding', 'install_profile', 'retained_home', 'completed_home', 'completed_setup', 'completed_applications', 'completed_applications_only', 'computer_apps_scope', 'planned_btrfs_resize', 'pending_computer_applications_scope'}):
        raise Refusal('authenticated prepared choices fields differ')
    completed = receipt.get('completed_home')
    apps_only = receipt.get('completed_applications_only', False)
    if not isinstance(apps_only, bool):
        raise Refusal('authenticated completed purpose is malformed')
    if apps_only and (completed is not None or receipt.get('completed_applications') is None):
        raise Refusal('authenticated applications purpose differs')
    if (completed is not None or apps_only) != (receipt.get('completed_setup') is not None):
        raise Refusal('authenticated completed generation is incomplete')
    if receipt.get('computer_apps_scope') is not None and not apps_only:
        raise Refusal('authenticated Computer applications scope is unexpected')
    if completed is None and not apps_only and receipt.get('completed_applications') is not None:
        raise Refusal('authenticated applications lack completed generation')
    if completed is not None or apps_only:
        # Reuse the canonical bounded archive receipt schema; this validates
        # public metadata only and never creates a source mount or grant.
        generation = _prepared_transport('selected-generation').decode('utf-8').strip()
        _prepared_transport('preflight-authenticated', '--source-dir', generation,
                            receipt=receipt)
        if ((receipt.get('retained_home') is not None and not apps_only)
                or (completed is not None and completed['setup_binding'] != receipt.get('setup_binding'))):
            raise Refusal('authenticated completed Home lineage differs')
    pending = _pending_computer_scope(receipt)
    plan = receipt.get('retained_home')
    if plan is None:
        return None
    plan = validate_migration(plan)
    resize = receipt.get('planned_btrfs_resize')
    if resize is not None:
        # This input is the existing password-authenticated native helper's
        # receipt. The helper independently verifies kernel/filesystem geometry
        # and checked readonly mount retirement before publishing this receipt.
        keys = {'schema', 'original_plan_sha256', 'reviewed_plan_sha256',
                'reviewed_plan', 'filesystem_uuid', 'partition_guid', 'device_id',
                'from_bytes', 'to_bytes', 'new_last_lba'}
        volume = plan['home']['volume']
        if (not isinstance(resize, dict) or set(resize) != keys
                or resize.get('schema') != 1 or resize.get('device_id') != 1
                or volume['filesystem'] != 'btrfs'
                or resize.get('filesystem_uuid') != volume['filesystem_uuid']
                or resize.get('partition_guid') != volume['partition_guid']
                or resize.get('new_last_lba') != volume['last_lba']
                or not isinstance(resize.get('to_bytes'), int)
                or isinstance(resize.get('to_bytes'), bool)
                or resize['to_bytes'] != (volume['last_lba'] - volume['first_lba'] + 1) * volume['sector_bytes']
                or not isinstance(resize.get('from_bytes'), int)
                or isinstance(resize.get('from_bytes'), bool)
                or resize['from_bytes'] <= resize['to_bytes']):
            raise Refusal('authenticated resized source differs')
    shared = plan.get('home_policy') == 'access-read-write' and plan['categories']['home']
    apps = (plan.get('home_policy', 'copy') == 'copy' and not plan['categories']['home']
            and plan['categories']['flatpak'] and 'flatpak_inventory' in plan)
    if not (shared or apps):
        raise Refusal('authenticated retained plan lacks shared-files or independent-app consent')
    if pending is not None:
        plan = dict(plan, pending_computer_applications_scope=pending)
    if len(canonical(plan)) > HANDOFF_MAX_BYTES:
        raise Refusal('authenticated original-files plan exceeds reboot delivery bound')
    return plan


def preflight_retained(args):
    plan = authenticated_retained_plan(sys.stdin)
    if plan is None:
        return
    if plan.get('home_policy') == 'access-read-write':
        minimum, maximum = regular_user_range()
        if not minimum <= plan['home']['original_uid'] <= maximum:
            raise Refusal('retained source UID is outside target regular-account policy')
    if not args.write_start or len(args.write_start) != len(args.write_size):
        raise Refusal('installation write extents are missing')
    node = lsblk_disk(args.disk)
    size = number(node, 'size')
    extents = list(zip(args.write_start, args.write_size))
    if any(start < 0 or length <= 0 or start + length > size for start, length in extents):
        raise Refusal('installation write extent is outside the target disk')
    source = plan['home']['volume']
    if str(node.get('ptuuid', '')).lower() != source['disk_guid'].lower():
        return
    if size != source['disk_bytes'] or number(node, 'log-sec') != source['sector_bytes']:
        raise Refusal('retained source disk geometry changed')
    matches = [part for part in node.get('children', [])
               if str(part.get('partuuid', '')).lower() == source['partition_guid'].lower()]
    if len(matches) != 1:
        raise Refusal('retained source partition is missing or ambiguous')
    part = matches[0]
    first = source['first_lba'] * source['sector_bytes']
    end = (source['last_lba'] + 1) * source['sector_bytes']
    if (number(part, 'start') * 512 != first or number(part, 'size') != end - first
            or str(part.get('uuid', '')).lower() != source['filesystem_uuid'].lower()
            or part.get('fstype') != source['filesystem']):
        raise Refusal('retained source filesystem or extent changed')
    if any(start < end and start + length > first for start, length in extents):
        raise Refusal('installation would modify the retained source filesystem')


def deliver_retained(args):
    plan = authenticated_retained_plan(sys.stdin)
    if plan is None:
        return
    data = canonical(plan)
    directory = _open_directory_chain(args.target_var)
    temporary = None
    try:
        root_facts = os.fstat(directory)
        if root_facts.st_uid != os.geteuid() or root_facts.st_mode & 0o022:
            raise Refusal('installed /var custody is unsafe')
        for component in ('lib', 'tilo'):
            try:
                os.mkdir(component, 0o755, dir_fd=directory)
                os.fsync(directory)
            except FileExistsError:
                pass
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=directory)
            facts = os.fstat(child)
            if facts.st_uid != os.geteuid() or facts.st_mode & 0o022:
                os.close(child)
                raise Refusal('installed original-files metadata namespace is unsafe')
            os.close(directory)
            directory = child
        try:
            existing = _read_plan_at(directory, 'migration-plan.json', os.geteuid())
        except FileNotFoundError:
            existing = None
        if existing is not None:
            if canonical(existing) != data:
                raise Refusal('installed migration intent conflicts with authenticated setup')
            return
        temporary = '.retained-' + __import__('secrets').token_hex(16)
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=directory)
        try:
            with os.fdopen(fd, 'wb') as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
            os.link(temporary, 'migration-plan.json', src_dir_fd=directory, dst_dir_fd=directory, follow_symlinks=False)
            os.fsync(directory)
        finally:
            os.unlink(temporary, dir_fd=directory)
            temporary = None
    finally:
        os.close(directory)


def regular_user_range():
    """Read the installed shadow-utils policy, never a source-system policy."""
    fd = os.open('/etc/login.defs', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    try:
        facts = os.fstat(fd)
        if (not stat.S_ISREG(facts.st_mode) or facts.st_uid != 0
                or facts.st_mode & 0o022 or facts.st_size > 65536):
            raise Refusal('installed regular-account policy is unsafe')
        with os.fdopen(fd, 'r', encoding='utf-8') as stream:
            fd = None
            contents = stream.read(65537)
            if len(contents) > 65536:
                raise Refusal('installed regular-account policy exceeds its size bound')
            values = {'UID_MIN': 1000, 'UID_MAX': 60000}
            seen = set()
            for line in contents.splitlines():
                fields = line.split('#', 1)[0].split()
                if fields and fields[0] in values:
                    if (len(fields) != 2 or not fields[1].isascii() or not fields[1].isdigit()
                            or fields[0] in seen):
                        raise Refusal('installed regular-account range is malformed')
                    seen.add(fields[0])
                    values[fields[0]] = int(fields[1])
        minimum, maximum = values['UID_MIN'], values['UID_MAX']
        if not 1000 <= minimum <= maximum < 0xffffffff:
            raise Refusal('installed regular-account range is invalid')
        return minimum, maximum
    finally:
        if fd is not None:
            os.close(fd)


def required_user_id(args):
    import pwd
    if os.geteuid() != 0 or not re.fullmatch('[a-z_][a-z0-9_-]{0,31}', args.username):
        raise Refusal('user-ID query requires root and a valid target username')
    seed = _open_directory_chain('/var/lib/tilo')
    try:
        facts = os.fstat(seed)
        if facts.st_uid != 0 or facts.st_mode & 0o022:
            raise Refusal('root migration seed directory has unsafe ownership or permissions')
        raw = _read_plan_at(seed, 'migration-plan.json', 0)
    finally:
        os.close(seed)
    if raw.get('home_policy', 'copy') != 'access-read-write':
        return
    uid = raw['home']['original_uid']
    minimum, maximum = regular_user_range()
    if not minimum <= uid <= maximum:
        raise Refusal('original source UID is outside the installed regular-account range')
    for lookup, value in ((pwd.getpwnam, args.username), (pwd.getpwuid, uid)):
        try:
            lookup(value)
        except KeyError:
            continue
        raise Refusal('target username or original source UID is already assigned')
    # A single decimal integer is the complete typed response. No source paths,
    # credential bytes or shell assignments cross this account-creation door.
    print(uid)


def deliver_user_plan(args):
    import pwd
    import secrets
    test = any(value is not None for value in (args.test_home, args.test_uid, args.test_gid, args.test_seed_dir))
    if test:
        if (os.geteuid() == 0 or args.test_uid != os.geteuid() or args.test_gid != os.getegid()
                or args.test_home is None or args.test_seed_dir is None):
            raise Refusal('fixture delivery is limited to the unprivileged caller own UID/GID')
        home, uid, gid, seed_path = args.test_home, args.test_uid, args.test_gid, args.test_seed_dir
    else:
        account = pwd.getpwnam(args.username)
        home, uid, gid, seed_path = account.pw_dir, account.pw_uid, account.pw_gid, '/var/lib/tilo'
        if os.geteuid() != 0 or uid == 0:
            raise Refusal('firstboot delivery requires root and a non-root target account')
    seed = _open_directory_chain(seed_path)
    destination = None
    temporary = None
    chain = []
    try:
        raw = _read_plan_at(seed, 'migration-plan.json', os.geteuid())
        data = canonical(raw)
        destination = _open_directory_chain(home)
        if os.fstat(destination).st_uid != uid:
            raise Refusal('migration target home is not owned by its actual account')
        for component in ('.local', 'state', 'tilo', 'migration-handoff'):
            try:
                os.mkdir(component, 0o700, dir_fd=destination)
                created = True
            except FileExistsError:
                created = False
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=destination)
            if created:
                os.fchown(child, uid, gid)
                os.fchmod(child, 0o700)
                os.fsync(destination)
            facts = os.fstat(child)
            # Existing .local can be 0755; the final plan directory is private.
            if facts.st_uid != uid or facts.st_mode & 0o022 or (component == 'migration-handoff' and facts.st_mode & 0o077):
                os.close(child)
                raise Refusal('migration metadata ancestor has unsafe ownership or permissions')
            chain.append((os.dup(destination), component, facts.st_dev, facts.st_ino))
            os.close(destination)
            destination = child
        def validate_chain():
            for parent, component, device, inode in chain:
                current = os.stat(component, dir_fd=parent, follow_symlinks=False)
                if not stat.S_ISDIR(current.st_mode) or (current.st_dev, current.st_ino) != (device, inode):
                    raise Refusal('migration metadata directory was replaced during delivery')
        validate_chain()
        name = raw['operation_id'] + '.migration.json'
        try:
            existing = _read_plan_at(destination, name, uid)
        except FileNotFoundError:
            existing = None
        if existing is not None:
            if canonical(existing) != data:
                raise Refusal('existing user migration consent differs; refusing replacement')
        else:
            temporary = '.migration-' + secrets.token_hex(16)
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=destination)
            try:
                os.fchown(fd, uid, gid)
                view = memoryview(data)
                while view:
                    count = os.write(fd, view)
                    if count <= 0:
                        raise Refusal('short migration metadata write')
                    view = view[count:]
                os.fsync(fd)
            finally:
                os.close(fd)
            os.link(temporary, name, src_dir_fd=destination, dst_dir_fd=destination, follow_symlinks=False)
            os.fsync(destination)
            if canonical(_read_plan_at(destination, name, uid)) != data:
                raise Refusal('published user migration plan failed verification')
        validate_chain()
        # Keep the authoritative root seed after verified delivery: first-login
        # receipts, rather than metadata delivery, determine actual completion.
        print('delivered migration metadata to the target account')
    finally:
        if destination is not None:
            if temporary is not None:
                try:
                    os.unlink(temporary, dir_fd=destination)
                    os.fsync(destination)
                except FileNotFoundError:
                    pass
            os.close(destination)
        for parent, _component, _device, _inode in chain:
            os.close(parent)
        os.close(seed)


# --- firmware handoff back ---------------------------------------------------

def efibootmgr_run(*argv):
    result = subprocess.run([efibootmgr(), '-q'] + list(argv), stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, universal_newlines=True)
    if result.returncode != 0:
        raise Refusal('efibootmgr %s failed: %s' % (' '.join(argv), result.stderr.strip()))


def delete_owned_entry(handoff):
    entry_id = handoff['owned_boot_entry']
    esp = handoff['staging_esp']
    path = os.path.join(efivars_dir(), 'Boot%s-%s' % (entry_id, EFI_GLOBAL))
    if not os.path.exists(path):
        print('staging boot entry Boot%s is already gone' % entry_id)
        return True
    entry = read_boot_entry(entry_id)
    if (entry is None or entry['partuuid'] != esp['partuuid']
            or entry['start_lba'] != esp['first_lba']
            or entry['size_lba'] != esp['last_lba'] - esp['first_lba'] + 1):
        print('warning: Boot%s no longer points at the staging ESP; leaving it untouched' % entry_id)
        return False
    if entry_id in boot_ids('BootOrder'):
        print('warning: Boot%s is referenced by BootOrder; leaving it for Tilo Imager to remove' % entry_id)
        return False
    efibootmgr_run('-b', entry_id, '-B')
    if os.path.exists(path):
        raise Refusal('Boot%s still exists after deletion' % entry_id)
    print('removed the staging boot entry Boot%s' % entry_id)
    return True


def target_entries(target_esp):
    return sorted(entry_id for entry_id, entry in all_boot_entries().items()
                  if entry is not None and entry['partuuid'] == target_esp)


def firmware_finalize(args):
    handoff = load_handoff_file(args.handoff)
    target_esp = args.target_esp_partuuid.lower()
    if not GUID_RE.match(target_esp) or target_esp in (handoff['staging_esp']['partuuid'],
                                                       handoff['staging_root']['partuuid']):
        raise Refusal('target ESP GUID is malformed or names a staging partition')
    removed = delete_owned_entry(handoff)
    candidates = target_entries(target_esp)
    if not candidates and args.target_disk and args.target_esp_partition_number and args.loader:
        order = boot_ids('BootOrder')
        efibootmgr_run('-c', '-d', args.target_disk, '-p', str(args.target_esp_partition_number),
                       '-L', 'Tilo', '-l', args.loader)
        if boot_ids('BootOrder') != order:
            if order:
                efibootmgr_run('-o', ','.join(order))
            else:
                efibootmgr_run('-O')
        if boot_ids('BootOrder') != order:
            raise Refusal('BootOrder changed while creating the Tilo boot entry')
        candidates = target_entries(target_esp)
    if len(candidates) != 1:
        raise Refusal('expected exactly one firmware entry for the installed Tilo ESP, found %d' % len(candidates))
    efibootmgr_run('-n', candidates[0])
    if boot_ids('BootNext') != [candidates[0]]:
        raise Refusal('BootNext did not persist')
    print('the next restart starts the installed Tilo (Boot%s) once; the default is unchanged' % candidates[0])
    return 0 if removed else 3


# --- entry point -------------------------------------------------------------

def main(argv):
    import argparse
    parser = argparse.ArgumentParser(prog='tilo-staging-handoff')
    sub = parser.add_subparsers(dest='command')
    detect_parser = sub.add_parser('detect')
    detect_parser.add_argument('--out', default='/run/tilo-installer/handoff.json')
    detect_parser.add_argument('--error', default='/run/tilo-installer/handoff.error')
    check = sub.add_parser('check-install')
    check.add_argument('--handoff', required=True)
    check.add_argument('--disk', required=True)
    check.add_argument('--mode', required=True, choices=('guided', 'erase', 'prepared'))
    for name in ('free-start-bytes', 'free-size-bytes', 'efi-start-bytes', 'efi-size-bytes',
                 'boot-start-bytes', 'boot-size-bytes', 'root-start-bytes', 'root-size-bytes', 'sector-size'):
        check.add_argument('--' + name, type=int, default=0)
    mounts_parser = sub.add_parser('check-mounts')
    mounts_parser.add_argument('--handoff', required=True)
    mounts_parser.add_argument('--disk', required=True)
    record = sub.add_parser('write-reclaim-record')
    record.add_argument('--handoff', required=True)
    record.add_argument('--target-var', required=True)
    record.add_argument('--target-root-partuuid', required=True)
    record.add_argument('--target-esp-partuuid', required=True)
    record.add_argument('--target-root-start-bytes', type=int, required=True)
    record.add_argument('--target-root-size-bytes', type=int, required=True)
    record.add_argument('--sector-size', type=int, required=True)
    retained_preflight = sub.add_parser('preflight-retained')
    retained_preflight.add_argument('--disk', required=True)
    retained_preflight.add_argument('--write-start', type=int, action='append', default=[])
    retained_preflight.add_argument('--write-size', type=int, action='append', default=[])
    retained_delivery = sub.add_parser('deliver-retained')
    retained_delivery.add_argument('--target-var', required=True)
    uid_query = sub.add_parser('required-user-id')
    uid_query.add_argument('--username', required=True)
    delivery = sub.add_parser('deliver-user-plan')
    delivery.add_argument('--username', required=True)
    delivery.add_argument('--test-home')
    delivery.add_argument('--test-uid', type=int)
    delivery.add_argument('--test-gid', type=int)
    delivery.add_argument('--test-seed-dir')
    finalize = sub.add_parser('firmware-finalize')
    finalize.add_argument('--handoff', required=True)
    finalize.add_argument('--target-esp-partuuid', required=True)
    finalize.add_argument('--target-disk', default='')
    finalize.add_argument('--target-esp-partition-number', type=int, default=0)
    finalize.add_argument('--loader', default='')
    args = parser.parse_args(argv)
    handlers = {'detect': detect, 'check-install': check_install, 'check-mounts': check_mounts_command,
                'write-reclaim-record': write_reclaim_record, 'deliver-user-plan': deliver_user_plan,
                'required-user-id': required_user_id, 'preflight-retained': preflight_retained,
                'deliver-retained': deliver_retained,
                'firmware-finalize': firmware_finalize}
    if args.command not in handlers:
        parser.print_usage(sys.stderr)
        return 2
    try:
        return handlers[args.command](args) or 0
    except (Refusal, OSError, KeyError) as error:
        print('error: staged installation: %s' % error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
