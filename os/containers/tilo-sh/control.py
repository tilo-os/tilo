#!/usr/bin/env python3
"""The control port of a stack held as one program on a build server.

A stack started where its cargo drop-in builds keeps its state with the
program that started it, and when that program is held for as long as its
caller lives (t's `exec --forward`), nothing else can enter the job to act on
the stack: `down`, `restart`, `logs`, `owner`, `mail`, `invite` and `tilo`
would act in a fresh copy with none of that state. So the held program owns
them: it serves them on one more forwarded port, and the commands that act on
the stack ask it, with the stack's own token.

  control.py serve --port PORT --token TOKEN --repo DIR --self SCRIPT [--start ARGS…]
  control.py request --port PORT --token TOKEN -- ARGS…
  control.py down --port PORT --token TOKEN

`serve` listens before it does anything else, so the port answers from the
moment the program starts; it then starts the stack with `--start` (a request
that arrives meanwhile waits for that), answers one command at a time by
starting stack.sh with a request's ARGS in DIR with the environment it was
started with, and exits once it has served `down`. `request` prints what that command printed and
exits with its status; `down` does the same and then expects the program to end.
"""
import argparse
import hmac
import json
import os
import subprocess
import sys
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

COMMAND_TIMEOUT = 600
MAX_BODY = 1 << 20


class Control:
    """Starts stack.sh commands where the stack is, one at a time."""

    def __init__(self, repo, script, token, start):
        self.repo = repo
        self.script = script
        self.token = token
        self.start = start
        self.lock = threading.Lock()
        self.stopping = threading.Event()
        self.ready = threading.Event()
        self.failed = None

    def authenticated(self, header):
        if not header or not header.startswith('Bearer '):
            return False
        return hmac.compare_digest(header[len('Bearer '):], self.token)

    def spawn(self, argv, capture, timeout=COMMAND_TIMEOUT):
        child = subprocess.Popen(
            ['bash', self.script, *argv], cwd=self.repo, text=True,
            stdout=subprocess.PIPE if capture else None,
            stderr=subprocess.PIPE if capture else None)
        try:
            out, err = child.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate()
            return None
        return child, out, err

    def start_stack(self):
        if self.start:
            with self.lock:
                # The stack's start builds its programs in the job first: the caller bounds the
                # whole bring-up (stack.sh's TILO_SH_REMOTE_TIMEOUT), and nothing here cuts it.
                ended = self.spawn(self.start, capture=False, timeout=None)
            if ended is None:
                self.failed = 124
            elif ended[0].returncode != 0:
                self.failed = ended[0].returncode
        self.ready.set()

    def execute(self, argv, down):
        if not isinstance(argv, list) or not all(isinstance(a, str) for a in argv):
            return 400, {'status': 2, 'stdout': '', 'stderr': 'argv must be a list of strings\n'}
        if not self.ready.wait(COMMAND_TIMEOUT):
            return 503, {'status': 2, 'stdout': '', 'stderr': 'the stack is still starting\n'}
        if self.failed is not None:
            return 503, {'status': self.failed, 'stdout': '',
                         'stderr': f'the stack did not start (exit {self.failed})\n'}
        with self.lock:
            ended = self.spawn(argv, capture=True)
            if ended is None:
                return 500, {'status': 124, 'stdout': '',
                             'stderr': f'stack.sh {argv} took longer than {COMMAND_TIMEOUT}s\n'}
            if down:
                # The stack is stopped and the response is on its way; the held program has
                # nothing left to keep alive, and ending frees the job.
                threading.Timer(0.2, self.stopping.set).start()
            child, out, err = ended
            return 200, {'status': child.returncode, 'stdout': out or '', 'stderr': err or ''}


def serve(args):
    control = Control(args.repo, args.self, args.token, args.start)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *_):
            pass

        def do_POST(self):
            if not control.authenticated(self.headers.get('Authorization')):
                self.respond(403, {'status': 2, 'stdout': '', 'stderr': 'not this stack\n'})
                return
            length = int(self.headers.get('Content-Length') or 0)
            if length <= 0 or length > MAX_BODY:
                self.respond(400, {'status': 2, 'stdout': '', 'stderr': 'no request\n'})
                return
            try:
                body = json.loads(self.rfile.read(length))
            except (ValueError, UnicodeDecodeError):
                self.respond(400, {'status': 2, 'stdout': '', 'stderr': 'bad request\n'})
                return
            down = bool(body.get('down'))
            status, answer = control.execute(body.get('argv'), down)
            self.respond(status, answer)

        def do_GET(self):
            ok = control.authenticated(self.headers.get('Authorization'))
            self.respond(200 if ok else 403, {'status': 0 if ok else 2})

        def respond(self, status, answer):
            payload = json.dumps(answer).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(('127.0.0.1', args.port), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    print(f'stack control on 127.0.0.1:{args.port}', file=sys.stderr, flush=True)
    control.start_stack()
    if control.failed is not None:
        print(f'stack.sh {control.start} did not start (exit {control.failed})', file=sys.stderr,
              flush=True)
        server.shutdown()
        return control.failed
    control.stopping.wait()
    server.shutdown()
    return 0


def ask(args, argv, down):
    host = '127.0.0.1'
    body = json.dumps({'argv': argv, 'down': down}).encode()
    request = urllib.request.Request(
        f'http://{host}:{args.port}/command', data=body, method='POST',
        headers={'Authorization': f'Bearer {args.token}', 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(request, timeout=COMMAND_TIMEOUT + 60) as answer:
            result = json.loads(answer.read())
    except (urllib.error.URLError, ConnectionError, TimeoutError, ValueError) as error:
        print(f'stack control is not answering on {host}:{args.port}: {error}', file=sys.stderr)
        return 4
    sys.stdout.write(result.get('stdout') or '')
    sys.stdout.flush()
    sys.stderr.write(result.get('stderr') or '')
    sys.stderr.flush()
    return int(result.get('status') or 0)


def main():
    parser = argparse.ArgumentParser(add_help=True)
    modes = parser.add_subparsers(dest='mode', required=True)
    for name in ('serve', 'request', 'down'):
        mode = modes.add_parser(name)
        mode.add_argument('--port', type=int, required=True)
        mode.add_argument('--token', required=True)
        if name == 'serve':
            mode.add_argument('--repo', required=True)
            mode.add_argument('--self', required=True)
            mode.add_argument('--start', nargs='*', default=[])
        else:
            mode.add_argument('argv', nargs='*')
    args = parser.parse_args()
    if not args.token:
        parser.error('a stack token is required')
    if args.mode == 'serve':
        return serve(args)
    if args.mode == 'down':
        return ask(args, ['down', *args.argv], True)
    return ask(args, args.argv, False)


if __name__ == '__main__':
    sys.exit(main())
