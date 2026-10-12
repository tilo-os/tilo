"""Observe actual selected custody -> default Browser -> private CEF restart.

Expected selections come from the independently authenticated prepared Manifest.
They describe owned source material and grant no authority. This observer does
not connect a service, read the vault, replace the broker, or certify remote
login. Its result is incomplete until the existing outer namespace owner has
observed every consumer/source/service retirement.
"""
import os
import re
import time

from apps import _row, _invoke, _pause, _application_row, _close
from cloud import user_environment
import service_processes

BROWSER = 'so.tilo.browser'
CHAT = {'slack': 'https://slack.com/app_redirect?team=T123&channel=C456',
        'discord': 'https://discord.com/channels/123/456'}
REPORT = {'service', 'session_id', 'status', 'authenticated', 'origin_storage_imported',
          'requires_reconnect', 'engine_shutdown_confirmed', 'destination_navigation_observed',
          'native_restart_readback_verified'}


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def selections(value):
    require(type(value) is dict and set(value) == {'schema', 'sessions'}
            and type(value['schema']) is int and value['schema'] == 1
            and type(value['sessions']) is list and len(value['sessions']) == 2,
            'actual admitted Slack and Discord selections required')
    ids, kinds, rows = set(), set(), []
    for row in value['sessions']:
        require(type(row) is dict and set(row) == {'id', 'service', 'setup_binding'},
                'actual service selection shape differs')
        identity, kind, binding = row['id'], row['service'], row['setup_binding']
        require(type(identity) is str and re.fullmatch('[A-Za-z0-9_-]{1,96}', identity)
                and identity not in ids and type(kind) is str and kind in CHAT and kind not in kinds
                and type(binding) is str and 0 < len(binding.encode()) <= 256
                and not any(ord(char) < 32 or ord(char) == 127 for char in binding),
                'actual service selection identity invalid')
        ids.add(identity); kinds.add(kind); rows.append(dict(row))
    return sorted(rows, key=lambda row: row['service'])


def imported(report, selected):
    require(type(report) is dict and set(report) == REPORT
            and report['session_id'] == selected['id'] and report['service'] == selected['service']
            and type(report['status']) is str
            and all(type(report[name]) is bool for name in REPORT
                    if name not in ('service', 'session_id', 'status')),
            'actual Browser service report differs from admitted selection')
    require(report['authenticated'] is False, 'offline qualification cannot claim remote login')
    require(report['status'] in ('waiting', 'importing', 'restarting', 'verifying', 'imported-unverified'),
            'actual default Browser could not import selected session')
    if report['status'] != 'imported-unverified':
        return False
    require(report['native_restart_readback_verified'],
            'cached profile navigation cannot certify fresh native restart/readback')
    require(not report['requires_reconnect']
            and (selected['service'] != 'discord' or report['origin_storage_imported']),
            'actual selected origin state requires reconnect')
    return report['destination_navigation_observed']


def verify(control, candidate, account, services, log, deadline, selected, subjects):
    # Same actual NSS user/paths/session SecretService as the default cloud lane;
    # no developer credentials or isolated alternative Browser session.
    user_environment(account)
    require(os.getuid() == account.pw_uid == 1000, 'actual receiving account required')
    rows = selections(selected)
    vault = service_processes.coordinate(account)
    services.check_live()
    require(not any(row.get('application') == BROWSER for row in control.rows()),
            'Browser already active before owned imported account qualification')
    baseline = service_processes.observe(subjects, account, vault)
    try:
        require(not baseline, 'signed Browser/CEF already active before qualification')
    finally:
        service_processes.close(baseline)
    result = []
    for selected in rows:
        launch = _row(control, lambda row: row.get('provider') == 'applications'
                      and row.get('id') == BROWSER, services, deadline)
        _invoke(control, launch, {}, services, deadline)
        # Wait for the normal certified Browser's private broker List to offer
        # this precise independently admitted selection, not a seeded UI row.
        _application_row(control, BROWSER, 'open-service-session', services, deadline,
                         'browser.service.' + selected['id'])
        # The same framework view action used by ordinary account navigation
        # carries the public chat anchor. Account cookies never enter arguments.
        opener = _application_row(control, BROWSER, 'tilo.app.open', services, deadline)
        _invoke(control, opener, {'anchor': 'service:' + selected['id'] + '|'
                                 + CHAT[selected['service']]}, services, deadline)
        observed = None
        while time.monotonic() < deadline:
            read = _application_row(control, BROWSER, 'read-service-session', services, deadline,
                                    'browser.service-status')
            report = _invoke(control, read, {}, services, deadline)
            if imported(report, selected):
                observed = report
                break
            _pause(services, deadline)
        require(observed is not None, 'actual CEF restart/readback/navigation did not complete')
        # The explicit readonly witness is set only after NativeRestart reaps
        # the old CEF process and the replacement verify_only reply matches
        # the actual selected cookie/origin counts. Cached reuse cannot pass.
        processes = service_processes.observe(subjects, account, vault)
        try:
            require(sum(row['kind'] == 'browser' for row in processes) == 1
                    and any(row['kind'] == 'cef' for row in processes),
                    'actual signed Browser/CEF process subjects were not observed')
            service_processes.private_profile(processes, selected, account)
            _close(control, BROWSER, services, deadline)
            while any(service_processes.live(row['pidfd']) for row in processes):
                _pause(services, deadline)
        finally:
            service_processes.close(processes)
        result.append({'id': selected['id'], 'service': selected['service'],
                       'native_profile_restart_readback': True,
                       'origin_storage_imported': observed['origin_storage_imported'],
                       'chat_navigation_observed': True, 'remote_authenticated': False})
    services.check_live()
    # Deliberately no revocation/no-vault/owned-retirement flags: those require
    # additional actual observations by the canonical owner, not queue ACKs.
    return {'schema': 1, 'sessions': result, 'browser_windows_closed': True,
            'remote_authenticated': False,
            'conditions': {'private_list_verified': True, 'private_resolve_verified': True,
                'isolated_profile_verified': True, 'no_vault_mount_verified': True,
                'origin_storage_persisted': True, 'chat_destination_navigation_verified': True,
                'revocation_verified': False}}
