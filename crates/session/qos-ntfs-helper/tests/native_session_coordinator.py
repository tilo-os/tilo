#!/usr/bin/env python3
"""Explicit disposable-Linux session acceptance. Never run implicitly.

Requires the prepared direct physical 12MiB fixture, existing parser/client
accounts and deployed binaries. Alias mode alone performs authorized temporary
RO mount attempts. Detach/replacement require the external action protocol from
native_coordinator.py. This file is executable acceptance, not a proof receipt.
"""
import argparse
import array
import ctypes
import errno
import hashlib
import json
import os
import pathlib
import pwd
import re
import select
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
from native_coordinator import require, select as select_device, held_readonly, ended, trace, waited, inventory


def full_hash(device):
    h = hashlib.sha256()
    with open(device, 'rb', buffering=0) as stream:
        while part := stream.read(1024 * 1024):
            h.update(part)
    return h.hexdigest()


def reacquire(device):
    fd = os.open(device, os.O_RDONLY | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC)
    os.close(fd)


class Session:
    def __init__(self, probe, selected, uid, root=5):
        self.uid, self.sequence, self.sock = uid, 0, None
        self.process = subprocess.Popen([probe, 'serve-read-selected', '--inventory-id',
            selected['inventory_id'], '--root-mft', str(root), '--client-uid', str(uid)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            require(select.select([self.process.stdout], [], [], 22)[0], 'acquisition deadline')
            line = self.process.stdout.readline(65537)
            require(0 < len(line) <= 65536, 'missing bounded admitted locator')
            self.locator = json.loads(line)
            require(self.locator['backend'] == 'native-exclusive-read', 'wrong backend')
            path = pathlib.Path(self.locator['socket'])
            require(path.parent.stat().st_uid == 0 and path.parent.stat().st_mode & 0o777 == 0o711,
                    'session parent must be exact root-owned search-only 0711 layout')
            require(path.stat().st_uid == uid and path.stat().st_mode & 0o777 == 0o600,
                    'socket authorization mode')
        except BaseException:
            self.close()
            raise

    def connect(self, uid=None):
        os.seteuid(self.uid if uid is None else uid)
        try:
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            self.sock.settimeout(23)
            self.sock.connect(self.locator['socket'])
            _, peer_uid, _ = struct.unpack('3i', self.sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            require(peer_uid == 0, 'nonroot broker')
        finally:
            os.seteuid(0)

    def request(self, operation, **fields):
        packet = {'schema': 3, 'nonce': self.locator['nonce'], 'id': self.sequence,
                  'operation': {'operation': operation, **fields}}
        self.sequence += 1
        self.sock.send(json.dumps(packet).encode())
        return packet

    def receive(self, request):
        data, ancillary, flags, _ = self.sock.recvmsg(65536, 1024)
        require(data and not ancillary and not flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC), 'invalid reply packet')
        reply = json.loads(data)
        require(reply['schema'] == 3 and reply['nonce'] == request['nonce'] and reply['id'] == request['id'],
                'stale/wrong-session reply')
        return reply['result']

    def begin(self, require_large=True):
        self.connect()
        require(self.receive(self.request('Hello'))['result'] == 'Hello', 'hello')
        require(self.receive(self.request('BeginObservation'))['result'] == 'Begun', 'begin')
        entries, issues, upcase = [], [], []
        while True:
            page = self.receive(self.request('NextManifestPage'))
            entries += page['entries']; issues += page['issues']; upcase += page['upcase']
            if page['done']:
                break
        require(len(upcase) == 65536, 'native UpCase incomplete')
        require(hashlib.sha256(struct.pack('<65536H', *upcase)).hexdigest() == self.locator['identity']['upcase'], 'UpCase digest')
        if require_large:
            self.large = next(e for e in entries if e['path'] == [[ord(c) for c in 'large.bin']])
            require(self.large['content_readable'] and self.large['size'] == 12 * 1024 * 1024, 'actual native large file required')
        return entries, issues

    def read(self, offset, length=1048576, before_receive=None):
        request = self.request('ReadChunk', token=self.large['token'], offset=offset, length=length)
        if before_receive:
            before_receive()
        content = bytearray()
        while True:
            reply = self.receive(request)
            if reply['result'] == 'ReadDone':
                require(reply['length'] == length and len(content) == length and
                        hashlib.sha256(content).hexdigest() == reply['sha256'], 'chunk completion/hash')
                return content
            require(reply['result'] == 'Data' and reply['offset'] == len(content), 'chunk offset')
            content.extend(reply['bytes'])
            require(len(content) <= length, 'chunk size')

    def finish(self):
        request = self.request('FinishObservation'); hashes, issues = {}, []
        while True:
            reply = self.receive(request)
            require(reply['result'] == 'Finished', 'finish result')
            for token, digest in reply['hashes']:
                require(token not in hashes, 'duplicate final hash'); hashes[token] = digest
            issues += reply['issues']
            if reply['done']:
                return hashes, issues

    def closed(self, timeout=23):
        self.process.wait(timeout=timeout)
        require(ended(self.process.pid), 'broker retained descriptors after exit')
        require(not pathlib.Path(self.locator['socket']).exists(), 'normal teardown socket leak')

    def close(self):
        if self.sock:
            self.sock.close(); self.sock = None
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=23)
            except subprocess.TimeoutExpired:
                self.process.kill(); self.process.wait(timeout=3)


def positive(probe, selected, uid, device, client):
    before = full_hash(device)
    session = Session(probe, selected, uid)
    try:
        held_readonly(session.process.pid, os.stat(device).st_rdev)
        env = dict(os.environ, QOS_NTFS_SESSION_LOCATOR=json.dumps(session.locator))
        def selected_credentials():
            account = pwd.getpwuid(uid)
            os.setgroups([]); os.setgid(account.pw_gid); os.setuid(uid)
            require(os.geteuid() == uid and uid != 0, 'client must run as selected nonroot UID')
            # Establish the actual permission boundary before executing the Rust client.
            # O_RDONLY must fail here; successful Client::connect therefore needs search-only pinning.
            try:
                fd = os.open(pathlib.Path(session.locator['socket']).parent,
                             os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            except OSError as error:
                require(error.errno == errno.EACCES, 'unexpected directory-open failure')
            else:
                os.close(fd)
                raise RuntimeError('selected client unexpectedly has directory read permission')
        result = subprocess.run([client], env=env, preexec_fn=selected_credentials,
                                capture_output=True, timeout=600, check=True)
        receipt = json.loads(result.stdout)
        require(receipt['large_file_verified'] and receipt['native_bytes'] >= 12*1024*1024,
                'Rust client did not complete actual native byte operations')
        session.closed()
        reacquire(device)
        require(full_hash(device) == before, 'complete device changed during positive native session')
        return receipt
    finally:
        session.close()


def ranges(probe, selected, uid, device):
    # Actual broker ranges remain readable even when whole-file hashing was interrupted.
    for restart in [False, True]:
        s = Session(probe, selected, uid)
        try:
            s.begin()
            for offset, length in [(0, 1048576), (2097149, 13), (17, 31)]:
                expected = bytes((at % 256 + at // 1048576) % 256
                                 for at in range(offset, offset + length))
                require(s.read(offset, length) == expected, 'arbitrary native range mismatch')
            if restart:
                digest = hashlib.sha256()
                for n in range(12):
                    digest.update(s.read(n * 1048576))
                require(digest.hexdigest() == os.environ['QOS_NTFS_NATIVE_FILE_SHA256'],
                        'fresh contiguous restart bytes')
            hashes, issues = s.finish()
            if restart:
                require(hashes[s.large['token']] == digest.hexdigest(), 'restart hash incomplete')
            else:
                require(s.large['token'] not in hashes, 'interrupted range synthesized full hash')
                require(any(issue['path'] == s.large['path'] and
                            issue['reason'] == 'content-hash-incomplete' for issue in issues),
                        'interrupted hash missing scoped pending')
            s.receive(s.request('Close')); s.closed(); reacquire(device)
        finally:
            s.close()


def transport(probe, selected, uid, device):
    for attack in ['wrong-uid', 'nonce', 'duplicate', 'gap', 'rights', 'oversize', 'malformed', 'unknown', 'disconnect', 'cancel']:
        s = Session(probe, selected, uid)
        try:
            s.connect(0 if attack == 'wrong-uid' else None)
            packet = {'schema': 3, 'nonce': s.locator['nonce'], 'id': 0, 'operation': {'operation': 'Hello'}}
            if attack == 'wrong-uid':
                pass  # UID 0 can reach the filesystem socket but is not the selected UID.
            elif attack == 'nonce':
                packet['nonce'] = '0'*64; s.sock.send(json.dumps(packet).encode())
            elif attack == 'gap':
                packet['id'] = 2; s.sock.send(json.dumps(packet).encode())
            elif attack == 'rights':
                fd = os.open('/dev/null', os.O_RDONLY)
                try:
                    s.sock.sendmsg([json.dumps(packet).encode()], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))])
                finally:
                    os.close(fd)
            elif attack == 'oversize':
                s.sock.send(b'x'*65537)
            elif attack == 'malformed':
                s.sock.send(b'{"schema":3')
            elif attack == 'unknown':
                packet['operation']['operation'] = 'Write'; s.sock.send(json.dumps(packet).encode())
            else:
                request = s.request('Hello'); s.receive(request)
                if attack == 'duplicate':
                    s.sock.send(json.dumps(request).encode())
                elif attack == 'cancel':
                    require(s.receive(s.request('Cancel'))['result'] == 'Closed', 'cancel did not close')
                else:
                    s.sock.close(); s.sock = None
            s.closed(); reacquire(device)
            retry = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            try:
                try:
                    retry.connect(s.locator['socket'])
                except OSError:
                    pass
                else:
                    raise RuntimeError('reconnect accepted')
            finally:
                retry.close()
        finally:
            s.close()


def fake_broker(client,uid):
    with tempfile.TemporaryDirectory(prefix='qos-ntfs-fake-',dir='/run') as directory:
        os.chown(directory,uid,0);os.chmod(directory,0o700)
        read_fd,write_fd=os.pipe();pid=os.fork()
        if pid==0:
            try:
                os.close(read_fd);os.setuid(uid)
                listener=socket.socket(socket.AF_UNIX,socket.SOCK_SEQPACKET)
                listener.bind(directory+'/read.sock');os.chmod(directory+'/read.sock',0o600);listener.listen(1)
                os.write(write_fd,b'R');connection,_=listener.accept()
                connection.settimeout(3)
                require(not connection.recv(65536),'client sent authority request to nonroot broker')
                os._exit(0)
            except BaseException:os._exit(125)
        os.close(write_fd)
        try:
            require(select.select([read_fd],[],[],5)[0] and os.read(read_fd,1)==b'R','fake listener setup')
            os.chown(directory,0,0);os.chmod(directory,0o711)
            locator={'socket':directory+'/read.sock','nonce':'a'*64,'identity':{'volume':'b'*64,'root':'c'*64,'upcase':'d'*64}}
            result=subprocess.run([client],env=dict(os.environ,QOS_NTFS_SESSION_LOCATOR=json.dumps(locator)),preexec_fn=lambda:os.setuid(uid),capture_output=True,timeout=5)
            require(result.returncode!=0 and not result.stdout,'nonroot broker authorized native client')
            status=waited(pid,time.monotonic()+5);require(os.WIFEXITED(status) and os.WEXITSTATUS(status)==0,'fake broker exchange was not refused before hello')
            pid=None
        finally:
            os.close(read_fd)
            if pid:
                os.kill(pid,signal.SIGKILL);os.waitpid(pid,0)


def synchronize_reader(s, operation="ReadChunk"):
    # Seize this already successful broker, then stop its next real native fork.
    trace(0x4206, s.process.pid, 2 | 0x00100000)  # SEIZE, TRACEFORK | EXITKILL
    request = s.request(operation, **({'token':s.large['token'],'offset':0,'length':1048576} if operation=='ReadChunk' else {}))
    # A first-chunk transaction runs strict manifest health before the actual
    # range worker. Step past that child, then hold the concrete range child.
    for stage in range(2 if operation=='ReadChunk' else 1):
        status = waited(s.process.pid, time.monotonic()+5)
        require(os.WIFSTOPPED(status) and status >> 16 == 1, 'no real native operation fork')
        child = ctypes.c_ulong(); trace(0x4201, s.process.pid, ctypes.addressof(child))
        status = waited(child.value, time.monotonic()+5)
        require(os.WIFSTOPPED(status), 'child fork stop missing')
        if operation=='ReadChunk' and stage==0:
            trace(17,child.value);trace(7,s.process.pid)
    return child.value, request


def lifetime(probe, selected, uid, device, mode):
    s = Session(probe, selected, uid); child = None
    try:
        if mode=='cancel-manifest':
            s.connect();s.receive(s.request('Hello'))
        else:
            s.begin()
        if mode == 'idle':
            s.closed(timeout=33); reacquire(device); return
        if mode == 'stalled-client':
            s.request('ReadChunk',token=s.large['token'],offset=0,length=1048576)
            s.process.wait(timeout=23);require(ended(s.process.pid),'stalled broker alive');reacquire(device);return
        operation='BeginObservation' if mode=='cancel-manifest' else 'FinishObservation' if mode=='cancel-postflight' else 'ReadChunk'
        child, request = synchronize_reader(s,operation)
        held_readonly(s.process.pid, os.stat(device).st_rdev); held_readonly(child, os.stat(device).st_rdev)
        if mode == 'parent-before-child':
            os.kill(s.process.pid, signal.SIGKILL); trace(17, child)
        elif mode == 'parser-death':
            trace(17, child, signal.SIGKILL); trace(17, s.process.pid)
        elif mode == 'watchdog':
            trace(17, child, signal.SIGSTOP); trace(17, s.process.pid)
        elif mode == 'client-death':
            s.sock.close(); s.sock = None; trace(17, child); trace(17, s.process.pid)
        elif mode in ['cancel','cancel-manifest','cancel-postflight']:
            s.request('Cancel'); trace(17, child); trace(17, s.process.pid)
        elif mode == 'parent-after-child':
            trace(17, child)
            deadline = time.monotonic()+5
            while time.monotonic()<deadline:
                status = pathlib.Path(f'/proc/{child}/status').read_text()
                values = dict(line.split(':',1) for line in status.splitlines())
                account=pwd.getpwnam('qos-ntfs')
                if values['Uid'].split()[0] == str(account.pw_uid) and values.get('Seccomp','').strip() == '2':
                    require(all(v==str(account.pw_uid) for v in values['Uid'].split()) and all(v==str(account.pw_gid) for v in values['Gid'].split()),'exact dedicated credentials')
                    require(values['NoNewPrivs'].strip() == '1' and int(values['CapEff'],16) == 0 and not values['Groups'].strip(), 'child credential boundary')
                    fds = set(p.name for p in pathlib.Path(f'/proc/{child}/fd').iterdir())
                    require(fds <= {'3','4'}, 'child inherited broker IPC authority')
                    limits = pathlib.Path(f'/proc/{child}/limits').read_text()
                    require(re.search(r'Max address space\s+268435456\s+268435456\s+bytes',limits) and re.search(r'Max cpu time\s+10\s+10\s+seconds',limits), 'child memory/CPU limits')
                    break
                time.sleep(.001)
            else:
                raise RuntimeError('new chunk operation did not enter enforced sandbox')
            os.kill(s.process.pid, signal.SIGKILL)
        else:
            raise RuntimeError('unknown lifetime case')
        s.process.wait(timeout=23)
        deadline = time.monotonic()+3
        while not ended(child) and time.monotonic()<deadline:
            time.sleep(.01)
        require(ended(child) and ended(s.process.pid), 'child or claim survived teardown')
        child = None; reacquire(device)
        # SIGKILL may leave a pathname; it must never leave an accepting endpoint.
        if s.sock:
            while True:
                try:
                    packet = s.sock.recv(65536)
                except ConnectionResetError:
                    break
                if not packet: break
                require(json.loads(packet)['result']['result'] != 'Finished', 'fault published final observation')
    finally:
        if child and not ended(child):
            os.kill(child, signal.SIGKILL)
        s.close()


def detached(probe, selected, uid, device, mode):
    s = Session(probe, selected, uid); child = None
    try:
        s.begin(); child, request = synchronize_reader(s)
        action = os.environ['QOS_NTFS_NATIVE_ACTION'];require(pathlib.Path(action).is_absolute(), 'absolute action required')
        message = {'schema':1,'action':mode,'phase':'production-session-chunk-fork','device':device,'inventory':selected,'broker_pid':s.process.pid,'reader_pid':child,'claim_fds':held_readonly(s.process.pid,os.stat(device).st_rdev)}
        result = subprocess.run([action,mode],input=json.dumps(message).encode(),capture_output=True,timeout=5,check=True)
        require(json.loads(result.stdout)=={'schema':1,'completed':mode},'action receipt')
        old=pathlib.Path(selected['sysfs'])
        try:
            changed=old.stat().st_ino!=selected['sysfs_inode'] or int((old.parent/'diskseq').read_text())!=selected['disk_sequence']
        except FileNotFoundError:
            changed=True
        require(changed,'no-op detach/replacement action')
        if mode=='replace':
            require(any(c['name']==selected['name'] and c['disk_sequence']!=selected['disk_sequence'] for c in inventory(probe)),'replacement not present')
        else:
            require(not old.exists(),'old partition remains')
        trace(17,child);trace(17,s.process.pid)
        try:
            s.receive(request)
        except (RuntimeError,OSError):
            pass
        else:
            raise RuntimeError('invalidated chunk emitted successful packet')
        s.closed();require(s.process.returncode!=0,'invalidated broker succeeded')
        deadline=time.monotonic()+3
        while not ended(child) and time.monotonic()<deadline:time.sleep(.01)
        require(ended(child),'detached child survived');child=None
    finally:
        if child and not ended(child):os.kill(child,signal.SIGKILL)
        s.close()


def endpoint_cancellation(probe, selected, uid, device, mode):
    # Discover the real ordered readable manifest so we can stop the final chunk
    # or postflight child without adding timing hooks to production code.
    baseline = Session(probe, selected, uid)
    try:
        entries, _ = baseline.begin()
        baseline.receive(baseline.request('Close')); baseline.closed(); reacquire(device)
    finally:
        baseline.close()
    subtree = next(e for e in entries if e['path'] == [[ord(c) for c in 'cancel']])
    require(subtree['directory'], 'native cancellation subtree required')
    baseline = Session(probe, selected, uid, root=subtree['record'])
    try:
        entries, _ = baseline.begin(require_large=False)
        require(len(entries) == 2 and entries[-1]['content_readable'] and entries[-1]['size'] > 0,
                'last native range must be final manifest entry, without later cancellation checks')
        baseline.large = entries[-1]
        require(baseline.read(0, 11) == b'final chunk', 'subtree native content prerequisite')
        hashes, _ = baseline.finish()
        require(hashes[baseline.large['token']] == hashlib.sha256(b'final chunk').hexdigest(),
                'subtree successful postflight prerequisite')
        baseline.receive(baseline.request('Close')); baseline.closed(); reacquire(device)
    finally:
        baseline.close()
    forks, hashed, last_chunk = 1, 0, None  # BeginObservation's manifest worker.
    for entry in entries:
        if (not entry['path'] or entry['directory'] or not entry['content_readable']
                or hashed + entry['size'] > 1073741824):
            continue
        hashed += entry['size']
        if entry['size']:
            chunks = (entry['size'] + 1048575) // 1048576
            last_chunk = forks + 1 + chunks  # health preflight, then range workers
            forks += chunks + 2  # plus final content health worker
    require(last_chunk is not None, 'fixture needs actual readable content')
    target = last_chunk if mode == 'endpoint-cancel-last-chunk' else forks + 1
    test_name = ('native_session_cancellation_during_last_chunk' if
                 mode == 'endpoint-cancel-last-chunk' else 'native_session_cancellation_during_postflight')
    endpoint_test = os.environ['QOS_NTFS_NATIVE_ENDPOINT_TEST']
    s = Session(probe, selected, uid, root=subtree['record'])
    control, child_control = socket.socketpair()
    child, runner = None, None
    try:
        trace(0x4206, s.process.pid, 2 | 0x00100000)  # TRACEFORK | EXITKILL
        env = dict(os.environ, QOS_NTFS_SESSION_LOCATOR=json.dumps(s.locator),
                   QOS_NTFS_CANCELLATION_CONTROL_FD=str(child_control.fileno()))
        def credentials():
            account = pwd.getpwuid(uid)
            os.setgroups([]); os.setgid(account.pw_gid); os.setuid(uid)
        runner = subprocess.Popen([endpoint_test, test_name, '--ignored', '--nocapture'],
                                  env=env, pass_fds=(child_control.fileno(),),
                                  preexec_fn=credentials, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        child_control.close()
        seen = 0
        deadline = time.monotonic() + 120
        while seen < target:
            status = waited(s.process.pid, deadline)
            require(os.WIFSTOPPED(status), 'broker ended before target native phase')
            if status >> 16 != 1:
                require(os.WSTOPSIG(status) == signal.SIGCHLD, 'unexpected broker signal stop')
                trace(7, s.process.pid)
                continue
            child_id = ctypes.c_ulong(); trace(0x4201, s.process.pid, ctypes.addressof(child_id))
            child = child_id.value
            require(os.WIFSTOPPED(waited(child, deadline)), 'native child fork stop missing')
            seen += 1
            if seen == target:
                held_readonly(child, os.stat(device).st_rdev)
                control.settimeout(5); control.sendall(b'C')
                require(control.recv(1) == b'A', 'endpoint flag was not flipped during blocking phase')
                trace(17, child); trace(17, s.process.pid)
            else:
                trace(17, child); trace(7, s.process.pid)
            child = None
        output, errors = runner.communicate(timeout=45)
        require(runner.returncode == 0 and b'test result: ok. 1 passed;' in output,
                'actual endpoint cancellation fixture failed: ' + repr((output, errors)))
        s.closed(); reacquire(device)
    finally:
        control.close(); child_control.close()
        if child and not ended(child):
            os.kill(child, signal.SIGKILL)
        if runner and runner.poll() is None:
            runner.kill(); runner.wait(timeout=3)
        s.close()


def aliases(probe, selected, uid, device):
    with tempfile.TemporaryDirectory(prefix='qos-ntfs-session-alias-') as mountpoint:
        # Baseline real NTFS3 mount must work, otherwise refusals cannot prove exclusivity.
        subprocess.run(['mount','-t','ntfs3','-o','ro',device,mountpoint],check=True,timeout=10)
        try:
            attempt=subprocess.run([probe,'serve-read-selected','--inventory-id',selected['inventory_id'],'--root-mft','5','--client-uid',str(uid)],capture_output=True,timeout=22)
            require(attempt.returncode!=0 and not attempt.stdout,'mounted alias authorized session')
        finally:
            subprocess.run(['umount',mountpoint],check=True,timeout=10)
        keeper_code="import subprocess,sys; subprocess.run(['mount','-t','ntfs3','-o','ro',sys.argv[1],sys.argv[2]],check=True); print('READY',flush=True); sys.stdin.read()"
        keeper=subprocess.Popen(['unshare','--mount','--propagation','private',sys.executable,'-c',keeper_code,device,mountpoint],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        try:
            require(select.select([keeper.stdout],[],[],10)[0] and keeper.stdout.readline()==b'READY\n','other namespace mount must actually succeed')
            attempt=subprocess.run([probe,'serve-read-selected','--inventory-id',selected['inventory_id'],'--root-mft','5','--client-uid',str(uid)],capture_output=True,timeout=22)
            require(attempt.returncode!=0 and not attempt.stdout,'other namespace mounted alias authorized session')
        finally:
            keeper.stdin.close()
            try:keeper.wait(timeout=5)
            except subprocess.TimeoutExpired:keeper.kill();keeper.wait(timeout=3)
        s=Session(probe,selected,uid)
        try:
            s.begin();hashes=hashlib.sha256()
            for n in range(12):
                for prefix in [[],['unshare','--mount','--propagation','private']]:
                    attempt=subprocess.run(prefix+['mount','-t','ntfs3','-o','ro',device,mountpoint],capture_output=True,timeout=10,env=dict(os.environ,LC_ALL='C'))
                    require(attempt.returncode!=0 and b'busy' in attempt.stderr.lower(),'concurrent mount not blocked by live claim')
                hashes.update(s.read(n*1048576))
            require(hashes.hexdigest()==os.environ['QOS_NTFS_NATIVE_FILE_SHA256'],'alias stream changed')
            final,_=s.finish();require(final[s.large['token']]==hashes.hexdigest(),'alias postflight')
            s.receive(s.request('Close'));s.closed();reacquire(device)
        finally:s.close()


def main():
    require(sys.platform=='linux' and os.geteuid()==0 and os.environ.get('QOS_NTFS_NATIVE_AUTHORIZATION')=='DISPOSABLE_FIXTURE_ONLY','explicit disposable Linux root authorization required')
    parser=argparse.ArgumentParser();parser.add_argument('--probe',required=True);parser.add_argument('--client',required=True)
    parser.add_argument('--mode',required=True,choices=['endpoint-cancel-last-chunk','endpoint-cancel-postflight','positive','ranges','transport','aliases','parent-before-child','parent-after-child','parser-death','client-death','cancel','cancel-manifest','cancel-postflight','watchdog','idle','stalled-client','detach','replace'])
    args=parser.parse_args();device=os.environ['QOS_NTFS_NATIVE_DEVICE'];uid=int(os.environ['QOS_NTFS_NATIVE_CLIENT_UID']);require(uid!=0,'select existing nonroot client UID')
    selected,_=select_device(args.probe,device)
    before=full_hash(device);receipt=positive(args.probe,selected,uid,device,args.client)
    if args.mode=='transport':
        transport(args.probe,selected,uid,device);fake_broker(args.client,uid)
    elif args.mode.startswith('endpoint-cancel-'):endpoint_cancellation(args.probe,selected,uid,device,args.mode)
    elif args.mode=='ranges':ranges(args.probe,selected,uid,device)
    elif args.mode=='aliases':aliases(args.probe,selected,uid,device)
    elif args.mode in ['detach','replace']:detached(args.probe,selected,uid,device,args.mode)
    elif args.mode!='positive':lifetime(args.probe,selected,uid,device,args.mode)
    if args.mode not in ['detach','replace']:require(full_hash(device)==before,'complete physical image changed')
    print(json.dumps({'positive':receipt,'mode':args.mode,'accepted':True}))


if __name__=='__main__':main()
