#!/usr/bin/env python3
"""Regenerate native parser fixtures from the last Python implementation.

The oracle lives only in tests; native tests consume the checked-in JSON and
never require a Python interpreter on the installed system.
"""
import copy
import json
import io
import os
from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
oracle = runpy.run_path(str(HERE / 'staging_handoff_oracle.py'))
GIB = 1024 ** 3
DISK_GUID = '11111111-2222-4333-8444-555555555555'
TARGET = (100 * GIB // 512 + 2048, 164 * GIB // 512 + 2047)
ESP = (TARGET[1] + 1, TARGET[1] + GIB // 512)
ROOT = (ESP[1] + 1, ESP[1] + 40 * GIB // 512)
BASE = {
    'schema': 1, 'operation_id': '0f0e0d0c-0b0a-4908-8706-050403020100',
    'host_os': 'windows', 'disk_guid': DISK_GUID.upper(),
    'disk_size_bytes': 256 * GIB,
    'staging_esp': {'partuuid': 'aaaaaaaa-0000-4000-8000-000000000001', 'first_lba': ESP[0], 'last_lba': ESP[1]},
    'staging_root': {'partuuid': 'bbbbbbbb-0000-4000-8000-000000000002', 'first_lba': ROOT[0], 'last_lba': ROOT[1]},
    'target_extent': {'first_lba': TARGET[0], 'last_lba': TARGET[1]},
    'owned_boot_entry': '0104', 'original_boot_order': ['0001', '0000'],
    'profile': {'schema': 1, 'hostname': 'studio', 'username': 'quinten', 'locale': 'en_US.UTF-8', 'keyboard': 'de', 'timezone': 'Europe/Berlin'},
}
MIGRATION = {
    'schema': 1, 'operation_id': BASE['operation_id'],
    'categories': {'home': True, 'flatpak': True, 'accounts': True},
    'home': {
        'volume': {'disk_guid': DISK_GUID, 'disk_bytes': 256 * GIB, 'sector_bytes': 512,
                   'partition_guid': '00000000-0000-4000-8000-00000000000b',
                   'first_lba': 206848, 'last_lba': 100 * GIB // 512 - 1,
                   'filesystem_uuid': '33333333-3333-4333-8333-333333333333', 'filesystem': 'ext4'},
        'relative': {'platform': 'Unix', 'units': list(b'home/person')},
        'inode': 123, 'original_uid': 1000, 'subvolume': None,
    },
    'flatpak_inventory': {'schema': 1, 'inventory': {'refs': [], 'remotes': []}, 'selected': [], 'installations': []},
}
cases = []


def add(name, document):
    data = json.dumps(document, ensure_ascii=False, separators=(',', ':'))
    case = {'name': name, 'input': data}
    try:
        case['normalized'] = oracle['parse_handoff'](data.encode())
    except oracle['Refusal'] as error:
        case['refusal'] = str(error)
    if name == 'btrfs subvolume inode 123':
        # The Python oracle checked the tag and inode but never bound the
        # subvolume's filesystem UUID to the retained source filesystem.
        case['native_refusal'] = 'migration Btrfs identity differs'
        case['native_reason'] = ('The all-zero subvolume filesystem identity does not match '
                                 'the retained filesystem UUID; native validation must bind both.')
    cases.append(case)


def change(name, path, value, migration=False):
    document = copy.deepcopy(BASE)
    if migration:
        document['host_os'] = 'linux'
        document['migration'] = copy.deepcopy(MIGRATION)
    target = document
    for key in path[:-1]:
        target = target[key]
    target[path[-1]] = value
    add(name, document)


add('legacy profile', BASE)
for field, values in {
    'purpose': ['computer', 'server', '', 'Computer', None, True],
    'device_label': ['', 'Studio', '界' * 21, '界' * 22, 'a' * 64, 'a' * 65, ' ', '\u0085', 'a\nb', None],
    'full_name': ['界' * 256, '界' * 257, 123],
    'filesystem': ['btrfs', 'ext4'],
}.items():
    for index, value in enumerate(values):
        change(f'profile {field} {index}', ['profile', field], value)
receipt = {'schema': 1, 'sealed_bytes': 48, 'ciphertext_sha256': 'a' * 64}
change('prepared minimum', ['profile', 'prepared_setup'], receipt)
for field, values in {'schema': [True, 2], 'sealed_bytes': [47, 32 * 1024 * 1024 + 16384, 32 * 1024 * 1024 + 16385, True, 48.0], 'ciphertext_sha256': ['A' * 64, 'a' * 63, 'z' * 64, None]}.items():
    for index, value in enumerate(values):
        candidate = dict(receipt, **{field: value})
        change(f'prepared {field} {index}', ['profile', 'prepared_setup'], candidate)
change('prepared unknown field', ['profile', 'prepared_setup'], dict(receipt, password='forbidden'))
change('prepared not object', ['profile', 'prepared_setup'], None)
change('migration default copy', ['migration', 'schema'], 1, True)
for index, value in enumerate(['copy', 'access-read-only', 'access-read-write', '', None, True]):
    change(f'home policy {index}', ['migration', 'home_policy'], value, True)
for name, path, value in [
    ('operation mismatch', ['operation_id'], '11111111-2222-4333-8444-555555555555'),
    ('schema boolean', ['schema'], True),
    ('empty categories', ['categories'], {'home': False, 'flatpak': False, 'accounts': False}),
    ('category integer', ['categories', 'home'], 1),
    ('unknown category', ['categories', 'passwords'], True),
    ('uid overflow', ['home', 'original_uid'], 2 ** 32),
    ('uid boolean', ['home', 'original_uid'], True),
    ('zero inode', ['home', 'inode'], 0),
    ('inode boolean', ['home', 'inode'], True),
    ('locator platform', ['home', 'relative', 'platform'], 'Windows'),
    ('locator traversal', ['home', 'relative', 'units'], list(b'home/../person')),
    ('locator absolute', ['home', 'relative', 'units'], list(b'/home/person')),
    ('locator empty component', ['home', 'relative', 'units'], list(b'home//person')),
    ('locator zero', ['home', 'relative', 'units'], [0]),
    ('locator boolean', ['home', 'relative', 'units'], [True]),
    ('locator non UTF8', ['home', 'relative', 'units'], [104, 255]),
    ('volume nil uuid', ['home', 'volume', 'filesystem_uuid'], '00000000-0000-0000-0000-000000000000'),
    ('volume wrong disk', ['home', 'volume', 'disk_guid'], '33333333-3333-4333-8333-333333333333'),
    ('volume wrong size', ['home', 'volume', 'disk_bytes'], 128 * GIB),
    ('volume bad sector', ['home', 'volume', 'sector_bytes'], 1024),
    ('volume unsupported fs', ['home', 'volume', 'filesystem'], 'ntfs'),
    ('volume overlap', ['home', 'volume', 'last_lba'], TARGET[0]),
    ('volume staging identity', ['home', 'volume', 'partition_guid'], BASE['staging_esp']['partuuid']),
    ('unexpected subvolume', ['home', 'subvolume'], {'volume': [3] + [0] * 40, 'inode': 123}),
    ('flatpak unknown field', ['flatpak_inventory', 'password'], 'forbidden'),
    ('flatpak boolean schema', ['flatpak_inventory', 'schema'], True),
    ('flatpak without consent', ['categories', 'flatpak'], False),
]:
    change(name, ['migration'] + path, value, True)
for inode in [123, 124, True]:
    document = copy.deepcopy(BASE)
    document['host_os'] = 'linux'
    document['migration'] = copy.deepcopy(MIGRATION)
    home = document['migration']['home']
    home['volume']['filesystem'] = 'btrfs'
    home['subvolume'] = {'volume': [3] + [0] * 40, 'inode': inode}
    add(f'btrfs subvolume inode {inode}', document)
# A correctly bound Btrfs identity exercises native acceptance and subsequent
# on-disk source revalidation as well as refusing the malformed oracle case.
home['subvolume']['inode'] = home['inode']
home['subvolume']['volume'] = ([3] + list(bytes.fromhex(home['volume']['filesystem_uuid'].replace('-', '')))
                               + [0] * 24)
add('btrfs bound subvolume identity', document)
for url in ['https://example.org/repo', 'http://example.org/repo', 'https://user:secret@example.org/repo', 'https://example.org/repo?token=secret', 'https://example.org/repo#fragment']:
    change(f'flatpak remote {url}', ['migration', 'flatpak_inventory', 'inventory', 'remotes'],
           [{'installation': 'User', 'name': 'example', 'url': url, 'options': ''}], True)
(HERE / 'staging_handoff_parity.json').write_text(json.dumps(cases, ensure_ascii=False, indent=2) + '\n')
print(f'wrote {len(cases)} parser cases')

# Authentication delegates its closed public-metadata schema to the real native
# extractor, exactly as the old command did. Never substitute a permissive stub.
extractor = Path(os.environ.get('TILO_PREPARED_EXTRACT_BINARY', HERE.parents[4] / 'target/debug/tilo-installer-prepared-setup-extract')).resolve()
if not extractor.is_file() or extractor.read_bytes()[:4] != b'\x7fELF':
    raise RuntimeError('build tilo-installer-prepared-setup-extract before regenerating authenticated fixtures')
oracle['authenticated_retained_plan'].__globals__['__file__'] = str(extractor.parent / 'staging_handoff_oracle.py')
auth_cases = []


def auth(name, receipt):
    data = json.dumps(receipt, ensure_ascii=False, separators=(',', ':'))
    case = {'name': name, 'input': data}
    try:
        case['plan'] = oracle['authenticated_retained_plan'](io.TextIOWrapper(io.BytesIO(data.encode()), encoding='utf-8'))
    except oracle['Refusal'] as error:
        case['refusal'] = str(error)
    auth_cases.append(case)


auth('no retained source', {'schema': 1})
for name, receipt in [
    ('schema boolean', {'schema': True}),
    ('unknown field', {'schema': 1, 'password': 'forbidden'}),
    ('applications boolean', {'schema': 1, 'completed_applications_only': 1}),
    ('applications purpose', {'schema': 1, 'completed_applications_only': True}),
    ('completed setup without generation', {'schema': 1, 'completed_setup': {}}),
    ('completed generation without setup', {'schema': 1, 'completed_home': {}}),
    ('unexpected computer scope', {'schema': 1, 'computer_apps_scope': {}}),
    ('applications without generation', {'schema': 1, 'completed_applications': {}}),
    ('malformed pending scope', {'schema': 1, 'pending_computer_applications_scope': {}}),
]:
    auth(name, receipt)
for policy in ['copy', 'access-read-only', 'access-read-write']:
    plan = dict(copy.deepcopy(MIGRATION), home_policy=policy)
    auth(f'home {policy}', {'schema': 1, 'retained_home': plan})
plan = copy.deepcopy(MIGRATION)
plan['categories']['home'] = False
auth('independent applications', {'schema': 1, 'retained_home': plan})
plan.pop('flatpak_inventory')
auth('independent applications missing inventory', {'schema': 1, 'retained_home': plan})
(HERE / 'staging_handoff_authenticated_parity.json').write_text(json.dumps(auth_cases, ensure_ascii=False, indent=2) + '\n')
print(f'wrote {len(auth_cases)} authenticated cases')
