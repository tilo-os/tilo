import hashlib,importlib.util,json,os,subprocess,sys,tarfile,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
from restart_fixture import restart_fixture
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('failure',ROOT/'os/dev/kernel_failure_checkpoint.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)


class FailureCheckpoint(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);base=Path(self.tmp.name);self.root=base/'kernel';self.root.mkdir();self.out=base/'cache';self.out.mkdir();self.cp=base/'checkpoint';self.cp.mkdir();self.srpm=base/'source.rpm';self.srpm.write_bytes(b'fixture-srpm')
  (self.cp/'toolchain.json').write_text('{"toolchain":{}}');(self.cp/'approval.json').write_text('{"source_revision":"'+m.SOURCE+'"}')
  (self.root/'.config').write_text('CONFIG_DEBUG_FS=n\n');(self.root/'driver.o').write_bytes(b'compiled object');(self.root/'built-in.a').write_bytes(b'aggregate');(self.root/'certs').mkdir();(self.root/'certs/signing_key.pem').write_bytes(b'-----BEGIN PRIVATE KEY-----\nplaceholder private bytes')
 def capture(self):
  with patch.object(m.subprocess,'check_output',return_value=b'fixture tool 1\n'):
   return m.capture(self.root,self.out,'42','1',23,self.cp,reserve=0,srpm=self.srpm)
 def test_public_archive_contents(self):
  with self.subTest(phase="public_archive_retains_objects_and_excludes_private_and_aggregates"):
   receipt=self.capture();self.assertEqual(receipt['make_exit_code'],23);self.assertFalse(receipt['build_success']);self.assertFalse(receipt['resume_admitted'])
   with tarfile.open(self.out/'tree.tar') as archive:
    names=archive.getnames();self.assertIn('driver.o',names);self.assertNotIn('built-in.a',names);self.assertNotIn('certs/signing_key.pem',names)
   scan=json.loads((self.out/'scan.json').read_text());self.assertEqual(scan['file_sha256']['driver.o'],hashlib.sha256(b'compiled object').hexdigest());self.assertEqual(scan['entries'],len(names));self.assertEqual((self.out/'ready').read_text(),m.SOURCE+':42:1\n')
  restart_fixture(self)
  with self.subTest(phase="unexpected_private_marker_prevents_export"):
   (self.root/'unexpected').write_bytes(b'-----BEGIN PRIVATE KEY-----\nplaceholder private bytes')
   with self.assertRaises(ValueError):self.capture()
   self.assertFalse((self.out/'ready').exists());self.assertEqual(list(self.out.iterdir()),[])
  restart_fixture(self)
  with self.subTest(phase="escaping_symlink_and_insufficient_reserve_fail_closed"):
   (self.root/'escape').symlink_to('/etc/passwd')
   with self.assertRaises(ValueError):self.capture()
   (self.root/'escape').unlink()
   with patch.object(m.shutil,'disk_usage',return_value=type('Space',(),{'free':0})()):
    with self.assertRaises(ValueError):self.capture()
   self.assertFalse((self.out/'ready').exists())
  restart_fixture(self)
  with self.subTest(phase="derived_export_has_no_compile_dependency_and_normal_failure_is_not_masked"):
   original=b'ARG BASE_REF\nFROM ${BASE_REF} AS build\nRUN --mount=type=bind,from=kernel-checkpoint,target=/checkpoint,ro set -eux; \\\n    make -j"$JOBS" all; \\\n    echo packaged\nFROM build AS publish\n'
   text=m.derive(original).decode();self.assertIn('sharing=locked',text);self.assertIn('KERNEL_CHECKPOINT_RUN_ID',text)
   self.assertNotIn('|| true',text);self.assertNotIn('exit 0',text)
   exported=text[text.index('FROM ${BASE_REF} AS failure-export'):];self.assertNotIn('FROM build',exported);self.assertNotIn('make ',exported);self.assertIn('sha256sum -c SHA256SUMS',exported)
 def test_status_and_cache_preservation(self):
  with self.subTest(phase="existing_run_cache_cannot_be_overwritten"):
   self.capture()
   with self.assertRaises(ValueError):self.capture()
  restart_fixture(self)
  with self.subTest(phase="make_failure_and_capture_failure_preserve_original_status"):
   seen=[];result=m.run_make([sys.executable,'-c','import sys;sys.exit(23)'],seen.append);self.assertEqual(result,23);self.assertEqual(seen,[23])
   def broken(status):raise ValueError('capture failed')
   self.assertEqual(m.run_make([sys.executable,'-c','import sys;sys.exit(17)'],broken),17)
   self.assertEqual(m.run_make([sys.executable,'-c','pass'],broken),0)
if __name__=='__main__':unittest.main()
