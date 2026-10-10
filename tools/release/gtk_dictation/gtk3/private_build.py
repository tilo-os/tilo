#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Preflight or build the private GTK3 candidate with verified, fetchless inputs.

Run on the compatibility builder. This never installs packages or a runtime.
The caller must separately qualify editing, Undo, accessibility and admission.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import uuid

IMAGE = 'sha256:a3f63d1003795b29af6e512fde6a3f37c7942b986d7a57e4fe4fd9f32c204d9e'
HERE = Path(__file__).resolve().parent


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(prepared, dependencies):
    receipt = json.loads((prepared / 'preparation-receipt.json').read_text())
    if receipt['source_lock_sha256'] != sha(HERE / 'source-lock.json'):
        raise ValueError('Source-lock receipt mismatch')
    tree = prepared / 'gtk-3.24.52'
    actual = {
        str(p.relative_to(tree)): ({'symlink': str(p.readlink())}
            if p.is_symlink() else {'sha256': sha(p)})
        for p in sorted(tree.rglob('*')) if p.is_file() or p.is_symlink()
    }
    if actual != receipt.get('source_tree'):
        raise ValueError('Prepared whole-source snapshot mismatch or missing')
    inputs = json.loads((dependencies / 'extraction-receipt.json').read_text())
    if inputs.get('image') != IMAGE or inputs.get('unresolved_requirements') != []:
        raise ValueError('Development dependency closure is not verified')
    root = dependencies / 'root'
    actual = {
        str(p.relative_to(root)): ({'symlink': str(p.readlink())}
            if p.is_symlink() else {'sha256': sha(p)})
        for p in sorted(root.rglob('*')) if p.is_file() or p.is_symlink()
    }
    if actual != inputs.get('files'):
        raise ValueError('Extracted dependency snapshot mismatch')
    return tree, root


INNER = r'''
import json,os,pathlib,subprocess,shlex,hashlib
out=pathlib.Path('/out');pc=out/'pkgconfig';pc.mkdir()
for source in pathlib.Path('/deps/usr/lib64/pkgconfig').glob('*.pc'):
 text=source.read_text().replace('prefix=/usr','prefix=/deps/usr')
 (pc/source.name).write_text(text)
for source in pathlib.Path('/deps/usr/share/pkgconfig').glob('*.pc'):
 text=source.read_text().replace('prefix=/usr','prefix=/deps/usr')
 (pc/source.name).write_text(text)
env=dict(os.environ,PYTHONPATH='/deps/usr/lib/python3.14/site-packages',
 PKG_CONFIG_PATH=str(pc),LD_LIBRARY_PATH='/deps/usr/lib64',
 HOME='/tmp',XDG_CACHE_HOME='/tmp/cache')
commands=[['python3','/deps/usr/bin/meson','setup','/out/build','/source',
 '--wrap-mode=nodownload','--buildtype=debugoptimized','-Dx11_backend=false',
 '-Dwayland_backend=true','-Dbroadway_backend=false','-Dprint_backends=file',
 '-Dcolord=no','-Dgtk_doc=false','-Dman=false','-Dintrospection=false',
 '-Ddemos=false','-Dexamples=false','-Dtests=false','-Dinstalled_tests=false',
 '-Dbuiltin_immodules=backend']]
for command in commands:subprocess.run(command,check=True,env=env,timeout=1800)
targets=json.loads((out/'build/meson-info/intro-targets.json').read_text())
library=[t for t in targets if t['name']=='gtk-3' and t['type']=='shared library']
assert len(library)==1 and len(library[0]['filename'])==1
target=str(pathlib.Path(library[0]['filename'][0]).relative_to(out/'build'))
command=['ninja','-C','/out/build','-j2',target]
subprocess.run(command,check=True,env=env,timeout=1800);commands.append(command)
flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs',
 'pangocairo','atk','gdk-pixbuf-2.0','gio-2.0','cairo'],env=env,text=True))
for source,name in [('/entry_subject.c','entry_subject'),('/native_history_gate.c','native_history_gate')]:
 command=['cc','-std=c11','-O2','-Wall','-Wextra','-Werror',source,
  '-Wl,-rpath,$ORIGIN/build/gtk:$ORIGIN/build/gdk',
  '-Wl,-rpath-link,/deps/usr/lib64','-I/source','-I/out/build',
  '-L/out/build/gtk','-L/out/build/gdk','-lgtk-3','-lgdk-3',
  *flags,'-o','/out/'+name]
 subprocess.run(command,check=True,env=env,timeout=60);commands.append(command)
(out/'compile-receipt.json').write_text(json.dumps(dict(commands=commands,
 binary_sha256=hashlib.sha256((out/'entry_subject').read_bytes()).hexdigest(),
 compiled=True,installed=False,editing_qualified=False,accessibility_qualified=False),indent=2)+'\n')
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepared', type=Path, required=True)
    parser.add_argument('--dependencies', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--preparation-receipt-sha256', required=True)
    parser.add_argument('--dependency-receipt-sha256', required=True)
    parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    if sha(args.prepared / 'preparation-receipt.json') != args.preparation_receipt_sha256:
        raise ValueError('Preparation receipt hash mismatch')
    if sha(args.dependencies / 'extraction-receipt.json') != args.dependency_receipt_sha256:
        raise ValueError('Dependency receipt hash mismatch')
    tree, root = verify(args.prepared.resolve(), args.dependencies.resolve())
    if args.out.exists():
        raise ValueError('Output must be fresh')
    command = ['podman', 'run', '--rm', '--name', 'tilo-gtk3-review-'+uuid.uuid4().hex,
        '--pull=never', '--network=none', '--read-only', '--cap-drop=ALL',
        '--security-opt', 'no-new-privileges', '--security-opt', 'label=disable',
        '--memory=3g', '--cgroup-conf=memory.swap.max=0', '--cpus=2', '--pids-limit=256',
        '--tmpfs', '/tmp:rw,nosuid,size=128m',
        '--volume', str(tree)+':/source:ro', '--volume', str(root)+':/deps:ro',
        '--volume', str(HERE/'entry_subject.c')+':/entry_subject.c:ro',
        '--volume', str(HERE/'native_history_gate.c')+':/native_history_gate.c:ro',
        '--volume', str(args.out.resolve())+':/out:rw',
        '--entrypoint', 'python3', IMAGE, '-c', INNER]
    if not args.execute:
        print(json.dumps({'preflight': 'pass', 'command': command, 'compiled': False}))
        return
    args.out.mkdir(mode=0o700, parents=False)
    try:
        subprocess.run(command, check=True, timeout=3700)
    finally:
        subprocess.run(['podman', 'rm', '-f', command[4]], capture_output=True, timeout=15)


if __name__ == '__main__':
    main()
