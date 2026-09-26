import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
from frozen_source_fixture import NATIVE_DRIVER, NATIVE_RECIPE
SPEC = importlib.util.spec_from_file_location('approval', ROOT/'os/dev/approve-native-reconstruction.py')
a = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(a)
# Any independently pinned helper commit; the tests supply its blobs from this tree.
HELPER = '6d390d9ddd96dbe734a6a7d90ac882bd8ef4adc8'


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


class ApprovalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.material = {'recipe': NATIVE_RECIPE.encode(), 'driver': NATIVE_DRIVER.encode(),
            'helper': (ROOT/'os/dev/native_resume.py').read_bytes(),
            'producer': (ROOT/'os/dev/resume-native-payload.py').read_bytes()}
        cls.resume = types.ModuleType('fixture_resume')
        exec(compile(cls.material['helper'],'pinned-helper','exec'),cls.resume.__dict__)
        cls.required = cls.resume.load_native(ROOT).REQUIRED

    def setUp(self):
        checkpoint = {'purpose':'tilo-development-partial-native-checkpoint','source_revision':a.SOURCE,
            'reference':'registry.tilo.so/tilo/tilo-development-partial-native-aarch64@sha256:'+'1'*64,
            'config_digest':'sha256:'+'2'*64,'metadata_sha256':'3'*64,
            'inventory':{'shell':{'size':64,'sha256':'4'*64}},
            'installable':False,'complete_native_builder':False,'build_success':False}
        self.checkpoints = [{'receipt_sha256':a.sha(a.canonical(checkpoint)),'receipt':checkpoint}]
        self.inputs={'revision':a.SOURCE,'architecture':'aarch64','fixture_expected_inputs':True}
        union = a.checkpoint_union(self.checkpoints,self.required)
        recipe,missing = self.resume.derive_recipe(self.material['recipe'].decode(),union,self.required)
        parameters={'source_path':'/work/repo','derived_recipe_path':'/work/tilo-native-resume-fixture/Containerfile.resumed',
                    'seed_image_id':'sha256:'+'5'*64}
        driver=self.resume.derive_driver(self.material['driver'].decode(),**parameters)
        reconstruction={'schema':1,'purpose':'tilo-native-reconstruction','source_revision':a.SOURCE,
            'original_recipe_sha256':a.sha(self.material['recipe']),'derived_recipe_sha256':a.sha(recipe.encode()),
            'original_driver_sha256':a.sha(self.material['driver']),'derived_driver_sha256':a.sha(driver.encode()),
            **parameters,'resume_helper_sha256':a.sha(self.material['helper']),'producer_sha256':a.sha(self.material['producer']),
            'checkpoints':self.checkpoints,'reused_inventory':union,'missing_targets':missing,'run_id':'21461','attempt':'1',
            'remaining_phases':['thin-app-shared-graph','elf-hardening','runtime-bundle','app-store'],
            'original_recipe_executed_verbatim':False}
        self.receipt={'schema':1,'purpose':'tilo-development-reconstructed-native-builder','inputs':self.inputs,
            'reference':'registry.tilo.so/tilo/tilo-development-reconstructed-native-aarch64@sha256:'+'6'*64,
            'config_digest':'sha256:'+'7'*64,'metadata_sha256':'8'*64,'reconstruction':reconstruction,
            'reconstruction_sha256':a.sha(a.canonical(reconstruction)),'complete_native_builder':True,
            'production_signed':False,'installable':False,'consumer_adoption_required':True}

    def approve(self, receipt):
        return a.approve(receipt,a.sha(a.canonical(receipt)),self.checkpoints,self.required,self.inputs,
            self.resume,self.material,HELPER,'21461','1')

    def test_approval_calculation(self):
        with self.subTest(phase="calculates_approval_from_pinned_source_and_preserves_scope"):
            result=self.approve(self.receipt)
            self.assertEqual(result['approved_reconstruction_sha256'],self.receipt['reconstruction_sha256'])
            self.assertEqual(result['receipt_sha256'],a.sha(a.canonical(self.receipt)))
            self.assertFalse(result['registry_verified'])
            self.assertFalse(result['complete_inventory_verified'])
            self.assertFalse(result['installable'])
        restart_fixture(self)
        with self.subTest(phase="self_consistent_remote_hash_cannot_approve_changed_recipe_driver_or_run"):
            for key,value in [('derived_recipe_sha256','0'*64),('derived_driver_sha256','0'*64),
                              ('producer_sha256','0'*64),('resume_helper_sha256','0'*64),
                              ('source_revision','0'*40),('run_id','21462'),('attempt','2')]:
                receipt=copy.deepcopy(self.receipt);receipt['reconstruction'][key]=value
                receipt['reconstruction_sha256']=a.sha(a.canonical(receipt['reconstruction']))
                with self.subTest(key=key),self.assertRaises(ValueError):self.approve(receipt)

    def test_substitution_rejections(self):
        with self.subTest(phase="checkpoint_and_input_substitution_rejected"):
            receipt=copy.deepcopy(self.receipt)
            receipt['reconstruction']['checkpoints'][0]['receipt']['inventory']['shell']['sha256']='9'*64
            receipt['reconstruction_sha256']=a.sha(a.canonical(receipt['reconstruction']))
            with self.assertRaises(ValueError):self.approve(receipt)
            receipt=copy.deepcopy(self.receipt);receipt['inputs']['revision']='0'*40
            with self.assertRaises(ValueError):self.approve(receipt)
        restart_fixture(self)
        with self.subTest(phase="source_checkout_mismatch_rejected_before_loading_code"):
            with patch.object(a,'git',side_effect=[b'commit',b'0'*40]),self.assertRaisesRegex(ValueError,'frozen4583'):
                a.trusted_inputs(ROOT,ROOT,HELPER,'rpms','wpe','a'*64)
        restart_fixture(self)
        with self.subTest(phase="receipt_pin_and_duplicate_fields_rejected"):
            with tempfile.TemporaryDirectory() as temporary:
                path=Path(temporary)/'receipt.json';path.write_bytes(a.canonical(self.receipt))
                with self.assertRaises(ValueError):a.read_pinned(path,'0'*64)
                self.assertEqual(a.read_pinned(path,a.sha(path.read_bytes())),self.receipt)
                path.write_bytes(b'{"schema":1,"schema":1}')
                with self.assertRaises(ValueError):a.read_pinned(path,a.sha(path.read_bytes()))


if __name__=='__main__':unittest.main()
