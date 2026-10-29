"""Receiving app evidence through the installed semantic door and real Flatpak.

Source descriptions select observations, never grant access. Missing actions,
unknown completion or process retirement refuse the qualification.
"""
import hashlib
import json
import os
from pathlib import Path
import select
import stat
import time
import uuid

FILES = 'so.tilo.file-explorer'
WRITER = 'so.tilo.word-processor'
SHARED_HOME = '/run/tilo/retained-home'
_UNKNOWN_FLATPAKS = []


def _pause(services, deadline):
    services.check_live()
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RuntimeError('actual app observation deadline elapsed')
    time.sleep(min(0.05, remaining))


def _unique(rows, predicate):
    matches = [row for row in rows if predicate(row)]
    if len(matches) > 1:
        raise RuntimeError('actual app action is ambiguous')
    return matches[0] if matches else None


def _row(control, predicate, services, deadline):
    while time.monotonic() < deadline:
        services.check_live()
        selected = _unique(control.rows(), predicate)
        if selected is not None:
            return selected
        _pause(services, deadline)
    raise RuntimeError('required actual app action was never published')


def _queried_rows(control, query):
    rows, cursor, seen = [], None, set()
    for _ in range(128):
        arguments = ['actions', 'list', query, '--json', '--limit', '128']
        if cursor is not None:
            arguments += ['--cursor', cursor]
        page = control.command(arguments)
        current = page.get('actions')
        if not isinstance(current, list) or len(current) > 128:
            raise RuntimeError('actual scoped resource action page is invalid')
        rows.extend(current)
        cursor = page.get('next_cursor')
        if cursor is None:
            return rows
        if not isinstance(cursor, str) or not cursor or cursor in seen:
            raise RuntimeError('actual scoped resource cursor is invalid')
        seen.add(cursor)
    raise RuntimeError('actual scoped resource pagination exceeded bound')


def _selected_open(control, leaf, services, deadline):
    while time.monotonic() < deadline:
        services.check_live()
        selected = _unique(_queried_rows(control, leaf), lambda row:
                           row.get('application') == FILES and row.get('operation') == 'open-with'
                           and WRITER in _choices(row, 'app') and 'edit-in-parent' in _choices(row, 'access')
                           and isinstance(row.get('subtitle'), str)
                           and row['subtitle'].split(' · ', 1)[0] == leaf)
        if selected is not None:
            return selected
        _pause(services, deadline)
    raise RuntimeError('actual selected source file lacks a unique editable OpenWith action')


def _param(row, name):
    return next((param for param in row.get('params', []) if param.get('name') == name), None)


def _choices(row, name):
    parameter = _param(row, name) or {}
    return parameter.get('choices') or (parameter.get('schema') or {}).get('enum') or []


def _uuid(value):
    if not isinstance(value, str) or not value or uuid.UUID(value).int == 0:
        raise RuntimeError('actual observation has invalid identity')
    return value


def _authenticate(control, services):
    prompts = [row for row in control.rows() if row.get('provider') == 'authentication-prompts'
               and row.get('id', '').startswith('answer@')]
    if prompts:
        callback = getattr(services, 'answer_authentication', None)
        if not callable(callback):
            raise RuntimeError('actual PAM prompt lacks the owned account responder')
        callback()  # Queue delivery is not original operation success.


def _invoke(control, row, args, services, deadline):
    services.check_live()
    reply = control.invoke(row, args)
    receipt = reply.get('receipt')
    if not isinstance(receipt, dict) or not isinstance(receipt.get('update'), dict):
        raise RuntimeError('actual invocation supplied no lifecycle receipt')
    invocation = _uuid(receipt['update'].get('invocation'))
    broker = receipt.get('broker_invocation_id')
    while time.monotonic() < deadline:
        services.check_live()
        update = receipt['update']
        if _uuid(update.get('invocation')) != invocation:
            raise RuntimeError('actual invocation identity changed')
        state = update.get('state')
        if state == 'succeeded':
            output = update.get('output')
            if not isinstance(output, dict):
                raise RuntimeError('actual completed action has no output')
            return output.get('value')
        if state in ('failed', 'cancelled'):
            raise RuntimeError('actual app invocation did not succeed')
        if state not in ('accepted', 'progress', 'item') or type(broker) is not int or broker <= 0:
            raise RuntimeError('actual pending invocation is not observable')
        _authenticate(control, services)
        reply = control.command(['actions', 'status', str(broker)])
        receipt = reply.get('receipt')
        if (not isinstance(receipt, dict) or receipt.get('broker_invocation_id') != broker
                or not isinstance(receipt.get('update'), dict)):
            raise RuntimeError('actual broker status changed custody')
        _pause(services, deadline)
    raise RuntimeError('actual invocation completion remains unknown')


def _application_row(control, application, operation, services, deadline, resource=None):
    return _row(control, lambda row: row.get('application') == application
                and row.get('operation') == operation
                and (resource is None or row.get('resource') == resource), services, deadline)


def _read_text(control, services, deadline):
    cursor, identity, text, seen = None, None, [], set()
    for _ in range(8):
        row = _application_row(control, WRITER, 'read-text', services, deadline)
        args = {'pageSize': 64, 'format': 'segments'}
        if cursor is not None:
            args['cursor'] = cursor
        page = _invoke(control, row, args, services, deadline)
        if not isinstance(page, dict) or page.get('record') is None or page.get('basis') is None:
            raise RuntimeError('actual Writer read is missing its record basis')
        current = (page['record'], page['basis'])
        if identity is not None and current != identity:
            raise RuntimeError('Writer changed during actual text pagination')
        identity = current
        segments = page.get('content', {}).get('segments')
        if not isinstance(segments, list) or len(segments) > 64:
            raise RuntimeError('actual text page exceeded its contract')
        for segment in segments:
            if (segment.get('kind') != 'paragraph' or segment.get('complete') is not True
                    or not isinstance(segment.get('text'), str)):
                raise RuntimeError('fixture document is not complete plain paragraph text')
            text.append(segment['text'] + '\n')
        if sum(len(value.encode()) for value in text) > 65536:
            raise RuntimeError('actual document exceeds receiving fixture bound')
        cursor = page.get('nextCursor')
        if cursor is None:
            return ''.join(text)
        if not isinstance(cursor, str) or not cursor or len(cursor) > 1024 or cursor in seen:
            raise RuntimeError('actual Writer cursor repeated or changed shape')
        seen.add(cursor)
    raise RuntimeError('actual Writer text pagination did not finish')


def _close(control, application, services, deadline):
    dump = control.command(['state-dump'])
    state = dump.get('state', dump)
    matches = []
    workspaces = state.get('workspace_tree', {}).get('workspaces')
    if not isinstance(workspaces, list):
        raise RuntimeError('actual desktop supplied no workspace tree')
    for workspace in workspaces:
        for strip in workspace.get('strips', []):
            for group in strip.get('window_groups', []):
                for tab in group.get('tabs', []):
                    tile = tab.get('tile', {})
                    if any(isinstance(tile.get(kind), dict) and tile[kind].get('app_id') == application
                           for kind in ('Foreign', 'LocalApp', 'PeerWindow', 'HostedWindow')):
                        matches.append((group.get('id'), tab.get('id')))
    if len(matches) != 1 or any(type(value) is not int or value < 0 for value in matches[0]):
        raise RuntimeError('actual app window was not uniquely owned by the desktop')
    group, tab = matches[0]
    row = _row(control, lambda row: row.get('provider') == 'desktop-windows'
               and row.get('id') == f'tab.close:{group}:{tab}', services, deadline)
    _invoke(control, row, {}, services, deadline)
    while any(row.get('application') == application for row in control.rows()):
        _pause(services, deadline)
    # Window disappearance alone is not source/namespace retirement.


def _history(control, person, services, deadline):
    cursor, seen, identity, versions = None, set(), None, {}
    for _ in range(16):
        row = _application_row(control, WRITER, 'history.list', services, deadline,
                               'tilo.host:history')
        value = _invoke(control, row, {} if cursor is None else {'cursor': cursor}, services, deadline)
        if not isinstance(value, dict) or set(value) != {'page', 'next_cursor'}:
            raise RuntimeError('actual file History supplied an unsupported wrapper')
        page = value['page']
        if not isinstance(page, dict) or set(page) != {'scope', 'versions', 'total_versions', 'heads', 'current_revision', 'next_after'}:
            raise RuntimeError('actual native History page changed schema')
        scope = page.get('scope', {})
        if not isinstance(scope, dict) or set(scope) != {'workspace_id', 'mount_lifetime', 'record_id', 'frontier'} or not _key(scope['frontier']):
            raise RuntimeError('actual native History scope is invalid')
        for name in ('workspace_id', 'mount_lifetime', 'record_id'):
            _uuid(scope.get(name))
        current = (scope, page.get('total_versions'), page.get('heads'))
        if identity is not None and current != identity:
            raise RuntimeError('actual native History changed during pagination')
        identity = current
        total, heads = current[1:]
        if type(total) is not int or not 66 <= total <= 192 or not isinstance(heads, list) or not heads:
            raise RuntimeError('actual native History lacks the authored Save lineage')
        for head in heads:
            _uuid(head)
        if len(set(heads)) != len(heads):
            raise RuntimeError('actual native History duplicated a head')
        items = page.get('versions')
        if not isinstance(items, list) or len(items) > 12:
            raise RuntimeError('actual History page exceeds canonical bound')
        for version in items:
            if not isinstance(version, dict) or set(version) != {'id', 'record_id', 'parents', 'actor', 'timestamp_ms', 'content_hash', 'metadata_hash', 'provenance'}:
                raise RuntimeError('actual History version changed schema')
            actor = version['actor']
            if (not isinstance(actor, dict) or set(actor) != {'person', 'device'}
                    or not _key(actor['person']) or not _key(actor['device'])
                    or not _key(version['metadata_hash'])
                    or (version['content_hash'] is not None and not _key(version['content_hash']))
                    or type(version['timestamp_ms']) is not int or version['timestamp_ms'] < 0):
                raise RuntimeError('actual History version has invalid provenance fields')
            key = _uuid(version.get('id'))
            parents = version.get('parents')
            if (key in versions or version.get('record_id') != scope['record_id']
                    or not isinstance(parents, list) or len(parents) > 8 or key in parents):
                raise RuntimeError('native History duplicated or substituted a version')
            for parent in parents:
                _uuid(parent)
            versions[key] = version
        cursor = value['next_cursor']
        if cursor is None:
            if page.get('next_after') is not None or len(versions) != total:
                raise RuntimeError('actual History pagination is incomplete')
            if any(head not in versions for head in heads) or any(
                    parent not in versions for version in versions.values() for parent in version['parents']):
                raise RuntimeError('actual History parent/head closure is incomplete')
            visited, active = set(), set()
            def visit(key):
                if key in active:
                    raise RuntimeError('actual native History contains a parent cycle')
                if key in visited:
                    return
                active.add(key)
                for parent in versions[key]['parents']:
                    visit(parent)
                active.remove(key)
                visited.add(key)
            for key in versions:
                visit(key)
            edits = sum(version.get('provenance') == 'Edit'
                        and version.get('actor', {}).get('person') == person for version in versions.values())
            if edits < 66:
                raise RuntimeError('actual History omitted current-person Save attribution')
            evidence = [scope, total, heads, sorted(versions.values(), key=lambda value: value['id'])]
            encoded = json.dumps(evidence, sort_keys=True, separators=(',', ':')).encode()
            return {'scope': scope, 'versions': total, 'authored_edits': edits,
                    'evidence_sha256': hashlib.sha256(encoded).hexdigest()}
        if (not isinstance(cursor, str) or not cursor or len(cursor) > 2048
                or cursor in seen or page.get('next_after') is None):
            raise RuntimeError('actual History cursor is invalid or repeated')
        seen.add(cursor)
    raise RuntimeError('actual History pagination exceeded bound')


def _retirement_entries(control, services, deadline):
    row = _row(control, lambda row: row.get('provider') == 'session'
               and row.get('id') == 'retained-retirement', services, deadline)
    value = _invoke(control, row, {}, services, deadline)
    if not isinstance(value, dict) or set(value) != {'schema', 'entries'} or type(value['schema']) is not int or value['schema'] != 1:
        raise RuntimeError('actual source retirement observation has invalid schema')
    entries = value['entries']
    if not isinstance(entries, list) or len(entries) > 32:
        raise RuntimeError('actual source retirement entries exceeded bound')
    seen = set()
    fields = {'id', 'source_operation', 'context_sha256', 'root_pid', 'state',
              'child_reaped', 'proof_terminations', 'pending_proofs'}
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != fields:
            raise RuntimeError('actual source retirement entry has unsupported fields')
        identity = _uuid(entry['id'])
        _uuid(entry['source_operation'])
        context = entry['context_sha256']
        if (identity in seen or not isinstance(context, str) or len(context) != 64
                or any(ch not in '0123456789abcdef' for ch in context)
                or entry['state'] not in ('preparing', 'running', 'retiring', 'retired', 'unknown')
                or type(entry['pending_proofs']) is not int or not 0 <= entry['pending_proofs'] <= 256
                or type(entry['child_reaped']) is not bool
                or not isinstance(entry['proof_terminations'], list) or len(entry['proof_terminations']) > 258
                or (entry['root_pid'] is not None and (type(entry['root_pid']) is not int or entry['root_pid'] <= 1))):
            raise RuntimeError('actual source retirement identity/custody is invalid')
        seen.add(identity)
    return entries


def _retired(control, original, services, deadline):
    acceptable = {'completed', 'cleanup:UnexpectedEof', 'cleanup:BrokenPipe',
                  'cleanup:ConnectionReset', 'cleanup:Interrupted', 'cleanup:PermissionDenied'}
    while time.monotonic() < deadline:
        entries = _retirement_entries(control, services, deadline)
        current = _unique(entries, lambda row: row['id'] == original['id'])
        if current is None or any(current[key] != original[key]
                                  for key in ('source_operation', 'context_sha256', 'root_pid')):
            raise RuntimeError('actual root retirement record disappeared or changed identity')
        if current['state'] == 'unknown':
            raise RuntimeError('actual root/source retirement custody is unknown')
        if current['state'] == 'retired':
            terminations = current['proof_terminations']
            if (current['child_reaped'] is not True or current['pending_proofs'] != 0
                    or not 5 <= len(terminations) <= 258
                    or any(value not in acceptable for value in terminations)):
                raise RuntimeError('root receipt lacks actual child reap and all proof joins')
            return current
        _pause(services, deadline)
    raise RuntimeError('actual root/source retirement did not complete')


def _read_owned(path, uid, bound=4096):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != uid or before.st_nlink != 1
                or before.st_mode & 0o7022 or before.st_size > bound):
            raise RuntimeError('fixture observation is not a protected owned regular file')
        data = os.read(fd, bound + 1)
        stamp = lambda item: (item.st_dev, item.st_ino, item.st_uid, item.st_gid, item.st_mode,
                             item.st_nlink, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
        if len(data) != before.st_size or stamp(before) != stamp(os.fstat(fd)) or stamp(before) != stamp(os.lstat(path)):
            raise RuntimeError('fixture observation changed while held')
        return data
    finally:
        os.close(fd)


def _directory(path, uid):
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or info.st_mode & 0o022:
        raise RuntimeError('actual receiving directory is not protected by its account')


def _same_file(first, second):
    a, b = os.stat(first), os.stat(second)
    if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino):
        raise RuntimeError('actual executable or shared state inode differs')


def _process(pid):
    text = Path(f'/proc/{pid}/stat').read_text()
    if len(text) > 4096:
        raise RuntimeError('actual process observation exceeds bound')
    prefix, fields = text.rsplit(')', 1)
    fields = fields.split()
    if prefix.split()[0] != str(pid) or len(fields) < 20:
        raise RuntimeError('actual process identity is malformed')
    return int(fields[1]), int(fields[19])


def _live(fd):
    poll = select.poll()
    poll.register(fd, select.POLLIN)
    return not poll.poll(0)


def _flatpak(control, home, description, services, deadline):
    app_id = description['selected_flatpak_id']
    if app_id != 'org.tilo.Fixture':
        raise RuntimeError('selected source fixture app identity differs')
    data = home
    for edge in ('.var', 'app', app_id, 'data'):
        data /= edge
        _directory(data, os.geteuid())
    selected = data / 'selected-document'
    expected = _bytes(description['selected_flatpak_bytes'])
    if _read_owned(selected, os.geteuid()) != expected:
        raise RuntimeError('actual selected Flatpak state did not transfer')
    for excluded in (home / description['declined_home_leaf'], home / '.var/app' / description['excluded_flatpak_id']):
        if os.path.lexists(excluded):
            raise RuntimeError('actual receiving Home contains declined/unselected source state')
    for name in ('launch-result.json', 'launch-ack'):
        if os.path.lexists(data / name):
            raise RuntimeError('Flatpak result predates the actual normal launch')
    launch = _row(control, lambda row: row.get('provider') == 'applications'
                  and (row.get('resource') == app_id or row.get('id') == app_id), services, deadline)
    _invoke(control, launch, {}, services, deadline)
    while not (data / 'launch-result.json').exists():
        _pause(services, deadline)
    result = json.loads(_read_owned(data / 'launch-result.json', os.geteuid(), 1024))
    if (set(result) != {'schema', 'app_id', 'selected_state_read', 'flatpak_info_read', 'uid', 'instance_id'}
            or type(result['schema']) is not int or result['schema'] != 1 or result['app_id'] != app_id
            or result['selected_state_read'] is not True or result['flatpak_info_read'] is not True
            or type(result['uid']) is not int or result['uid'] != os.geteuid() or not isinstance(result['instance_id'], str)
            or not result['instance_id'].isascii() or not result['instance_id'].isdecimal()
            or not 1 <= len(result['instance_id']) <= 20):
        raise RuntimeError('actual launched Flatpak did not observe selected state/identity')
    runtime = Path(os.environ['XDG_RUNTIME_DIR'])
    instance = runtime / '.flatpak' / result['instance_id']
    for directory in (runtime, runtime / '.flatpak', instance):
        _directory(directory, os.geteuid())
    leader = int(_read_owned(instance / 'pid', os.geteuid(), 64).strip())
    child = json.loads(_read_owned(instance / 'bwrapinfo.json', os.geteuid()))['child-pid']
    if type(child) is not int or child <= 1 or leader <= 1:
        raise RuntimeError('actual Flatpak instance has invalid kernel tasks')
    owner = []
    complete = False
    try:
        for pid in (leader, child):
            owner.append(os.pidfd_open(pid, 0))
        original = (_process(leader), _process(child))
        if not all(_live(fd) for fd in owner):
            raise RuntimeError('actual Flatpak exited before kernel observation')
        process = Path(f'/proc/{child}')
        status = (process / 'status').read_text()
        uids = next((line.split()[1:] for line in status.splitlines() if line.startswith('Uid:')), None)
        if not uids or len(uids) != 4 or any(int(value) != os.geteuid() for value in uids):
            raise RuntimeError('actual Flatpak task belongs to another kernel account')
        arch = os.uname().machine
        if arch not in ('x86_64', 'aarch64'):
            raise RuntimeError('unsupported actual installed fixture architecture')
        _same_file(process / 'exe', home / f'.local/share/flatpak/runtime/org.tilo.FixtureRuntime/{arch}/stable/active/files/bin/bash')
        _same_file(process / 'root/app/bin/fixture', home / f'.local/share/flatpak/app/{app_id}/{arch}/stable/active/files/bin/fixture')
        if os.stat(process / 'ns/mnt').st_ino == os.stat('/proc/self/ns/mnt').st_ino:
            raise RuntimeError('actual Flatpak shares the receiving mount namespace')
        _same_file(process / 'root' / str(selected).lstrip('/'), selected)
        if not any(_inode(Path(f'/proc/{leader}/exe')) == _inode(Path(path))
                   for path in ('/usr/bin/bwrap', '/usr/libexec/flatpak-bwrap') if Path(path).exists()):
            raise RuntimeError('actual Flatpak leader is not distribution Bubblewrap')
        current = child
        for _ in range(8):
            if current == leader:
                break
            current = _process(current)[0]
        else:
            raise RuntimeError('actual Flatpak child has different launcher ancestry')
        if original != (_process(leader), _process(child)) or not all(_live(fd) for fd in owner):
            raise RuntimeError('actual Flatpak identity changed while checked')
        fd = os.open(data / 'launch-ack', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            os.write(fd, result['instance_id'].encode())
            os.fsync(fd)
        finally:
            os.close(fd)
        while any(_live(fd) for fd in owner):
            _pause(services, deadline)
        if _read_owned(selected, os.geteuid()) != expected:
            raise RuntimeError('actual Flatpak modified its selected fixture state')
        complete = True
        return {'app_id': app_id, 'distinct_mount_namespace': True, 'exact_instance_exit': True,
                'state_sha256': hashlib.sha256(expected).hexdigest()}
    finally:
        if complete:
            for fd in owner:
                os.close(fd)
        else:
            # Unknown tasks remain held until the outer owned namespace retires.
            # Never signal a scalar PID or label a timed-out app as retired.
            _UNKNOWN_FLATPAKS.append(owner)


def _inode(path):
    info = os.stat(path)
    return info.st_dev, info.st_ino


def _key(value):
    return isinstance(value, list) and len(value) == 32 and all(type(item) is int and 0 <= item <= 255 for item in value)


def _bytes(value):
    if isinstance(value, str):
        value = value.encode()
    elif isinstance(value, list) and all(type(item) is int and 0 <= item <= 255 for item in value):
        value = bytes(value)
    if not isinstance(value, bytes) or not 0 < len(value) <= 4096:
        raise RuntimeError('source descriptive fixture bytes are invalid')
    return value


def verify(control, candidate, account, source_observations, services, private_log, deadline):
    """Use real app operations; only return completion after their evidence joins."""
    if os.getuid() != account.pw_uid or os.geteuid() != account.pw_uid or account.pw_uid != 1000:
        raise RuntimeError('app verifier is not the actual firstboot account')
    services.check_live()
    description = source_observations() if callable(source_observations) else source_observations
    if not isinstance(description, dict):
        raise RuntimeError('source observations are missing')
    required = ('source_operation', 'selected_document', 'selected_document_bytes', 'person',
                'selected_flatpak_id', 'selected_flatpak_bytes', 'excluded_flatpak_id', 'declined_home_leaf')
    if any(key not in description for key in required):
        raise RuntimeError('source description lacks the actual app fixture subjects')
    _uuid(description['source_operation'])
    leaf = description['selected_document']
    if not isinstance(leaf, str) or leaf in ('', '.', '..') or '/' in leaf or '\x00' in leaf:
        raise RuntimeError('actual selected document description is not one leaf')
    for key in ('excluded_flatpak_id', 'declined_home_leaf'):
        value = description[key]
        if not isinstance(value, str) or value in ('', '.', '..') or '/' in value or '\x00' in value:
            raise RuntimeError('actual declined fixture description is invalid')
    expected = _bytes(description['selected_document_bytes']).decode()
    person = description['person']
    if isinstance(person, str) and len(person) == 64 and all(ch in '0123456789abcdef' for ch in person):
        person = list(bytes.fromhex(person))
    if not isinstance(person, list) or len(person) != 32 or any(type(item) is not int or not 0 <= item <= 255 for item in person):
        raise RuntimeError('actual attributed fixture person is missing')
    home = Path(account.pw_dir)
    _directory(home, account.pw_uid)
    if os.path.lexists(home / leaf):
        raise RuntimeError('source document was copied into receiving Home instead of retained access')
    prior_retirements = {row['id'] for row in _retirement_entries(control, services, deadline)}
    launch = _row(control, lambda row: row.get('provider') == 'applications' and row.get('id') == FILES, services, deadline)
    _invoke(control, launch, {}, services, deadline)
    go = _application_row(control, FILES, 'go-to', services, deadline)
    _invoke(control, go, {'path': SHARED_HOME}, services, deadline)
    # Actual Files folder actions are bound to its observed current directory.
    created, renamed = 'receiving-created-folder', 'receiving-renamed-folder'
    initial = _application_row(control, FILES, 'rename-here', services, deadline)
    entries = _choices(initial, 'entry')
    if leaf not in entries or created in entries or renamed in entries:
        raise RuntimeError('actual Files folder is not the fresh selected shared source')
    new = _application_row(control, FILES, 'new-folder-here', services, deadline)
    _invoke(control, new, {'name': created}, services, deadline)
    rename = _row(control, lambda row: row.get('application') == FILES
                  and row.get('operation') == 'rename-here' and created in _choices(row, 'entry'), services, deadline)
    _invoke(control, rename, {'entry': created, 'name': renamed}, services, deadline)
    _row(control, lambda row: row.get('application') == FILES and row.get('operation') == 'rename-here'
         and renamed in _choices(row, 'entry') and created not in _choices(row, 'entry'), services, deadline)
    selected = _selected_open(control, leaf, services, deadline)
    _invoke(control, selected, {'app': WRITER, 'always': False, 'access': 'edit-in-parent'}, services, deadline)
    actual = _read_text(control, services, deadline)
    if actual != ''.join(line + '\n' for line in expected.splitlines()):
        raise RuntimeError('ordinary signed Writer did not open the actual source document')
    while True:
        entries = _retirement_entries(control, services, deadline)
        owner = _unique(entries, lambda row: row['id'] not in prior_retirements
                        and row['source_operation'] == description['source_operation']
                        and row['state'] == 'running' and row['root_pid'] is not None)
        if owner is not None:
            break
        _pause(services, deadline)
    _close(control, FILES, services, deadline)
    for ordinal in range(1, 67):
        marker = f'Retained qualification revision {ordinal:02}'
        edit = _application_row(control, WRITER, 'append-paragraph', services, deadline)
        _invoke(control, edit, {'text': marker}, services, deadline)
        actual += marker + '\n'
        save = _application_row(control, WRITER, 'save', services, deadline)
        _invoke(control, save, {'confirm': True}, services, deadline)
        if _read_text(control, services, deadline) != actual:
            raise RuntimeError('actual Writer Save differs from invoked edits')
    history = _history(control, person, services, deadline)
    _close(control, WRITER, services, deadline)
    retirement = _retired(control, owner, services, deadline)
    flatpak = _flatpak(control, home, description, services, deadline)
    services.check_live()
    return {'shared_files_readwrite': True, 'writer_shared_file_edit': True, 'apps_data_and_launch': True,
            'owned_resources_retired': True, 'root_retirement': retirement,
            'created_and_renamed': renamed, 'saves_after_files_close': 66, 'history': history,
            'flatpak': flatpak, 'expected_native_bytes': len(actual.encode()),
            'expected_native_sha256': hashlib.sha256(actual.encode()).hexdigest()}
