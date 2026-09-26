import importlib.util
from pathlib import Path
import tempfile
import unittest
import hashlib
import errno
import json
import tarfile
import io
from unittest.mock import patch
from types import SimpleNamespace

from frozen_source_fixture import KERNEL_RECIPE

spec=importlib.util.spec_from_file_location('restore',Path(__file__).resolve().parents[1]/'dev/kernel_checkpoint_restore.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class Restoration(unittest.TestCase):
 def test_output_invalidation(self):
  with self.subTest(phase="active_output_and_composite_invalidated"):
   with tempfile.TemporaryDirectory() as d:
    root=Path(d)
    for name in ['leaf.o','active.o','.active.o.cmd','joined.o']:(root/name).write_text('x')
    (root/'.joined.o.cmd').write_text('savedcmd_joined.o := ld -r -o joined.o leaf.o\n')
    m.invalidations(root,['active.o'])
    self.assertTrue((root/'leaf.o').exists())
    for name in ['active.o','.active.o.cmd','joined.o','.joined.o.cmd']:self.assertFalse((root/name).exists())
  with self.subTest(phase="invalid_active_path_rejected"):
   with tempfile.TemporaryDirectory() as d:
    with self.assertRaises(ValueError):m.invalidations(Path(d),['../escape'])
  with self.subTest(phase="wrapped_arm_final_images_invalidated"):
   with tempfile.TemporaryDirectory() as d:
    root=Path(d);boot=root/'arch/arm64/boot';boot.mkdir(parents=True)
    for name in ('vmlinuz.efi','image.fit','.vmlinuz.efi.cmd','.image.fit.d'):
     (boot/name).write_text('stale final output')
    removed=m.invalidations(root,[])
    self.assertIn('arch/arm64/boot/vmlinuz.efi',removed)
    self.assertIn('arch/arm64/boot/image.fit',removed)
    self.assertEqual(list(boot.iterdir()),[])
  with self.subTest(phase="kernel_linker_source_inputs_survive_output_invalidation"):
   sources=('include/asm-generic/vmlinux.lds.h','arch/arm64/kernel/vmlinux.lds.S',
            'arch/x86/kernel/vmlinux.lds.S','tools/lib/bpf/vmlinux.h',
            'scripts/vmlinux.lds.h','arch/arm64/boot/Image.source',
            'vmlinux.lds.S')
   outputs=('vmlinux','vmlinux.o','.tmp_vmlinux.kallsyms1',
            'arch/arm64/boot/Image','arch/arm64/boot/Image.gz','certs/system_certificates.o')
   with tempfile.TemporaryDirectory() as d:
    root=Path(d)
    for name in sources+outputs:
     path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(name)
    removed=m.invalidations(root,[])
    for name in sources:
     self.assertEqual((root/name).read_text(),name)
     self.assertNotIn(name,removed)
    for name in outputs:self.assertFalse((root/name).exists())
  with self.subTest(phase="source_input_invalidation_fails_before_any_delete"):
   for source in ('include/asm-generic/vmlinux.lds.h','arch/arm64/kernel/vmlinux.lds.S','Makefile'):
    with self.subTest(source=source),tempfile.TemporaryDirectory() as d:
     root=Path(d);path=root/source;path.parent.mkdir(parents=True,exist_ok=True);path.write_text('source')
     (root/'vmlinux').write_text('output')
     with self.assertRaisesRegex(ValueError,'source input'):m.invalidations(root,[source])
     self.assertTrue((root/'vmlinux').exists());self.assertEqual(path.read_text(),'source')
 def test_archive_validation(self):
  with self.subTest(phase="archive_hash_and_mtime_validation"):
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
  restart_fixture(self)
  with self.subTest(phase="archive_symlink_parent_rejected"):
   with tempfile.TemporaryDirectory() as d:
    root=Path(d);archive=root/'tree.tar'
    with tarfile.open(archive,'w') as t:
     link=tarfile.TarInfo('alias');link.type=tarfile.SYMTYPE;link.linkname='real';link.mode=511;t.addfile(link)
     child=tarfile.TarInfo('alias/child');child.size=1;child.mode=420;t.addfile(child,io.BytesIO(b'x'))
    scan={'capture':{'files':[{'path':'alias','size':4,'mode':511,'mtime_ns':1},{'path':'alias/child','size':1,'mode':420,'mtime_ns':1}],'invalidated':[]},'file_sha256':{'alias/child':hashlib.sha256(b'x').hexdigest()}}
    target=root/'out';target.mkdir()
    with self.assertRaisesRegex(ValueError,'symlink'):m.unpack(archive,scan,target)
  restart_fixture(self)
  with self.subTest(phase="exact_recipe_and_tail"):
   original=KERNEL_RECIPE.encode()
   with self.assertRaisesRegex(ValueError,'original recipe differs'):m.derive(original,'a'*64)
   pin=patch.object(m,'RECIPE',hashlib.sha256(original).hexdigest());pin.start();self.addCleanup(pin.stop)
   derived=m.derive(original,'a'*64)
   tail=b'    KVER="$(make -s kernelrelease)";'
   self.assertEqual(original[original.index(tail):],derived[derived.index(tail):-len(m.EXPORT_STAGE)])
   self.assertTrue(derived.endswith(m.EXPORT_STAGE))
   self.assertIn(b'assert-embedded',derived)
   self.assertIn(b'from=kernel-checkpoint',derived)
   with self.assertRaises(ValueError):m.derive(original+b'changed','a'*64)
  restart_fixture(self)
  with self.subTest(phase="command_derived_source_header_is_rejected"):
   with tempfile.TemporaryDirectory() as d:
    root=Path(d);(root/'vmlinux.lds.h').write_text('source')
    (root/'.vmlinux.lds.h.cmd').write_text('savedcmd_vmlinux.lds.h := ld -r input.o -o vmlinux.lds.h')
    with self.assertRaisesRegex(ValueError,'source input'):m.invalidations(root,[])
    self.assertTrue((root/'vmlinux.lds.h').exists())
 def test_replacement_rollback(self):
  with self.subTest(phase="replacement_removes_prepared_backup"):
   with tempfile.TemporaryDirectory() as d:
    root=Path(d)/'source';root.mkdir();(root/'old').write_text('old')
    new=Path(d)/'new';new.mkdir();(new/'valid').write_text('verified')
    m.replace_prepared(root,new)
    self.assertEqual((root/'valid').read_text(),'verified')
    self.assertFalse(root.with_name('source.before-resume').exists())
  with self.subTest(phase="replacement_rename_failure_rolls_back"):
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
  with self.subTest(phase="overlay_exdev_success_and_failed_install_rollback"):
   for fail_install in (False,True):
    with self.subTest(fail_install=fail_install), tempfile.TemporaryDirectory() as d:
     parent=Path(d);root=parent/'source';root.mkdir();(root/'old').write_bytes(b'original')
     (root/'link').symlink_to('old');stamp=1700000000123456789
     import os
     os.utime(root/'old',ns=(stamp,stamp));os.chmod(root/'old',0o640)
     new=parent/'new';new.mkdir();(new/'valid').write_bytes(b'verified')
     rename=Path.rename
     def overlay_rename(path,target):
      if path==root:raise OSError(errno.EXDEV,'lower overlay directory')
      if path==new and fail_install:raise OSError(errno.EIO,'injected install failure')
      return rename(path,target)
     with patch.object(Path,'rename',overlay_rename):
      if fail_install:
       with self.assertRaises(OSError):m.replace_prepared(root,new)
      else:m.replace_prepared(root,new)
     self.assertFalse(parent.joinpath('source.before-resume').exists())
     if fail_install:
      self.assertEqual((root/'old').read_bytes(),b'original')
      self.assertEqual((root/'old').stat().st_mtime_ns,stamp)
      self.assertEqual((root/'old').stat().st_mode&0o777,0o640)
      self.assertTrue((root/'link').is_symlink())
     else:self.assertEqual((root/'valid').read_bytes(),b'verified')
  with self.subTest(phase="overlay_copy_failure_preserves_original"):
   with tempfile.TemporaryDirectory() as d:
    parent=Path(d);root=parent/'source';root.mkdir();(root/'old').write_text('original')
    new=parent/'new';new.mkdir()
    with patch.object(Path,'rename',side_effect=OSError(errno.EXDEV,'overlay')), patch.object(m.shutil,'copytree',side_effect=OSError(errno.ENOSPC,'full')):
     with self.assertRaises(OSError):m.replace_prepared(root,new)
    self.assertEqual((root/'old').read_text(),'original')
  with self.subTest(phase="overlay_rollback_failure_retains_complete_backup"):
   with tempfile.TemporaryDirectory() as d:
    parent=Path(d);root=parent/'source';root.mkdir();(root/'old').write_text('original')
    new=parent/'new';new.mkdir()
    def failing_rename(path,target):
     raise OSError(errno.EXDEV if path==root else errno.EIO,'injected')
    with patch.object(Path,'rename',failing_rename):
     with self.assertRaisesRegex(RuntimeError,'complete backup retained'):m.replace_prepared(root,new)
    self.assertEqual((parent/'source.before-resume/old').read_text(),'original')
  with self.subTest(phase="overlay_partial_removal_failure_restores_complete_original"):
   with tempfile.TemporaryDirectory() as d:
    parent=Path(d);root=parent/'source';root.mkdir();(root/'old').write_text('original');(root/'other').write_text('other')
    new=parent/'new';new.mkdir();rename=Path.rename;rmtree=m.shutil.rmtree;failed=False
    def overlay_rename(path,target):
     if path==root:raise OSError(errno.EXDEV,'overlay')
     return rename(path,target)
    def partial_remove(path,*a,**kw):
     nonlocal failed
     if path==root and not failed:
      failed=True;(root/'old').unlink();raise OSError(errno.EIO,'partial source removal')
     return rmtree(path,*a,**kw)
    with patch.object(Path,'rename',overlay_rename),patch.object(m.shutil,'rmtree',partial_remove):
     with self.assertRaises(OSError):m.replace_prepared(root,new)
    self.assertEqual((root/'old').read_text(),'original')
    self.assertEqual((root/'other').read_text(),'other')
 def test_toolchain_and_evidence(self):
  with self.subTest(phase="toolchain_requires_actual_rpm_inventory"):
   expected={'rpm_nevras':['fixture-0:1-1.aarch64'],'rpm_nevras_sha256':hashlib.sha256(b'fixture-0:1-1.aarch64\n').hexdigest(),'executables':[]}
   with self.assertRaisesRegex(ValueError,'installed RPM'):
    m.verify_toolchain(expected,lambda *a,**k:'different-0:1-1.aarch64\n')
   with self.assertRaisesRegex(ValueError,'compiler executable inventory'):
    m.verify_toolchain(expected,lambda *a,**k:'fixture-0:1-1.aarch64\n')
  with self.subTest(phase="toolchain_rejects_changed_compiler_bytes"):
   names=['/usr/bin/gcc','/usr/bin/as','/usr/bin/ld','/usr/bin/make','/usr/libexec/gcc/aarch64-redhat-linux/16/cc1']
   expected={'rpm_nevras':['fixture'],'rpm_nevras_sha256':hashlib.sha256(b'fixture\n').hexdigest(),
     'executables':[{'requested':n,'resolved':n,'size':1,'sha256':'a'*64} for n in names]}
   with patch.object(Path,'is_file',return_value=True), patch.object(Path,'resolve',lambda p:p), patch.object(Path,'stat',return_value=SimpleNamespace(st_size=1)), patch.object(m,'sha',return_value='a'*64):
    m.verify_toolchain(expected,lambda *a,**k:'fixture\n')
    with patch.object(m,'sha',return_value='b'*64):
     with self.assertRaisesRegex(ValueError,'compiler executable bytes'):
      m.verify_toolchain(expected,lambda *a,**k:'fixture\n')
  with self.subTest(phase="capture_wrapper_and_hash_binding"):
   with tempfile.TemporaryDirectory() as d:
    p=Path(d)/'receipt.json';value={'success':True,'capture':{'archive_sha256':'a'*64,'scan_sha256':'b'*64},'events':[]};p.write_text(json.dumps(value))
    approval={'capture_receipt_sha256':m.sha(p),'archive_sha256':'a'*64,'scan_sha256':'b'*64}
    self.assertEqual(m.read_capture_receipt(p,approval),value['capture'])
    approval['archive_sha256']='c'*64
    with self.assertRaisesRegex(ValueError,'artifact binding'):m.read_capture_receipt(p,approval)
  with self.subTest(phase="actual_public_runtime_evidence_and_missing_certificate_refusal"):
   with tempfile.TemporaryDirectory() as d:
    work=Path(d);root=work/'source'
    (work/'module-signing').mkdir();(root/'certs').mkdir(parents=True)
    (root/'arch/arm64/boot').mkdir(parents=True)
    cert=b'fixture public DER bytes'
    (work/'module-signing/signing-cert.der').write_bytes(cert)
    (root/'certs/signing_key.x509').write_bytes(cert)
    (root/'arch/arm64/boot/Image').write_bytes(b'image prefix'+cert+b'tail')
    inv=work/'kernel-reconstruction-invalidations.json';inv.write_text('["certs/system_certificates.o"]')
    m.assert_embedded(work,root)
    evidence=json.loads((work/'kernel-reconstruction-evidence.json').read_bytes())
    self.assertEqual(evidence['module_certificate_der_sha256'],hashlib.sha256(cert).hexdigest())
    self.assertEqual(evidence['invalidations_sha256'],m.sha(inv))
    self.assertEqual(evidence['invalidated_paths'],1)
    self.assertTrue(evidence['embedded_certificate_verified'])
    self.assertTrue(evidence['regenerated_certificate_matches'])
    (work/'kernel-reconstruction-evidence.json').unlink()
    (root/'arch/arm64/boot/Image').write_bytes(b'no certificate')
    with self.assertRaisesRegex(ValueError,'absent'):m.assert_embedded(work,root)
    self.assertFalse((work/'kernel-reconstruction-evidence.json').exists())
if __name__=='__main__':unittest.main()
