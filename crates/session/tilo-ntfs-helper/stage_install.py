#!/usr/bin/env python3
"""Produce a reproducible root-owned tar staging layout; never install or activate."""
import argparse, io, pathlib, re, tarfile, tomllib
from source_artifact import source_bytes
ROOT=pathlib.Path(__file__).resolve().parent
ap=argparse.ArgumentParser();ap.add_argument('--binary',required=True);ap.add_argument('--output',required=True);ap.add_argument('--lockfile',default=str(ROOT.parents[2]/'Cargo.lock'));a=ap.parse_args()
lock_bytes=pathlib.Path(a.lockfile).read_bytes()
packages=tomllib.loads(lock_bytes.decode())['package']
helper=next(p for p in packages if p['name']=='tilo-ntfs-helper')
versions={}
for dependency in [*helper['dependencies'], 'tempfile', 'tokio', 'postcard']:
 name,*version=dependency.split()
 matches=[p['version'] for p in packages if p['name']==name and (not version or p['version']==version[0])]
 if len(matches)!=1:raise SystemExit('ambiguous dependency lock: '+dependency)
 versions[name]=matches[0]
source_archive=source_bytes()
binary=pathlib.Path(a.binary).read_bytes()
if binary[:4]!=b'\x7fELF':raise SystemExit('only a native Linux ELF may be staged')
items={
 'usr/libexec/tilo-ntfs-probe':(binary,0o755),
 'usr/share/licenses/tilo-ntfs-helper/COPYING':((ROOT/'vendor/COPYING').read_bytes(),0o644),
 'usr/share/tilo-ntfs-helper/ntfs-3g-source.tar.gz':(source_archive,0o644),
 'usr/share/tilo-ntfs-helper/ntfs-3g.lock':((ROOT/'vendor/ntfs-3g.lock').read_bytes(),0o644),
 'usr/share/doc/tilo-ntfs-helper/README.md':((ROOT/'README.md').read_bytes(),0o644),
}
items['usr/share/tilo-ntfs-helper/source/vendor/ntfs-3g-source.tar.gz']=(source_archive,0o644)
# Keep the existing source/ helper location and its sibling relative dependency.
# Each crate also builds independently, without inheriting the Tilo workspace.
for crate, destination in [(ROOT, 'source'), (ROOT.parent / 'tilo-ntfs-protocol', 'tilo-ntfs-protocol'), (ROOT.parents[1] / 'system/wire-frame', 'wire-frame')]:
 for source in sorted(crate.rglob('*')):
  if not source.is_file() or '__pycache__' in source.parts or source.suffix=='.img':continue
  relative=source.relative_to(crate).as_posix()
  data=source.read_bytes()
  if relative=='Cargo.toml':
   manifest=data.decode()
   # Dependency additions must remain usable outside the repository workspace.
   for name, version in versions.items():
    manifest=manifest.replace(name+'.workspace = true',name+' = "='+version+'"')
    manifest=re.sub(r'(?m)^'+re.escape(name)+r' = \{ workspace = true',name+' = { version = "='+version+'"',manifest)
   manifest=re.sub(r'(?m)^workspace-hack = .*\n','',manifest)
   manifest=manifest.replace('path = "../../system/wire-frame"','path = "../wire-frame"')
   manifest=manifest.replace('license.workspace = true','license = "AGPL-3.0-only"').replace('repository.workspace = true','repository = "https://code.tilo.so/tilo/tilo"').replace('homepage.workspace = true','homepage = "https://tilo.so"')
   manifest=manifest.replace('[lints]\nworkspace = true','[workspace]')
   data=manifest.encode()
  items['usr/share/tilo-ntfs-helper/'+destination+'/'+relative]=(data,0o644)
 items['usr/share/tilo-ntfs-helper/'+destination+'/Cargo.lock']=(lock_bytes,0o644)
items['usr/share/tilo-ntfs-helper/wire-frame/LICENSE']=((ROOT.parents[2]/'LICENSE').read_bytes(),0o644)
with open(a.output,'xb') as target,tarfile.open(fileobj=target,mode='w',format=tarfile.USTAR_FORMAT) as tar:
 for name,(data,mode) in sorted(items.items()):
  entry=tarfile.TarInfo(name);entry.size=len(data);entry.mode=mode;entry.uid=entry.gid=0;entry.uname=entry.gname='root';entry.mtime=0;tar.addfile(entry,io.BytesIO(data))
