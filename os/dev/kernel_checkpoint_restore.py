"""Offline kernel7864 recipe derivation and admitted checkpoint restoration."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile

SOURCE = '33e83f64cde9df4ee65ff47a708f125d932256da'
RECIPE = '8308652298b8fb5b00f0fbacba9c5dcb796b1b628f6b296e2663e5a829f4ceb2'
KSRC = '/work/rpmbuild/BUILD/kernel-7.2.5-build/kernel-7.2.5/linux-7.2.5-200.fc44.aarch64'
CERT_OUTPUTS = {'signing_key.x509', 'x509_certificate_list', 'x509_revocation_list', 'blacklist_hash_list'}

def require(ok, message):
    if not ok: raise ValueError(message)

def sha(path):
    h=hashlib.sha256()
    with open(path,'rb') as f:
        for block in iter(lambda:f.read(1024*1024),b''): h.update(block)
    return h.hexdigest()

def read_json(path, expected):
    require(Path(path).stat().st_size <= 64*1024**2, 'metadata bound')
    require(sha(path)==expected, 'independent metadata hash mismatch')
    return json.loads(Path(path).read_bytes())

def relative(value):
    p=PurePosixPath(value)
    require(value and not p.is_absolute() and '..' not in p.parts and str(p)==value, 'noncanonical relative path')
    return p

def invalidations(root, active):
    """Keep leaf objects; discard aggregates, cert outputs and active products."""
    removed=set(active)
    for path in root.rglob('*'):
        if not path.is_file() or path.is_symlink(): continue
        rel=path.relative_to(root).as_posix(); name=path.name
        if (name.endswith(('.a','.ko','.mod','.mod.c','.mod.o')) or
            name in ('System.map','Module.symvers','modules.order','modules.builtin','modules.builtin.modinfo') or
            name.startswith(('vmlinux','.tmp_vmlinux','.tmp_kallsyms')) or
            rel.startswith('arch/arm64/boot/Image') or
            rel in ('arch/arm64/boot/vmlinuz.efi', 'arch/arm64/boot/image.fit') or
            (rel.startswith('certs/') and (name.endswith('.o') or name in CERT_OUTPUTS))):
            removed.add(rel)
        if name.endswith('.cmd'):
            require(path.stat().st_size<=4*1024**2, 'command record bound')
            text=path.read_text()
            # Kbuild linker/ar composite outputs need regeneration; unknown
            # generated output classes must be covered by independent review.
            if re.search(r'(?m)^(?:savedcmd|cmd)_.*:=.*(?:\b(?:ld|ld\.lld|ar|llvm-ar)\b|\s-r\s)',text):
                target=path.with_name(name[1:-4]).relative_to(root).as_posix()
                removed.update((rel,target))
    for value in list(removed):
        p=relative(value); removed.update((str(p.parent/('.'+p.name+'.cmd')),str(p.parent/('.'+p.name+'.d'))))
    for value in sorted(removed):
        p=root/relative(value)
        require(not p.is_symlink(), 'invalidated output is a symlink')
        require(not p.is_dir(), 'invalidated output is a directory')
        if p.exists(): p.unlink()
    return sorted(removed)

def unpack(archive, scan, destination):
    records={x['path']:x for x in scan['capture']['files']}
    require(len(records)==len(scan['capture']['files']) and len(records)<=250000, 'inventory duplicates/bound')
    seen=set(); directories=[]; links=[]; total=0
    link_paths=set()
    with tarfile.open(archive,'r:') as tar:
        for m in tar:
            relative(m.name)
            require(not any(str(parent) in link_paths for parent in PurePosixPath(m.name).parents), 'entry traverses archive symlink')
            require(m.name not in seen, 'duplicate archive entry'); seen.add(m.name)
            if m.name=='.tilo-checkpoint.json':
                require(m.isfile() and m.size<=64*1024**2,'capture metadata bound')
                data=tar.extractfile(m).read()
                require(hashlib.sha256(data).hexdigest()==scan['file_sha256'][m.name] and json.loads(data)==scan['capture'],'capture metadata differs')
                continue
            require(m.name in records and (m.isfile() or m.isdir() or m.issym()), 'archive inventory/type differs')
            r=records[m.name]; target=destination/m.name
            require(m.isfile()==(m.name in scan['file_sha256']), 'regular file inventory type differs')
            require(m.mode==r['mode'] and not m.mode & 0o6000,'mode differs or privileged')
            # Delay all links until files have been created; never extract through one.
            target.parent.mkdir(parents=True,exist_ok=True)
            if m.isdir(): target.mkdir(exist_ok=True); directories.append((target,r))
            elif m.issym():
                require(not any(value.startswith(m.name+'/') for value in seen), 'symlink replaces archive parent')
                link_paths.add(m.name)
                require(len(m.linkname.encode())==r['size'], 'symlink size differs')
                require(not PurePosixPath(m.linkname).is_absolute(),'absolute link')
                resolved=os.path.normpath(os.path.join(os.path.dirname(m.name),m.linkname))
                relative(resolved); links.append((target,m.linkname,r))
            else:
                total+=m.size; require(total<=8*1024**3 and m.size==r['size'],'payload bound/size')
                with target.open('xb') as out: shutil.copyfileobj(tar.extractfile(m),out,1024*1024)
                require(sha(target)==scan['file_sha256'][m.name],'file hash differs')
                os.chmod(target,m.mode); os.utime(target,ns=(r['mtime_ns'],r['mtime_ns']))
    require(seen==set(records)|{'.tilo-checkpoint.json'},'archive missing inventory')
    for target,link,r in links:
        require(not target.exists(),'link collides with parent directory')
        target.symlink_to(link); os.utime(target,ns=(r['mtime_ns'],r['mtime_ns']),follow_symlinks=False)
    for target,r in reversed(directories):
        os.chmod(target,r['mode']); os.utime(target,ns=(r['mtime_ns'],r['mtime_ns']))
    require(not(set(scan['capture']['invalidated']) & set(records)), 'capture retained invalidated files')

def replace_prepared(root, replacement):
    # Production caller pins root to KSRC. Fixture callers use an isolated temp.
    require(root.is_dir() and not root.is_symlink() and root.resolve()==root,
            'prepared source path changed')
    require(replacement.is_dir() and not replacement.is_symlink(), 'replacement changed')
    backup=root.with_name(root.name+'.before-resume')
    require(not backup.exists() and not backup.is_symlink(),'prior restore exists')
    root.rename(backup)
    try: replacement.rename(root)
    except BaseException:
        backup.rename(root)
        raise
    require(backup.parent==root.parent and backup.name==root.name+'.before-resume'
            and backup.resolve()==backup and backup.is_dir() and not backup.is_symlink(),
            'prepared backup path changed')
    shutil.rmtree(backup)


def verify_toolchain(expected, command=subprocess.check_output):
    names=expected['rpm_nevras']
    require(names==sorted(names) and 0<len(names)<=10000,'RPM inventory order/bound')
    require(hashlib.sha256(('\n'.join(names)+'\n').encode()).hexdigest()==expected['rpm_nevras_sha256'],
            'expected RPM inventory hash differs')
    actual=command(['/usr/bin/rpm','-qa','--qf','%{NAME}-%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\n'],text=True)
    require(len(actual)<=4*1024**2 and sorted(actual.splitlines())==names,'installed RPM inventory differs')
    records=expected['executables']
    required={'/usr/bin/gcc','/usr/bin/as','/usr/bin/ld','/usr/bin/make',
              '/usr/libexec/gcc/aarch64-redhat-linux/16/cc1'}
    require(len(records)==5 and {r['requested'] for r in records}==required,'compiler executable inventory differs')
    for record in records:
        path=Path(record['requested'])
        require(path.is_file() and str(path.resolve())==record['resolved'],'compiler resolved path differs')
        require(path.stat().st_size==record['size'] and sha(path)==record['sha256'],'compiler executable bytes differ')


def read_capture_receipt(path, approval):
    capture_attempt=read_json(path,approval['capture_receipt_sha256'])
    require(capture_attempt.get('success') is True, 'capture transaction did not succeed')
    receipt=capture_attempt['capture']
    require(receipt['archive_sha256']==approval['archive_sha256']
            and receipt['scan_sha256']==approval['scan_sha256'], 'capture transaction artifact binding differs')
    return receipt


def restore(directory, approval_sha):
    approval=read_json(directory/'approval.json',approval_sha)
    require(approval['purpose']=='tilo-kbuild-reconstruction-admission' and approval['source_revision']==SOURCE,'wrong approval purpose/source')
    for field in ('writer_mapping_complete','dependency_invalidation_reviewed','toolchain_identity_required','excluded_selftests_not_build_inputs'):
        require(approval.get(field) is True,'unresolved admission: '+field)
    require(approval['restore_helper_sha256']==sha(__file__), 'restore helper differs')
    toolchain=read_json(directory/'toolchain.json',approval['toolchain_evidence_sha256'])
    verify_toolchain(toolchain['toolchain'])
    scan=read_json(directory/'scan.json',approval['scan_sha256'])
    receipt=read_capture_receipt(directory/'capture-receipt.json',approval)
    require(scan['purpose']=='scanned-incomplete-kbuild-checkpoint' and scan['private_marker_scan_passed'] is True,'scan not admitted')
    require(scan['capture']['root']==KSRC and scan['capture']['fresh_key_required_in_resume_run'] is True,'capture root/key contract differs')
    require(receipt['source_revision']==SOURCE and receipt['actual_source_rpm_sha256']==sha('/work/kernel.src.rpm'),'SRPM differs')
    require(receipt['resolved_config_sha256']==sha(Path(KSRC)/'.config'),'prepared config differs')
    versions=[subprocess.check_output([command,'--version']).decode().splitlines()[0] for command in ('gcc','ld','make')]
    require(versions==receipt['toolchain_versions'],'toolchain version differs')
    require(os.environ.get('KBUILD_BUILD_TIMESTAMP')=='@1789697917','epoch differs')
    require(sha(directory/'tree.tar')==approval['archive_sha256'],'archive differs')
    root=Path(KSRC); require(root.is_dir() and not root.is_symlink(),'prepared KSRC missing')
    with tempfile.TemporaryDirectory(prefix='tilo-restored-',dir=root.parent) as temporary:
        replacement=Path(temporary)/'tree'; replacement.mkdir()
        unpack(directory/'tree.tar',scan,replacement)
        require(sha(replacement/'.config')==receipt['resolved_config_sha256'],'restored config differs')
        removed=invalidations(replacement,scan['capture']['invalidated']+approval['additional_invalidations'])
        # Original prepared tree is retained until the entire replacement validates.
        replace_prepared(root, replacement)
    (Path('/work')/'kernel-reconstruction-invalidations.json').write_text(json.dumps(removed))
    # Root and captured source remain separate from this new private identity.
    with tempfile.TemporaryDirectory(prefix='module-fresh-',dir='/work') as temporary:
        fresh=Path(temporary)
        subprocess.run(['tilo-crypto','generate-secure-boot-cert','Tilo ephemeral kernel module signing key',str(fresh/'signing-key.pem'),str(fresh/'signing-cert.pem')],check=True)
        subprocess.run(['tilo-crypto','certificate-der',str(fresh/'signing-cert.pem'),str(fresh/'signing-cert.der')],check=True)
        for name in ('signing-key.pem','signing-cert.pem','signing-cert.der'):
            os.replace(fresh/name,Path('/work/module-signing')/name)

def assert_embedded(work=Path('/work'), root=None):
    root=Path(KSRC) if root is None else root
    cert=(work/'module-signing/signing-cert.der').read_bytes()
    require(0<len(cert)<=65536,'certificate bound')
    require((root/'certs/signing_key.x509').read_bytes()==cert,'new cert not regenerated')
    tail=b''; found=False
    with (root/'arch/arm64/boot/Image').open('rb') as image:
        for block in iter(lambda:image.read(1024*1024),b''):
            found |= cert in tail+block; tail=(tail+block)[-len(cert):]
    require(found,'new certificate absent from kernel Image')
    invalidations_path=work/'kernel-reconstruction-invalidations.json'
    removed=json.loads(invalidations_path.read_bytes())
    require(isinstance(removed,list) and all(isinstance(x,str) for x in removed), 'invalidations manifest malformed')
    evidence=dict(schema=1, purpose='tilo-kernel-reconstruction-runtime-evidence',
                  invalidations_sha256=sha(invalidations_path), invalidated_paths=len(removed),
                  module_certificate_der_sha256=hashlib.sha256(cert).hexdigest(),
                  regenerated_certificate_matches=True, embedded_certificate_verified=True)
    (work/'kernel-reconstruction-evidence.json').write_text(json.dumps(evidence,sort_keys=True,indent=2)+'\n')

EXPORT_STAGE = b'''\n# Public reconstruction evidence only; original publish and packaging stay intact.
FROM publish AS reconstructed-publish
COPY --from=build /work/kernel-reconstruction-evidence.json /reconstruction/runtime.json
COPY --from=build /work/kernel-reconstruction-invalidations.json /reconstruction/invalidations.json
'''

def derive(original, approval_sha):
    require(hashlib.sha256(original).hexdigest()==RECIPE,'original recipe differs')
    require(re.fullmatch('[0-9a-f]{64}',approval_sha),'approval SHA required')
    text=original.decode(); marker='    make -j"$JOBS" all; \\\n'
    require(text.count(marker)==1,'make seam differs')
    start=text.rfind('RUN set -eux; \\\n',0,text.index(marker))
    require(start>=0,'RUN seam missing')
    text=text[:start]+text[start:].replace('RUN set -eux; \\\n','RUN --mount=type=bind,from=kernel-checkpoint,target=/checkpoint,ro set -eux; \\\n',1)
    text=text.replace(marker,
        f'    python3 /checkpoint/kernel_checkpoint_restore.py restore --approval-sha256 {approval_sha}; \\\n    cd "$KSRC"; \\\n'+marker+'    python3 /checkpoint/kernel_checkpoint_restore.py assert-embedded; \\\n',1)
    return text.encode()+EXPORT_STAGE

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['derive','restore','assert-embedded']);p.add_argument('--approval-sha256');p.add_argument('--original',type=Path);p.add_argument('--output',type=Path);a=p.parse_args()
    if a.action=='derive':
        with a.output.open('xb') as f:f.write(derive(a.original.read_bytes(),a.approval_sha256))
    elif a.action=='restore':restore(Path('/checkpoint'),a.approval_sha256)
    else:assert_embedded()
