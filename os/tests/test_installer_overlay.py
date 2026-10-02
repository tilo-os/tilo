import importlib.util,tempfile,unittest,sys
from pathlib import Path
sys.path.insert(0,str(Path(__file__).parents[1]/'dev'))
import installer_overlay as overlay
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


class Overlay(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
  self.source=self.root/'source';self.helpers=self.root/'helpers'
  for rel in ['usr/libexec/tilo-security-audit',*overlay.OVERRIDES,'etc/greetd/config.toml','etc/greetd/config.installer.toml']:
   p=self.source/'os'/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text('source:'+rel)
  for rel in [*overlay.OVERRIDES,'dev/uki-cmdline.py','Containerfile.installer']:
   p=self.helpers/'os'/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text('helper:'+rel)
 def test_overlay_context_and_command(self):
  with self.subTest(phase="preserves_source_policy_and_uses_reviewed_install_overrides"):
   out=self.root/'out';proof=overlay.stage(self.source,self.helpers,out)
   self.assertEqual((out/'rootfs/usr/libexec/tilo-security-audit').read_text(),'source:usr/libexec/tilo-security-audit')
   self.assertEqual((out/'rootfs/usr/libexec/tilo-install').read_text(),'helper:usr/libexec/tilo-install')
   self.assertEqual((out/'rootfs/usr/libexec/tilo-install-dev-ssh.py').read_text(),'helper:usr/libexec/tilo-install-dev-ssh.py')
   self.assertIn('helpers:usr/libexec/tilo-install-dev-ssh.py',proof)
   self.assertFalse((out/'rootfs/usr/bin/tilo-installer').exists())
   self.assertIn('helpers:Containerfile.installer',proof)
   self.assertIn('source:usr/libexec/tilo-security-audit',proof)
  restart_fixture(self)
  with self.subTest(phase="refuses_missing_required_overlay_and_existing_context"):
   (self.helpers/'os/Containerfile.installer').unlink()
   with self.assertRaises(ValueError):overlay.stage(self.source,self.helpers,self.root/'out')
   with self.assertRaises(ValueError):overlay.stage(self.source,self.helpers,self.root/'out')
  restart_fixture(self)
  with self.subTest(phase="command_pins_local_image_and_disables_network_and_pulls"):
   cmd=overlay.command('/context','sha256:'+'1'*64,123,'/image.iid')
   self.assertIn('--pull=never',cmd);self.assertIn('--network=none',cmd)
   self.assertIn('TILO_BASE_IMAGE=sha256:'+'1'*64,cmd)
   self.assertFalse(any('installer-base=container-image://' in value for value in cmd))
   self.assertIn('TILO_INSTALLER_DEV_SSH=0',cmd)
   with self.assertRaises(ValueError):overlay.command('/context','mutable:tag',123,'/image.iid')
if __name__=='__main__':unittest.main()
