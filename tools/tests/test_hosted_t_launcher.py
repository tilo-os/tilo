"""The guest's t launcher: token from the environment, download once, cache, exec.

os/hosted-guest/t is what /usr/local/bin/t is in a workspace's VM. It runs
against a local HTTP server standing in for the t server, so the download the
launcher makes is the one checked: the request path, the Authorization header
following the server's redirect, the cached copy, and the arguments it execs
the program with. Nothing here talks to the real t server.
"""
import http.server
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import threading
import unittest


REPO = Path(__file__).resolve().parents[2]
LAUNCHER = REPO / 'os/hosted-guest/t'
TOKEN = 's3cret-workspace-token'


def an_elf():
    """A small program the launcher can cache and exec: any ELF that ignores
    --version and echoes its arguments (`echo`)."""
    for candidate in ('/usr/bin/echo', '/bin/echo'):
        path = Path(candidate)
        if path.is_file() and path.read_bytes()[:4] == b'\x7fELF':
            return path.read_bytes()
    raise unittest.SkipTest('no ELF /bin/echo to serve as the program')


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def redirect(self, location):
        self.send_response(303)
        self.send_header('Location', location)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def page(self, body, status=200):
        body = body.encode()
        self.send_response(status)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        with self.server.lock:
            self.server.requests.append((self.path, self.headers.get('Authorization')))
        mode = self.server.mode
        if self.path == '/download/t':
            self.redirect('/repos/download/t')
        elif self.path == '/repos/download/t':
            if mode == 'login' or self.headers.get('Authorization') != f'Bearer {TOKEN}':
                self.redirect('/login?next=%2Frepos%2Fdownload%2Ft')
            elif mode == 'broken':
                self.page('upstream failed', status=500)
            else:
                self.send_response(200)
                self.send_header('Content-Type', 'application/octet-stream')
                self.send_header('Content-Length', str(len(self.server.elf)))
                self.end_headers()
                self.wfile.write(self.server.elf)
        elif self.path.startswith('/login'):
            self.page('<html><body>log in</body></html>')
        else:
            self.page('not found', status=404)


class LauncherTest(unittest.TestCase):
    def setUp(self):
        self.server = Server(('127.0.0.1', 0), Handler)
        self.server.requests = []
        self.server.lock = threading.Lock()
        self.server.mode = 'ok'
        self.server.elf = an_elf()
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.home = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.home, True)

    def environ(self, **overrides):
        env = {key: value for key, value in os.environ.items()
               if key not in ('T_SERVER', 'T_TOKEN', 'XDG_CACHE_HOME')}
        env.update(HOME=str(self.home), PATH=os.environ['PATH'],
                   T_SERVER=f'http://127.0.0.1:{self.server.server_address[1]}')
        env.update(overrides)
        env = {key: value for key, value in env.items() if value is not None}
        return env

    def run_launcher(self, *args, **overrides):
        return subprocess.run([str(LAUNCHER), *args], env=self.environ(**overrides),
                              capture_output=True, text=True, timeout=120)

    def cached(self):
        return self.home / '.cache' / 't' / 't'

    def test_the_script_is_valid_sh(self):
        checked = subprocess.run(['sh', '-n', str(LAUNCHER)], capture_output=True, text=True)
        self.assertEqual(checked.returncode, 0, checked.stderr)

    def test_no_token_names_the_variable_and_the_vault_item(self):
        done = self.run_launcher('--help', T_TOKEN=None, T_SERVER=None)
        self.assertEqual(done.returncode, 1)
        said = (done.stderr or done.stdout).strip().splitlines()
        self.assertEqual(len(said), 1, said)
        self.assertIn('T_TOKEN', said[0])
        self.assertIn('vault', said[0])
        self.assertIn('grant', said[0])
        self.assertFalse(self.cached().exists())
        self.assertEqual(self.server.requests, [])

    def test_first_use_downloads_with_the_bearer_token_caches_and_execs(self):
        done = self.run_launcher('hello', 'world', T_TOKEN=TOKEN)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, 'hello world\n')
        self.assertEqual([path for path, _ in self.server.requests],
                         ['/download/t', '/repos/download/t'])
        self.assertEqual([header for _, header in self.server.requests],
                         [f'Bearer {TOKEN}', f'Bearer {TOKEN}'])
        cache = self.cached()
        self.assertEqual(cache.read_bytes(), self.server.elf)
        self.assertTrue(cache.stat().st_mode & stat.S_IXUSR)
        self.assertEqual(list(cache.parent.glob('.t.*')), [], 'a temporary download was left')

    def test_the_token_is_never_written_to_disk(self):
        self.assertEqual(self.run_launcher('hello', T_TOKEN=TOKEN).returncode, 0)
        for path in self.home.rglob('*'):
            if path.is_file():
                self.assertNotIn(TOKEN.encode(), path.read_bytes(), path)

    def test_a_cached_program_needs_no_token_and_no_network(self):
        cache = self.cached()
        cache.parent.mkdir(parents=True)
        cache.write_bytes(self.server.elf)
        cache.chmod(0o755)
        done = self.run_launcher('cached', T_TOKEN=None)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, 'cached\n')
        self.assertEqual(self.server.requests, [])

    def test_a_login_page_is_refused_and_not_cached(self):
        self.server.mode = 'login'
        done = self.run_launcher('--help', T_TOKEN=TOKEN)
        self.assertEqual(done.returncode, 1)
        self.assertIn('Bearer', done.stderr)
        self.assertIn('/download/t', done.stderr)
        self.assertFalse(self.cached().exists())
        self.assertEqual(list((self.home / '.cache' / 't').glob('.t.*')), [])

    def test_a_failed_download_says_so_and_caches_nothing(self):
        self.server.mode = 'broken'
        done = self.run_launcher('--help', T_TOKEN=TOKEN)
        self.assertEqual(done.returncode, 1)
        self.assertIn('could not download', done.stderr)
        self.assertFalse(self.cached().exists())


if __name__ == '__main__':
    unittest.main()
