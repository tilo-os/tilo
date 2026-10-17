#!/usr/bin/env python3
"""The persistent room's enabled limits and kept state, against the real
binaries.

Starts tilo-room-service in persistent mode (which spawns its child
tilo-relay) on scratch state with small, known limits, and checks over plain
HTTP, as a visitor's page and the operator's Prometheus see it:

  - the guest cap, the spectator cap and the per-address join windows each
    admit exactly their configured number and refuse the next with its
    status, wording and Retry-After; a refused join holds no seat; a held
    seat frees after its hold, so a visitor told "full" gets in later;
  - /metrics exports the caps the service runs with and counts each refusal
    under its reason;
  - a restart keeps the room, its spectator person, the guest ledger (hashes
    only, never an address) and every address's guest join window;
  - the seeded apps' reset snapshots stay bounded at the relay's ten per app
    across restarts, and chat is never snapshotted or reset;
  - the service stops with its relay within eu1's termination grace period.

Writes summary.json, summary.md and the service's log to --out. Exits
non-zero when any check fails; a check that cannot observe its subject fails.
"""

import argparse
import json
import os
import pathlib
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

# The room service's local relay listens this far above the port base
# (LOCAL_PLAIN_OFFSET in src/config.rs).
LOCAL_PLAIN_OFFSET = 64
# How long an admission holds its seat before the relay's count must show it
# (SEAT_HOLD_MS in src/limits.rs).
SEAT_HOLD_SECS = 30
# What /join and /spectate tell a visitor to wait when the room is full
# (FULL_RETRY_SECS in src/persistent.rs).
FULL_RETRY_SECS = 60
# How many reset snapshots of each seeded app the relay keeps
# (SNAPSHOTS_KEPT in tilo-relay's room_seed.rs).
SNAPSHOTS_KEPT = 10
# eu1's terminationGracePeriodSeconds: the service must persist the room and
# stop its relay within it.
GRACE_SECS = 20

NAME = 'limits'
MAX_GUESTS = 3
MAX_SPECTATORS = 4
GUEST_JOINS_PER_10_MIN = 2
SPECTATES_PER_10_MIN = 2

FULL = 'the room is full'
TOO_MANY = 'too many joins from your address; try again later'
BAD_KEY = 'userPublicKey must be 64 lowercase hex characters'


def log(message):
    print(f'limits-e2e: {message}', file=sys.stderr, flush=True)


class Checks:
    def __init__(self):
        self.results = []

    def check(self, name, passed, observed, expected=None):
        self.results.append({'name': name, 'passed': bool(passed), 'observed': observed, 'expected': expected})
        log(f'{"ok  " if passed else "FAIL"} {name}: {observed}')
        return bool(passed)

    @property
    def failed(self):
        return [result for result in self.results if not result['passed']]


def hex32():
    return secrets.token_hex(32)


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def free_base():
    """A port base whose TLS port and plain port (+64) are both free."""
    for base in range(20700, 29000, 131):
        ports = (base, base + LOCAL_PLAIN_OFFSET)
        if all(port_free(port) for port in ports):
            return base
    sys.exit('limits-e2e: no free port base')


def port_free(port):
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            probe.bind(('127.0.0.1', port))
        except OSError:
            return False
    return True


class Api:
    def __init__(self, origin):
        self.origin = origin

    def call(self, method, path, body=None, address=None):
        """(status, headers, decoded body) of one request; `address` is the
        visitor the trusted local proxy names in X-Forwarded-For."""
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.origin + path, data=data, method=method)
        if data is not None:
            request.add_header('Content-Type', 'application/json')
        if address is not None:
            request.add_header('X-Forwarded-For', address)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, headers, raw = response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            status, headers, raw = error.code, error.headers, error.read()
        except (urllib.error.URLError, OSError) as error:
            # Not listening yet, or gone: no answer, which no check accepts.
            return None, None, str(error)
        try:
            decoded = json.loads(raw) if raw else None
        except ValueError:
            decoded = raw.decode(errors='replace')
        return status, headers, decoded

    def join(self, address, key=None):
        return self.call('POST', '/join', {'userPublicKey': key or hex32(), 'displayName': 'Limit Check'}, address)

    def spectate(self, address):
        return self.call('POST', '/spectate', {'devicePublicKey': hex32()}, address)

    def metrics(self):
        status, _, page = self.call('GET', '/metrics')
        if status != 200 or not isinstance(page, str):
            return {}
        samples = {}
        for line in page.splitlines():
            if line and not line.startswith('#'):
                name, _, value = line.rpartition(' ')
                samples[name] = value
        return samples


def describe(answer):
    status, headers, body = answer
    retry = headers.get('Retry-After') if headers else None
    shown = body if not isinstance(body, dict) else {k: body[k] for k in body if k in ('error', 'room')}
    return {'status': status, 'retryAfter': retry, 'body': shown}


def admitted(answer, field):
    status, _, body = answer
    return status == 200 and isinstance(body, dict) and bool(body.get(field))


def refused(answer, status, message, retry=None):
    """The refusal a visitor's page shows: status, wording and, where the
    service names one, a Retry-After in `retry` (an exact value, or a
    (low, high) range)."""
    got_status, headers, body = answer
    if got_status != status or not isinstance(body, dict) or body.get('error') != message or headers is None:
        return False
    value = headers.get('Retry-After')
    if retry is None:
        return value is None
    if value is None or not value.isdigit():
        return False
    low, high = retry if isinstance(retry, tuple) else (retry, retry)
    return low <= int(value) <= high


class Service:
    def __init__(self, args, scratch):
        self.args, self.scratch = args, scratch
        self.api_port, self.port_base = free_port(), free_base()
        self.process = None
        self.log = open(args.out / 'service.log', 'ab')  # noqa: SIM115 - lives as long as the service

    @property
    def room_dir(self):
        return self.scratch / 'rooms' / NAME

    def env(self):
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('TILO_ROOMS_', 'TILO_RELAY_'))}
        env.update({
            'TILO_ROOMS_PERSISTENT': NAME,
            'TILO_ROOMS_LOCAL': '1',
            'TILO_ROOMS_API': f'127.0.0.1:{self.api_port}',
            'TILO_ROOMS_PUBLIC_API': f'http://localhost:{self.api_port}',
            'TILO_ROOMS_PORT_BASE': str(self.port_base),
            'TILO_ROOMS_ROOT_FILE': str(self.scratch / 'room-root'),
            'TILO_ROOMS_STATE': str(self.scratch / 'rooms'),
            'TILO_ROOMS_ASSETS': str(self.scratch / 'assets'),
            'TILO_ROOMS_RELAY_BIN': str(self.args.relay_bin),
            'TILO_SECRETS_DIR': str(self.scratch / 'secrets'),
            # This check is the trusted proxy: it names each visitor.
            'TILO_ROOMS_TRUSTED_PROXIES': '127.0.0.1/32,::1/128',
            'TILO_ROOMS_MAX_GUESTS': str(MAX_GUESTS),
            'TILO_ROOMS_MAX_SPECTATORS': str(MAX_SPECTATORS),
            'TILO_ROOMS_GUEST_JOINS_PER_10_MIN': str(GUEST_JOINS_PER_10_MIN),
            'TILO_ROOMS_SPECTATES_PER_10_MIN': str(SPECTATES_PER_10_MIN),
            # The production seed apps, reset every second so the snapshot
            # bound is reached within the run.
            'TILO_ROOMS_SEED_APPS': 'so.tilo.chat,so.tilo.notes,so.tilo.canvas',
            'TILO_ROOMS_RESET_SECS': '1',
        })
        return env

    def start(self):
        self.process = subprocess.Popen([self.args.room_service_bin], env=self.env(), stdout=self.log,
                                        stderr=subprocess.STDOUT, start_new_session=True)
        self.started = time.monotonic()

    def stop(self):
        """SIGTERM, as Kubernetes stops the pod: how long until the service
        and every process it started have exited (`seconds`, None past the
        grace period), and what was still running then."""
        if self.process is None:
            return {'seconds': 0.0, 'running': []}
        began = time.monotonic()
        if self.process.poll() is None:
            self.process.terminate()
        deadline = began + GRACE_SECS
        gone, running = None, []
        while time.monotonic() < deadline:
            running = group_members(self.process.pid)
            if self.process.poll() is not None and not running:
                gone = round(time.monotonic() - began, 1)
                break
            time.sleep(0.1)
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.process.wait()
        self.process = None
        return {'seconds': gone, 'running': running}

    def log_tail(self, lines=40):
        self.log.flush()
        text = (self.args.out / 'service.log').read_text(errors='replace')
        return '\n'.join(text.splitlines()[-lines:])


def group_members(pgid):
    """The processes of a process group that have not exited. Exited ones
    nobody reaped (zombies, which a container's init may leave) do not
    count: they hold no file, port or store."""
    proc = pathlib.Path('/proc')
    if (proc / 'self' / 'stat').is_file():
        members = []
        for stat in proc.glob('[0-9]*/stat'):
            try:
                text = stat.read_text()
            except OSError:
                continue
            # pid (comm) state ppid pgrp ...; comm may hold spaces.
            comm = text[text.find('(') + 1:text.rfind(')')]
            state, _, group = text[text.rfind(')') + 2:].split()[:3]
            if group == str(pgid) and state != 'Z':
                members.append(f'{stat.parent.name} {comm}')
        return members
    listing = subprocess.run(['ps', '-A', '-o', 'pid=,pgid=,stat=,comm='], capture_output=True, text=True,
                             check=True).stdout
    members = []
    for line in listing.splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4 and fields[1] == str(pgid) and not fields[2].startswith('Z'):
            members.append(f'{fields[0]} {fields[3]}')
    return members


def wait_ready(service, api, attempts_from):
    """Waits until the service answers /spectate through its relay, each
    attempt from a fresh address so no window fills; the admitted probe
    holds one spectator seat. Returns the probe's answer or None."""
    deadline = time.monotonic() + 180
    attempt = 0
    while time.monotonic() < deadline:
        if service.process.poll() is not None:
            log(f'tilo-room-service exited ({service.process.returncode})')
            return None
        if (service.room_dir / 'room-id').is_file():
            attempt += 1
            answer = api.spectate(f'{attempts_from}.{attempt % 250 + 1}')
            if admitted(answer, 'certificate'):
                return answer
        time.sleep(0.5)
    return None


def certified_person(answer):
    """The user a spectator certificate was issued under. The service
    passes the certificate as JSON text, so the page gets its nanosecond
    timestamps exactly."""
    certificate = answer[2]['certificate']
    if isinstance(certificate, str):
        certificate = json.loads(certificate)
    return certificate.get('user_public_key')


def snapshot_stamps(service, app):
    directory = service.room_dir / 'relay' / 'snapshots' / app
    if not directory.is_dir():
        return []
    return sorted(int(path.stem) for path in directory.glob('*.bin') if path.stem.isdigit())


def check_snapshots(checks, service, label):
    """Each seeded app keeps exactly the relay's ten newest snapshots once it
    has been reset more often than that. Returns the newest stamps."""
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        counts = [len(snapshot_stamps(service, app)) for app in ('so.tilo.notes', 'so.tilo.canvas')]
        if min(counts) >= SNAPSHOTS_KEPT:
            break
        time.sleep(0.5)
    # A few more resets: the bound must hold while snapshots keep coming.
    first = {app: snapshot_stamps(service, app) for app in ('so.tilo.notes', 'so.tilo.canvas')}
    time.sleep(4)
    newest = {}
    for app in ('so.tilo.notes', 'so.tilo.canvas'):
        stamps = snapshot_stamps(service, app)
        moved = bool(stamps) and bool(first[app]) and stamps[0] > first[app][0]
        checks.check(f'{label}: {app} keeps exactly {SNAPSHOTS_KEPT} reset snapshots while resets continue',
                     len(stamps) == SNAPSHOTS_KEPT and moved,
                     {'kept': len(stamps), 'oldestBefore': first[app][:1], 'oldestAfter': stamps[:1]},
                     SNAPSHOTS_KEPT)
        newest[app] = stamps[-1] if stamps else None
    chat = service.room_dir / 'relay' / 'snapshots' / 'so.tilo.chat'
    checks.check(f'{label}: chat is never snapshotted or reset', not chat.exists(), {'exists': chat.exists()})
    return newest


def run_checks(service, checks):
    api = Api(f'http://127.0.0.1:{service.api_port}')
    service.start()
    probe = wait_ready(service, api, '198.51.100')
    if not checks.check('the room comes up and certifies a watching page', probe is not None,
                        'ready' if probe else 'no spectator certificate within 3 minutes'):
        return
    room = probe[2]['room']
    spectator_person = certified_person(probe)

    metrics = api.metrics()
    caps = {key: metrics.get(key) for key in ('tilo_rooms_max_guests', 'tilo_rooms_max_spectators')}
    checks.check('/metrics exports the caps the service runs with',
                 caps == {'tilo_rooms_max_guests': str(MAX_GUESTS), 'tilo_rooms_max_spectators': str(MAX_SPECTATORS)},
                 caps, {'max_guests': MAX_GUESTS, 'max_spectators': MAX_SPECTATORS})

    # Spectators first, while the probe's seat is held: the probe and two
    # from one address fill three of four seats.
    window = [api.spectate('203.0.113.10') for _ in range(SPECTATES_PER_10_MIN)]
    checks.check(f'{SPECTATES_PER_10_MIN} spectator certificates from one address are given',
                 all(admitted(answer, 'certificate') for answer in window), [describe(a) for a in window])
    answer = api.spectate('203.0.113.10')
    checks.check(f'spectator certificate {SPECTATES_PER_10_MIN + 1} from that address is refused for its window',
                 refused(answer, 429, TOO_MANY, (1, 600)), describe(answer), {'status': 429, 'error': TOO_MANY})
    answer = api.spectate('203.0.113.11')
    checks.check('the refused spectator held no seat: the last one goes to another address',
                 admitted(answer, 'certificate'), describe(answer))
    answer = api.spectate('203.0.113.12')
    checks.check(f'spectator {MAX_SPECTATORS + 1} is told the room is full',
                 refused(answer, 503, FULL, FULL_RETRY_SECS), describe(answer),
                 {'status': 503, 'error': FULL, 'retryAfter': FULL_RETRY_SECS})

    answer = api.call('POST', '/join', {'userPublicKey': 'zz', 'displayName': ''}, '203.0.113.20')
    checks.check('a malformed join is refused with its reason', refused(answer, 400, BAD_KEY), describe(answer))

    keys = {label: hex32() for label in ('a1', 'a2', 'b', 'c')}
    first_seat = time.monotonic()
    window = [api.join('203.0.113.20', keys['a1']), api.join('203.0.113.20', keys['a2'])]
    checks.check(f'{GUEST_JOINS_PER_10_MIN} guest joins from one address are pre-admitted',
                 all(admitted(answer, 'admissionToken') for answer in window), [describe(a) for a in window])
    answer = api.join('203.0.113.20')
    checks.check(f'guest join {GUEST_JOINS_PER_10_MIN + 1} from that address is refused for its window',
                 refused(answer, 429, TOO_MANY, (1, 600)), describe(answer), {'status': 429, 'error': TOO_MANY})
    answer = api.join('203.0.113.21', keys['b'])
    checks.check('the refused guest held no seat: the last one goes to another address',
                 admitted(answer, 'admissionToken'), describe(answer))
    answer = api.join('203.0.113.22', keys['c'])
    checks.check(f'guest {MAX_GUESTS + 1} is told the room is full',
                 refused(answer, 503, FULL, FULL_RETRY_SECS), describe(answer),
                 {'status': 503, 'error': FULL, 'retryAfter': FULL_RETRY_SECS})

    metrics = api.metrics()
    counted = {
        'joins': metrics.get('tilo_rooms_joins_total'),
        'rate_limited': metrics.get('tilo_rooms_join_failures_total{reason="rate_limited"}'),
        'full': metrics.get('tilo_rooms_join_failures_total{reason="full"}'),
        'invalid': metrics.get('tilo_rooms_join_failures_total{reason="invalid"}'),
    }
    checks.check('/metrics counts each admission and each refusal under its reason',
                 counted == {'joins': '3', 'rate_limited': '2', 'full': '2', 'invalid': '1'}, counted,
                 {'joins': 3, 'rate_limited': 2, 'full': 2, 'invalid': 1})

    # Recovery: nobody opened a session, so the first held seat frees after
    # its hold and the visitor told "full" gets in.
    while True:
        answer = api.join('203.0.113.22', keys['c'])
        if admitted(answer, 'admissionToken') or time.monotonic() - first_seat > SEAT_HOLD_SECS + 20:
            break
        time.sleep(1)
    waited = round(time.monotonic() - first_seat, 1)
    checks.check('a visitor told "full" gets in once an unused seat\'s hold ends',
                 admitted(answer, 'admissionToken') and waited >= SEAT_HOLD_SECS - 1,
                 {'answer': describe(answer), 'secondsAfterFirstSeat': waited},
                 f'admitted, not before {SEAT_HOLD_SECS} s')

    ledger_path = service.room_dir / 'guests.json'
    ledger = ledger_path.read_text() if ledger_path.is_file() else ''
    checks.check('the guest ledger is kept on the volume, by key and address hash only',
                 all(key in ledger for key in keys.values()) and '203.0.113' not in ledger
                 and '127.0.0.1' not in ledger,
                 {'bytes': len(ledger), 'keysFound': sum(key in ledger for key in keys.values())})

    before = check_snapshots(checks, service, 'first start')

    stopped = service.stop()
    checks.check(f'SIGTERM stops the service and its relay within {GRACE_SECS} s', stopped['seconds'] is not None,
                 stopped)

    service.start()
    probe = wait_ready(service, api, '198.51.101')
    if not checks.check('the room comes back after a restart', probe is not None,
                        'ready' if probe else 'no spectator certificate within 3 minutes'):
        return
    checks.check('a restart keeps the room', probe[2]['room'] == room, {'before': room, 'after': probe[2]['room']})
    again = certified_person(probe)
    checks.check('a restart keeps the spectator person', again == spectator_person and again is not None,
                 'same' if again == spectator_person else 'changed')
    metrics = api.metrics()
    caps = {key: metrics.get(key) for key in ('tilo_rooms_max_guests', 'tilo_rooms_max_spectators')}
    checks.check('the caps are exported again after a restart',
                 caps == {'tilo_rooms_max_guests': str(MAX_GUESTS), 'tilo_rooms_max_spectators': str(MAX_SPECTATORS)},
                 caps)
    answer = api.join('203.0.113.20')
    checks.check('a restart does not give an address its guest join window back',
                 refused(answer, 429, TOO_MANY, (1, 600)), describe(answer), {'status': 429, 'error': TOO_MANY})
    answer = api.join('203.0.113.21')
    checks.check('an address with room in its window still joins after a restart',
                 admitted(answer, 'admissionToken'), describe(answer))
    after = check_snapshots(checks, service, 'after a restart')
    checks.check('resets continue after a restart under the same bound',
                 all(after[app] is not None and before[app] is not None and after[app] > before[app] for app in after),
                 {'newestBefore': before, 'newestAfter': after})

    stopped = service.stop()
    checks.check(f'SIGTERM stops the restarted service and its relay within {GRACE_SECS} s',
                 stopped['seconds'] is not None, stopped)


def write_summary(out, checks, started):
    summary = {'passed': not checks.failed, 'seconds': round(time.monotonic() - started, 1),
               'configured': {'maxGuests': MAX_GUESTS, 'maxSpectators': MAX_SPECTATORS,
                              'guestJoinsPer10Min': GUEST_JOINS_PER_10_MIN,
                              'spectatesPer10Min': SPECTATES_PER_10_MIN, 'resetSecs': 1},
               'checks': checks.results}
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    lines = ['# Persistent room: enabled limits and kept state', '',
             f'{"PASS" if summary["passed"] else "FAIL"}: {len(checks.results) - len(checks.failed)} of '
             f'{len(checks.results)} checks in {summary["seconds"]} s', '', '| check | result |', '|---|---|']
    lines += [f'| {result["name"]} | {"ok" if result["passed"] else "FAIL"} |' for result in checks.results]
    (out / 'summary.md').write_text('\n'.join(lines) + '\n')


def main():
    parser = argparse.ArgumentParser(prog='limits-e2e.py', description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--relay-bin', type=pathlib.Path, required=True,
                        help='tilo-relay built with --features agents, as the image ships it')
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
    started = time.monotonic()
    (scratch / 'assets').mkdir(parents=True)
    root = scratch / 'room-root'
    root.touch(mode=0o600)
    root.write_text(secrets.token_hex(32))
    checks = Checks()
    service = Service(args, scratch)
    try:
        run_checks(service, checks)
    finally:
        service.stop()
        write_summary(args.out, checks, started)
        if checks.failed:
            # The workflow log is where a failure is read first.
            log(f'the service log ends:\n{service.log_tail()}')
    if checks.failed:
        log(f'{len(checks.failed)} of {len(checks.results)} checks failed; see {args.out / "summary.md"}')
        sys.exit(1)
    log(f'all {len(checks.results)} checks passed')


if __name__ == '__main__':
    main()
