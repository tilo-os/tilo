import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
p=Path(__file__).resolve().parents[1]/'dev/compile-development-os.py'
s=importlib.util.spec_from_file_location('cache_driver',p);d=importlib.util.module_from_spec(s);s.loader.exec_module(d)
class AssemblyCache(unittest.TestCase):
 def env(self):
  with patch.object(d,'hardware_definition'):
   e=d.build_environment('', '1', 'b'*40, 'c'*64, '/var/lib/signer', {}, 'aarch64',
                         adoption_sha256='a'*64, verified_native_reuse=True)
  e['TILO_NATIVE_BUILDER_IMAGE_ID']='sha256:'+'d'*64
  return e
 def test_fixed_arch_cache_has_no_compiler_secret_arguments(self):
  e=self.env();before=e.copy();r=d.assembly_cache(e,'aarch64','full-build',e['TILO_NATIVE_BUILDER_IMAGE_ID'],'a'*64)
  self.assertEqual(r['repository'],'code.podesta.ai/qos/tilo-development-assembly-cache-aarch64-v1')
  self.assertNotIn('--secret',e['TILO_BUILD_ARGS'])
  self.assertIn('--skip-unused-stages=true',e['TILO_BUILD_ARGS'])
  self.assertEqual({k:v for k,v in e.items() if k!='TILO_BUILD_ARGS'},{k:v for k,v in before.items() if k!='TILO_BUILD_ARGS'})
 def test_native_build_default_keeps_compiler_secrets_and_cannot_enable_cache(self):
  with patch.object(d,'hardware_definition'):
   e=d.build_environment('', '1', 'b'*40, 'c'*64, '/var/lib/signer', {}, 'aarch64',adoption_sha256='a'*64)
  self.assertIn('--secret id=aws_secret_access_key',e['TILO_BUILD_ARGS'])
  e['TILO_NATIVE_BUILDER_IMAGE_ID']='sha256:'+'d'*64
  with self.assertRaises(ValueError):d.assembly_cache(e,'aarch64','full-build',e['TILO_NATIVE_BUILDER_IMAGE_ID'],'a'*64)
 def test_gate_rejects_wrong_mode_identity_trust_or_wpe(self):
  for field,value in [('TILO_NATIVE_BUILDER_IMAGE_ID','mutable:tag'),('TILO_RAW_ROOTFS_ONLY','0'),('TILO_PUBLIC_TRUST_MODE','production'),('TILO_WPE_ADOPTION_SHA256','f'*64),('TILO_WITH_BROWSER','0')]:
   e=self.env();e[field]=value
   with self.subTest(field=field),self.assertRaises(ValueError):d.assembly_cache(e,'aarch64','full-build','sha256:'+'d'*64,'a'*64)
  for mode,arch in [('native-payload','aarch64'),('full-build','other')]:
   e=self.env()
   with self.assertRaises(ValueError):d.assembly_cache(e,arch,mode,'sha256:'+'d'*64,'a'*64)
 def test_injected_cache_flags_are_rejected(self):
  e=self.env();e['TILO_BUILD_ARGS']+=' --cache-to attacker.invalid/cache'
  with self.assertRaises(ValueError):d.assembly_cache(e,'aarch64','full-build','sha256:'+'d'*64,'a'*64)
if __name__=='__main__':unittest.main()
