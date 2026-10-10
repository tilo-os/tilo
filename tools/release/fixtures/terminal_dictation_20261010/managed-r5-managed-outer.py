import ctypes,json,os,subprocess,sys,tempfile
from pathlib import Path
assert os.getpid()==1
b=Path(sys.argv[1]);out=Path(sys.argv[2])
ready_r,ready_w=os.pipe();release_r,release_w=os.pipe();keeper=os.fork()
if keeper==0:
 os.close(ready_r);os.close(release_w);libc=ctypes.CDLL(None,use_errno=True)
 if libc.unshare(0x10000000)!=0:os.write(ready_w,b'E');os._exit(1)
 os.write(ready_w,b'R');os.close(ready_w)
 while os.read(release_r,1):pass
 os._exit(0)
os.close(ready_w);os.close(release_r);namespace=None
try:
 assert os.read(ready_r,1)==b'R'
 Path(f'/proc/{keeper}/uid_map').write_text('0 0 1\n65534 65534 1\n');Path(f'/proc/{keeper}/gid_map').write_text('0 0 1\n65534 65534 1\n');namespace=os.open(f'/proc/{keeper}/ns/user',os.O_RDONLY|os.O_CLOEXEC)
 with tempfile.TemporaryDirectory() as temp:
  drop=Path(temp)/'drop.py';drop.write_text("import ctypes,os,runpy\nassert ctypes.CDLL(None,use_errno=True).prctl(38,1,0,0,0)==0\nos.chmod('/tmp',0o1777);os.setgroups([]);os.setgid(65534);os.setuid(65534);runpy.run_path('/journey.py',run_name='__main__')\n")
  command=['bwrap','--userns',str(namespace),'--cap-add','CAP_SETUID','--cap-add','CAP_SETGID','--unshare-ipc','--unshare-uts','--unshare-net','--die-with-parent','--new-session','--ro-bind','/usr','/usr','--ro-bind','/lib','/lib','--ro-bind','/lib64','/lib64','--ro-bind','/etc','/etc','--bind','/proc','/proc','--dev','/dev','--tmpfs','/tmp','--ro-bind',str(b/'target'),'/rust','--ro-bind',str(out/'host-root'),'/qual-host','--ro-bind',str(out/'managed-inner.py'),'/journey.py','--ro-bind',str(drop),'/drop.py','--clearenv','--setenv','PATH','/usr/bin:/bin','--','/usr/bin/python3','/drop.py']
  p=subprocess.run(command,pass_fds=(namespace,),timeout=90);print('MANAGED_OUTER_EXIT',p.returncode,flush=True)
  assert p.returncode==0
finally:
 if namespace is not None:os.close(namespace)
 os.close(ready_r);os.close(release_w);_,status=os.waitpid(keeper,0);assert os.waitstatus_to_exitcode(status)==0;print('PASS managed private namespace keeper reaped',flush=True)
