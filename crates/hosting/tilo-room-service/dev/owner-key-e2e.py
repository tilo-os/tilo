#!/usr/bin/env python3
"""A throwaway room's owner routes, against the real binaries.

Starts tilo-room-service in throwaway-room mode (local review, so no
certificate or host mapping is needed), creates a room as a site's page does
and checks over plain HTTP that only the page holding the `POST /rooms`
answer can mint invitations and watch members:

  - everything a guest link shows (the room's host label, its uuid, the link
    without its ticket) mints no invitation and opens no member stream, nor
    does a wrong key or the uuid with the right key; each refusal reads
    exactly as a room that does not exist;
  - those refused attempts spend none of the room's guest slots: the page
    still mints every one it was given, then hears 429;
  - the `invitations` and `events` URLs the answer holds work as given (what
    web/rooms.mjs does), and no guest link carries the owner key;
  - an invitee still redeems their ticket at `<tickets><room uuid>/admit`:
    the room takes it for admission (a second admission of the same ticket
    is told it is busy), while a made-up ticket or room is refused.

Writes summary.json, summary.md and the service's log to --out. Exits
non-zero when any check fails; a check that cannot observe its subject fails.
"""

import argparse
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

# The room service's local relay listens this far above its port
# (LOCAL_PLAIN_OFFSET in src/config.rs).
LOCAL_PLAIN_OFFSET = 64
MAX_GUESTS = 3
NO_SUCH_ROOM = {'error': 'no such room'}


def log(message):
    print(f'owner-key-e2e: {message}', file=sys.stderr, flush=True)


class Checks:
    def __init__(self):
        self.results = []

    def check(self, name, passed, observed):
        self.results.append({'name': name, 'passed': bool(passed), 'observed': observed})
        log(f'{"ok  " if passed else "FAIL"} {name}: {observed}')
        return bool(passed)

    @property
    def failed(self):
        return [result for result in self.results if not result['passed']]


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def port_free(port):
    for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
        with socket.socket(socket.AF_INET, kind) as probe:
            try:
                probe.bind(('127.0.0.1', port))
            except OSError:
                return False
    return True


def free_base():
    """A port base whose room port and its plain port (+64) are free."""
    for base in range(30700, 39000, 131):
        if port_free(base) and port_free(base + LOCAL_PLAIN_OFFSET):
            return base
    sys.exit('owner-key-e2e: no free port base')


def call(method, url, body=None, timeout=30):
    """(status, decoded body, content type) of one request; status None
    when nothing answered."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        request.add_header('Content-Type', 'application/json')
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            status, kind = response.status, response.headers.get('Content-Type', '')
            # An event stream never ends: its first event is enough.
            raw = response.readline() if kind.startswith('text/event-stream') else response.read()
    except urllib.error.HTTPError as error:
        status, kind, raw = error.code, error.headers.get('Content-Type', ''), error.read()
    except (urllib.error.URLError, OSError) as error:
        return None, str(error), ''
    try:
        decoded = json.loads(raw) if raw and kind.startswith('application/json') else raw.decode(errors='replace')
    except ValueError:
        decoded = raw.decode(errors='replace')
    return status, decoded, kind


class Service:
    def __init__(self, args, scratch):
        self.args, self.scratch = args, scratch
        self.api_port, self.tls_port, self.port_base = free_port(), free_port(), free_base()
        self.api = f'http://localhost:{self.api_port}'
        self.process = None
        self.log = open(args.out / 'service.log', 'ab')  # noqa: SIM115 - lives as long as the service

    def start(self):
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('TILO_ROOMS_', 'TILO_RELAY_'))}
        env.update({
            'TILO_ROOMS_LOCAL': '1',
            'TILO_ROOMS_API': f'127.0.0.1:{self.api_port}',
            'TILO_ROOMS_PUBLIC_API': self.api,
            'TILO_ROOMS_TLS': f'127.0.0.1:{self.tls_port}',
            'TILO_ROOMS_PORT_BASE': str(self.port_base),
            'TILO_ROOMS_STATE': str(self.scratch / 'rooms'),
            'TILO_ROOMS_ASSETS': str(self.scratch / 'assets'),
            'TILO_ROOMS_RELAY_BIN': str(self.args.relay_bin),
            'TILO_ROOMS_MAX_GUESTS': str(MAX_GUESTS),
        })
        self.process = subprocess.Popen([self.args.room_service_bin], env=env, stdout=self.log,
                                        stderr=subprocess.STDOUT, start_new_session=True)

    def ready(self):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                log(f'tilo-room-service exited ({self.process.returncode})')
                return False
            if call('GET', f'{self.api}/healthz', timeout=2)[0] == 200:
                return True
            time.sleep(0.2)
        return False

    def stop(self):
        if self.process is None:
            return
        try:
            os.killpg(self.process.pid, signal.SIGTERM)
            self.process.wait(timeout=20)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            pass
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.process.wait()
        self.process = None

    def log_tail(self, lines=40):
        self.log.flush()
        text = (self.args.out / 'service.log').read_text(errors='replace')
        return '\n'.join(text.splitlines()[-lines:])


def link_parts(join_url):
    """(relay host label, room uuid, ticket) of a guest link:
    `<api>/open/app/#/@<host>/w/<room>/<app>[/<record>]?ticket=…`."""
    fragment = urllib.parse.urlsplit(join_url).fragment
    route, _, query = fragment.partition('?')
    segments = route.strip('/').split('/')
    host = segments[0].removeprefix('@')
    room = segments[segments.index('w') + 1]
    ticket = urllib.parse.parse_qs(query).get('ticket', [None])[0]
    return host.split('.')[0].split(':')[0], room, ticket


def run_checks(service, checks):
    service.start()
    if not checks.check('the service answers', service.ready(), service.api):
        return
    status, room, _ = call('POST', f'{service.api}/rooms', timeout=120)
    if not checks.check('a page creates a room', status == 201 and isinstance(room, dict),
                        {'status': status, 'body': room if status != 201 else sorted(room)}):
        return
    api, room_id = service.api, room['roomId']
    key = urllib.parse.parse_qs(urllib.parse.urlsplit(room['invitations']).query).get('key', [''])[0]
    checks.check('the answer hands its page an owner key of 32 random bytes in hex',
                 len(key) == 64 and all(c in '0123456789abcdef' for c in key)
                 and urllib.parse.parse_qs(urllib.parse.urlsplit(room['events']).query).get('key') == [key],
                 {'invitations': room['invitations'].replace(key, '<key>'),
                  'events': room['events'].replace(key, '<key>')})
    host_label, uuid, ticket = link_parts(room['joinUrl'])
    checks.check('the guest link names the room by its uuid and carries no owner key',
                 uuid == room['room'] and key not in room['joinUrl'] and key not in room['admit'],
                 {'linkRoom': uuid, 'room': room['room']})

    # What anyone who saw the link (with or without its ticket) may try.
    names = {room_id, uuid, host_label}
    attempts = []
    for route in ('invitations', 'events'):
        for name in sorted(names):
            attempts += [f'{api}/rooms/{name}/{route}', f'{api}/rooms/{name}/{route}?key={"0" * 64}',
                         f'{api}/rooms/{name}/{route}?key={uuid}', f'{api}/rooms/{name}/{route}?ticket={ticket}']
        attempts.append(f'{api}/rooms/{uuid}/{route}?key={key}')
    # More rounds than the room has guest slots, so a leak would show as
    # spent slots below even if a refusal looked right.
    answered, leaks = 0, {}
    for _ in range(MAX_GUESTS + 2):
        for url in attempts:
            method = 'POST' if '/invitations' in url else 'GET'
            status, body, _ = call(method, url, {} if method == 'POST' else None)
            answered += 1
            if (status, body) != (404, NO_SUCH_ROOM):
                leaks[url.replace(key, '<key>')] = {'status': status, 'body': body}
    checks.check('nothing a guest link shows mints an invitation or opens the member stream, '
                 'and each refusal reads as no such room',
                 answered > 0 and not leaks,
                 {'attempts': answered, 'names': sorted(names), 'notRefused': leaks})
    made_up = call('POST', f'{api}/rooms/ffffffffffff/invitations', {})
    checks.check('a room that does not exist answers the same', made_up[:2] == (404, NO_SUCH_ROOM),
                 made_up[:2])

    status, body, kind = call('GET', room['events'])
    checks.check("the answer's events URL streams members as given",
                 status == 200 and kind.startswith('text/event-stream'), {'status': status, 'first': body})
    # The page and the first guest hold two of the room's MAX_GUESTS + 1
    # tickets; every other one is still the page's to mint.
    minted = [call('POST', room['invitations'], {}) for _ in range(MAX_GUESTS - 1)]
    full = call('POST', room['invitations'], {})
    checks.check(f'the refused attempts spent no guest slot: the page mints its other {MAX_GUESTS - 1}, then 429',
                 all(answer[0] == 201 and isinstance(answer[1], dict) and key not in answer[1].get('joinUrl', key)
                     for answer in minted)
                 and full[0] == 429,
                 {'minted': [answer[0] for answer in minted], 'next': full[:2]})

    # The invitee's entry page redeems at `<tickets><room uuid>/admit`. No
    # browser shows the code here, so the relay would answer only after a
    # minute; that the ticket is busy admitting is the proof it was taken.
    admit = f'{api}/rooms/{uuid}/admit'
    first = threading.Thread(target=call, args=('POST', admit, {'ticket': ticket, 'pairingCode': '111111'}),
                             kwargs={'timeout': 90}, daemon=True)
    first.start()
    # Asked before the first reaches the service, the probe would take the
    # ticket itself and wait out the minute instead: let the first go ahead.
    time.sleep(2)
    busy, deadline = None, time.monotonic() + 10
    while time.monotonic() < deadline:
        busy = call('POST', admit, {'ticket': ticket, 'pairingCode': '222222'})
        if busy[0] == 403 and 'already admitting' in str(busy[1]):
            break
        time.sleep(0.2)
    checks.check("the invitee's ticket is taken for admission at the room's uuid",
                 busy is not None and busy[0] == 403 and 'already admitting' in str(busy[1]),
                 busy and busy[:2])
    forged = call('POST', admit, {'ticket': '0' * 40, 'pairingCode': '333333'})
    checks.check('a made-up ticket is refused', forged[0] == 403 and 'does not exist' in str(forged[1]),
                 forged[:2])
    nowhere = call('POST', f'{api}/rooms/ffffffffffff/admit', {'ticket': ticket, 'pairingCode': '444444'})
    checks.check('a made-up room is refused', nowhere[:2] == (404, NO_SUCH_ROOM), nowhere[:2])


def write_summary(out, checks, started):
    summary = {'passed': not checks.failed, 'seconds': round(time.monotonic() - started, 1),
               'configured': {'maxGuests': MAX_GUESTS}, 'checks': checks.results}
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    lines = ['# Throwaway room: owner routes', '',
             f'{"PASS" if summary["passed"] else "FAIL"}: {len(checks.results) - len(checks.failed)} of '
             f'{len(checks.results)} checks in {summary["seconds"]} s', '', '| check | result |', '|---|---|']
    lines += [f'| {result["name"]} | {"ok" if result["passed"] else "FAIL"} |' for result in checks.results]
    (out / 'summary.md').write_text('\n'.join(lines) + '\n')


def main():
    parser = argparse.ArgumentParser(prog='owner-key-e2e.py', description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--relay-bin', type=pathlib.Path, required=True, help='tilo-relay built with --features web')
    parser.add_argument('--room-service-bin', type=pathlib.Path, required=True)
    parser.add_argument('--out', type=pathlib.Path, required=True, help='summary.json, summary.md, service.log')
    args = parser.parse_args()
    for name in ('relay_bin', 'room_service_bin'):
        path = getattr(args, name).resolve()
        if not os.access(path, os.X_OK):
            parser.error(f'--{name.replace("_", "-")}: {path} is not an executable')
        setattr(args, name, path)
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    scratch = args.out / 'scratch'
    shutil.rmtree(scratch, ignore_errors=True)
    (scratch / 'assets').mkdir(parents=True)
    (scratch / 'rooms').mkdir(parents=True)
    started = time.monotonic()
    checks = Checks()
    service = Service(args, scratch)
    try:
        run_checks(service, checks)
    finally:
        service.stop()
        write_summary(args.out, checks, started)
        if checks.failed:
            log(f'the service log ends:\n{service.log_tail()}')
    if not checks.results or checks.failed:
        log(f'{len(checks.failed)} of {len(checks.results)} checks failed; see {args.out / "summary.md"}')
        sys.exit(1)
    log(f'all {len(checks.results)} checks passed')


if __name__ == '__main__':
    main()
