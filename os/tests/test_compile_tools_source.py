import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('tools_source', Path(__file__).resolve().parents[1]/'dev/stage-compile-tools-recipe.py')
m = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(m)


class ToolsSourceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.controller, self.current = self.repo('controller', b'FROM current\nRUN install native-crypto\n')
        self.source, self.frozen = self.repo('application-source', b'FROM frozen\nRUN install openssl sbsigntools\n')

    def repo(self, name, recipe):
        path = self.root/name
        (path/'os').mkdir(parents=True)
        (path/m.RECIPE).write_bytes(recipe)
        def git(*args):
            return subprocess.check_output(['git','-C',str(path),*args],stderr=subprocess.DEVNULL)
        git('init','--quiet')
        git('config','core.autocrlf','false')
        git('add','os')
        git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--quiet','-m','fixture')
        return path,git('rev-parse','HEAD').decode().strip()

    def stage(self, separate=True):
        return m.stage(self.controller,self.current,self.source if separate else None,self.frozen if separate else '',
                       self.root/'Containerfile',self.root/'receipt.json')

    def test_separate_source_preserves_required_legacy_tools(self):
        value=self.stage()
        self.assertEqual((self.root/'Containerfile').read_bytes(),(self.source/m.RECIPE).read_bytes())
        self.assertIn(b'openssl sbsigntools',(self.root/'Containerfile').read_bytes())
        self.assertEqual(value['application_revision'],self.frozen)
        self.assertTrue(value['selected_application_checkout'])

    def test_default_retains_controller_recipe(self):
        value=self.stage(False)
        self.assertEqual((self.root/'Containerfile').read_bytes(),(self.controller/m.RECIPE).read_bytes())
        self.assertEqual(value['application_revision'],self.current)
        self.assertFalse(value['selected_application_checkout'])

    def test_dirty_or_wrong_source_never_stages_recipe(self):
        self.frozen='0'*40
        with self.assertRaises(ValueError):self.stage()
        self.assertFalse((self.root/'Containerfile').exists())
        self.frozen=m.git(self.source,'rev-parse','HEAD').decode().strip()
        (self.source/m.RECIPE).write_bytes(b'modified\n')
        with self.assertRaises(ValueError):self.stage()
        self.assertFalse((self.root/'Containerfile').exists())

    def test_compatible_helpers_have_their_own_revision(self):
        helpers, helper_revision=self.repo('helper-controller', b'FROM compatible-helper\nRUN install openssl sbsigntools\n')
        value=m.stage(helpers,helper_revision,self.source,self.frozen,
                      self.root/'Containerfile',self.root/'receipt.json')
        self.assertEqual(value['controller_revision'],helper_revision)
        self.assertNotEqual(value['controller_revision'],self.current)
        self.assertEqual(value['application_revision'],self.frozen)
        self.assertEqual((self.root/'Containerfile').read_bytes(),(self.source/m.RECIPE).read_bytes())
        with self.assertRaises(ValueError):
            m.stage(helpers,self.current,self.source,self.frozen,self.root/'bad',self.root/'bad.json')


if __name__=='__main__':unittest.main()
