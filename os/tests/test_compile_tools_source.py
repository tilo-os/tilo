import hashlib
import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('tools_source', Path(__file__).resolve().parents[1]/'dev/stage-compile-tools-recipe.py')
m = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(m)


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


class ToolsSourceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Committing the two git fixtures dominates setUp: build them once and
        # give every test (and merged phase) its own copy with the same revisions.
        template = tempfile.TemporaryDirectory()
        cls.addClassCleanup(template.cleanup)
        fixture = object.__new__(cls)
        fixture.root = cls.template = Path(template.name)
        _, cls.current = fixture.repo('controller', b'FROM current\nRUN install native-crypto\n')
        _, cls.frozen = fixture.repo('application-source', b'FROM frozen\nRUN install openssl sbsigntools\n')

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in ('controller', 'application-source'):
            shutil.copytree(self.template/name, self.root/name, symlinks=True)
        self.controller, self.source = self.root/'controller', self.root/'application-source'

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

    def test_recipe_sources(self):
        with self.subTest(phase="separate_source_preserves_required_legacy_tools"):
            value=self.stage()
            self.assertEqual((self.root/'Containerfile').read_bytes(),(self.source/m.RECIPE).read_bytes()+m.BUILD_REQUIREMENTS)
            self.assertIn(b'openssl sbsigntools',(self.root/'Containerfile').read_bytes())
            self.assertEqual(value['application_revision'],self.frozen)
            self.assertTrue(value['selected_application_checkout'])
        restart_fixture(self)
        with self.subTest(phase="default_retains_controller_recipe"):
            value=self.stage(False)
            self.assertEqual((self.root/'Containerfile').read_bytes(),(self.controller/m.RECIPE).read_bytes()+m.BUILD_REQUIREMENTS)
            self.assertEqual(value['application_revision'],self.current)
            self.assertFalse(value['selected_application_checkout'])
        restart_fixture(self)
        with self.subTest(phase="assembly_and_native_recipe_bytes_match"):
            value=self.stage()
            spec=importlib.util.spec_from_file_location('native_tools', Path(__file__).resolve().parents[1]/'dev/launch-native-resume.py')
            native=importlib.util.module_from_spec(spec); spec.loader.exec_module(native)
            build=self.root/'native-build'; build.mkdir()
            cosign=self.root/'cosign'; cosign.write_bytes(b'public tool fixture')
            native.stage_tools_context(self.source,build,cosign)
            self.assertEqual((build/'Containerfile').read_bytes(),(self.root/'Containerfile').read_bytes())
            self.assertEqual(hashlib.sha256((build/'Containerfile').read_bytes()).hexdigest(),value['build_recipe_sha256'])
        restart_fixture(self)
        with self.subTest(phase="derived_recipe_hash_and_clean_source_without_final_newline"):
            source,revision=self.repo('no-final-newline', b'FROM legacy')
            value=m.stage(self.controller,self.current,source,revision,self.root/'Containerfile',self.root/'receipt.json')
            source_data=(source/m.RECIPE).read_bytes()
            built=(self.root/'Containerfile').read_bytes()
            self.assertEqual(source_data,b'FROM legacy')
            self.assertEqual(built,b'FROM legacy\nRUN chmod 0755 /var/lib && /usr/local/bin/cosign version\n')
            self.assertEqual(value['recipe_sha256'],hashlib.sha256(source_data).hexdigest())
            self.assertEqual(value['build_recipe_sha256'],hashlib.sha256(built).hexdigest())
            self.assertNotEqual(value['recipe_sha256'],value['build_recipe_sha256'])
            self.assertEqual(m.git(source,'status','--porcelain'),b'')

    def test_source_revision_guards(self):
        with self.subTest(phase="dirty_or_wrong_source_never_stages_recipe"):
            self.frozen='0'*40
            with self.assertRaises(ValueError):self.stage()
            self.assertFalse((self.root/'Containerfile').exists())
            self.frozen=m.git(self.source,'rev-parse','HEAD').decode().strip()
            (self.source/m.RECIPE).write_bytes(b'modified\n')
            with self.assertRaises(ValueError):self.stage()
            self.assertFalse((self.root/'Containerfile').exists())
        restart_fixture(self)
        with self.subTest(phase="compatible_helpers_have_their_own_revision"):
            helpers, helper_revision=self.repo('helper-controller', b'FROM compatible-helper\nRUN install openssl sbsigntools\n')
            value=m.stage(helpers,helper_revision,self.source,self.frozen,
                          self.root/'Containerfile',self.root/'receipt.json')
            self.assertEqual(value['controller_revision'],helper_revision)
            self.assertNotEqual(value['controller_revision'],self.current)
            self.assertEqual(value['application_revision'],self.frozen)
            self.assertEqual((self.root/'Containerfile').read_bytes(),(self.source/m.RECIPE).read_bytes()+m.BUILD_REQUIREMENTS)
            with self.assertRaises(ValueError):
                m.stage(helpers,self.current,self.source,self.frozen,self.root/'bad',self.root/'bad.json')


if __name__=='__main__':unittest.main()
