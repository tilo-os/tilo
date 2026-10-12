#!/usr/bin/env python3
"""Root-owned native fixture material only; no build, Flatpak/app execution or downloaded SDK."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import types
ROOT=Path('/run/tilo-flatpak-runnable-runtime')
PINS={'assemble-app-runtime.py':'8477185cc33d585f98ccb322419be32b145e638f15dac182035d2416b0f06f9f','bounded_native.py':'b3a2d91d56719af5d10e9d7bba8aede8ddcbd0552c898ad2dfb539a6c897aad9','common.py':'7c840eb66224ba026173f6aacf3279d4c5a815c07b0d557491ed64d680dba589'}
def dependency(name):
    path=Path(__file__).resolve().parent/'dependencies'/name
    fd=os.open(path,os.O_RDONLY|os.O_NONBLOCK|os.O_NOFOLLOW|os.O_CLOEXEC)
    try:
        before=os.fstat(fd);raw=os.read(fd,128*1024+1)
        if not stat.S_ISREG(before.st_mode) or len(raw)!=before.st_size or len(raw)>128*1024 or hashlib.sha256(raw).hexdigest()!=PINS[name]:raise ValueError('reviewed helper dependency refused')
        stamp=lambda m:(m.st_dev,m.st_ino,m.st_mode,m.st_uid,m.st_gid,m.st_nlink,m.st_size,m.st_mtime_ns,m.st_ctime_ns)
        if stamp(before)!=stamp(os.fstat(fd)) or stamp(before)!=stamp(path.lstat()):raise ValueError('dependency changed')
    finally:os.close(fd)
    module=types.ModuleType(name);module.__file__=str(path);exec(compile(raw,str(path),'exec'),module.__dict__);return module

def main():
    if os.getuid()!=0 or os.geteuid()!=0:raise ValueError('owned native namespace root required')
    for path in (Path('/'),Path('/run')):
        m=path.lstat()
        if not stat.S_ISDIR(m.st_mode) or m.st_uid!=0 or m.st_mode&0o022:raise ValueError('protected native fixture ancestry required')
    common=dependency('common.py');bounded=dependency('bounded_native.py');canonical=dependency('assemble-app-runtime.py')
    environment={'PATH':'/usr/bin:/usr/sbin','LC_ALL':'C','LANG':'C'}
    def finite(argv,*,env=None,capture=False):
        if len(argv)!=2 or argv[0]!='ldd':raise ValueError('canonical fixture dependency command differs')
        status,out,err=bounded.bounded(['/usr/bin/ldd',str(argv[1])],timeout=10,limit=65536,env=environment)
        if status:raise ValueError('actual fixture ELF dependency inspection failed')
        return types.SimpleNamespace(stdout=out.decode('utf-8','strict'))
    canonical.run=finite
    bash=Path('/usr/bin/bash');bash_facts=common.file_facts(bash,16*1024**2)
    sleep=Path('/usr/bin/sleep')
    closure=list(dict.fromkeys([bash,sleep,*canonical.native_dependencies([bash,sleep],environment)]))
    if len(closure)>64:raise ValueError('runtime closure entry bound refused')
    staged=[];mapping={}
    for source in closure:
        # Root-owned public ELF symlinks are resolved once; exact actual target
        # bytes and named path are reattested before/after copying.
        real=source.resolve(strict=True)
        for parent in [real,*real.parents]:
            m=parent.lstat()
            if m.st_uid!=0 or m.st_mode&0o7022 or stat.S_ISLNK(m.st_mode):raise ValueError('public native ELF ancestry refused')
        relative=str(source).removeprefix('/')
        if relative.startswith('usr/'):relative=relative[4:]
        elif relative.startswith(('lib/','lib64/')):pass
        else:raise ValueError('canonical public runtime path refused')
        if '..' in Path(relative).parts or relative in mapping:raise ValueError('ambiguous runtime projection')
        mapping[relative]=real
        facts=common.file_facts(real,16*1024**2)
        if not facts['mode']&0o111:raise ValueError('runtime ELF is not executable')
        raw=common.held_bytes(real,16*1024**2)
        if not raw.startswith(b'\x7fELF') or hashlib.sha256(raw).hexdigest()!=facts['sha256']:raise ValueError('held public ELF differs')
        staged.append((relative,source,real,facts,raw))
    if sum(len(r[4]) for r in staged)>64*1024**2:raise ValueError('runtime closure total bound refused')
    ROOT.mkdir(mode=0o700)
    entries=[]
    for relative,source,real,facts,raw in staged:
        destination=ROOT/'usr'/relative;destination.parent.mkdir(parents=True,exist_ok=True)
        with destination.open('xb') as stream:stream.write(raw);stream.flush();os.fsync(stream.fileno())
        destination.chmod(0o555)
        if source.resolve(strict=True)!=real or common.file_facts(real,16*1024**2)!=facts or common.file_facts(destination,16*1024**2)['sha256']!=facts['sha256']:raise ValueError('native runtime source/readback changed')
        entries.append({'path':relative,'sha256':facts['sha256'],'bytes':facts['bytes']})
    if common.file_facts(bash,16*1024**2)!=bash_facts:raise ValueError('actual Bash changed during preparation')
    for directory in sorted((p for p in (ROOT/'usr').rglob('*') if p.is_dir()),key=lambda p:len(p.parts),reverse=True):directory.chmod(0o555)
    (ROOT/'usr').chmod(0o555)
    with (ROOT/'runtime.json').open('x') as stream:json.dump({'schema':1,'entries':entries,'native_app_executed':False},stream,sort_keys=True);stream.write('\n');stream.flush();os.fsync(stream.fileno())
    (ROOT/'runtime.json').chmod(0o444)
    ROOT.chmod(0o555)
    print(json.dumps({'schema':1,'runtime_entries':len(entries),'bytes':sum(e['bytes'] for e in entries),'native_app_executed':False}))
if __name__=='__main__':main()
