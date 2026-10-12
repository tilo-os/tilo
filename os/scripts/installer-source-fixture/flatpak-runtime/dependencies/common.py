import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tomllib

SELECTED_PACKAGES=('browser','file-explorer','word-processor')
OUTPUTS=('thin-app/browser','thin-app/file-explorer','thin-app/word-processor','thin-app/libruntime_dylib.so','thin-app/tilo-runtime-account-capability')

def features(root):
    value=tomllib.loads((root/'dist/apps/app-collections.toml').read_text())
    pairs=[package+'/'+feature for package,values in value['features'].items() for feature in values]
    image=re.search(r'APP_FEATURES="([^"]*)"',(root/'os/Containerfile').read_text())
    if image is None or image.group(1).split(',')!=pairs:raise ValueError('distro collection and image feature union differ')
    return ','.join(pairs)

def expected(root):
    packages=[];thin=[];crates=[]
    for manifest in sorted((root/'apps').glob('*/tilo-app.toml')):
        package=tomllib.loads((manifest.parent/'Cargo.toml').read_text())['package']['name']
        binary=tomllib.loads(manifest.read_text())['entrypoint']['bin'].removeprefix('bin/')
        packages+=['-p',package];thin.append(package+'/thin');crates.append(binary.replace('-','_'))
    crates.append('tilo_runtime_account_capability')
    return ['build','--locked','--profile','thin-app','--features',','.join(thin)+','+features(root),'--message-format=json',
        '--config','build.rustc-workspace-wrapper="'+str(root/'tools/ci/rustc-thin-app-wrapper.sh')+'"',
        '--config','env.TILO_THIN_BIN_CRATES="'+':'.join(crates)+'"',
        '-p','runtime-dylib','--bin','tilo-runtime-account-capability',*packages,*[argument for binary in SELECTED_PACKAGES for argument in ('--bin',binary)]]

def file_facts(path,limit=64*1024**2):
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        m=os.fstat(fd)
        if not stat.S_ISREG(m.st_mode) or not 0<=m.st_size<=limit:raise ValueError('source/artifact regular-file bound refused')
        digest=hashlib.sha256();count=0
        while True:
            data=os.read(fd,min(1024**2,limit+1-count))
            if not data:break
            count+=len(data)
            if count>limit:raise ValueError('source/artifact read bound exceeded')
            digest.update(data)
        after=os.fstat(fd);named=os.stat(path,follow_symlinks=False)
        def identity(value):return (value.st_dev,value.st_ino,value.st_mode,value.st_uid,value.st_gid,value.st_nlink,value.st_size,value.st_mtime_ns,value.st_ctime_ns)
        if identity(m)!=identity(after) or identity(m)!=identity(named) or count!=m.st_size:raise ValueError('listed input changed while held')
        return dict(sha256=digest.hexdigest(),bytes=count,mode=stat.S_IMODE(m.st_mode))
    finally:os.close(fd)

def ledger(root):
    command=subprocess.run(['git','ls-files','-z','--cached','--others','--exclude-standard'],cwd=root,check=True,stdout=subprocess.PIPE)
    if len(command.stdout)>8*1024**2:raise ValueError('source path ledger exceeds bound')
    names=sorted(set(os.fsdecode(name) for name in command.stdout.split(b'\0') if name))
    if len(names)>32768:raise ValueError('source input count exceeds bound')
    result={};total=0
    for name in names:
        path=Path(name)
        if path.is_absolute() or '..' in path.parts or path.parts[0] in ('target','.git'):raise ValueError('invalid listed source path')
        source=root/path
        if not os.path.lexists(source):
            result[name]={'absent':True}
        elif source.is_symlink():
            target=os.readlink(source)
            result[name]={'symlink':target}
        else:
            result[name]=file_facts(source);total+=result[name]['bytes']
            if total>2*1024**3:raise ValueError('source ledger byte budget exceeded')
    return result


def held_bytes(path,limit):
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        original=os.fstat(fd)
        if not stat.S_ISREG(original.st_mode) or original.st_size>limit:raise ValueError('compiler stream regular-file bound refused')
        output=bytearray()
        while len(output)<=limit:
            chunk=os.read(fd,min(1024**2,limit+1-len(output)))
            if not chunk:break
            output.extend(chunk)
        if len(output)>limit:raise ValueError('compiler stream exceeds bound')
        def stamp(m):return (m.st_dev,m.st_ino,m.st_size,m.st_mode,m.st_uid,m.st_gid,m.st_nlink,m.st_mtime_ns,m.st_ctime_ns)
        if stamp(original)!=stamp(os.fstat(fd)) or stamp(original)!=stamp(os.stat(path,follow_symlinks=False)) or len(output)!=original.st_size:raise ValueError('held compiler stream changed')
        return bytes(output)
    finally:os.close(fd)
