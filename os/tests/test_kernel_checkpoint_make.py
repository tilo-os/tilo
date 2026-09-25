"""Checkpoint invalidation against a real make run; needs make."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

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
if __name__=='__main__':unittest.main()
