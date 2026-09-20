"""Verify host utility provenance selection without invoking a compiler."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch, call

spec=importlib.util.spec_from_file_location('compile',Path(__file__).resolve().parents[1]/'dev/compile-development-os.py')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)

class HostCrypto(unittest.TestCase):
 def test_controller_build_uses_isolated_target_and_absolute_install_source(self):
  with patch.object(c,'REPO',Path('/application')),patch.object(c,'CONTROLLER',Path('/controller')),patch.object(c,'PRIVATE',Path('/private')),patch.object(c,'run') as run:
   c.build_host_crypto()
   self.assertEqual(run.call_args_list,[
    call('cargo','build','--locked','--release','-p','tilo-crypto','--target-dir','/private/host-tools-target',cwd=Path('/controller')),
    call('install','-m','0755','/private/host-tools-target/release/tilo-crypto','/usr/local/bin/tilo-crypto',cwd=Path('/controller'))])
   self.assertEqual(c.REPO,Path('/application'))
 def test_default_commands_still_use_application_source_and_explicit_cwd_is_respected(self):
  with patch.object(c,'REPO',Path('/application')),patch.object(c.subprocess,'run',return_value=SimpleNamespace(stdout=b'ok')) as run:
   self.assertEqual(c.run('cargo','metadata'),'ok')
   self.assertEqual(run.call_args.kwargs['cwd'],Path('/application'))
   c.run('cargo','build',cwd=Path('/controller'))
   self.assertEqual(run.call_args.kwargs['cwd'],Path('/controller'))

if __name__=='__main__':unittest.main()
