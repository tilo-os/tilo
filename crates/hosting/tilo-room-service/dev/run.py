#!/usr/bin/env python3
"""Run the Tilo browser stack locally, the way production runs it.

Starts the room service in persistent mode (which spawns its child
`tilo-relay`) and serves the desktop bundle as the web entry, laid out by the
web entry image's own `layout.py`, with the `entry.json` that opens the
hosted room as the home workspace. Ctrl-C stops everything it started. Run
`run.py --help` for what it starts, its options and the edit loop.
"""

import argparse
import datetime
import http.server
import json
import os
import pathlib
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[4]
RUNTIME = REPO / 'crates/browser/tilo-browser-runtime'
# The room service's local relay listens this far above the port base
# (LOCAL_PLAIN_OFFSET in src/config.rs).
LOCAL_PLAIN_OFFSET = 64

HELP = """\
What it starts (each piece is the production one; only TLS and origins differ):
  room service  tilo-room-service in persistent mode (TILO_ROOMS_PERSISTENT),
                configured through the TILO_ROOMS_* variables the image uses,
                with TILO_ROOMS_LOCAL=1: the room's relay serves browsers on
                http://localhost:<port-base + 64> and the API on
                http://localhost:<api-port>. It spawns its child tilo-relay
                (built with --features agents, as the image) and restarts it
                if it exits. The owner root is generated once into
                <scratch>/state/room-root (TILO_ROOMS_ROOT_FILE) and sealed
                secrets live in <scratch>/state/secrets (TILO_SECRETS_DIR, as
                in the image), so the room survives restarts until --fresh.
  binaries      <scratch>/bin holds tilo-relay, tilo-room-service and the
                product fonts beside them (tools/font-catalog.py prepare), the
                image's /usr/local/bin layout.
  web entry     the desktop bundle laid out by
                crates/browser/tilo-browser-runtime/container/layout.py into
                <scratch>/site and served on http://localhost:<web-port> with
                the routes and cache headers of container/Caddyfile.
                entry.json has the shape container/entrypoint.sh writes, with
                plain local origins:
                  {"home": {"name", "host": {"origin": <relay>},
                            "admission": <api>/[, "app"]}[, "about"]}
                with "about" from TILO_WEB_ABOUT_NOTIFY, _SOURCE and
                _COMMUNITY, as the image writes it.

Without the browser (--no-bundle):
  Serves only entry.json (and /healthz) on the web entry: a site for Tilo
  computers, which join its home workspace through the door as they join
  tilo.coffee's (dist/dev/nested-pair.sh runs two nested sessions on one). No
  bundle is built and the web toolchain is not needed. --ready-file FILE
  writes {"site", "relay", "api", "room"} as JSON once the room answers.

Building:
  Without --relay-bin/--room-service-bin it builds both release binaries with
  `$CARGO build` (else `cargo build`; see --cargo, which may name any cargo
  drop-in, such as one that builds remotely) and the image workflow's
  flags. Without --bundle it runs
  crates/browser/tilo-browser-runtime/scripts/build.sh <scratch>/bundles/<time>
  --bundle desktop, which needs a git checkout, cargo and the web toolchain
  (wasm32 target, wasm-bindgen 0.2.108, wasm-opt, node and npm), as in
  dist/dev/Containerfile.fedora44 or CI's builder image. `--bundle-profile dev`
  skips wasm-opt and is faster to build but slower in the browser.
  Every run prints the paths it used, so the next run can reuse them.

Rebuild only the bundle, then reload:
  build.sh DIR --bundle desktop --profile quick --update [--no-split]
  `--update` reuses DIR (new, empty, or an earlier quick desktop bundle)
  and redoes only what changed: seconds for page scripts, about a minute
  for Rust. `--no-split` leaves the module whole when you are not testing
  lazy app loading. A quick bundle also builds from a tree without git
  metadata, such as a synced copy. Ctrl-C, then rerun with --bundle DIR
  and the binaries printed at start:
    run.py --bundle DIR --relay-bin <scratch>/bin/tilo-relay \\
           --room-service-bin <scratch>/bin/tilo-room-service
  The room keeps its state, so reloading the page continues where it was.
  Only after changing Rust in the relay or the service rebuild the binaries
  (rerun without --relay-bin/--room-service-bin).

Headless Chromium (tools/web/chrome.mjs: $CHROME, else Playwright's
chromium; every launch() is a fresh profile, so a fresh person). Headless
Chromium draws WebGPU only through SwiftShader, so pass its flags; the first
load compiles the bundle and takes a minute or two there:
  node --input-type=module -e "
    import {launch} from './tools/web/chrome.mjs';
    const browser = await launch({args: ['--enable-features=Vulkan',
      '--use-vulkan=swiftshader', '--use-angle=swiftshader']});
    const tab = await browser.tab();
    await tab.navigate('<a URL run.py prints>');
    await tab.waitFor('globalThis.tiloMount', {timeout: 240000});
    (await import('node:fs')).writeFileSync('shot.png', await tab.screenshot());
    console.log(tab.log.join('\\n'));
    await browser.close();"
  `tab.reload()` loads the same profile again. A person who joins is a guest:
  the service admits 5 per address per 10 minutes, so restart run.py (which
  keeps the room) to test more fresh profiles.

Other TILO_ROOMS_* and TILO_RELAY_* variables in the environment reach the
service unchanged (for example TILO_ROOMS_SEED_APPS, TILO_ROOMS_CHAT_CHANNELS,
TILO_ROOMS_STRIPS, TILO_RELAY_LOG); the ones above are set by this script.
"""


def log(message):
    print(f'run.py: {message}', file=sys.stderr, flush=True)


def run(command, **kwargs):
    log('$ ' + ' '.join(str(part) for part in command))
    subprocess.run(command, check=True, **kwargs)


def parse_args():
    parser = argparse.ArgumentParser(
        prog='run.py',
        description='Run the room service in persistent mode and its web entry locally, as production does.',
        epilog=HELP, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--scratch', type=pathlib.Path, default=REPO / 'target/room-service-dev',
                        help='where binaries, state, bundles and the site go (default: target/room-service-dev)')
    parser.add_argument('--name', default='local',
                        help='the persistent room (TILO_ROOMS_PERSISTENT; default: local)')
    parser.add_argument('--shown-name', help='the name people see (TILO_ROOMS_NAME; default: the name, capitalized)')
    parser.add_argument('--home-app', help='the app in front of the home workspace (entry.json home.app), e.g. so.tilo.notes')
    parser.add_argument('--fresh', action='store_true', help='forget the room: remove its state and root first')
    parser.add_argument('--relay-bin', type=pathlib.Path, help='a prebuilt tilo-relay (built with --features agents)')
    parser.add_argument('--room-service-bin', type=pathlib.Path, help='a prebuilt tilo-room-service')
    parser.add_argument('--bundle', type=pathlib.Path, help='a prebuilt desktop bundle (build.sh DIR --bundle desktop)')
    parser.add_argument('--no-bundle', action='store_true',
                        help='no browser bundle: the web entry serves only entry.json (see "Without the browser")')
    parser.add_argument('--ready-file', type=pathlib.Path,
                        help='write the site, relay and API origins and the room as JSON here once the room answers')
    parser.add_argument('--bundle-profile', choices=['release', 'dev'], default='release',
                        help='build.sh --profile when building the bundle (default: release)')
    parser.add_argument('--cargo', default=os.environ.get('CARGO', 'cargo'),
                        help='what builds the binaries: cargo or any drop-in for it (default: $CARGO, else cargo)')
    parser.add_argument('--web-port', type=int, default=8080, help='the web entry (default: 8080)')
    parser.add_argument('--api-port', type=int, default=8790, help='the room API (TILO_ROOMS_API; default: 8790)')
    parser.add_argument('--port-base', type=int, default=9700,
                        help='TILO_ROOMS_PORT_BASE: the relay listens on it (TLS) and on it + 64 (plain; default: 9700)')
    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    if args.no_bundle and args.bundle is not None:
        parser.error('--bundle and --no-bundle exclude each other')
    for name in ('relay_bin', 'room_service_bin', 'bundle'):
        path = getattr(args, name)
        if path is not None:
            path = path.resolve()
            if not path.exists():
                parser.error(f'--{name.replace("_", "-")}: {path} does not exist')
            setattr(args, name, path)
    return args


def target_dir():
    return pathlib.Path(os.environ.get('CARGO_TARGET_DIR', REPO / 'target'))


def build_binaries(args):
    """The release binaries the image ships, built as its workflow builds them."""
    env = dict(os.environ, CARGO_PROFILE_RELEASE_LTO='thin', CARGO_PROFILE_RELEASE_CODEGEN_UNITS='16')
    if args.relay_bin is None:
        run([args.cargo, 'build', '--locked', '--release', '-p', 'tilo-relay', '--features', 'agents',
             '--bin', 'tilo-relay'], cwd=REPO, env=env)
        args.relay_bin = target_dir() / 'release/tilo-relay'
    if args.room_service_bin is None:
        run([args.cargo, 'build', '--locked', '--release', '-p', 'tilo-room-service'], cwd=REPO, env=env)
        args.room_service_bin = target_dir() / 'release/tilo-room-service'


def install(source, dest):
    """Copy `source` to `dest` unless it is already that file."""
    if source == dest:
        return
    if dest.exists():
        a, b = source.stat(), dest.stat()
        if a.st_size == b.st_size and a.st_mtime_ns == b.st_mtime_ns:
            return
        dest.unlink()
    shutil.copy2(source, dest)


def assemble_bin(args):
    """<scratch>/bin: the binaries and, beside them, the product fonts."""
    bin_dir = args.scratch / 'bin'
    bin_dir.mkdir(parents=True, exist_ok=True)
    install(args.relay_bin, bin_dir / 'tilo-relay')
    install(args.room_service_bin, bin_dir / 'tilo-room-service')
    source = REPO / 'crates/sdk/ui-core/assets/fonts'
    fonts = bin_dir / 'fonts'
    manifest = fonts / 'manifest.toml'
    if not manifest.is_file() or manifest.read_bytes() != (source / 'manifest.toml').read_bytes():
        shutil.rmtree(fonts, ignore_errors=True)
        run([sys.executable, '-I', REPO / 'tools/font-catalog.py', 'prepare', source, fonts, '--scope', 'product'],
            env=dict(os.environ, TILO_FONT_ASSETS_OFFLINE='0'))
    return bin_dir


def build_bundle(args):
    if args.bundle is not None:
        return args.bundle
    missing = [tool for tool in ('cargo', 'wasm-bindgen', 'node', 'npm') if not shutil.which(tool)]
    if missing:
        sys.exit(f'run.py: building the bundle needs {", ".join(missing)} on PATH; '
                 'pass a prebuilt one with --bundle DIR (see --help)')
    out = args.scratch / 'bundles' / datetime.datetime.now().strftime('%Y%m%d-%H%M%S')
    out.parent.mkdir(parents=True, exist_ok=True)
    # As the image workflows: build.sh reads the workspace metadata offline.
    run(['cargo', 'fetch', '--locked'], cwd=REPO)
    run([RUNTIME / 'scripts/build.sh', out, '--bundle', 'desktop', '--profile', args.bundle_profile], cwd=REPO)
    return out


def lay_out(bundle, site):
    """The web entry image's site: layout.py over the bundle."""
    if site.exists():
        shutil.rmtree(site)
    output = subprocess.run([sys.executable, '-I', RUNTIME / 'container/layout.py', bundle, site],
                            check=True, capture_output=True, text=True)
    return output.stdout.strip()


def entry_json(args, relay_origin, api_origin):
    """What container/entrypoint.sh writes for a home workspace, with the
    local origins: the relay's plain listener and the API, which an ingress
    joins under one origin in production."""
    home = {'name': args.shown_name or args.name.capitalize(), 'host': {'origin': relay_origin},
            'admission': f'{api_origin}/'}
    if args.home_app:
        home['app'] = args.home_app
    entry = {'home': home}
    # The About sheet's links, from the same variables the image reads.
    about = {key: url for key in ('notify', 'source', 'community')
             if (url := os.environ.get(f'TILO_WEB_ABOUT_{key.upper()}', ''))}
    if about:
        entry['about'] = about
    return entry


class Site(http.server.SimpleHTTPRequestHandler):
    """The routes of container/Caddyfile: /healthz, /entry.json from the
    configuration directory, /v/ immutable, everything else revalidated."""

    site: pathlib.Path
    entry: pathlib.Path
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      '.mjs': 'text/javascript', '.js': 'text/javascript', '.wasm': 'application/wasm',
                      '.json': 'application/json'}

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(self.site), **kwargs)

    def translate_path(self, path):
        if urllib.parse.urlsplit(path).path == '/entry.json':
            return str(self.entry)
        return super().translate_path(path)

    def do_GET(self):
        if urllib.parse.urlsplit(self.path).path == '/healthz':
            body = b'ok'
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def end_headers(self):
        immutable = urllib.parse.urlsplit(self.path).path.startswith('/v/')
        self.send_header('Cache-Control', 'public, max-age=31536000, immutable' if immutable else 'no-cache')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Referrer-Policy', 'no-referrer')
        super().end_headers()

    def list_directory(self, path):
        # Caddy's file_server does not list directories either.
        self.send_error(404)

    def log_request(self, code='-', size='-'):
        if str(code)[:1] in '45':
            super().log_request(code, size)


def serve(args, site, entry):
    handler = type('Handler', (Site,), {'site': site, 'entry': entry})
    server = http.server.ThreadingHTTPServer(('127.0.0.1', args.web_port), handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def free(port):
    with socket.socket() as probe:
        # As the listeners themselves: connections of a previous run still
        # in TIME_WAIT do not hold the port.
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            probe.bind(('127.0.0.1', port))
        except OSError:
            return False
    return True


def answers(url):
    try:
        with urllib.request.urlopen(url, timeout=2):
            return True
    except urllib.error.HTTPError:
        return True
    except OSError:
        return False


def main():
    args = parse_args()
    started = time.monotonic()
    relay_port = args.port_base + LOCAL_PLAIN_OFFSET
    for port in (args.web_port, args.api_port, args.port_base, relay_port):
        if not free(port):
            sys.exit(f'run.py: port {port} is in use (another run.py? see --web-port, --api-port, --port-base)')
    build_binaries(args)
    bin_dir = assemble_bin(args)
    site = args.scratch / 'site'
    if args.no_bundle:
        # Nothing but entry.json: the service's /app/ serves an empty
        # directory, and the site's own routes answer 404 past entry.json.
        bundle = args.scratch / 'no-bundle'
        bundle.mkdir(parents=True, exist_ok=True)
        if site.exists():
            shutil.rmtree(site)
        site.mkdir(parents=True)
        version = 'none'
    else:
        bundle = build_bundle(args)
        version = lay_out(bundle, site)

    state = args.scratch / 'state'
    if args.fresh:
        shutil.rmtree(state, ignore_errors=True)
    rooms = state / 'rooms'
    rooms.mkdir(parents=True, exist_ok=True)
    root = state / 'room-root'
    if not root.exists():
        root.touch(mode=0o600)
        root.write_text(secrets.token_hex(32))

    web_origin = f'http://localhost:{args.web_port}'
    api_origin = f'http://localhost:{args.api_port}'
    relay_origin = f'http://localhost:{relay_port}'
    entry = args.scratch / 'entry/entry.json'
    entry.parent.mkdir(parents=True, exist_ok=True)
    entry.write_text(json.dumps(entry_json(args, relay_origin, api_origin)) + '\n')

    env = dict(os.environ)
    env.update({
        'TILO_ROOMS_PERSISTENT': args.name,
        'TILO_ROOMS_LOCAL': '1',
        'TILO_ROOMS_API': f'127.0.0.1:{args.api_port}',
        'TILO_ROOMS_PUBLIC_API': api_origin,
        'TILO_ROOMS_SITE_ORIGINS': web_origin,
        'TILO_ROOMS_WEB_ENTRY': f'{web_origin}/',
        'TILO_ROOMS_PORT_BASE': str(args.port_base),
        'TILO_ROOMS_ROOT_FILE': str(root),
        'TILO_ROOMS_STATE': str(rooms),
        'TILO_ROOMS_ASSETS': str(bundle),
        'TILO_ROOMS_RELAY_BIN': str(bin_dir / 'tilo-relay'),
        'TILO_SECRETS_DIR': str(state / 'secrets'),
    })
    if args.shown_name:
        env['TILO_ROOMS_NAME'] = args.shown_name

    server = serve(args, site, entry)
    service = subprocess.Popen([bin_dir / 'tilo-room-service'], env=env, start_new_session=True)

    def stop(*_):
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        log('stopping')
        server.shutdown()
        if service.poll() is None:
            # The service closes its relay's store cleanly on SIGTERM.
            service.terminate()
            try:
                service.wait(timeout=15)
            except subprocess.TimeoutExpired:
                pass
        try:
            # Anything of the service's session left, the relay included.
            os.killpg(service.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        sys.exit(0)

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)

    room_id = rooms / args.name / 'room-id'
    deadline = time.monotonic() + 120
    while not (room_id.is_file() and answers(f'{api_origin}/healthz') and answers(f'{relay_origin}/')):
        if service.poll() is not None:
            log(f'tilo-room-service exited ({service.returncode})')
            stop()
        if time.monotonic() > deadline:
            log('the room did not come up within 2 minutes')
            stop()
        time.sleep(0.2)
    room = room_id.read_text().strip()
    host = urllib.parse.urlsplit(relay_origin).netloc
    if args.ready_file is not None:
        ready = json.dumps({'site': f'{web_origin}/', 'relay': relay_origin, 'api': api_origin, 'room': room})
        partial = args.ready_file.with_name(args.ready_file.name + '.partial')
        partial.write_text(ready + '\n')
        partial.replace(args.ready_file)
    print(f"""
Tilo is up in {time.monotonic() - started:.1f}s, room {args.name!r} ({room}), bundle {version}

  open        {web_origin}/
  Notes       {web_origin}/#/@{host}/w/{room}/so.tilo.notes   (on the desktop)
  full tab    {web_origin}/app/#/@{host}/w/{room}/so.tilo.notes
  room API    {api_origin}   relay {relay_origin}
  reuse       --bundle {bundle} --relay-bin {bin_dir / 'tilo-relay'} --room-service-bin {bin_dir / 'tilo-room-service'}
  state       {state}   (--fresh forgets it)

Ctrl-C stops everything.
""", flush=True)
    while service.poll() is None:
        time.sleep(0.5)
    log(f'tilo-room-service exited ({service.returncode})')
    stop()


if __name__ == '__main__':
    main()
