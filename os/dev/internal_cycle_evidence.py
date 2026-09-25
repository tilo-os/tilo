#!/usr/bin/python3
"""Evidence for the USB-free installation VM round trip (design spec §9).

`collect` runs inside the retained guest as root and prints one JSON document:
the system disk's partition table (`sfdisk --json`), every firmware boot
variable decoded from efivarfs *bytes* (plus `efibootmgr -v` text for humans),
SHA-256 of every EFI loader on the retained ESP, and `/etc/machine-id`.

The `check-*` subcommands run on the host and compare such documents against
the design contract. Each prints a JSON verdict and exits 1 on any violation,
so os/dev/test-internal-install-cycle.sh fails instead of reporting a pass it
could not observe.

  collect
  check-staged   --baseline B --staged S --summary REQUEST_SUMMARY
  check-unstaged --baseline B --after A [--expect-boot-next ID]
  check-final    --baseline B --staged S --summary REQUEST_SUMMARY --final F

Only the standard library is used; the module is also imported by
os/tests/internal_install_cycle_test.py with fixture documents.
"""
import argparse
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import uuid

EFI_GLOBAL = '8be4df61-93ca-11d2-aa0d-00e098032b8c'
ESP_TYPE = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
LINUX_TYPE = '0fc63daf-8483-4772-8e79-3d69d8477de4'
TILO_ROOT_TYPE = '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab'
MIB_SECTORS = 2048
TILO_ESP_BYTES = 2 << 30
TILO_BOOT_BYTES = 1 << 30


# --- collection (guest, root) ------------------------------------------------

def decode_load_option(data):
    """EFI_LOAD_OPTION (without the efivarfs attribute prefix) -> summary."""
    if len(data) < 6:
        return {'description': None, 'partuuid': None, 'path': None}
    path_length = struct.unpack_from('<H', data, 4)[0]
    offset = 6
    while offset + 2 <= len(data) and data[offset:offset + 2] != b'\0\0':
        offset += 2
    description = data[6:offset].decode('utf-16-le', 'replace')
    path = data[offset + 2:offset + 2 + path_length]
    partuuid = file_path = None
    part = None
    cursor = 0
    while cursor + 4 <= len(path):
        kind, subtype, length = struct.unpack_from('<BBH', path, cursor)
        if length < 4:
            break
        body = path[cursor + 4:cursor + length]
        if kind == 0x7F:
            break
        if kind == 0x04 and subtype == 0x01 and length == 42 and body[37] == 0x02:
            part = struct.unpack_from('<IQQ', body, 0)
            partuuid = str(uuid.UUID(bytes_le=bytes(body[20:36])))
        elif kind == 0x04 and subtype == 0x04:
            file_path = body.decode('utf-16-le', 'replace').rstrip('\0')
        cursor += length
    result = {'description': description, 'partuuid': partuuid, 'path': file_path}
    if part:
        result.update(partition_number=part[0], start_lba=part[1], size_lba=part[2])
    return result


def read_var(directory, name):
    path = os.path.join(directory, '%s-%s' % (name, EFI_GLOBAL))
    try:
        with open(path, 'rb') as handle:
            return handle.read()[4:]
    except FileNotFoundError:
        return None


def ids(value):
    if value is None:
        return []
    return ['%04X' % item for (item,) in struct.iter_unpack('<H', value[:len(value) // 2 * 2])]


def firmware(directory='/sys/firmware/efi/efivars'):
    entries = {}
    pattern = re.compile(r'^Boot([0-9A-Fa-f]{4})-' + EFI_GLOBAL + '$')
    for name in sorted(os.listdir(directory)):
        match = pattern.match(name)
        if not match:
            continue
        number = match.group(1).upper()
        data = read_var(directory, 'Boot' + number)
        entry = decode_load_option(data)
        entry['hex'] = data.hex()
        entries[number] = entry
    next_ids = ids(read_var(directory, 'BootNext'))
    current = ids(read_var(directory, 'BootCurrent'))
    return {
        'boot_order': ids(read_var(directory, 'BootOrder')),
        'boot_next': next_ids[0] if next_ids else None,
        'boot_current': current[0] if current else None,
        'entries': entries,
    }


def system_disk():
    source = subprocess.check_output(['findmnt', '-no', 'SOURCE', '/'], text=True).strip()
    source = re.sub(r'\[.*\]$', '', source)
    parent = subprocess.check_output(['lsblk', '-no', 'PKNAME', source], text=True).split()
    if len(parent) != 1:
        raise SystemExit('cannot resolve the disk holding / (%s)' % source)
    return '/dev/' + parent[0]


def esp_loaders(root='/boot/efi'):
    found = {}
    for directory, _, files in os.walk(root):
        for name in files:
            if name.lower().endswith('.efi'):
                path = os.path.join(directory, name)
                with open(path, 'rb') as handle:
                    found[os.path.relpath(path, root)] = hashlib.sha256(handle.read()).hexdigest()
    return dict(sorted(found.items()))


def collect():
    disk = system_disk()
    table = json.loads(subprocess.check_output(['sfdisk', '--json', disk], text=True))['partitiontable']
    efibootmgr = subprocess.run(['efibootmgr', '-v'], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True).stdout
    with open('/etc/machine-id', encoding='ascii') as handle:
        machine_id = handle.read().strip()
    return {
        'schema': 1,
        'disk': disk,
        'disk_size_bytes': int(subprocess.check_output(['blockdev', '--getsize64', disk], text=True)),
        'partition_table': table,
        'firmware': firmware(),
        'efibootmgr_v': efibootmgr,
        'esp_loaders': esp_loaders(),
        'machine_id': machine_id,
    }


# --- comparison (host) -------------------------------------------------------

def partitions(document):
    table = document['partition_table']
    if int(table['sectorsize']) != 512:
        raise ValueError('the round trip is defined for 512-byte sectors')
    result = []
    for part in table.get('partitions', []):
        start, size = int(part['start']), int(part['size'])
        result.append({'uuid': part['uuid'].lower(), 'type': part['type'].lower(),
                       'name': part.get('name', ''), 'first_lba': start, 'last_lba': start + size - 1})
    return sorted(result, key=lambda p: p['first_lba'])


def identity(part):
    return (part['uuid'], part['type'], part['first_lba'], part['last_lba'])


def extent(summary, key):
    value = summary[key]
    return int(value['first_lba']), int(value['last_lba'])


class Verdict:
    def __init__(self, name):
        self.name, self.failures, self.facts = name, [], {}

    def require(self, condition, message):
        if not condition:
            self.failures.append(message)
        return condition

    def emit(self):
        print(json.dumps({'check': self.name, 'ok': not self.failures, 'failures': self.failures,
                          'facts': self.facts}, indent=2, sort_keys=True))
        return 0 if not self.failures else 1


def baseline_preserved(verdict, baseline, after, label):
    before = {identity(p) for p in partitions(baseline)}
    now = {identity(p) for p in partitions(after)}
    missing = sorted(before - now)
    verdict.require(not missing, '%s: retained partitions changed or disappeared: %s' % (label, missing))
    verdict.require(after['esp_loaders'] == baseline['esp_loaders'],
                    '%s: retained ESP loader bytes changed' % label)
    verdict.require(after['machine_id'] == baseline['machine_id'],
                    '%s: /etc/machine-id changed (not the retained system)' % label)
    changed = sorted(number for number, entry in baseline['firmware']['entries'].items()
                     if after['firmware']['entries'].get(number, {}).get('hex') != entry['hex'])
    verdict.require(not changed, '%s: retained firmware entries changed: %s' % (label, changed))
    return set(now) - set(before)


def check_staged(baseline, staged, summary):
    verdict = Verdict('staged')
    added = baseline_preserved(verdict, baseline, staged, 'after stage')
    parts = {p['uuid']: p for p in partitions(staged)}
    esp_first, esp_last = extent(summary, 'staging_esp')
    root_first, root_last = extent(summary, 'staging_root')
    target_first, target_last = extent(summary, 'target_extent')
    esp = parts.get(str(summary['staging_esp'].get('partition_guid') or '').lower())
    root = parts.get(str(summary['staging_root'].get('partition_guid') or '').lower())
    verdict.require(len(added) == 2, 'expected exactly two new partitions, found %d' % len(added))
    if verdict.require(esp is not None, 'staging ESP with the planned GUID is missing'):
        verdict.require((esp['type'], esp['first_lba'], esp['last_lba']) == (ESP_TYPE, esp_first, esp_last),
                        'staging ESP type or extent differs from the plan')
        verdict.require(esp['name'] == 'Tilo installer boot', 'staging ESP is not named "Tilo installer boot"')
    if verdict.require(root is not None, 'staging root with the planned GUID is missing'):
        verdict.require((root['type'], root['first_lba'], root['last_lba']) == (LINUX_TYPE, root_first, root_last),
                        'staging root type or extent differs from the plan (must be the generic Linux type)')
        verdict.require(root['name'] == 'Tilo installer', 'staging root is not named "Tilo installer"')
    verdict.require(esp_first == target_last + 1, 'staging ESP does not start right after the target extent')
    verdict.require(root_first == esp_last + 1, 'staging root does not follow the staging ESP')
    target_sectors = target_last - target_first + 1
    verdict.require(target_sectors * 512 >= 64 << 30, 'target extent is smaller than the requested 64 GiB')
    for value in (target_first, target_sectors, esp_first, esp_last + 1, root_first, root_last + 1):
        verdict.require(value % MIB_SECTORS == 0, 'staging geometry is not 1 MiB aligned')
    others = [p for p in partitions(staged) if p['uuid'] not in (esp and esp['uuid'], root and root['uuid'])]
    for part in others:
        verdict.require(part['last_lba'] < target_first or part['first_lba'] > root_last,
                        '%s overlaps the target or staging extent' % part['uuid'])
        if part['last_lba'] < target_first:
            verdict.require(target_first - part['last_lba'] - 1 >= MIB_SECTORS,
                            'target extent starts within 1 MiB of %s' % part['uuid'])
    fw_before, fw = baseline['firmware'], staged['firmware']
    verdict.require(fw['boot_order'] == fw_before['boot_order'], 'BootOrder changed while staging')
    owned = fw['boot_next']
    verdict.facts['owned_boot_entry'] = owned
    if verdict.require(owned is not None, 'BootNext was not set'):
        verdict.require(owned not in fw_before['entries'], 'BootNext names a pre-existing entry')
        entry = fw['entries'].get(owned) or {}
        verdict.require(entry.get('partuuid') == (esp and esp['uuid']),
                        'owned Boot%s does not point at the staging ESP' % owned)
        verdict.require((entry.get('start_lba'), entry.get('size_lba')) == (esp_first, esp_last - esp_first + 1),
                        'owned Boot%s device path does not match the staging ESP extent' % owned)
        verdict.require(entry.get('description') == 'Tilo installer',
                        'owned Boot%s is not described as "Tilo installer"' % owned)
        verdict.require(str(entry.get('path') or '').upper() in ('\\EFI\\BOOT\\BOOTX64.EFI', '\\EFI\\BOOT\\BOOTAA64.EFI'),
                        'owned Boot%s does not load the fallback loader path' % owned)
        verdict.require(owned not in fw['boot_order'], 'owned Boot%s entered BootOrder' % owned)
    new_entries = sorted(set(fw['entries']) - set(fw_before['entries']))
    verdict.require(new_entries == ([owned] if owned else []),
                    'unexpected new firmware entries: %s' % new_entries)
    return verdict


def check_unstaged(baseline, after, expect_boot_next=None):
    verdict = Verdict('unstaged')
    added = baseline_preserved(verdict, baseline, after, 'after')
    verdict.require(not added, 'partitions were added: %s' % sorted(added))
    fw_before, fw = baseline['firmware'], after['firmware']
    verdict.require(fw['boot_order'] == fw_before['boot_order'], 'BootOrder changed')
    verdict.require(fw['boot_next'] == expect_boot_next,
                    'BootNext is %s, expected %s' % (fw['boot_next'], expect_boot_next))
    extra = sorted(set(fw['entries']) - set(fw_before['entries']))
    verdict.require(not extra, 'firmware entries were added or left behind: %s' % extra)
    return verdict


def check_final(baseline, staged, summary, final):
    verdict = Verdict('final')
    added = baseline_preserved(verdict, baseline, final, 'after reclaim')
    owned = staged['firmware']['boot_next']
    staging = {str(summary[key].get('partition_guid') or '').lower() for key in ('staging_esp', 'staging_root')}
    parts = partitions(final)
    new = [p for p in parts if identity(p) in added]
    verdict.require(not any(p['uuid'] in staging for p in parts), 'a staging partition survived')
    target_first, _ = extent(summary, 'target_extent')
    _, root_last = extent(summary, 'staging_root')
    esps = [p for p in new if p['type'] == ESP_TYPE]
    boots = [p for p in new if p['type'] == LINUX_TYPE]
    roots = [p for p in new if p['type'] == TILO_ROOT_TYPE]
    verdict.require(len(new) == 3 and len(esps) == 1 and len(boots) == 1 and len(roots) == 1,
                    'expected exactly one new Tilo ESP, boot, and encrypted root, found %s' % [identity(p) for p in new])
    if esps and boots and roots:
        esp, boot, root = esps[0], boots[0], roots[0]
        verdict.facts.update(tilo_esp=esp['uuid'], tilo_boot=boot['uuid'], tilo_root=root['uuid'])
        verdict.require(esp['first_lba'] == target_first, 'Tilo ESP does not start at the staged target extent')
        verdict.require((esp['last_lba'] - esp['first_lba'] + 1) * 512 == TILO_ESP_BYTES, 'Tilo ESP is not 2 GiB')
        verdict.require(boot['first_lba'] == esp['last_lba'] + 1, 'Tilo boot does not follow the Tilo ESP')
        verdict.require((boot['last_lba'] - boot['first_lba'] + 1) * 512 == TILO_BOOT_BYTES, 'Tilo boot is not 1 GiB')
        verdict.require(root['first_lba'] == boot['last_lba'] + 1, 'Tilo root does not follow Tilo boot')
        verdict.require(root['last_lba'] == root_last,
                        'Tilo root ends at LBA %d, not at the end of the former staging region (%d)'
                        % (root['last_lba'], root_last))
    fw_before, fw = baseline['firmware'], final['firmware']
    verdict.require(owned not in fw['entries'], 'owned staging entry Boot%s survived' % owned)
    tilo_ids = {number for number, entry in fw['entries'].items()
                if esps and entry.get('partuuid') == esps[0]['uuid']}
    verdict.facts['tilo_boot_entries'] = sorted(tilo_ids)
    # The installer restores BootOrder exactly; only an entry for the new Tilo
    # ESP may appear (firmware may list a newly booted option), nothing else.
    residual = [number for number in fw['boot_order'] if number not in tilo_ids]
    verdict.require(residual == fw_before['boot_order'],
                    'BootOrder differs from the retained system beyond the Tilo entry: %s vs %s'
                    % (fw['boot_order'], fw_before['boot_order']))
    extra = sorted(set(fw['entries']) - set(fw_before['entries']) - tilo_ids)
    verdict.require(not extra, 'unexpected firmware entries: %s' % extra)
    verdict.require(fw['boot_next'] is None, 'BootNext is still set to %s' % fw['boot_next'])
    return verdict


def load(path):
    with open(path, encoding='utf-8') as handle:
        return json.load(handle)


def main(argv):
    parser = argparse.ArgumentParser(prog='internal_cycle_evidence')
    sub = parser.add_subparsers(dest='command')
    sub.add_parser('collect')
    staged = sub.add_parser('check-staged')
    staged.add_argument('--baseline', required=True)
    staged.add_argument('--staged', required=True)
    staged.add_argument('--summary', required=True)
    unstaged = sub.add_parser('check-unstaged')
    unstaged.add_argument('--baseline', required=True)
    unstaged.add_argument('--after', required=True)
    unstaged.add_argument('--expect-boot-next')
    final = sub.add_parser('check-final')
    final.add_argument('--baseline', required=True)
    final.add_argument('--staged', required=True)
    final.add_argument('--summary', required=True)
    final.add_argument('--final', required=True)
    args = parser.parse_args(argv)
    if args.command == 'collect':
        print(json.dumps(collect(), indent=2, sort_keys=True))
        return 0
    if args.command == 'check-staged':
        return check_staged(load(args.baseline), load(args.staged), load(args.summary)).emit()
    if args.command == 'check-unstaged':
        return check_unstaged(load(args.baseline), load(args.after), args.expect_boot_next).emit()
    if args.command == 'check-final':
        return check_final(load(args.baseline), load(args.staged), load(args.summary),
                           load(args.final)).emit()
    parser.print_usage(sys.stderr)
    return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
