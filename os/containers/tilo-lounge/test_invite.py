#!/usr/bin/env python3
"""The Lounge's invite helper against a stand-in operator socket: the command
line it writes, and what it prints and answers for each event the workspace
sends back."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import unittest

HELPER = Path(__file__).with_name('invite')


class Workspace:
    """A stand-in operator socket that answers each line with `answer(command)`."""

    def __init__(self, answer):
        self.directory = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.directory.name, 'operator.sock')
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(1)
        self.commands = []
        self.answer = answer
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        connection, _ = self.server.accept()
        with connection:
            line = b''
            while not line.endswith(b'\n'):
                line += connection.recv(4096)
            command = json.loads(line)
            self.commands.append(command)
            event = self.answer(command)
            connection.sendall(f'tilo-workspace-operator {json.dumps(event)}\n'.encode())

    def close(self):
        self.server.close()
        self.directory.cleanup()


def run(*arguments):
    return subprocess.run([sys.executable, '-I', str(HELPER), *arguments],
                          capture_output=True, text=True, timeout=20, check=False)


class Invite(unittest.TestCase):
    def workspace(self, answer):
        workspace = Workspace(answer)
        self.addCleanup(workspace.close)
        return workspace

    def test_an_editor_link_is_asked_by_default_and_printed(self):
        workspace = self.workspace(lambda command: {
            'event': 'invited', 'request': command['request'], 'link': 'http://tilo.localhost/i/a#b',
            'invite': 'ab' * 32, 'expires_at_ns': 1})
        done = run(workspace.path, '--workspace', 'Acme')
        self.assertEqual((done.returncode, done.stdout), (0, 'http://tilo.localhost/i/a#b\n'), done.stderr)
        (command,) = workspace.commands
        self.assertEqual(command['op'], 'invite')
        self.assertEqual(command['role'], 'editor')
        self.assertEqual(command['workspace'], 'Acme')
        self.assertNotIn('unless_owned', command)

    def test_an_owner_link_unless_owned_answers_three_when_the_lounge_has_one(self):
        workspace = self.workspace(lambda command: {'event': 'owned', 'request': command['request']})
        done = run(workspace.path, '--role', 'owner', '--unless-owned')
        self.assertEqual((done.returncode, done.stdout), (3, ''), done.stderr)
        (command,) = workspace.commands
        self.assertEqual((command['role'], command['unless_owned']), ('owner', True))
        self.assertNotIn('workspace', command, 'no workspace named is the Lounge itself')

    def test_a_refusal_is_said_on_stderr(self):
        workspace = self.workspace(lambda command: {
            'event': 'invite_refused', 'request': command['request'], 'reason': 'no workspace here is called Acme'})
        done = run(workspace.path, '--workspace', 'Acme')
        self.assertEqual(done.returncode, 1)
        self.assertIn('no workspace here is called Acme', done.stderr)

    def test_a_role_that_is_none_is_refused_before_asking(self):
        done = run('/nonexistent.sock', '--role', 'boss')
        self.assertEqual(done.returncode, 2)
        self.assertIn('not a role', done.stderr)


if __name__ == '__main__':
    unittest.main()
