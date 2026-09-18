import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import native_source_compatibility as c
spec = importlib.util.spec_from_file_location('compile_compatibility', Path(__file__).resolve().parents[1]/'dev/compile-development-os.py')
d = importlib.util.module_from_spec(spec); spec.loader.exec_module(d)


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name); self.repo = self.root/'repo'; self.repo.mkdir()
        self.git('init', '-q'); self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture'); self.git('config', 'core.autocrlf', 'false')
        for name in (*c.PATHS, 'Cargo.toml', 'os/Containerfile'):
            path=self.repo/name; path.parent.mkdir(parents=True, exist_ok=True);path.write_bytes(b'original\n')
        self.git('add','.')
        with patch.dict(os.environ, {'GIT_COMMITTER_DATE':'2026-01-01T00:00:00Z'}):
            self.git('commit','-qm','initial')
        self.base=self.git('rev-parse','HEAD').decode().strip()
        for name in c.PATHS:(self.repo/name).write_bytes(b'corrected\n')
        with patch.dict(os.environ, {'GIT_COMMITTER_DATE':'2026-01-02T00:00:00Z'}):
            self.git('commit','-qam','validator correction')
        self.revision=self.git('rev-parse','HEAD').decode().strip()
        self.approval=self.root/'approval.json'
        self.value=dict(schema=1,purpose='tilo-windows-provider-native-source-compatibility',
            native_source_revision=self.base,assembly_source_revision=self.revision,
            native_receipt_sha256='a'*64,approved_reconstruction_sha256='b'*64,changes={})
        for name in c.PATHS:
            self.value['changes'][name]=dict(old_blob=self.git('rev-parse',self.base+':'+name).decode().strip(),
                new_blob=self.git('rev-parse',self.revision+':'+name).decode().strip(),
                old_sha256=c.sha(b'original\n'),new_sha256=c.sha(b'corrected\n'))

    def git(self,*args):
        return subprocess.check_output(['git','-C',str(self.repo),*args],stderr=subprocess.DEVNULL)

    def verify(self):
        raw=json.dumps(self.value).encode();self.approval.write_bytes(raw)
        with patch.object(c,'SOURCE',self.base):
            return c.verify(self.repo,self.revision,self.approval,c.sha(raw),'a'*64,'b'*64)

    def test_exact_two_file_correction_preserves_native_inputs_and_assembly_epoch(self):
        result=self.verify()
        self.assertNotEqual(result['native_epoch'],result['assembly_epoch'])
        assembly=dict(revision=self.revision,epoch=result['assembly_epoch'],containerfile_sha256='c'*64,
                      architecture='aarch64',wpe_reference='unchanged',rpms='unchanged')
        with patch.object(c,'SOURCE',self.base):native=c.original_native_inputs(assembly,result)
        self.assertEqual(native,dict(assembly,revision=self.base,epoch=result['native_epoch']))
        self.assertEqual(assembly['revision'],self.revision)
        self.assertTrue(result['complete_native_inventory_required'])

    def test_unapproved_blob_or_native_receipt_rejected(self):
        self.value['changes'][c.PATHS[0]]['new_sha256']='0'*64
        with self.assertRaisesRegex(ValueError,'blob differs'):self.verify()
        self.value['native_receipt_sha256']='0'*64
        with self.assertRaisesRegex(ValueError,'identity differs'):self.verify()

    def test_dirty_or_additional_tracked_change_rejected(self):
        (self.repo/'Cargo.toml').write_bytes(b'changed\n')
        with self.assertRaisesRegex(ValueError,'clean'):self.verify()
        self.git('commit','-qam','additional edit');self.revision=self.git('rev-parse','HEAD').decode().strip()
        self.value['assembly_source_revision']=self.revision
        with self.assertRaisesRegex(ValueError,'directly descend'):self.verify()
        self.git('reset','--soft',self.base);self.git('commit','-qm','combined correction and cargo edit')
        self.revision=self.git('rev-parse','HEAD').decode().strip();self.value['assembly_source_revision']=self.revision
        with self.assertRaisesRegex(ValueError,'complete tree diff'):self.verify()

    def test_source_admission_pin_and_duplicate_json_rejected(self):
        self.approval.write_bytes(b'{"schema":1,"schema":1}')
        with self.assertRaisesRegex(ValueError,'duplicate'):c.read_approval(self.approval,c.sha(self.approval.read_bytes()))
        with self.assertRaisesRegex(ValueError,'bytes differ'):c.read_approval(self.approval,'0'*64)

    def test_selection_requires_explicit_full_reconstructed_adoption(self):
        kwargs=dict(wpe='code.sandbox.example/qos/tilo-wpe-buildroot@sha256:'+'a'*64,generation='1',
            revision='c'*40,branch='refs/heads/main',mode='full-build',architecture='aarch64',
            kernel_candidate='code.sandbox.example/qos/kernel@sha256:'+'d'*64,
            fex_payload_candidate='code.sandbox.example/qos/fex@sha256:'+'e'*64,
            windows_guest_candidate='code.sandbox.example/qos/guest@sha256:'+'f'*64,
            reconstructed_native_receipt='receipt.json',reconstructed_native_sha256='a'*64,
            approved_reconstruction_sha256='b'*64,native_source_compatibility='compatibility.json',
            native_source_compatibility_sha256='d'*64)
        d.validate_inputs(**kwargs)
        for changes in ({'native_source_compatibility_sha256':''},{'reconstructed_native_receipt':None},
                        {'mode':'native-payload'},{'revision':d.RECONSTRUCTION_SOURCE},
                        {'native_source_compatibility':None}):
            with self.subTest(changes=changes),self.assertRaises(ValueError):d.validate_inputs(**(kwargs|changes))


if __name__=='__main__':unittest.main()
