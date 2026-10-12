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
REVOCATION_REPORT = {'custody_revoked', 'custody_backend_needed', 'local_only'}


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
    if type(report) is dict and report.get('status') == 'opening':
        require(report.get('session_id') == selected['id']
                and report.get('authenticated') is False
                and report.get('engine_shutdown_confirmed') is False,
                'pending Browser opening differs from admitted selection')
        return False
    require(type(report) is dict and REPORT <= set(report)
            and set(report) <= REPORT | REVOCATION_REPORT
            and report['session_id'] == selected['id'] and report['service'] == selected['service']
            and type(report['status']) is str
            and all(type(report[name]) is bool for name in report
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


def revoked(report, selected):
    require(type(report) is dict and report.get('session_id') == selected['id'],
            'revocation report differs from selected native session')
    for name in ('custody_revoked', 'custody_backend_needed', 'local_only',
                 'engine_shutdown_confirmed'):
        require(type(report.get(name)) is bool, 'revocation observation lacks native result')
    if report.get('status') != 'forgotten':
        require(report.get('status') in ('forgetting', 'forget-incomplete'),
                'Browser did not perform genuine selected custody revocation')
        return False
    require(report['custody_revoked'] and report['engine_shutdown_confirmed']
            and not report['custody_backend_needed'] and not report['local_only'],
            'local profile cleanup alone cannot certify custody revocation')
    return True


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
            parent = service_processes.hold_profile_parent(processes, selected)
            try:
                forget = _application_row(control, BROWSER, 'forget-service-session', services, deadline,
                                          'browser.service.' + selected['id'])
                _invoke(control, forget, {}, services, deadline)
                while True:
                    read = _application_row(control, BROWSER, 'read-service-session', services, deadline,
                                            'browser.service-status')
                    if revoked(_invoke(control, read, {}, services, deadline), selected):
                        break
                    _pause(services, deadline)
                require(not any(service_processes.live(row['pidfd']) for row in processes
                                if row['kind'] == 'cef')
                        and service_processes.profile_removed(parent, selected),
                        'selected native engine/profile retirement remains unobserved')
            finally:
                os.close(parent)
            _close(control, BROWSER, services, deadline)
            while any(service_processes.live(row['pidfd']) for row in processes):
                _pause(services, deadline)
        finally:
            service_processes.close(processes)
        # A fresh default Browser must replay private List after revocation;
        # absence in the old cached list or a Remove ACK is not enough.
        launch = _row(control, lambda row: row.get('provider') == 'applications'
                      and row.get('id') == BROWSER, services, deadline)
        _invoke(control, launch, {}, services, deadline)
        inventory = _application_row(control, BROWSER, 'list-service-sessions', services, deadline,
                                     'browser.service-sessions')
        listed = _invoke(control, inventory, {}, services, deadline)
        require(type(listed) is dict and set(listed) == {'schema', 'sessions'}
                and type(listed['schema']) is int and listed['schema'] == 1
                and type(listed['sessions']) is list
                and all(type(row) is dict and type(row.get('id')) is str
                        for row in listed['sessions'])
                and all(row['id'] != selected['id'] for row in listed['sessions']),
                'fresh authenticated Browser List still offers revoked selection')
        # Challenge Resolve through the same ordinary view action with the old
        # public anchor; a disappeared resource row alone cannot prove denial.
        opener = _application_row(control, BROWSER, 'tilo.app.open', services, deadline)
        _invoke(control, opener, {'anchor': 'service:' + selected['id']}, services, deadline)
        while True:
            read = _application_row(control, BROWSER, 'read-service-session', services, deadline,
                                    'browser.service-status')
            denied = _invoke(control, read, {}, services, deadline)
            require(type(denied) is dict and denied.get('session_id') == selected['id'],
                    'stale selected Resolve has no native observation')
            if denied.get('status') == 'needs-reconnect':
                require(denied.get('requires_reconnect') is True,
                        'stale selected Resolve did not require reconnect')
                break
            require(denied.get('status') in ('opening', 'waiting'),
                    'revoked selected Resolve was not denied')
            _pause(services, deadline)
        fresh = service_processes.observe(subjects, account, vault)
        try:
            require(any(row['kind'] == 'browser' for row in fresh),
                    'fresh signed Browser retirement subject absent')
            _close(control, BROWSER, services, deadline)
            while any(service_processes.live(row['pidfd']) for row in fresh):
                _pause(services, deadline)
        finally:
            service_processes.close(fresh)
        result.append({'id': selected['id'], 'service': selected['service'],
                       'native_profile_restart_readback': True,
                       'origin_storage_imported': observed['origin_storage_imported'],
                       'chat_navigation_observed': True, 'remote_authenticated': False})
    services.check_live()
    # The outer owner still independently proves source/target retirement.
    return {'schema': 1, 'sessions': result, 'browser_windows_closed': True,
            'remote_authenticated': False,
            'conditions': {'private_list_verified': True, 'private_resolve_verified': True,
                'isolated_profile_verified': True, 'no_vault_mount_verified': True,
                'origin_storage_persisted': True, 'chat_destination_navigation_verified': True,
                'revocation_verified': True}}
