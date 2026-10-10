import fcntl,json,os,pty,select,socket,struct,subprocess,termios,time
from pathlib import Path
assert os.getuid()==65534 and os.getgid()==65534
assert next(l for l in Path('/proc/self/status').read_text().splitlines() if l.startswith('CapEff:')).split()[1]=='0000000000000000'
root=Path('/tmp/managed');root.mkdir(mode=0o700)
images=root/'images';image=images/('11'*32);image.mkdir(parents=True)
(image/'usr').symlink_to('/usr',target_is_directory=True)
(image/'etc').mkdir();(image/'etc/passwd').write_text('root:x:0:0:root:/root:/bin/bash\n');(image/'etc/group').write_text('root:x:0:\n')
(images/'current-runtime').write_text('11'*32+'\nprivate qualified fixture\n')
home=root/'store/environments/default/home';home.mkdir(parents=True)
(home/'.bashrc').write_text("PS1='MANAGED> '; PS2='CONT> '; HISTFILE=/dev/null\nset -o emacs\nbind 'set enable-active-region on'\nread -e -p 'STARTUP_PRIVATE> ' synthetic\n")
a,b=socket.socketpair(socket.AF_UNIX,socket.SOCK_SEQPACKET);a.settimeout(.2)
master,slave=pty.openpty()
env={'PATH':'/usr/bin:/bin','HOME':str(root),'USER':'ordinary','LANG':'C.UTF-8','TERM':'xterm','TILO_USH_HOME':str(root/'store'),'TILO_USH_IMAGES':str(images),'TILO_USH_BIN':'/rust/debug/tilo-ush'}
def child():
 os.setsid();fcntl.ioctl(slave,termios.TIOCSCTTY,0)
owned=subprocess.Popen(['/rust/debug/tilo-ush','run','--host-root','/qual-host','--editor-fd',str(b.fileno()),'-v'],env=env,stdin=slave,stdout=slave,stderr=slave,pass_fds=(b.fileno(),),preexec_fn=child)
b.close();os.close(slave);os.set_blocking(master,False)
stream=[];transcript=bytearray();sequence=0
started=time.monotonic()
def drain():
 while True:
  try:
   data=os.read(master,65536)
   if not data:return
   transcript.extend(data)
  except BlockingIOError:return
  except OSError:return
  assert len(transcript)<262144,'bounded native transcript exceeded'
def call(op=1,lease=None,transaction=0,text=''):
 global sequence
 sequence+=1;frame=bytearray(80);frame[:8]=b'TBE1'+bytes([1,op,0,0]);struct.pack_into('<Q',frame,8,sequence)
 if lease:frame[16:64]=lease
 struct.pack_into('<Q',frame,64,transaction);payload=text.encode();struct.pack_into('<I',frame,72,len(payload));a.send(frame+payload)
 end=time.monotonic()+8
 while time.monotonic()<end:
  drain()
  assert owned.poll() is None,('managed supervisor exited',owned.returncode,transcript.decode(errors='replace')[-4000:])
  try:reply=a.recv(4177)
  except socket.timeout:continue
  assert 80<=len(reply)<=4176 and reply[:6]==b'TBE1\x01\x00' and reply[76:80]==bytes(4)
  size=struct.unpack_from('<I',reply,72)[0];assert len(reply)==80+size
  record={'request':struct.unpack_from('<Q',reply,8)[0],'status':reply[6],'flags':reply[7],'lease':reply[16:64] if reply[7]&4 else None,'transaction':struct.unpack_from('<Q',reply,64)[0],'text':reply[80:].decode()}
  stream.append({**record,'lease':record['lease'].hex() if record['lease'] else None})
  if record['request']==sequence:return record
  assert record['request']==0,'unexpected managed acknowledgement'
 raise AssertionError(('managed editor reply deadline',transcript.decode(errors='replace')[-4000:]))
def key(text):os.write(master,text);time.sleep(.1)
def ready(text):
 r=call();assert r['status']==2 and r['text']==text and r['lease'],r
 return r
try:
 end=time.monotonic()+15
 while b'STARTUP_PRIVATE> ' not in transcript:
  drain();assert owned.poll() is None,('managed launch failed',transcript.decode(errors='replace')[-4000:]);assert time.monotonic()<end,('startup prompt absent',transcript.decode(errors='replace')[-4000:]);time.sleep(.05)
 r=call();assert r['status']==1 and not r['text'] and not r['lease'],r
 print('PASS production managed Bash startup refuses editable field',flush=True)
 key(b'\n');ready('')
 key('café ☕'.encode());key(b'\x02\x00\x01\x18\x18');before=ready('café ☕');assert struct.unpack_from('<II',before['lease'],40)==(0,6)
 r=call(2,before['lease'],61,'salut ');assert r['status']==3 and not r['text'] and not r['lease'],r
 after=ready('salut ☕');assert after['flags']&1 and not after['flags']&2
 r=call(3,after['lease'],61);assert r['status']==4 and not r['text'],r
 undone=ready('café ☕');assert undone['lease'][40:48]==before['lease'][40:48]
 r=call(4,undone['lease'],61);assert r['status']==5 and not r['text'],r
 redone=ready('salut ☕');r=call(3,redone['lease'],61);assert r['status']==4;ready('café ☕')
 print('PASS production ush launcher delivers authenticated Unicode selection, Undo/Redo/Undo and native restored selection',flush=True)
 old=ready('café ☕');key(b'\x05\x02\x06');current=ready('café ☕');assert current['lease']!=old['lease']
 r=call(2,old['lease'],62,'wrong');assert r['status']==6;r=ready('café ☕')
 print('PASS managed native caret net-zero race retires original lease',flush=True)
 call(6);r=call();assert r['status']==1 and not r['text']
 print('PASS managed editor cancellation revokes original capability',flush=True)
 print('MANAGED_TERMINAL_JOURNEY_COMPLETE',json.dumps({'seconds':time.monotonic()-started,'full_desktop_acceptance':False,'physical_microphone':False,'installation':False}),flush=True)
finally:
 a.close()
 if owned.poll() is None:os.killpg(owned.pid,15)
 try:owned.wait(timeout=5)
 except subprocess.TimeoutExpired:os.killpg(owned.pid,9);owned.wait(timeout=5)
 os.close(master)
 print('MANAGED_OWNED_SUPERVISOR_REAPED',owned.returncode,flush=True)
 print('MANAGED_EVENT_STREAM',json.dumps(stream),flush=True)
 print('MANAGED_PTY_DIAGNOSTIC',json.dumps(transcript.decode(errors='replace')[-6000:]),flush=True)
