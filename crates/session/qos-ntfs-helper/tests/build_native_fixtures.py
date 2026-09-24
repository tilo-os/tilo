#!/usr/bin/env python3
"""Generate disposable native images; refuse an existing output directory."""
import argparse, hashlib, json, pathlib, shutil, subprocess, sys
ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from build_native import build, SHA
ap = argparse.ArgumentParser()
ap.add_argument('--output', required=True)
ap.add_argument('--tools-output', default='/tmp/qos-ntfs-stage1-tools-v2')
a = ap.parse_args()
out = pathlib.Path(a.output).resolve(); out.mkdir(mode=0o700)
src = build(a.tools_output, True)
exe = out / 'fixture-builder'
subprocess.run(['cc', '-D_DARWIN_C_SOURCE', '-D_GNU_SOURCE', '-DHAVE_CONFIG_H', '-I'+str(src), '-I'+str(src/'include/ntfs-3g'), str(ROOT/'tests/fixture_builder.c'), str(src/'libntfs-3g/.libs/libntfs-3g.a'), *(['-liconv'] if sys.platform == 'darwin' else []), '-o', str(exe)], check=True)
def run(*args): subprocess.run(list(map(str,args)), check=True)
clean = out/'clean.img'
with clean.open('xb') as f: f.truncate(64*1024*1024)
run(src/'ntfsprogs/mkntfs', '-F', '-Q', '-T', '-L', 'QOS_FIXTURE', clean)
content = out/'content'; content.write_bytes(b'Connected NTFS fixture\n')
run(src/'ntfsprogs/ntfscp', clean, content, '/hello.txt')
run(exe, clean, 'serial')
for name, mode in [('times','times'), ('times-v1','times-v1'), ('times-missing','times-missing'), ('times-duplicate','times-duplicate'), ('times-invalid-flags','times-invalid-flags'), ('dos-alias','dos-alias'), ('large-metadata','large-metadata'), ('restart-clean','restart-clean'), ('restart-bad-first','restart-bad-first'), ('restart-bad-second','restart-bad-second'), ('restart-v2','restart-v2'), ('dirty','dirty'), ('badlog','badlog'), ('generation','generation'), ('metadata','metadata'), ('upcase','upcase'), ('oversize','oversize'), ('attrlist-cycle','attrlist-cycle'), ('acl','acl'), ('hardlink','hardlink')]:
    dest=out/(name+'.img'); shutil.copyfile(clean,dest); run(exe,dest,mode)
large = out/'large.img'; shutil.copyfile(clean, large)
content.write_bytes(b''.join(bytes((j+block)%256 for j in range(256)) * 4096 for block in range(12))); run(src/'ntfsprogs/ntfscp', large, content, '/large.bin')
content.write_bytes(bytes(range(251))*17); run(src/'ntfsprogs/ntfscp', large, content, '/tail.bin')
content.write_bytes(b''); run(src/'ntfsprogs/ntfscp', large, content, '/empty.bin')
run(exe, large, 'cancellation')
content.write_bytes(b'final chunk'); run(src/'ntfsprogs/ntfscp', large, content, '/cancel/last.bin')
copy_tree = out/'copy-tree.img'; shutil.copyfile(large, copy_tree); run(exe, copy_tree, 'copy-directories')
for name, data in [('zero-hiber',bytes(4096)), ('hiber',b'hibr'+bytes(4092)), ('nonzero-hiber',bytes(4095)+b'x'), ('short-hiber',bytes(32))]:
    dest=out/(name+'.img'); shutil.copyfile(clean,dest)
    content.write_bytes(data); run(src/'ntfsprogs/ntfscp',dest,content,'/hiberfil.sys')
shutil.copyfile(out/'zero-hiber.img',out/'hiber-io.img'); run(exe,out/'hiber-io.img','hiber-io')
for name in ['bitlocker','truncated','corrupt']:
    dest=out/(name+'.img'); shutil.copyfile(clean,dest)
    with dest.open('r+b') as f:
        if name=='bitlocker': f.seek(3); f.write(b'-FVE-FS-')
        elif name=='truncated': f.truncate(300)
        else: f.seek(3); f.write(b'BROKEN!!')
content.write_bytes(b'ordinary sibling\n')
for name in ['times','times-v1','times-duplicate','times-invalid-flags','metadata','acl','hardlink','dos-alias']:
    scoped = out/('copy-scoped-'+name+'.img'); shutil.copyfile(out/(name+'.img'), scoped)
    run(src/'ntfsprogs/ntfscp', scoped, content, '/ordinary.txt')
manifest={'fixture_builder_sha256':hashlib.sha256((ROOT/'tests/fixture_builder.c').read_bytes()).hexdigest(),'source_sha256':SHA, 'format':'mkntfs -F -Q -T; serial set through NTFS_BOOT_SECTOR', 'expected':{'serial':'1122334455667788','root_record':5,'hello_content_hex':b'Connected NTFS fixture\n'.hex()},'images':{}}
for f in sorted(out.glob('*.img')):
    manifest['images'][f.name]={'sha256':hashlib.sha256(f.read_bytes()).hexdigest(),'bytes':f.stat().st_size}
manifest['expected']['large_bytes']=12*1024*1024
manifest['expected']['large_sha256']=hashlib.sha256(b''.join(bytes((j+block)%256 for j in range(256))*4096 for block in range(12))).hexdigest()
manifest['expected']['large_pattern']='byte=(offset%256 + offset//1048576)%256'
manifest['expected_by_image']={
 'restart-clean.img':{'status':0,'paired_restart_pages':'1.1'},
 'restart-bad-first.img':{'status':3,'invalid_page':0},
 'restart-bad-second.img':{'status':3,'invalid_page':1},
 'restart-v2.img':{'status':3,'paired_restart_pages':'2.0'},
 'clean.img':{'status':0,'root_sequence':5,'hello_record':64},
 'dirty.img':{'status':3},'badlog.img':{'status':'nonzero refusal'},
 'zero-hiber.img':{'status':0},'hiber.img':{'status':4},'nonzero-hiber.img':{'status':4},'short-hiber.img':{'status':4},
 'hiber-io.img':{'status':'nonzero refusal'},'bitlocker.img':{'status':2,'synthetic_signature_only':True},
 'truncated.img':{'status':'nonzero refusal'},'corrupt.img':{'status':1},'oversize.img':{'status':6},
 'generation.img':{'status':0,'root_sequence':42},'attrlist-cycle.img':{'status':'nonzero refusal'},
 'metadata.img':{'status':0,'root_named_data_utf16le':'640069007200','native_reparse_attribute':192},
 'upcase.img':{'status':0,'name_utf16le':'e4002e00740078007400','key_utf16le':'d6002e00540058005400'},
 'acl.img':{'status':0,'hello_record':64,'native_security_id':258,'source':'$Secure:$SDS via $SII'},
 'hardlink.img':{'status':0,'hello_links':2,'additional_name':'second'},
 'times.img':{'status':0,'standard_information_bytes':72,'last_data_change_100ns':116444735999999999,'last_access_100ns':116444736012345678},
 'times-v1.img':{'status':0,'standard_information_bytes':48,'last_data_change_100ns':116444735999999999,'last_access_100ns':116444736012345678},
 'times-missing.img':{'status':'nonzero structural refusal'},
 'times-duplicate.img':{'status':0,'copy_times_unavailable_reason':'duplicate-standard-information'},
 'times-invalid-flags.img':{'status':0,'copy_times_unavailable_reason':'invalid-standard-information-coverage'}
}
(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(out/'manifest.json')
