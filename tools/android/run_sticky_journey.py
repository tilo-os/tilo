#!/usr/bin/env python3
"""Observe ART process loss in the owned foreground fixture; private evidence only."""
import os
from pathlib import Path
import signal
import time

import run_foreground_journey as foreground
from run_arity_journey import wait_for, REPO
from qualify_apk import digest


def art_process(journey):
    matches = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            command = (entry / 'cmdline').read_bytes().split(b'\0')
            if os.fsencode(journey.package) not in command or b'--gapplication-app-id' not in command:
                continue
            if entry.stat().st_uid != os.geteuid():
                continue
            descriptor = os.pidfd_open(int(entry.name))
            if (entry / 'cmdline').read_bytes().split(b'\0') != command:
                os.close(descriptor)
                raise RuntimeError('ART process identity changed')
            matches.append((int(entry.name), descriptor))
        except (FileNotFoundError, ProcessLookupError):
            continue
    if len(matches) != 1:
        for _, descriptor in matches:
            os.close(descriptor)
        raise RuntimeError('expected exactly one fixture ART process')
    return matches[0]


def exercise_work(journey, output, profile, package, args, documents, grants):
    source, destination = documents
    frame = journey.launch()
    private = profile / 'data/tilo-rooms' / journey.workspace()['room'] / package
    def record():
        paths = list(private.rglob('foreground.txt'))
        return paths[0] if len(paths) == 1 and 'return=START_STICKY' in paths[0].read_text() else None
    recorded = wait_for(record, 'actual sticky service started')
    (recorded.parent / 'scope-inputs').write_text(str(source) + '\n' + str(destination) + '\n')
    wait_for(lambda: 'scoped-descriptors=open' in recorded.read_text(), 'scoped worker descriptors')
    wait_for(lambda: len(destination.read_text().splitlines()) >= 3, 'actual scoped writes')
    journey.request('snapshot', path=str(output / 'before-process-loss.png'))
    pid, descriptor = art_process(journey)
    try:
        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
    finally:
        os.close(descriptor)
    wait_for(lambda: not journey.tabs(), 'lost ART surface removed')
    if args.expect_sticky_incompatibility:
        time.sleep(2)
        if recorded.read_text().count('service-create;main=true') != 1 or journey.tabs():
            raise RuntimeError('baseline sticky process-loss gap not reproduced')
        stopped = len(destination.read_text().splitlines())
        time.sleep(1)
        if len(destination.read_text().splitlines()) != stopped:
            raise RuntimeError('lost ART worker still writing')
        for identifier in list(grants):
            journey.request('revoke', id=identifier)
            grants.remove(identifier)
        (recorded.parent / 'revoke-fixture').write_text('Disposable fixture only\n')
        journey.launch()
        wait_for(lambda: 'revoked-relaunch=complete;service-not-started' in recorded.read_text(), 'denied relaunch')
        journey.close()
        if (recorded.parent / 'visits.txt').read_text().splitlines() != ['visit=1', 'visit=2']:
            raise RuntimeError('private visits lost')
        (output / 'foreground.txt').write_text(recorded.read_text())
        return {'actual_ART': True, 'runtime_accepted': False, 'architecture': args.architecture, 'killed_pid': pid, 'sticky_recovery': False,
                'baseline_gap_reproduced': True, 'revoked_relaunch_denied': True,
                'private_visits_preserved': True, 'presented_frame': frame, 'real_device': False}
    wait_for(lambda: 'service-start;intent=null;id=1' in recorded.read_text(), 'actual null-Intent sticky recovery')
    wait_for(journey.frame, 'recovered runtime surface presented')
    if recorded.read_text().count('service-create;main=true') != 2:
        raise RuntimeError('sticky service recovery count differs')
    if (recorded.parent / 'visits.txt').read_text().splitlines() != ['visit=1']:
        raise RuntimeError('recovery incorrectly restarted application Activity')
    def notification():
        return next((n for n in journey.state()['chrome']['notifications']
                     if n['title'] == 'Android foreground fixture'), None)
    notice = wait_for(notification, 'recovered foreground notification')
    if not notice['persistent']:
        raise RuntimeError('recovered worker lost foreground visibility')
    wait_for(lambda: recorded.read_text().count('scoped-descriptors=open') == 2, 'recovered scoped descriptors')
    journey.request('snapshot', path=str(output / 'recovered-service.png'))
    second_pid, descriptor = art_process(journey)
    if second_pid == pid:
        os.close(descriptor)
        raise RuntimeError('ART process was not replaced')
    try:
        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
    finally:
        os.close(descriptor)
    log = output / 'compositor.log'
    wait_for(lambda: log.read_text().count('foreground sticky recovery pending') == 2,
             'second recovery backoff before revocation')
    before = len(destination.read_text().splitlines())
    for identifier in list(grants):
        journey.request('revoke', id=identifier)
        grants.remove(identifier)
    wait_for(lambda: 'application interrupted after accepted filesystem authority withdrawal' in log.read_text(),
             'accepted authority interrupts original recovery scope')
    time.sleep(3)
    if journey.tabs() or notification() or recorded.read_text().count('service-create;main=true') != 2:
        raise RuntimeError('revoked recovery recreated worker or visibility')
    if len(destination.read_text().splitlines()) != before:
        raise RuntimeError('revoked recovery wrote through stale authority')
    (recorded.parent / 'revoke-fixture').write_text('Disposable fixture only\n')
    journey.launch()
    wait_for(lambda: 'revoked-relaunch=complete;service-not-started' in recorded.read_text(), 'denied revoked relaunch')
    journey.request('snapshot', path=str(output / 'denied-relaunch.png'))
    journey.close()
    if (recorded.parent / 'visits.txt').read_text().splitlines() != ['visit=1', 'visit=2']:
        raise RuntimeError('private visits were not preserved')
    if source.read_text() != 'Tilo disposable scoped source\n':
        raise RuntimeError('scoped source modified')
    (output / 'foreground.txt').write_text(recorded.read_text())
    return {'actual_ART': True, 'runtime_accepted': True, 'architecture': args.architecture, 'real_device': False, 'sticky_recovery': True,
            'initial_pid': pid, 'recovered_pid': second_pid, 'original_activity_restarted': False,
            'recovery_intent': 'null', 'foreground_notification_persistent': True,
            'revocation_during_recovery': True, 'revoked_relaunch_denied': True,
            'private_visits_preserved': True, 'apk_sha256': args.apk_sha256,
            'runtime_receipt_sha256': args.runtime_receipt_sha256,
            'production_binaries': {name: digest(REPO / 'target/debug' / name)
                                    for name in ('shell', 'tilo-launcher')}}


if __name__ == '__main__':
    foreground.main(exercise_work)
