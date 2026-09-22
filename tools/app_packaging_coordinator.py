#!/usr/bin/env python3
"""One active / latest pending packaging experiment. Dry-run by default.

Only the named private, nonpublishing fixture can be dispatched. SQLite must be
on one host's durable local disk (not a network filesystem); all workers share
one database. No scheduler, production activation, retries or cancellation.
"""
import argparse
import base64
from contextlib import contextmanager
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import time
import uuid

from app_packaging_contract import canonical, receipt_key

REPO = 'tilo/ci-workflow-pilot-20260918-9245-repeat'
WORKFLOW = 'packaging-coordinator-probe.yml'
FIXTURE = Path(__file__).resolve().parents[1] / 'experiments/ci-forgejo' / WORKFLOW
TERMINAL = {'success', 'failure', 'cancelled', 'skipped'}
EMPTY = {'sequence': -1, 'desired': None, 'baseline': None, 'active': None, 'last_attempt': None}


class VerificationRequired(ValueError):
    """A completed build is not yet an independently verified packaging baseline."""


class NoBuildAcceptance:
    """Trusted validator result: handled source, not a new package baseline."""
    def __init__(self, evidence):
        self.evidence = evidence


def validate_receipt(receipt):
    if not isinstance(receipt, dict) or not re.fullmatch('[0-9a-f]{40}', receipt.get('source_sha', '')):
        raise ValueError('desired receipt requires a full lowercase source_sha')
    return receipt


class Store:
    def __init__(self, path, writable=False):
        self.path = Path(path).resolve()
        self.writable = writable
        if writable:
            self.db = sqlite3.connect(self.path, timeout=10)
            self.db.execute('PRAGMA synchronous=FULL')
            self.db.execute('CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL)')
            self.db.execute('CREATE TABLE IF NOT EXISTS attempts (id TEXT PRIMARY KEY, value TEXT NOT NULL)')
            self.db.execute('INSERT OR IGNORE INTO state VALUES (1,?)', (canonical(EMPTY),))
            self.db.commit()
        else:
            self.db = sqlite3.connect(self.path.as_uri() + '?mode=ro', uri=True, timeout=10) if self.path.exists() else None

    def read(self):
        return json.loads(self.db.execute('SELECT value FROM state WHERE id=1').fetchone()[0]) if self.db else dict(EMPTY)

    @contextmanager
    def transaction(self):
        if not self.writable:
            raise ValueError('read-only store')
        self.db.execute('BEGIN IMMEDIATE')
        try:
            state = self.read()
            yield state
            self.db.execute('UPDATE state SET value=? WHERE id=1', (canonical(state),))
            self.db.commit()
        except BaseException:
            self.db.rollback()
            raise

    def record(self, attempt):
        self.db.execute('INSERT INTO attempts VALUES (?,?) ON CONFLICT(id) DO UPDATE SET value=excluded.value',
                        (attempt['id'], canonical(attempt)))

    def observe(self, sequence, receipt):
        validate_receipt(receipt)
        with self.transaction() as state:
            if sequence <= state['sequence']:
                if sequence == state['sequence'] and receipt == state['desired']:
                    return
                raise ValueError('stale or conflicting observation sequence')
            state['sequence'], state['desired'] = sequence, receipt


class Forgejo:
    def call(self, path, data=None):
        command = ['fgj', '--hostname', 'code.podesta.ai', 'api', path]
        if data is not None:
            command += ['-X', 'POST']
        result = subprocess.run(command, input=canonical(data) if data is not None else None,
                                capture_output=True, text=True, timeout=30, check=True)
        return json.loads(result.stdout)

    def qualify(self, sha):
        repository = self.call('repos/' + REPO)
        if repository.get('full_name') != REPO or repository.get('private') is not True:
            raise ValueError('fixture repository identity/privacy mismatch')
        contents = self.call(f'repos/{REPO}/contents/.forgejo/workflows/{WORKFLOW}?ref={sha}')
        if base64.b64decode(contents['content']).replace(b'\r\n', b'\n') != FIXTURE.read_text().encode():
            raise ValueError('source does not contain exact nonpublishing fixture')

    def dispatch(self, intent):
        return self.call(f'repos/{REPO}/actions/workflows/{WORKFLOW}/dispatches',
                         {'ref': intent['desired']['source_sha'], 'return_run_info': True,
                          'inputs': {'coordinator_intent': intent['id'], 'baseline_sha': baseline_sha(intent)}})

    def run(self, run_id):
        return self.call(f'repos/{REPO}/actions/runs/{run_id}')


def baseline_sha(intent):
    return intent['baseline']['source_sha'] if intent['baseline'] else 'none'


def validate_run(intent, run):
    payload = json.loads(run.get('event_payload') or '{}')
    expected = {'coordinator_intent': intent['id'], 'baseline_sha': baseline_sha(intent)}
    if (run.get('repository', {}).get('full_name') != REPO or run.get('workflow_id') != WORKFLOW
            or run.get('trigger_event') != 'workflow_dispatch'
            or run.get('commit_sha') != intent['desired']['source_sha']
            or payload.get('ref') != intent['desired']['source_sha']
            or payload.get('inputs') != expected or not isinstance(run.get('id'), int) or run['id'] <= 0):
        raise ValueError('run does not match persisted dispatch intent')


def next_action(state):
    if state['active']:
        return 'poll' if state['active'].get('run_id') else 'reconcile-required'
    desired = state['desired']
    if desired is None or desired == state['baseline'] or desired == state.get('handled'):
        return 'idle'
    last = state['last_attempt']
    if last and last['desired'] == desired:
        return 'failed-source-blocked'
    return 'dispatch'


def accept_run(store, intent_id, run, validator=validate_run):
    observed = store.read()['active']
    if not observed or observed['id'] != intent_id:
        raise ValueError('active intent changed')
    if observed.get('run_id') not in (None, run['id']):
        raise ValueError('run ID conflicts with accepted dispatch')
    fingerprint = canonical(observed)
    # Artifact downloads and canonical verification can be slow. Observers must
    # remain able to record the newest desired revision while that work runs.
    evidence = validator(observed, run)
    with store.transaction() as state:
        intent = state['active']
        if not intent or intent['id'] != intent_id:
            raise ValueError('active intent changed')
        if canonical(intent) != fingerprint:
            raise ValueError('active intent changed during verification')
        intent.update(run_id=run['id'], status=run['status'], updated_at=time.time())
        if run['status'] in TERMINAL:
            if run['status'] == 'success':
                if isinstance(evidence, NoBuildAcceptance):
                    state['handled'] = intent['desired']
                    intent['outcome'] = 'verified-no-build'
                    intent['verification'] = evidence.evidence
                else:
                    state['baseline'] = intent['desired']
                    state['baseline_evidence'] = evidence
                    state.pop('handled', None)
                    intent['verification'] = evidence
            state['last_attempt'] = intent
            state['active'] = None
        store.record(intent)


def tick(store, api):
    # Qualification reads precede claiming work; transaction rechecks desired to
    # prevent racing observers/workers from dispatching an obsolete snapshot.
    snapshot = store.read()
    action = next_action(snapshot)
    if action == 'poll':
        intent = snapshot['active']
        try:
            accept_run(store, intent['id'], api.run(intent['run_id']),
                       getattr(api, 'validate_run', validate_run))
        except VerificationRequired:
            with store.transaction() as state:
                if state['active'] and state['active']['id'] == intent['id']:
                    state['active']['status'] = 'awaiting-verification'
                    store.record(state['active'])
        return
    if action != 'dispatch':
        return
    if hasattr(api, 'qualify_receipt'):
        api.qualify_receipt(snapshot['desired'])
    else:
        api.qualify(snapshot['desired']['source_sha'])
    with store.transaction() as state:
        if next_action(state) != 'dispatch' or state['desired'] != snapshot['desired']:
            return
        intent = {'id': str(uuid.uuid4()), 'desired': state['desired'], 'baseline': state['baseline'],
                  'baseline_evidence': state.get('baseline_evidence'),
                  'receipt_sha256': receipt_key(state['desired']), 'status': 'dispatch-intent', 'created_at': time.time()}
        state['active'] = intent
        store.record(intent)
    # A crash anywhere after the committed intent leaves reconciliation required,
    # including a crash before POST. Never automatically resend an uncertain POST.
    try:
        response = api.dispatch(intent)
        run_id = response['id']
        if not isinstance(run_id, int) or run_id <= 0:
            raise ValueError('dispatch response lacks run ID')
        with store.transaction() as state:
            if state['active']['id'] != intent['id']:
                raise ValueError('active intent changed during dispatch')
            state['active']['run_id'] = run_id
            state['active']['status'] = 'accepted-unverified'
            store.record(state['active'])
        accept_run(store, intent['id'], api.run(run_id), getattr(api, 'validate_run', validate_run))
    except VerificationRequired:
        with store.transaction() as state:
            active = state['active']
            if active and active['id'] == intent['id']:
                active['status'] = 'awaiting-verification'
                store.record(active)
    except Exception as error:
        with store.transaction() as state:
            active = state['active']
            if active and active['id'] == intent['id']:
                # Avoid persisting subprocess output: it may contain API details.
                active['error'] = type(error).__name__
                active['status'] = 'dispatch-uncertain'
                store.record(active)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state', type=Path, required=True)
    parser.add_argument('--apply-private-fixture', action='store_true')
    commands = parser.add_subparsers(dest='command', required=True)
    observe = commands.add_parser('observe')
    observe.add_argument('--sequence', type=int, required=True)
    observe.add_argument('--receipt', type=Path, required=True)
    commands.add_parser('status')
    commands.add_parser('tick')
    reconcile = commands.add_parser('reconcile')
    reconcile.add_argument('--run-id', type=int, required=True)
    args = parser.parse_args()
    store = Store(args.state, writable=args.apply_private_fixture and args.command != 'status')
    try:
        if args.command == 'observe':
            receipt = validate_receipt(json.loads(args.receipt.read_text()))
            if args.apply_private_fixture:
                store.observe(args.sequence, receipt)
        elif args.apply_private_fixture and args.command == 'tick':
            tick(store, Forgejo())
        elif args.apply_private_fixture and args.command == 'reconcile':
            intent = store.read()['active']
            if not intent:
                raise ValueError('no active intent to reconcile')
            accept_run(store, intent['id'], Forgejo().run(args.run_id))
        state = store.read()
        print(json.dumps({'dry_run': not args.apply_private_fixture, 'next_action': next_action(state), 'state': state}, indent=2))
    finally:
        if store.db:
            store.db.close()


if __name__ == '__main__':
    main()
