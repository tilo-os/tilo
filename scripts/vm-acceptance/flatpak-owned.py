#!/usr/bin/env python3
"""Install and open one distinct uninstalled Flathub app, preserving all existing windows."""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import shlex
import uuid

from acceptance.action_jobs import require, begin
from acceptance.vm import first_json, wait_until

_spec = importlib.util.spec_from_file_location('store_chooser', Path(__file__).with_name('store-chooser.py'))
chooser = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(chooser)
app = chooser.app


def host_evidence(state):
    require(state.get('chrome', {}).get('session_locked') is False, 'owner session locked')
    evidence = state['chrome'].get('host_window_provenance')
    require(isinstance(evidence, dict) and evidence.get('method') == 'observed-host-launch-wayland-peer'
            and evidence.get('omitted') == 0 and isinstance(evidence.get('windows'), list),
            'host window custody unavailable or truncated')
    return evidence


def subject(state, room, desktop_id, previous, invocation):
    evidence = host_evidence(state)
    rows = [row for row in evidence['windows'] if row.get('room') == room
            and row.get('desktop_id') == desktop_id and row.get('attempt_id') not in previous
            and row.get('invocation') == invocation]
    if not rows:
        return None
    require(len(rows) == 1, 'competing observed host launches')
    row = rows[0]
    for key in ('attempt_id', 'adoption_id'):
        require(str(uuid.UUID(row[key])) == row[key], 'noncanonical host lifetime identity')
    require(all(type(row.get(key)) is int and row[key] > 0 for key in
                ('tab_id', 'surface_id', 'peer_pid', 'launcher_pid')), 'unobservable host peer')
    require(all(isinstance(row.get(key), str) and row[key].isdecimal() for key in
                ('peer_start_time_ticks', 'launcher_start_time_ticks')), 'missing kernel lifetime')
    tabs = [tab for workspace in state['workspace_tree']['workspaces'] if workspace['room'] == room
            for strip in workspace['strips'] for group in strip['window_groups'] for tab in group['tabs']
            if tab['id'] == row['tab_id'] and (tab.get('tile') or {}).get('Foreign', {}).get('surface_id') == row['surface_id']]
    require(len(tabs) == 1, 'observed host surface replaced or closed')
    return row


def run(args):
    require(re.fullmatch(r'[A-Za-z0-9]+(?:\.[A-Za-z0-9_-]+){2,}', args.app), 'invalid explicit Flatpak app')
    require(re.fullmatch(r'[A-Za-z0-9_-]+', args.executable), 'invalid deployment executable component')
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    vm = chooser.OwnerVm(str(args.binary.resolve()), args.vm, args.slot, out / 'commands.log')
    receipt, shown, installed = app.cohort(vm, args, out)
    baseline = first_json(vm.run('slot', 'exec', '--', 'python3', '-c', app.BASELINE))
    require(baseline['uid'] == args.uid and args.app not in baseline['flatpak_apps'],
            'wrong account or selected app already installed; never uninstall user apps')
    require(baseline['flathub'] and all(row['official'] and not row['disabled'] for row in baseline['flathub']),
            'enabled official Flathub not observed')
    door = app.store(vm, out)
    chooser.workspace(vm, door.bound[1])
    current = vm.state()
    host_evidence(current)  # Refuse a missing Shell capability before any installation.
    native = current['chrome'].get('native_window_provenance')
    require(isinstance(native, dict) and native.get('omitted') == 0, 'Store native custody unavailable')
    store_workers = [row for row in native['windows'] if row.get('package_id') == app.STORE
                     and row.get('room') == door.bound[1]]
    require(len(store_workers) == 1, 'Store worker association missing or ambiguous')
    selected_store = store_workers[0]
    app.mapped(vm, 'store', installed, args.uid, out, selected=selected_store)
    require(selected_store in vm.state()['chrome']['native_window_provenance']['windows'],
            'Store surface changed during mapped SDK proof')
    initial = door.settled()
    require(initial['flatpak_available'] is True and initial['remotes_truncated'] is False,
            'default Flatpak sources unavailable')
    remotes = [row for row in initial['remotes'] if row['name'] == 'flathub' and row['disabled'] is False]
    require(remotes, 'Store does not expose default Flathub')
    remote = next((row for row in remotes if row['scope'] == 'account'), remotes[0])
    door.call('browse-remote', {'remote': remote['resource']}, timeout=300)
    require(door.settled()['matching'] > 0, 'Flathub browse empty')
    door.call('set-search', {'query': args.query})
    search = door.settled()
    require(search['query'] == args.query and any(row['id'] == args.app for row in search['items']),
            'exact selected app absent from real search')
    door.call('show-details', {'app': args.app})
    detail = door.settled(args.app)['detail']
    require(detail['source'] == 'Flatpak' and detail['installed'] is False
            and all(detail.get(key) for key in ('name', 'description', 'latest_version', 'repository', 'trust')),
            'readable first-install details missing')
    before = vm.state()
    require(not app.windows(before, args.app), 'selected app already has a window')
    previous = {row['attempt_id'] for row in host_evidence(before)['windows']}
    door.call('flatpak-install', {'app': args.app}, timeout=600)
    require(door.settled(args.app)['detail']['installed'] is True, 'Store installation unobserved')
    location = vm.run('slot', 'exec', '--', 'flatpak', '--user', 'info', '--show-location', args.app).strip()
    require(location.startswith('/') and '\n' not in location, 'installed deployment location unavailable')
    args_open = {'app': args.app}
    row = app.choose(app.catalog(vm, app.STORE), app.STORE, 'store.catalog', 'flatpak-open', args_open, door.bound)
    opening = begin(vm, row, args_open, out, 'flatpak-open')
    opening.finish(timeout=120)
    selected = wait_until(lambda: subject(vm.state(), door.bound[1], args.app + '.desktop', previous, opening.invocation), 60)
    require(selected, 'opened GTK surface custody never appeared')
    chooser.workspace(vm, door.bound[1])
    require(subject(vm.state(), door.bound[1], args.app + '.desktop', previous, opening.invocation) == selected,
            'host surface changed before executable proof')
    shared = Path(__file__).parent / 'acceptance'
    source = (shared / 'runtime_backing.py').read_text().split('if __name__ ==')[0]
    source += '\n' + (shared / 'host_backing.py').read_text().replace('from .runtime_backing import hash_backing, start_time', '')
    source += '\nprint(json.dumps(probe(' + repr(args.uid) + ',' + repr(selected['peer_pid']) + ',' + repr(selected['peer_start_time_ticks']) + ',' + repr(location + '/files/bin/' + args.executable) + ')))'
    backing = first_json(vm.root_exec('python3 -c ' + shlex.quote(source)))
    require(backing['reader_uid'] == 0, 'kernel backing reader unavailable')
    require(subject(vm.state(), door.bound[1], args.app + '.desktop', previous, opening.invocation) == selected,
            'host surface changed during executable proof')
    result = {'source': receipt['cohort_source'], 'app': args.app, 'installed_before': False,
              'default_flathub': True, 'selected_window': selected, 'kernel_backing': backing,
              'unrelated_windows_closed': False, 'native_sdk_claim': False}
    require(first_json(vm.run('--json', 'slot', 'show'))['slot'] == shown['slot'], 'cohort changed')
    (out / 'proof.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('binary', 'cohort-receipt', 'out'):
        parser.add_argument('--' + key, required=True, type=Path)
    for key in ('vm', 'slot', 'app', 'query', 'executable'):
        parser.add_argument('--' + key, required=True)
    parser.add_argument('--uid', type=int, required=True)
    run(parser.parse_args())
