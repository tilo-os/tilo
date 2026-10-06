"""Stage public installer plumbing without rebuilding the native installer."""
import hashlib,shutil
from pathlib import Path
OVERRIDES=('usr/libexec/tilo-install','usr/libexec/tilo-verified-boot-install-preflight',
           'usr/libexec/tilo-install-dev-ssh.py',
           'usr/libexec/tilo-install-handoff-copy.py',
           'usr/libexec/tilo-installer-offline-target.py',
           'usr/libexec/tilo-installer-dev-target-uki.py',
           'usr/libexec/tilo-installer-code-protocol.py',
           'usr/libexec/tilo-installer-code-refresh.py',
           'usr/libexec/tilo-vm-install-proof.py',
           'usr/lib/systemd/system/tilo-installer-code-refresh.service')
def stage(source,helpers,context):
 source=Path(source)/'os';helpers=Path(helpers)/'os';context=Path(context)
 if context.exists():raise ValueError('fresh installer context required')
 if not (source/'usr').is_dir() or (source/'usr').is_symlink():raise ValueError('actual source usr overlay required')
 provenance={}
 def copy(src,dst,label):
  if src.is_symlink() or not src.is_file():raise ValueError('regular installer source required: '+label)
  data=src.read_bytes()
  if len(data)>4*1024*1024:raise ValueError('installer source bound exceeded')
  dst.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(src,dst);provenance[label]=hashlib.sha256(data).hexdigest()
 context.mkdir(mode=0o700)
 # Preserve the actual assembly source's policy/runtime scripts. Only the
 # previously reviewed target-UKI promotion fixes come from coherent helpers.
 for path in sorted((source/'usr').rglob('*')):
  if path.is_symlink():raise ValueError('installer source symlink requires explicit admission')
  if path.is_file():
   rel=path.relative_to(source).as_posix()
   if rel=='usr/bin/tilo-installer':continue
   copy(path,context/'rootfs'/rel,'source:'+rel)
 for rel in OVERRIDES:copy(helpers/rel,context/'rootfs'/rel,'helpers:'+rel)
 for src,dst in [('etc/greetd/config.toml','usr/share/tilo/greetd-config.toml'),('etc/greetd/config.installer.toml','etc/greetd/config.toml')]:copy(source/src,context/'rootfs'/dst,'source:'+src)
 copy(helpers/'dev/uki-cmdline.py',context/'rootfs/usr/libexec/tilo-uki-cmdline','helpers:dev/uki-cmdline.py')
 (context/'rootfs/usr/libexec/tilo-uki-cmdline').chmod(0o755)
 copy(helpers/'Containerfile.installer',context/'Containerfile','helpers:Containerfile.installer')
 return provenance

def command(context,identifier,epoch,iid):
 import re
 if not re.fullmatch(r'sha256:[0-9a-f]{64}',identifier):raise ValueError('immutable provisional installer base required')
 return ['podman','--remote=false','build','--pull=never','--network=none','--source-date-epoch',str(epoch),'--rewrite-timestamp',
         '--build-arg','TILO_BASE_IMAGE='+identifier,
         '--build-arg','TILO_INSTALLER_DEV_SSH=0','--iidfile',str(iid),'--file',str(Path(context)/'Containerfile'),str(context)]
