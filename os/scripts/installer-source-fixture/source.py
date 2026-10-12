#!/usr/bin/env python3
"""Normal source Imager, real PAM login and owned physical Computer source.

Called only by the admitted root native qualifier. Observations passed to the
receiver are descriptive; native authenticate-install independently admits the
ciphertext, person, protected source declarations and current physical source.
"""
import argparse
import base64
from contextlib import contextmanager
import ctypes
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import ssl
import shutil
import stat
import subprocess
import sys
import threading
import time
import urllib.parse

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from bounded_native import bounded, observe_exit, retire_child, assert_children_retired
from owned_gpt import owned_source

SOURCE_USER = 'tilo-source'
SOURCE_PASSWORD = b'Owned source PAM fixture only'
PASSWORD = b'Owned receiving identity fixture only'
SELECTED_BYTES = b'genuine selected application state'
DOCUMENT_BYTES = b'genuine retained file for normal Writer editing\n'
DAV_BYTES = b'genuine selected remote account document\n'
TLS_SERVER_NAME = 'tilo-owned-dav.test'
_OBLIGATIONS = []


def require(value, message):
    if not value:
        raise RuntimeError(message)


def unique(pairs):
    value = {}
    for key, entry in pairs:
        require(key not in value, 'duplicate private source observation')
        value[key] = entry
    return value


def fixed(argv, *, input=None, deadline=None, pass_fds=()):
    descriptor = None
    try:
        if input is not None:
            require(isinstance(input, bytes) and len(input) <= 81920, 'private source input bound')
            descriptor = os.memfd_create('source-private-input', os.MFD_CLOEXEC)
            view = memoryview(input)
            while view:
                view = view[os.write(descriptor, view):]
            os.lseek(descriptor, 0, os.SEEK_SET)
        timeout = min(120, (deadline - time.monotonic()) if deadline is not None else 120)
        require(timeout > 0, 'source operation deadline elapsed')
        code, output, _ = bounded(argv, timeout=timeout, stdin=descriptor, pass_fds=pass_fds)
        require(code == 0, 'fixed native source operation refused')
        return output
    finally:
        if descriptor is not None:
            os.ftruncate(descriptor, 0)
            os.close(descriptor)


def private(path, uid=0):
    path = Path(path)
    path.mkdir(mode=0o700, parents=True, exist_ok=False)
    os.chown(path, uid, uid)
    return path


def write_new(path, value, uid=0, mode=0o600):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, mode)
    try:
        os.fchown(descriptor, uid, uid)
        view = memoryview(value)
        while view:
            view = view[os.write(descriptor, view):]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_uid, info.st_gid,
            info.st_mode, info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def held_bytes(path, maximum):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(descriptor)
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1
                and 0 < before.st_size <= maximum, 'source observation file custody/bound')
        data = bytearray()
        while part := os.read(descriptor, min(65536, maximum + 1 - len(data))):
            data.extend(part)
            require(len(data) <= maximum, 'source observation grew beyond bound')
        require(stamp(before) == stamp(os.fstat(descriptor)) == stamp(Path(path).lstat()),
                'source held observation was replaced or changed')
        return bytes(data)
    finally:
        os.close(descriptor)


class Dav:
    """Actual loopback WebDAV, authenticated through the canonical rclone importer."""
    service_name = 'fixture-dav'
    expected = {'Documents/selected-document.txt': DAV_BYTES}
    account_ids = ()

    def __init__(self, directory):
        require(os.geteuid() == 0, 'fixture TLS material requires owned root namespace')
        directory = private(directory)
        self.public_certificate = directory / 'public-ca.pem'
        self.private_key = directory / 'private-key.pem'
        fixed(['/usr/bin/openssl', 'req', '-x509', '-newkey', 'ed25519', '-nodes',
               '-days', '1', '-subj', '/CN=Owned loopback DAV fixture',
               '-addext', 'subjectAltName=DNS:' + TLS_SERVER_NAME,
               '-addext', 'basicConstraints=critical,CA:TRUE',
               '-keyout', str(self.private_key), '-out', str(self.public_certificate)])
        self.public_certificate.chmod(0o444)
        self.private_key.chmod(0o400)
        self.gets = 0
        self.delivered_ranges = []
        self.uploads = {}
        self.lock = threading.Lock()
        owner = self
        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def setup(self):
                super().setup()
                self.connection.settimeout(5)
            def log_message(self, *args):
                pass
            def reply(self, code, body=b'', kind='application/octet-stream'):
                self.send_response(code)
                self.send_header('Content-Length', str(len(body)))
                self.send_header('Content-Type', kind)
                self.send_header('ETag', '"source-fixture-1"')
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(body)
            def authenticated(self):
                if self.headers.get('Authorization') != 'Basic ' + base64.b64encode(b'ada:correct horse').decode():
                    self.send_response(401)
                    self.send_header('WWW-Authenticate', 'Basic realm="owned fixture"')
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                    return False
                return True
            def do_PROPFIND(self):
                if not self.authenticated():
                    return
                require(int(self.headers.get('Content-Length', '0')) <= 65536, 'DAV request body bound')
                self.rfile.read(int(self.headers.get('Content-Length', '0')))
                path = urllib.parse.urlsplit(self.path).path
                if path not in ('/dav', '/dav/', '/dav/Documents/', '/dav/Documents/selected-document.txt'):
                    self.reply(404)
                    return
                def row(name, folder, length):
                    resource = '<d:collection/>' if folder else ''
                    return ('<d:response><d:href>' + name + '</d:href><d:propstat><d:prop>'
                            '<d:resourcetype>' + resource + '</d:resourcetype>'
                            '<d:getcontentlength>' + str(length) + '</d:getcontentlength>'
                            '<d:getetag>"source-fixture-1"</d:getetag>'
                            '<d:getlastmodified>Sun, 11 Oct 2026 00:00:00 GMT</d:getlastmodified>'
                            '</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>')
                folder = path in ('/dav', '/dav/', '/dav/Documents/')
                normalized = '/dav/' if path == '/dav' else path
                rows = row(normalized, folder, 0 if folder else len(DAV_BYTES))
                if folder and self.headers.get('Depth') != '0':
                    rows += row('/dav/Documents/', True, 0) if normalized == '/dav/' else row('/dav/Documents/selected-document.txt', False, len(DAV_BYTES))
                self.reply(207, ('<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">' + rows + '</d:multistatus>').encode(), 'application/xml')
            def do_GET(self):
                if not self.authenticated():
                    return
                path = urllib.parse.urlsplit(self.path).path
                if path != '/dav/Documents/selected-document.txt':
                    self.reply(404)
                    return
                interval = self.headers.get('Range')
                if interval:
                    require(interval.startswith('bytes=') and ',' not in interval, 'actual DAV range syntax')
                    first, last = interval[6:].split('-', 1)
                    first = int(first)
                    last = min(int(last) if last else len(DAV_BYTES) - 1, len(DAV_BYTES) - 1)
                    require(0 <= first <= last < len(DAV_BYTES), 'actual DAV range extent')
                    body = DAV_BYTES[first:last + 1]
                    self.send_response(206)
                    self.send_header('Content-Length', str(len(body)))
                    self.send_header('Content-Range', f'bytes {first}-{last}/{len(DAV_BYTES)}')
                    self.send_header('ETag', '"source-fixture-1"')
                    self.end_headers()
                    if self.command != 'HEAD':
                        self.wfile.write(body)
                    delivered = (first, last + 1)
                else:
                    self.reply(200, DAV_BYTES)
                    delivered = (0, len(DAV_BYTES))
                if self.command == 'GET':
                    # HEAD and failed/truncated writes cannot establish a
                    # receiving read. Record only successfully served bodies.
                    with owner.lock:
                        owner.gets += 1
                        owner.delivered_ranges.append(delivered)
            def do_PUT(self):
                if not self.authenticated():
                    return
                path = urllib.parse.urlsplit(self.path).path
                if path != '/dav/Documents/written-document.txt':
                    self.reply(403)
                    return
                size = int(self.headers.get('Content-Length', '-1'))
                require(0 <= size <= 1048576, 'actual DAV upload bound')
                value = self.rfile.read(size)
                require(len(value) == size, 'actual DAV upload was truncated')
                with owner.lock:
                    owner.uploads[path] = value
                self.reply(201)
            do_HEAD = do_GET
            def do_OPTIONS(self):
                if self.authenticated():
                    self.reply(200)
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = False
        self.server.timeout = 1
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(str(self.public_certificate), str(self.private_key))
        self.server.socket = tls.wrap_socket(self.server.socket, server_side=True)
        self.server.socket.settimeout(5)
        self.url = 'https://' + TLS_SERVER_NAME + ':' + str(self.server.server_port) + '/dav'
        self.thread = threading.Thread(target=self.server.serve_forever, name='owned-source-dav')
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        require(not self.thread.is_alive(), 'actual source DAV retirement unknown')


def load_semantics():
    path = HERE.parent / 'installer-receive-fixture/semantic.py'
    spec = importlib.util.spec_from_file_location('source_canonical_semantics', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Main:
    def __init__(self, candidate, home, runtime, deadline):
        require(os.getuid() == 1000 and os.geteuid() == 1000, 'source main requires actual logged-in UID1000')
        self.deadline = deadline
        self.child = None
        self.agent = None
        self.logs = []
        _OBLIGATIONS.append(self)
        executable = candidate / 'host/bin/tilo-imager'
        subject = json.loads(held_bytes(candidate / 'subjects.json', 65536), object_pairs_hook=unique)['host']
        require(hashlib.sha256(held_bytes(executable, 512 << 20)).hexdigest() == subject['sha256'],
                'actual main differs from measured candidate host role')
        self.socket = runtime / 'imager.sock'
        environment = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'LANG': 'C', 'HOME': str(home),
                       'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_DATA_HOME': str(home / '.local/share'),
                       'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_STATE_HOME': str(home / '.local/state'),
                       'XDG_RUNTIME_DIR': str(runtime), 'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(runtime / 'bus')}
        try:
            self.logs = [(runtime / name).open('xb') for name in ('main.stdout', 'main.stderr')]
            self.child = subprocess.Popen([str(executable), '--installer', '--headless-drive',
                json.dumps({'viewport': [1180, 820], 'serve': str(self.socket)})], env=environment,
                stdout=self.logs[0], stderr=self.logs[1], start_new_session=True)
            while not self.socket.exists():
                self.alive()
                time.sleep(.025)
            info = self.socket.lstat()
            require(stat.S_ISSOCK(info.st_mode) and info.st_uid == 1000 and not info.st_mode & 0o077,
                    'source console socket ownership differs')
            self.socket_stamp = (info.st_dev, info.st_ino)
            # The real process agent performs normal Polkit PAM. No policy
            # authorization result is synthesized by this source observer.
            fields = Path('/proc/' + str(self.child.pid) + '/stat').read_text().rsplit(') ', 1)[1].split()
            process = str(self.child.pid) + ',' + fields[19]
            notified, notifier = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
            try:
                self.agent = subprocess.Popen(['/usr/bin/pkttyagent', '--process', process,
                    '--notify-fd', str(notifier)], env=environment,
                    stdin=sys.stdin, stdout=sys.stdout, stderr=sys.stderr,
                    process_group=0, pass_fds=(notifier,))
                os.close(notifier)
                notifier = None
                signal.signal(signal.SIGTTOU, signal.SIG_IGN)
                os.tcsetpgrp(sys.stdin.fileno(), self.agent.pid)
                # pkttyagent's actual registration closes its notify FD. EOF
                # alone cannot qualify an exited/refused agent.
                while not select.select([notified], [], [], .025)[0]:
                    self.alive()
                    require(observe_exit(self.agent) is None, 'actual source agent registration refused')
                require(os.read(notified, 1) == b'' and observe_exit(self.agent) is None,
                        'actual source agent readiness absent')
                self.alive()
            finally:
                os.close(notified)
                if notifier is not None:
                    os.close(notifier)
            self.control = load_semantics().Control(self.socket, deadline, self.logs[1])
        except BaseException:
            self.close(False)
            raise

    def alive(self):
        require(time.monotonic() < self.deadline and observe_exit(self.child) is None,
                'actual source main stopped or exceeded deadline')
        require(all(os.fstat(log.fileno()).st_size <= 16 << 20 for log in self.logs), 'source main diagnostics exceeded bound')

    def invoke(self, name, args=None):
        self.alive()
        rows = [row for row in self.control.rows() if row.get('provider') == 'so.tilo.imager'
                and row.get('id', '').split('@', 1)[0] == name]
        require(len(rows) == 1, 'normal source action was not uniquely offered: ' + name)
        response = self.control.invoke(rows[0], args or {})
        answer = response.get('answer')
        if isinstance(answer, dict):
            require(answer.get('state') == 'completed', 'normal source action refused: ' + name)
            return answer.get('output')
        update = response.get('receipt', {}).get('update', {})
        require(update.get('state') == 'succeeded', 'normal source action refused: ' + name)
        return update.get('output', {}).get('value')

    def read(self):
        value = self.invoke('read-preview')
        require(isinstance(value, dict), 'normal source inspection has no state')
        return value

    def wait(self, predicate):
        while True:
            value = self.read()
            require(value.get('media_preparation', {}).get('state') not in ('failed', 'source-unavailable', 'source-changed'),
                    'normal source preparation refused')
            if predicate(value):
                return value
            time.sleep(.025)

    def choose(self, value):
        self.invoke('choose-preview', {'choice': value})
        self.invoke('continue-preview')

    def close(self, success):
        if self.child is None:
            return
        if success:
            # Same canonical console quit; actual zero exit is required before
            # source epoch retirement. Forced cleanup can never qualify it.
            client_path = candidate_control_path()
            spec = importlib.util.spec_from_file_location('source_existing_control_client', client_path)
            client = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(client)
            reply = client.call(str(self.socket), {'op': 'quit'}, timeout=5)
            require('ok' in reply, 'normal source console did not accept quit')
            while observe_exit(self.child) is None:
                require(time.monotonic() < self.deadline, 'normal source main retirement unknown')
                time.sleep(.025)
            observed = observe_exit(self.child)
            require(observed.si_code == os.CLD_EXITED and observed.si_status == 0,
                    'normal source main did not close successfully')
        retire_child(self.child)
        self.child = None
        if self.agent is not None:
            retire_child(self.agent)
            self.agent = None
        for log in self.logs:
            log.close()
        self.logs = []
        os.tcsetpgrp(sys.stdin.fileno(), os.getpgrp())


def candidate_control_path():
    return HERE.parents[2] / 'scripts/headless-examples/_control.py'


def user_stage(candidate, config_path):
    config = json.loads(held_bytes(config_path, 65536), object_pairs_hook=unique)
    require(os.getuid() == os.geteuid() == 1000, 'source stage requires genuine PAM account')
    home = Path(os.environ['HOME'])
    runtime = Path('/run/user/1000')
    deadline = time.monotonic() + 1200
    from flatpak_material import install
    install(home, deadline)
    from source_sessions import generate
    generate(home, runtime, deadline)
    write_new(home / 'receiver-document.txt', DOCUMENT_BYTES, 1000)
    private(home / '.config/rclone', 1000)
    write_new(home / '.config/rclone/rclone.conf',
        ('[fixture-dav]\ntype = webdav\nurl = ' + config['dav_url'] + '\nvendor = other\nuser = ada\n'
         'pass = AAECAwQFBgcICQoLDA0OD2LwFVdisTSqpU56FmE\n').encode(), 1000)
    main = Main(candidate, home, runtime, deadline)
    try:
        main.wait(lambda state: state['restart']['state'] == 'missing')
        for value in ('computer', 'migrate', 'new'):
            main.choose(value)
        main.invoke('set-preview-name', {'value': 'Genuine Qualified Person'})
        main.invoke('continue-preview')
        main.invoke('set-preview-password', {'value': PASSWORD.decode()})
        main.invoke('set-preview-confirmation', {'value': PASSWORD.decode()})
        main.invoke('continue-preview')
        main.wait(lambda state: state['page'] == 'devices' and state['prepared_identity']['state'] == 'prepared')
        main.invoke('continue-preview')
        main.invoke('check-belongings')
        discovered = main.wait(lambda state: not state['belongings_checking'])
        sources = discovered['belongings']['website_sessions']['sources']
        for service in ('slack', 'discord'):
            offered = [row for row in sources if row['service'] == service and row['selectable']]
            require(len(offered) == 1, 'genuine native service source not uniquely offered')
            main.invoke('set-preview-service-session', {'source_id': offered[0]['id'],
                        'service': service, 'included': True})
        main.invoke('select-migration-home', {'source': str(home)})
        main.wait(lambda state: state['page'] == 'accounts')
        main.invoke('account-step', {'step': 'review-saved', 'selection': None})
        main.invoke('account-step', {'step': 'import-saved', 'selection': None})
        main.wait(lambda state: state['accounts']['prepared_accounts'] == 1 and state['accounts']['can_continue'])
        main.invoke('continue-preview')
        main.wait(lambda state: state['belongings_preparation']['prepared'] and state['page'] == 'review')
        main.invoke('review-prepared-belongings', {'target_gib': 35})
        ready = main.wait(lambda state: not state['belongings_preparation']['pending']
                          and state['media_preparation']['artifact'].get('pending_computer_applications_scope') is not None)
        artifact = ready['media_preparation']['artifact']
        scope = artifact['pending_computer_applications_scope']
        require(scope['source_operation'] == artifact['source_operation']
                and len(artifact['account_ids']) == 1, 'actual prepared source scope/account differs')
        package = Path(config['export']) / 'prepared-setup.sealed'
        main.invoke('export-prepared-belongings', {'destination': str(package)})
        exported = main.wait(lambda state: isinstance(state['media_preparation']['exported'], dict))
        info = exported['media_preparation']['exported']
        actual = held_bytes(package, 32 * 1024 * 1024 + 16384)
        require(info['sealed_bytes'] == len(actual), 'actual export differs from authenticated PackageInfo')
        receipt = {'schema': 1, 'sealed_bytes': len(actual), 'ciphertext_sha256': hashlib.sha256(actual).hexdigest()}
        write_new(package.parent / 'receipt.json', json.dumps(receipt, separators=(',', ':')).encode(), 1000)
        result = {'source_operation': scope['source_operation'], 'apps_stage': scope['staging_operation'],
                  'person': artifact['public_key'], 'account_ids': artifact['account_ids'], 'receipt': receipt,
                  'setup_binding': artifact['setup_binding'],
                  'selected_service_kinds': ['slack', 'discord']}
        main.close(True)
        return result
    except BaseException:
        main.close(False)
        raise


def copy_public_candidate(candidate, public):
    """A readable RO copy of measured public artifacts, never provider secrets."""
    private(public)
    total = 0
    count = 0
    for parent, directories, files in os.walk(candidate, followlinks=False):
        current = Path(parent)
        metadata = current.lstat()
        require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == 0 and not metadata.st_mode & 0o022,
                'candidate public directory custody')
        destination = public / current.relative_to(candidate)
        destination.mkdir(mode=0o755, exist_ok=True)
        for directory in directories:
            info = (current / directory).lstat()
            require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022,
                    'candidate redirected public directory')
        for filename in files:
            path = current / filename
            info = path.lstat()
            count += 1
            total += info.st_size
            require(count <= 65536 and total <= 2 << 30 and stat.S_ISREG(info.st_mode)
                    and info.st_uid == 0 and not info.st_mode & 0o022,
                    'candidate public artifact tree exceeds custody/bounds')
            raw = held_bytes(path, 512 << 20)
            target = destination / filename
            write_new(target, raw, mode=0o555 if info.st_mode & 0o111 else 0o444)
            require(hashlib.sha256(held_bytes(target, 512 << 20)).digest() == hashlib.sha256(raw).digest(),
                    'public candidate copy changed')
    for parent, _, _ in os.walk(public, topdown=False):
        Path(parent).chmod(0o555)


class Services:
    def __init__(self, root):
        self.root = root
        self.children = []
        self.logs = []
        self.stopped = False
        _OBLIGATIONS.append(self)

    def start(self, name, arguments):
        output = (self.root / (name + '.log')).open('xb')
        self.logs.append(output)
        child = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=output, stderr=output,
                                 env={'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'}, start_new_session=True)
        self.children.append(child)
        return child

    def live(self):
        require(all(observe_exit(child) is None for child in self.children), 'actual source service exited')
        require(all(os.fstat(log.fileno()).st_size <= 8 << 20 for log in self.logs), 'source service diagnostics bound')

    def stop(self, child):
        require(any(owned is child for owned in self.children), 'source service exact ownership absent')
        retire_child(child)
        self.children = [owned for owned in self.children if owned is not child]

    def close(self):
        for child in reversed(self.children):
            retire_child(child)
        self.children = []
        for output in self.logs:
            output.close()
        self.logs = []
        self.stopped = True


def install_ca(certificate):
    anchor = Path('/etc/pki/ca-trust/source/anchors/tilo-native-owned-dav.pem')
    require(not anchor.exists() and not anchor.is_symlink(), 'fixture CA anchor already exists')
    write_new(anchor, certificate, mode=0o644)
    fixed(['/usr/bin/update-ca-trust', 'extract'])
    hosts = Path('/etc/hosts')
    existing = held_bytes(hosts, 65536)
    require(TLS_SERVER_NAME.encode() not in existing, 'fixture TLS name already configured')
    with hosts.open('ab') as stream:
        stream.write(b'\n127.0.0.1 ' + TLS_SERVER_NAME.encode() + b'\n')
        stream.flush()
        os.fsync(stream.fileno())


def source_namespace(candidate, source, work, baseline_etc_fd, dav, export, service_request=None):
    require(os.geteuid() == 0, 'source namespace construction requires root')
    original_ns = Path('/proc/self/ns/mnt').stat().st_ino
    os.unshare(os.CLONE_NEWNS)
    fixed(['/usr/bin/mount', '--make-rprivate', '/'])
    require(Path('/proc/self/ns/mnt').stat().st_ino != original_ns, 'source mount namespace not independent')
    # Capture clean etc before any account exists. Running services never have
    # their backing /etc or /run replaced underneath them.
    etc = private(work / 'source-etc')
    fixed(['/usr/bin/cp', '-a', f'/proc/self/fd/{baseline_etc_fd}/.', str(etc)], pass_fds=(baseline_etc_fd,))
    fixed(['/usr/bin/mount', '--bind', str(etc), '/etc'])
    fixed(['/usr/bin/mount', '-t', 'tmpfs', '-o', 'mode=0755,nosuid,nodev', 'source-runtime', '/run'])
    public = Path('/run/tilo-source-public')
    copy_public_candidate(candidate, public)
    fixed(['/usr/bin/mount', '--bind', str(public), str(public)])
    fixed(['/usr/bin/mount', '-o', 'remount,bind,ro', str(public)])
    if service_request is not None:
        import service_contract
        with (work / 'native-service-source-admission.log').open('xb') as log:
            service_contract.install_native_scope(service_request, 'source',
                public / 'host/bin/tilo-imager', log)
    install_ca(held_bytes(dav.public_certificate, 65536))
    # /run is now source-private. Build the existing reviewed public ELF
    # closure here, rather than relying on the hidden outer runtime directory.
    preparer = Path('/usr/libexec/tilo-flatpak-runnable-runtime/prepare-runtime.py')
    from source_sessions import public as public_source_path
    public_source_path(preparer, True)
    public_source_path(Path('/usr/libexec/tilo-installer-source-fixture/fixture-app'), True)
    fixed(['/usr/bin/python3', str(preparer)])
    slack = Path('/usr/lib/slack')
    require(slack.is_dir() and not slack.is_symlink(),
            'disposable image lacks genuine canonical Slack mount endpoint')
    fixed(['/usr/bin/mount', '--bind', str(public / 'service-source/slack'), str(slack)])
    fixed(['/usr/bin/mount', '-o', 'remount,bind,ro', str(slack)])
    for path in ('/sysroot', '/var/lib/tilo-imager'):
        target = Path(path)
        if not target.exists():
            target.mkdir(mode=0o755, parents=True)
        require(target.is_dir() and not target.is_symlink(), 'source canonical mount endpoint redirected')
    fixed(['/usr/bin/mount', '--bind', str(source.mount), '/sysroot'])
    private(source.system_store.parent)
    fixed(['/usr/bin/mount', '--bind', str(source.system_store.parent), '/var/lib/tilo-imager'])
    require(Path('/var/lib/tilo-imager').stat().st_dev == source.mount.stat().st_dev,
            'source canonical record Store is not physically source GPT-backed')
    # Target uses its own Btrfs Store; this namespace never substitutes it for
    # original source records or copies original declarations into that Store.
    home = private(source.home, 1000)
    runtime = private('/run/user/1000', 1000)
    import pwd
    try:
        pwd.getpwuid(1000)
    except KeyError:
        pass
    else:
        raise RuntimeError('source image already has UID1000; no existing account is deleted')
    digest = fixed(['/usr/bin/openssl', 'passwd', '-6', '-stdin'], input=SOURCE_PASSWORD).rstrip(b'\n')
    require(digest.startswith(b'$6$') and b'\n' not in digest, 'actual source password hash refused')
    fixed(['/usr/sbin/useradd', '--uid', '1000', '--user-group', '--groups', 'wheel',
           '--home-dir', str(home), '--no-create-home', '--shell', '/bin/bash',
           '--password', digest.decode(), SOURCE_USER])
    observed = pwd.getpwnam(SOURCE_USER)
    require(observed.pw_uid == 1000 and observed.pw_dir == str(home), 'actual source NSS differs')
    config = Path('/run/source-stage.json')
    write_new(config, json.dumps({'dav_url': dav.url, 'export': str(export)}, separators=(',', ':')).encode(), mode=0o644)
    entry = public / 'os/scripts/installer-source-fixture/source.py'
    profile = ('exec /usr/bin/python3 -I ' + str(entry) + ' --logged-in-source --candidate '
               + str(public) + ' --config ' + str(config) + '\n')
    write_new(home / '.bash_profile', profile.encode(), 1000)
    services = Services(work)
    try:
        Path('/run/dbus').mkdir(mode=0o755)
        services.start('system-bus', ['/usr/bin/dbus-daemon', '--system', '--nofork', '--nopidfile'])
        deadline = time.monotonic() + 30
        while not Path('/run/dbus/system_bus_socket').exists():
            services.live()
            require(time.monotonic() < deadline, 'source system bus did not become ready')
            time.sleep(.025)
        services.start('polkit', ['/usr/lib/polkit-1/polkitd', '--no-debug'])
        services.start('udev', ['/usr/lib/systemd/systemd-udevd', '--debug'])
        for device in (source.disk, source.partition):
            fixed(['/usr/bin/udevadm', 'trigger', '--action=add', '/sys/class/block/' + device.name])
        fixed(['/usr/bin/udevadm', 'settle', '--timeout=10'])
        services.start('udisks', ['/usr/libexec/udisks2/udisksd', '--no-debug'])
        deadline = time.monotonic() + 30
        for name in ('org.freedesktop.PolicyKit1', 'org.freedesktop.UDisks2'):
            while True:
                services.live()
                require(time.monotonic() < deadline, 'source native service did not become ready')
                answer = fixed(['/usr/bin/gdbus', 'call', '--system', '--dest', 'org.freedesktop.DBus',
                    '--object-path', '/org/freedesktop/DBus', '--method',
                    'org.freedesktop.DBus.NameHasOwner', name], deadline=deadline)
                if answer.strip() == b'(true,)':
                    break
                require(answer.strip() == b'(false,)', 'source bus readiness response refused')
                time.sleep(.025)
        user_bus = services.start('user-bus', ['/usr/sbin/runuser', '-u', SOURCE_USER, '--', '/usr/bin/dbus-daemon',
                                  '--session', '--nofork', '--address=unix:path=' + str(runtime / 'bus')])
        deadline = time.monotonic() + 30
        while not (runtime / 'bus').exists():
            services.live()
            require(time.monotonic() < deadline, 'source user session bus did not become ready')
            time.sleep(.025)
        # login uses the distribution PAM stack on a genuine controlling PTY.
        master, slave = pty.openpty()
        def own_tty():
            os.setsid()
            import fcntl, termios
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        login = subprocess.Popen(['/usr/bin/login', '-p', SOURCE_USER], stdin=slave, stdout=slave, stderr=slave,
            env={'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C',
                 'XDG_RUNTIME_DIR': str(runtime), 'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(runtime / 'bus')},
            preexec_fn=own_tty, close_fds=True)
        os.close(slave)
        _OBLIGATIONS.append(login)
        secret_owner = None
        next_secret_check = time.monotonic()
        try:
            output = bytearray()
            replied = 0
            deadline = time.monotonic() + 1200
            while observe_exit(login) is None:
                services.live()
                require(time.monotonic() < deadline, 'actual source PAM/main deadline elapsed')
                if secret_owner is None and time.monotonic() >= next_secret_check:
                    from secret_owner import discover
                    secret_owner = discover(runtime, SOURCE_USER, fixed, deadline)
                    next_secret_check = time.monotonic() + .5
                if select.select([master], [], [], .025)[0]:
                    try:
                        part = os.read(master, 4096)
                    except OSError as error:
                        if error.errno == 5:
                            break
                        raise
                    output.extend(part)
                    require(len(output) <= 1048576, 'source PTY output exceeded private bound')
                    # Secrets answer only real PAM/Polkit prompts on this owned
                    # login terminal; no authentication result is synthesized.
                    if b'Password:' in output or b'Password: ' in output or b'password:' in output:
                        replied += 1
                        require(replied <= 16, 'actual source PAM repeatedly refused')
                        os.write(master, SOURCE_PASSWORD + b'\n')
                        output.clear()
            observed = observe_exit(login)
            successful = (observed is not None and observed.si_code == os.CLD_EXITED and observed.si_status == 0)
            retire_child(login)
            # A D-Bus activated foreground daemon may remain a child of our
            # actual user bus. After Main/login stop, retire that owned bus
            # before requiring genuine subreaper adoption of the held daemon.
            services.stop(user_bus)
            if secret_owner is not None:
                secret_owner.retire_after_login(min(deadline, time.monotonic() + 5))
            require(successful and secret_owner is not None,
                    'actual source login/main or observed PAM Secrets owner refused')
        except BaseException:
            retire_child(login)
            raise
        finally:
            os.close(master)
        services.close()
        assert_children_retired()
        result = json.loads(held_bytes(export / 'source-observations.json', 65536), object_pairs_hook=unique)
        return result
    finally:
        services.close()


def source_observations(result):
    return {'source_operation': result['source_operation'], 'person': result['person'],
            'selected_document': 'receiver-document.txt', 'selected_document_bytes': DOCUMENT_BYTES.decode(),
            'selected_flatpak_id': 'org.tilo.Fixture', 'selected_flatpak_state': 'data/selected-document',
            'selected_flatpak_bytes': SELECTED_BYTES.decode(), 'excluded_flatpak_id': 'org.tilo.Excluded',
            'declined_home_leaf': 'owner-document'}


def retain_unknown_namespace():
    """Hold the full source stack until the actual outer namespace owner retires it."""
    while True:
        try:
            time.sleep(1)
        except BaseException:
            # A signal, broken diagnostic sink or caller interruption is not
            # evidence that the owned source/target children have stopped.
            continue


def wait_source_namespace(pid, deadline):
    """Reserve/reap only the actual forked source owner; unknown holds its stack."""
    while True:
        try:
            observed = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except BaseException:
            retain_unknown_namespace()
        if observed is not None:
            try:
                reaped, _ = os.waitpid(pid, 0)
            except BaseException:
                retain_unknown_namespace()
            if reaped != pid:
                retain_unknown_namespace()
            return observed
        if time.monotonic() >= deadline:
            retain_unknown_namespace()
        try:
            time.sleep(.025)
        except BaseException:
            retain_unknown_namespace()


def run(candidate, evidence, receiving, *, service_qualification=False, service_candidate=None,
        service_request=None):
    require(type(service_qualification) is bool, 'source service qualification mode must be boolean')
    if service_qualification:
        require(type(service_candidate) is dict
                and set(service_candidate) == {'required_protocol', 'browser_package_hash',
                    'cef_runtime_root_hash', 'sdk_runtime_root_hash',
                    'receiving_route_qualification_sha256'}
                and type(service_candidate['required_protocol']) is int
                and service_candidate['required_protocol'] == 2
                and all(type(value) is str and len(value) == 64
                        and all(char in '0123456789abcdef' for char in value)
                        for key, value in service_candidate.items() if key != 'required_protocol'),
                'source qualification lacks exact authenticated candidate scope')
        import service_contract
        request = service_contract.decode(json.dumps(service_request, separators=(',', ':')).encode())
        require(service_contract.candidate_scope({'request': request}) == service_candidate,
                'source native request differs from candidate scope')
        service_candidate = dict(service_candidate)
    else:
        require(service_candidate is None and service_request is None,
                'ordinary source cannot accept qualification scope')
    require(os.geteuid() == 0 and callable(receiving), 'owned root source receiving callback required')
    candidate, evidence = Path(candidate), Path(evidence)
    require(candidate == Path('/owned/candidate') and evidence.is_dir(), 'fixed admitted candidate/evidence required')
    baseline = os.open('/etc', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    initial = os.open('/owned/initial-pidns', os.O_RDONLY | os.O_CLOEXEC)
    work = private(evidence / 'source')
    dav = Dav(work / 'dav-tls')
    stopped = False
    target_started = False
    target_stopped = False
    exported = private('/var/tmp/tilo-native-source-export', 1000)
    try:
        def all_consumers_retired():
            require(stopped, 'actual source main/helper/services retirement unobserved')
            assert_children_retired()
            require(not target_started or target_stopped, 'actual target retirement unobserved')
        with owned_source(work / 'physical', initial, all_consumers_retired) as source:
            pid = os.fork()
            if pid == 0:
                result = None
                status = 1
                try:
                    # This exact parent retains/reaps orphaned helpers. A failed
                    # producer cannot leave a publisher behind and exit anyway.
                    require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0,
                            'source helper subreaper unavailable')
                    result = source_namespace(candidate, source, work, baseline, dav, exported, service_request)
                    status = 0
                except BaseException:
                    status = 1
                # A live or unobservable child retains this source namespace,
                # including its private mounts, until the outer container owner
                # observes retirement. No numeric descendant kill is inferred.
                try:
                    while True:
                        child, child_status = os.waitpid(-1, os.WNOHANG)
                        if child == 0:
                            retain_unknown_namespace()
                        if child_status != 0:
                            status = 1
                except ChildProcessError:
                    if status == 0:
                        try:
                            assert_children_retired()
                            write_new(exported / 'root-source-result.json',
                                      json.dumps(result, separators=(',', ':')).encode())
                        except BaseException:
                            status = 1
                    os._exit(status)
                except BaseException:
                    retain_unknown_namespace()
            observed = wait_source_namespace(pid, time.monotonic() + 1300)
            stopped = True
            require(observed.si_code == os.CLD_EXITED and observed.si_status == 0,
                    'actual source account/main/helper/services refused')
            result = json.loads(held_bytes(exported / 'root-source-result.json', 65536), object_pairs_hook=unique)
            actual = held_bytes(exported / 'prepared-setup.sealed', 32 * 1024 * 1024 + 16384)
            require(result['receipt']['sealed_bytes'] == len(actual)
                    and result['receipt']['ciphertext_sha256'] == hashlib.sha256(actual).hexdigest(),
                    'actual exported package changed after source shutdown')
            package = private(work / 'published-package')
            write_new(package / 'prepared-setup.sealed', actual)
            transported = {'schema': 1, 'sealed_bytes': len(actual),
                           'ciphertext_sha256': hashlib.sha256(actual).hexdigest()}
            write_new(package / 'receipt.json',
                      json.dumps(transported, separators=(',', ':')).encode())
            require(held_bytes(package / 'prepared-setup.sealed', len(actual)) == actual,
                    'Root package ciphertext transport changed')
            dav.account_ids = tuple(result['account_ids'])
            require(len(dav.account_ids) == 1, 'actual source selected account not unique')
            source.retire_source_epoch(all_consumers_retired)
            with dav.lock:
                before_ranges = len(dav.delivered_ranges)
            target_started = True
            try:
                context = {'package_directory': package, 'password': PASSWORD,
                'source_operation': result['source_operation'], 'applications_stage': result['apps_stage'],
                'source': source, 'baseline_etc_fd': baseline, 'tls_ca_certificate': held_bytes(dav.public_certificate, 65536),
                'tls_server_name': TLS_SERVER_NAME,
                'dav': {'account_id': dav.account_ids[0], 'mount_name': dav.service_name,
                        'relative': 'Documents/selected-document.txt', 'expected_hex': DAV_BYTES.hex(),
                        'write_relative': 'Documents/written-document.txt', 'write_hex': b'actual receiving write\n'.hex()},
                'source_observations': source_observations(result),
                'selected_service_kinds': result['selected_service_kinds'],
                'setup_binding': result['setup_binding']}
                if service_qualification:
                    # Public artifact scope only, never a published service proof.
                    # Native import separately checks the root-owned namespace,
                    # exact helper and authenticated sealed person/binding.
                    context['service_qualification'] = True
                    context['service_candidate'] = dict(service_candidate)
                completed = receiving(context)
            except BaseException:
                # Receiver owns exact target descendants. Without its actual
                # stop receipt we cannot disconnect their shared source disk.
                retain_unknown_namespace()
            target_stopped = (completed.get('receiving_namespace_retired') is True
                and completed.get('receiving_services_retired') is True
                and completed.get('target_physical_retired') is True)
            if not target_stopped:
                retain_unknown_namespace()
            with dav.lock:
                frontier = 0
                for first, last in sorted(dav.delivered_ranges[before_ranges:]):
                    if first <= frontier:
                        frontier = max(frontier, last)
                require(frontier == len(DAV_BYTES) and dav.uploads.get('/dav/Documents/written-document.txt') == b'actual receiving write\n',
                        'actual target cloud GET/PUT was not observed by live server')
            require(completed.get('receiving_namespace_retired') is True
                    and completed.get('receiving_services_retired') is True
                    and completed.get('target_physical_retired') is True, 'target actual retirement incomplete')
            completed['cloud']['server_upload_observed'] = True
        completed['source_physical_retired'] = True
        return completed
    finally:
        dav.close()
        os.close(initial)
        os.close(baseline)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--logged-in-source', action='store_true')
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--config', type=Path, required=True)
    args = parser.parse_args()
    require(args.logged_in_source and os.getuid() == 1000, 'source entry is only the real logged-in actor')
    result = user_stage(args.candidate, args.config)
    config = json.loads(held_bytes(args.config, 65536), object_pairs_hook=unique)
    write_new(Path(config['export']) / 'source-observations.json', json.dumps(result, separators=(',', ':')).encode(), 1000)


if __name__ == '__main__':
    main()
