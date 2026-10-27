"""Drive the measured normal tilo-control executable; no alternate controller."""
import json
import os
from pathlib import Path
import importlib.util
import time


_path = Path(__file__).resolve().parent.parent / 'installer-source-fixture/bounded_native.py'
_spec = importlib.util.spec_from_file_location('installer_bounded_native', _path)
_native = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_native)


def bounded_command(argv, deadline, log, value=None, maximum=2 * 1024 * 1024):
    descriptor = None
    try:
        if value is not None:
            if not isinstance(value, bytes) or len(value) > 81920:
                raise RuntimeError('private native input exceeded bound')
            descriptor = os.memfd_create('tilo-private-qualification-stdin', os.MFD_CLOEXEC)
            offset = 0
            while offset < len(value):
                if time.monotonic() >= deadline:
                    raise RuntimeError('private native input deadline elapsed')
                offset += os.write(descriptor, value[offset:])
            os.lseek(descriptor, 0, os.SEEK_SET)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('native receiving deadline elapsed')
        status, output, diagnostics = _native.bounded(argv, timeout=remaining, limit=maximum,
                                                     stdin=descriptor)
        log.write(diagnostics)
        log.flush()
        if status != 0:
            raise RuntimeError('normal native receiving operation refused')
        return output
    finally:
        if descriptor is not None:
            os.ftruncate(descriptor, 0)
            os.close(descriptor)


class Control:
    def __init__(self, socket, deadline, private_log):
        self.socket = Path(socket)
        self.deadline = deadline
        self.private_log = private_log

    def command(self, arguments, value=None):
        # Secret parameters are bounded anonymous stdin, never argv/logs.
        encoded = None if value is None else json.dumps(value, separators=(',', ':')).encode()
        output = bounded_command(['/usr/libexec/tilo/tilo-control', '--socket', str(self.socket), *arguments],
                                 min(self.deadline, time.monotonic() + 35), self.private_log, encoded)
        result = json.loads(output)
        if not isinstance(result, dict):
            raise RuntimeError('canonical semantic response is not an object')
        return result

    def rows(self):
        rows, cursor, seen = [], None, set()
        for _ in range(128):
            arguments = ['actions', 'list', '--json', '--limit', '128']
            if cursor is not None:
                arguments.extend(['--cursor', cursor])
            page = self.command(arguments)
            current = page.get('actions')
            if not isinstance(current, list) or len(current) > 128:
                raise RuntimeError('invalid canonical action page')
            rows.extend(current)
            cursor = page.get('next_cursor')
            if cursor is None:
                return rows
            if not isinstance(cursor, str) or not cursor or cursor in seen:
                raise RuntimeError('invalid repeated action cursor')
            seen.add(cursor)
        raise RuntimeError('canonical action pagination exceeded bound')

    def invoke(self, row, arguments=None):
        return self.command(['actions', 'invoke', row['provider'], row['id'], '--args-stdin'],
                            arguments or {})

    def terminal(self, reply, answer_authentication=None):
        """Follow the same admitted broker job; queue admission is not completion."""
        receipt = reply.get('receipt')
        if not isinstance(receipt, dict):
            raise RuntimeError('canonical invocation lacks an actual receipt')
        invocation = receipt.get('broker_invocation_id')
        while time.monotonic() < self.deadline:
            update = receipt.get('update', {})
            state = update.get('state')
            if state == 'succeeded':
                return receipt
            if state in ('failed', 'cancelled', 'refused'):
                raise RuntimeError('actual canonical action terminated without success')
            if type(invocation) is not int or invocation <= 0:
                raise RuntimeError('pending action lacks actual broker custody')
            if answer_authentication is not None:
                answer_authentication()
            observed = self.command(['actions', 'status', str(invocation), '--wait=500'])
            receipt = observed.get('receipt')
            if not isinstance(receipt, dict) or receipt.get('broker_invocation_id') != invocation:
                raise RuntimeError('canonical status refers to another invocation')
        raise RuntimeError('actual canonical invocation completion remains pending')

    def offered(self, provider, identity):
        rows = [row for row in self.rows()
                if row.get('provider') == provider and row.get('id') == identity]
        if len(rows) != 1:
            raise RuntimeError('required canonical action was not uniquely offered')
        return rows[0]

    def inspect(self):
        reply = self.invoke(self.offered('onboarding', 'inspect'))
        receipt = reply.get('receipt', {})
        update = receipt.get('update', {})
        if update.get('state') != 'succeeded':
            raise RuntimeError('onboarding observation did not complete')
        output = update.get('output', {}).get('value')
        if not isinstance(output, dict):
            raise RuntimeError('onboarding observation missing actual output')
        return output

    def answer_authentication(self, account, secret):
        tickets = [row for row in self.rows()
                   if row.get('provider') == 'authentication-prompts'
                   and row.get('id', '').startswith('answer@')]
        for ticket in tickets:
            offered = next((param.get('choices') for param in ticket.get('params', [])
                            if param.get('name') == 'identity'), None)
            if not isinstance(offered, list) or account not in offered:
                raise RuntimeError('actual PAM prompt did not offer target account')
            reply = self.invoke(ticket, {'identity': account, 'secret': secret})
            if reply.get('receipt', {}).get('update', {}).get('state') != 'succeeded':
                raise RuntimeError('actual authentication queue delivery failed')
        # Delivery is not PAM success; the original pending action is observed
        # independently by the receiving state machine.
        return len(tickets)
