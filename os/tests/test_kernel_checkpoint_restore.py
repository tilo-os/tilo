import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
import hashlib
import json
import tarfile
import io
from unittest.mock import patch
from types import SimpleNamespace

spec=importlib.util.spec_from_file_location('restore',Path(__file__).resolve().parents[1]/'dev/kernel_checkpoint_restore.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
class Restoration(unittest.TestCase):
 def test_real_make_preserves_completed_leaf_and_rebuilds_certificate_and_archive(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'certs').mkdir()
   (root/'leaf.c').write_text('leaf')
   (root/'identity').write_text('oldcert')
   (root/'Makefile').write_text('all: Image\nleaf.o: leaf.c\n\tcp $< $@\ncerts/signing_key.x509: identity\n\tcp $< $@\ncerts/system_certificates.o: certs/signing_key.x509\n\tcp $< $@\nbuilt-in.a: leaf.o certs/system_certificates.o\n\tcat $^ > $@\nImage: built-in.a\n\tcp $< $@\n')
   subprocess.run(['make','-s','-C',d],check=True)
   before=(root/'leaf.o').stat().st_mtime_ns
   (root/'identity').write_text('freshcert')
   removed=m.invalidations(root,['Image'])
   self.assertIn('certs/system_certificates.o',removed)
   self.assertFalse((root/'built-in.a').exists())
   subprocess.run(['make','-s','-C',d],check=True)
   self.assertEqual(before,(root/'leaf.o').stat().st_mtime_ns)
   self.assertEqual((root/'Image').read_text(),'leaffreshcert')
 def test_active_output_and_composite_invalidated(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d)
   for name in ['leaf.o','active.o','.active.o.cmd','joined.o']:(root/name).write_text('x')
   (root/'.joined.o.cmd').write_text('savedcmd_joined.o := ld -r -o joined.o leaf.o\n')
   m.invalidations(root,['active.o'])
   self.assertTrue((root/'leaf.o').exists())
   for name in ['active.o','.active.o.cmd','joined.o','.joined.o.cmd']:self.assertFalse((root/name).exists())
 def test_invalid_active_path_rejected(self):
  with tempfile.TemporaryDirectory() as d:
   with self.assertRaises(ValueError):m.invalidations(Path(d),['../escape'])
 def test_archive_hash_and_mtime_validation(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);data=b'completed object';stamp=1234567890123456789
   capture={'files':[{'path':'leaf.o','mode':420,'size':len(data),'mtime_ns':stamp}],'invalidated':[]}
   meta=json.dumps(capture).encode();archive=root/'tree.tar'
   with tarfile.open(archive,'w') as t:
    for name,content in [('leaf.o',data),('.tilo-checkpoint.json',meta)]:
     info=tarfile.TarInfo(name);info.mode=420;info.size=len(content);t.addfile(info,io.BytesIO(content))
   scan={'capture':capture,'file_sha256':{'leaf.o':hashlib.sha256(data).hexdigest(),'.tilo-checkpoint.json':hashlib.sha256(meta).hexdigest()}}
   target=root/'valid';target.mkdir();m.unpack(archive,scan,target)
   self.assertEqual((target/'leaf.o').stat().st_mtime_ns,stamp)
   scan['file_sha256']['leaf.o']='0'*64
   target=root/'bad';target.mkdir()
   with self.assertRaisesRegex(ValueError,'file hash'):m.unpack(archive,scan,target)
 def test_replacement_removes_prepared_backup(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d)/'source';root.mkdir();(root/'old').write_text('old')
   new=Path(d)/'new';new.mkdir();(new/'valid').write_text('verified')
   m.replace_prepared(root,new)
   self.assertEqual((root/'valid').read_text(),'verified')
   self.assertFalse(root.with_name('source.before-resume').exists())
 def test_replacement_rename_failure_rolls_back(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d)/'source';root.mkdir();(root/'old').write_text('old')
   new=Path(d)/'new';new.mkdir();original=Path.rename
   def fail(path,target):
    if path==new:raise OSError('fixture rename failure')
    return original(path,target)
   with patch.object(Path,'rename',fail):
    with self.assertRaises(OSError):m.replace_prepared(root,new)
   self.assertEqual((root/'old').read_text(),'old')
   self.assertFalse(root.with_name('source.before-resume').exists())
 def test_toolchain_requires_actual_rpm_inventory(self):
  expected={'rpm_nevras':['fixture-0:1-1.aarch64'],'rpm_nevras_sha256':hashlib.sha256(b'fixture-0:1-1.aarch64\n').hexdigest(),'executables':[]}
  with self.assertRaisesRegex(ValueError,'installed RPM'):
   m.verify_toolchain(expected,lambda *a,**k:'different-0:1-1.aarch64\n')
  with self.assertRaisesRegex(ValueError,'compiler executable inventory'):
   m.verify_toolchain(expected,lambda *a,**k:'fixture-0:1-1.aarch64\n')
 def test_toolchain_rejects_changed_compiler_bytes(self):
  names=['/usr/bin/gcc','/usr/bin/as','/usr/bin/ld','/usr/bin/make','/usr/libexec/gcc/aarch64-redhat-linux/16/cc1']
  expected={'rpm_nevras':['fixture'],'rpm_nevras_sha256':hashlib.sha256(b'fixture\n').hexdigest(),
    'executables':[{'requested':n,'resolved':n,'size':1,'sha256':'a'*64} for n in names]}
  with patch.object(Path,'is_file',return_value=True), patch.object(Path,'resolve',lambda p:p), patch.object(Path,'stat',return_value=SimpleNamespace(st_size=1)), patch.object(m,'sha',return_value='a'*64):
   m.verify_toolchain(expected,lambda *a,**k:'fixture\n')
   with patch.object(m,'sha',return_value='b'*64):
    with self.assertRaisesRegex(ValueError,'compiler executable bytes'):
     m.verify_toolchain(expected,lambda *a,**k:'fixture\n')
 def test_capture_wrapper_and_hash_binding(self):
  with tempfile.TemporaryDirectory() as d:
   p=Path(d)/'receipt.json';value={'success':True,'capture':{'archive_sha256':'a'*64,'scan_sha256':'b'*64},'events':[]};p.write_text(json.dumps(value))
   approval={'capture_receipt_sha256':m.sha(p),'archive_sha256':'a'*64,'scan_sha256':'b'*64}
   self.assertEqual(m.read_capture_receipt(p,approval),value['capture'])
   approval['archive_sha256']='c'*64
   with self.assertRaisesRegex(ValueError,'artifact binding'):m.read_capture_receipt(p,approval)
 def test_archive_symlink_parent_rejected(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);archive=root/'tree.tar'
   with tarfile.open(archive,'w') as t:
    link=tarfile.TarInfo('alias');link.type=tarfile.SYMTYPE;link.linkname='real';link.mode=511;t.addfile(link)
    child=tarfile.TarInfo('alias/child');child.size=1;child.mode=420;t.addfile(child,io.BytesIO(b'x'))
   scan={'capture':{'files':[{'path':'alias','size':4,'mode':511,'mtime_ns':1},{'path':'alias/child','size':1,'mode':420,'mtime_ns':1}],'invalidated':[]},'file_sha256':{'alias/child':hashlib.sha256(b'x').hexdigest()}}
   target=root/'out';target.mkdir()
   with self.assertRaisesRegex(ValueError,'symlink'):m.unpack(archive,scan,target)
 def test_exact_recipe_and_tail(self):
  repo=Path(__file__).resolve().parents[3]
  original=subprocess.check_output(['git','-c','safe.directory='+str(repo),'-C',str(repo),'show',m.SOURCE+':os/kernel/Containerfile'])
  derived=m.derive(original,'a'*64)
  tail=b'    KVER="$(make -s kernelrelease)";'
  self.assertEqual(original[original.index(tail):],derived[derived.index(tail):])
  self.assertIn(b'assert-embedded',derived)
  self.assertIn(b'from=kernel-checkpoint',derived)
  with self.assertRaises(ValueError):m.derive(original+b'changed','a'*64)
if __name__=='__main__':unittest.main()
