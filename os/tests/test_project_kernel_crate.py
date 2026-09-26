import copy
import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('project_crate',ROOT/'os/kernel/project-crate.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)


class ProjectionTests(unittest.TestCase):
    def test_projection_dependencies(self):
        with self.subTest(phase="actual_signer_dependencies_match_source"):
            root=tomllib.loads((ROOT/'Cargo.toml').read_text());member=tomllib.loads((ROOT/'crates/system/tilo-crypto/Cargo.toml').read_text())
            projected=module.project_manifest(root,member)
            self.assertEqual(set(projected['dependencies']),set(member['dependencies']))
            for name in ('ring','cms','x509-cert','rsa'):
                self.assertEqual(projected['dependencies'][name],member['dependencies'][name])
            # Workspace-inherited dependencies are resolved against the root manifest.
            self.assertEqual(member['dependencies']['getrandom'],{'workspace':True})
            self.assertEqual(projected['dependencies']['getrandom'],root['workspace']['dependencies']['getrandom'])
            self.assertEqual(projected['dependencies']['sha2']['version'],root['workspace']['dependencies']['sha2'])
        with self.subTest(phase="inherited_alias_target_features_and_defaults"):
            root={'workspace':{'package':{'license':'AGPL-3.0-only'},'dependencies':{'alias':{'package':'upstream','version':'=2.0','features':['base'],'default-features':False},'defaulted':'1'},'lints':{'rust':{'unsafe_code':'deny'}}}}
            member={'package':{'name':'fixture','version':'1','license':{'workspace':True}},'dependencies':{'alias':{'workspace':True,'features':['base','extra'],'default-features':True},'defaulted':{'workspace':True,'default-features':False}},'target':{'cfg(unix)':{'build-dependencies':{'alias':{'workspace':True,'features':['unix']}}}},'lints':{'workspace':True}}
            result=module.project_manifest(root,member)
            self.assertEqual(result['dependencies']['alias'],{'package':'upstream','version':'=2.0','features':['base','extra'],'default-features':True})
            self.assertNotIn('default-features',result['dependencies']['defaulted'])
            self.assertEqual(result['target']['cfg(unix)']['build-dependencies']['alias']['features'],['base','unix'])
            self.assertEqual(result['package']['license'],'AGPL-3.0-only')
            self.assertEqual(tomllib.loads('\n'.join(module.toml(k)+' = '+module.toml(v) for k,v in result.items())),result)
        with self.subTest(phase="direct_path_dependency_rejected"):
            with self.assertRaisesRegex(ValueError,'registry'):
                module.project_manifest({'workspace':{}},{'package':{'name':'x'},'dependencies':{'local':{'path':'../local'}}})
        with self.subTest(phase="reachable_workspace_patch_rejected"):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory)
                (root/'workspace').write_text('[workspace]\n')
                (root/'member').write_text('[package]\nname="tool"\nversion="1"\n[dependencies]\npatched="1"\n')
                (root/'lock').write_text('version=4\n[[package]]\nname="tool"\nversion="1"\ndependencies=["patched"]\n[[package]]\nname="patched"\nversion="1"\n')
                with self.assertRaisesRegex(ValueError,'path/git patch'):module.project(root/'workspace',root/'member',root/'lock',root/'out')


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


class LockTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name)
        (self.root/'Cargo.toml').write_text('[package]\nname="tool"\nversion="1"\n')
        self.packages=[{'name':'tool','version':'1','dependencies':['dep']},{'name':'dep','version':'1','source':'registry+index','checksum':'abc','dependencies':['optional']},{'name':'optional','version':'1','source':'registry+index','checksum':'def'}]
        self.write('root.lock',self.packages)

    def write(self,name,packages):
        (self.root/name).write_text('version=4\n'+'\n'.join('[[package]]\n'+'\n'.join(module.toml(k)+' = '+module.toml(v) for k,v in p.items()) for p in packages))

    def test_lock_pruning_and_rejections(self):
        with self.subTest(phase="optional_feature_pruning_keeps_frozen_inputs"):
            projected=copy.deepcopy(self.packages[:2]);projected[1].pop('dependencies');self.write('Cargo.lock',projected)
            module.verify_lock(self.root/'root.lock',self.root/'Cargo.lock')
        restart_fixture(self)
        with self.subTest(phase="versions_sources_checksums_and_new_edges_rejected"):
            for field,value in [('version','2'),('source','registry+other'),('checksum','different'),('dependencies',['tool'])]:
                with self.subTest(field=field):
                    changed=copy.deepcopy(self.packages);changed[1][field]=value;self.write('Cargo.lock',changed)
                    with self.assertRaises(ValueError):module.verify_lock(self.root/'root.lock',self.root/'Cargo.lock')


if __name__=='__main__':unittest.main()
