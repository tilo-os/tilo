#!/usr/bin/env python3
"""Read-only original CI boot-input fixture probe for VM-only helper refresh.

This is development evidence, never CI/release or physical admission authority.
It deliberately does not import or hash every layer again.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import shutil
import tarfile
import struct

HELPER = Path('/usr/libexec/tilo-installer-dev-target-uki.py')
FIXTURE_DEVICE = Path('/dev/disk/by-id/virtio-tilo-code-fixture')
SYS_BLOCK = Path('/sys/class/block')
SCRATCH_ROOT = Path('/run')


def require(ok, message):
    if not ok: raise ValueError(message)


def run(*args, cwd=None):
    result = subprocess.run(args, capture_output=True, text=True, timeout=30, cwd=cwd)
    diagnostic = result.stderr[-4096:]
    require(result.returncode == 0, 'boot-input fixture probe: ' + diagnostic)
    return result.stdout.strip()


def bounded(path, limit):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= limit, 'unsafe boot-input fixture metadata')
    return path.read_bytes()


def non_cmdline_bytes(raw):
    data=bytearray(raw); require(len(data)>64 and data[:2]==b'MZ','invalid fixture PE')
    pe=struct.unpack_from('<I',data,60)[0]
    require(pe+24<=len(data) and data[pe:pe+4]==b'PE\0\0','invalid fixture PE header')
    count=struct.unpack_from('<H',data,pe+6)[0]; optional=struct.unpack_from('<H',data,pe+20)[0]
    table=pe+24+optional; matched=0
    require(count<=96 and table+count*40<=len(data),'invalid fixture PE table')
    for number in range(count):
        offset=table+number*40
        if data[offset:offset+8].rstrip(b'\0')==b'.cmdline':
            matched+=1; size,start=struct.unpack_from('<II',data,offset+16)
            require(0<size<=65536 and start+size<=len(data),'invalid cmdline fixture bounds')
            data[start:start+size]=bytes(size); data[offset+8:offset+12]=bytes(4)
    require(matched==1,'fixture requires exactly one command-line section')
    return data


def promotion_probe(helper, root, manifest, version):
    metadata=manifest['promotion']
    require(type(metadata) is dict, 'invalid scratch promotion fixture')
    digest=metadata.get('composefs_digest','')
    require(re.fullmatch('[0-9a-f]{128}',digest) and all(type(metadata.get(key)) is str and 0<len(metadata[key])<=16384
            for key in ('origin','loader','update_origin')),'invalid scratch promotion fixture')
    target=root/'promotion-target'; esp=root/'promotion-esp'; deployment=target/'state/deploy'/digest
    (deployment/'etc').mkdir(parents=True); origin=deployment/(digest+'.origin'); origin.write_text(metadata['origin'])
    entry=esp/'loader/entries/fixture.conf'; entry.parent.mkdir(parents=True); entry.write_text(metadata['loader'])
    original=root/'boot/EFI/Linux'/(version+'.efi')
    destination=esp/'EFI/Linux/bootc'/('bootc_composefs-'+digest+'.efi'); destination.parent.mkdir(parents=True)
    shutil.copyfile(original,destination); before=destination.read_bytes()
    selected={'target_digest':manifest['target_digest'],'update_origin':metadata['update_origin']}
    result=helper.promote(root,target,esp,selected)
    after=destination.read_bytes()
    require(result==digest and origin.read_text()==metadata['origin'] and entry.read_text()==metadata['loader'],
            'scratch promotion changed OCI origin or loader')
    require(non_cmdline_bytes(before)==non_cmdline_bytes(after),'scratch promotion changed non-command-line PE bytes')
    return {'method':'scratch-bootc-format-origin','physical_admission':False,'composefs_digest':result,
            'origin_preserved':True,'loader_preserved':True,'non_cmdline_pe_bytes_preserved':True,
            'input_uki_sha256':hashlib.sha256(before).hexdigest(),'output_uki_sha256':hashlib.sha256(after).hexdigest()}


def fixture_probe(helper):
    device = FIXTURE_DEVICE
    require(device.is_block_device(), 'read-only original CI input fixture missing')
    actual = device.resolve(strict=True)
    require((SYS_BLOCK/actual.name/'ro').read_text().strip() == '1', 'fixture is writable')
    size = int(run('blockdev','--getsize64',str(actual)))
    require(1024*1024 <= size <= 512*1024*1024, 'fixture outside 1-512 MiB bound')
    sha = hashlib.sha256()
    with device.open('rb') as stream:
        for chunk in iter(lambda:stream.read(1024*1024), b''): sha.update(chunk)
    with tempfile.TemporaryDirectory(prefix='tilo-boot-fixture-',dir=SCRATCH_ROOT) as temporary:
        root=Path(temporary); files={}; manifest=None; total=0
        with device.open('rb') as stream, tarfile.open(fileobj=stream,mode='r|') as archive:
            for member in archive:
                require(member.isfile() and member.name not in files and len(files)<16,
                        'fixture requires unique bounded regular files')
                parts=Path(member.name).parts
                require(parts and not any(part in ('.','..','/') for part in parts)
                        and Path(member.name).as_posix() == member.name and len(member.name) <= 256, 'unsafe fixture path')
                require(member.size>0 and member.size<=256*1024*1024, 'fixture member outside bound')
                total+=member.size; require(total<=512*1024*1024, 'fixture total exceeds bound')
                content=archive.extractfile(member)
                destination=root/member.name; destination.parent.mkdir(parents=True,exist_ok=True)
                with destination.open('wb') as output: shutil.copyfileobj(content,output)
                files[member.name]=hashlib.sha256(destination.read_bytes()).hexdigest()
                if member.name=='fixture.json':
                    require(member.size<=16384,'oversized fixture manifest'); manifest=json.loads(destination.read_text())
        require(manifest and manifest.get('schema')=='tilo.installer-code-boot-fixture.v1'
                and manifest.get('physical_admission') is False
                and re.fullmatch('sha256:[0-9a-f]{64}',manifest.get('target_digest','')), 'invalid fixture manifest')
        require(manifest.get('files')=={name:value for name,value in files.items() if name!='fixture.json'},
                'fixture bytes differ from input manifest')
        version,kernel,initrd,dtb=helper.image_inputs(root)
        profile=json.loads(bounded(root/'usr/lib/tilo/hardware-profile.json',16384))
        promotion=promotion_probe(helper,root,manifest,version) if 'promotion' in manifest else None
        return {'promotion':promotion,'schema':'tilo.installer-code-cached-target-probe.v1','physical_admission':False,
                'target_digest':manifest['target_digest'],'profile':profile['id'],'kernel_version':version,
                'mount':{'method':'readonly-original-ci-boot-fixture','cache_authority':'none',
                         'uki_fixture':{'sha256':sha.hexdigest(),'bytes':size,'physical_admission':False}},
                'input_sha256':files,'kernel_sha256':hashlib.sha256(kernel.read_bytes()).hexdigest(),
                'initrd_sha256':hashlib.sha256(initrd.read_bytes()).hexdigest(),
                'dtb_sha256':hashlib.sha256(dtb.read_bytes()).hexdigest() if dtb else None,
                'validator_sha256':hashlib.sha256(HELPER.read_bytes()).hexdigest()}


def probe():
    require(os.geteuid() == 0 and Path('/usr/share/tilo/unverified-vm-install').is_file()
            and Path('/usr/lib/tilo/image-flavour').read_bytes() == b'dev\n'
            and all(Path('/dev/virtio-ports/' + name).is_char_device()
                    for name in ('org.tilo.installer-code', 'org.tilo.vm-install-proof'))
            and 'tilo.install' in Path('/proc/cmdline').read_text().split(), 'boot-input probe requires development scenario VM')
    run('systemd-detect-virt', '--vm', '--quiet')
    if Path('/dev/disk/by-id/virtio-tilo-code-fixture').exists():
        spec=importlib.util.spec_from_file_location('original_ci_input_validator',HELPER)
        helper=importlib.util.module_from_spec(spec); spec.loader.exec_module(helper)
        return fixture_probe(helper)
    raise ValueError('pure target validator probe requires original CI boot-input fixture')
