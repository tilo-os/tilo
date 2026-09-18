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

    def test_rootfs_stage_isolation(self):
        old=b'ARG BASE_REF=x\nFROM x AS builder\nRUN compile\nFROM ${BASE_REF} AS rootfs\nRUN old\nFROM rootfs AS split\nRUN split\n'
        new=old.replace(b'RUN old', b'RUN fixed')
        c.verify_rootfs(old,new)
        for bad in (new.replace(b'RUN compile',b'RUN changed'), new.replace(b'RUN split',b'RUN changed'), new.replace(b'RUN fixed',b'FROM evil AS injected')):
            with self.assertRaises(ValueError):c.verify_rootfs(old,bad)

    def test_rootfs_three_commit_tree_and_wpe_receipt_bridge(self):
        # Build a separate exact baseline with a real multi-stage recipe.
        self.git('reset','--hard',self.base)
        old=b'FROM x AS builder\nRUN compile\nFROM ${BASE_REF} AS rootfs\nRUN old\nFROM rootfs AS split\n'
        (self.repo/'os/Containerfile').write_bytes(old)
        (self.repo/'os/dev/build-wpe.sh').write_bytes(b'wpe\n')
        self.git('add','.');self.git('commit','-qm','baseline recipe')
        self.base=self.git('rev-parse','HEAD').decode().strip()
        for name in c.PATHS:(self.repo/name).write_bytes(b'corrected\n')
        self.git('commit','-qam','provider')
        provider=self.git('rev-parse','HEAD').decode().strip()
        new=old.replace(b'RUN old',b'RUN fixed');(self.repo/'os/Containerfile').write_bytes(new)
        self.git('commit','-qam','rootfs');self.revision=self.git('rev-parse','HEAD').decode().strip()
        original=dict(reference='immutable',config_digest='same',payload_sha256='same',audit_sha256='a'*64,
            audit=dict(compatible_recipes={'os/Containerfile':c.sha(old),'os/dev/build-wpe.sh':c.sha(b'wpe\n')},original_source_revision=None))
        updated=json.loads(json.dumps(original));updated['audit']['compatible_recipes']['os/Containerfile']=c.sha(new)
        updated['audit_sha256']=c.sha((json.dumps(updated['audit'],sort_keys=True,separators=(',',':'))+'\n').encode())
        bridge={}
        for prefix,value in (('original',original),('assembly',updated)):
            raw=json.dumps(value);bridge[prefix+'_receipt_json']=raw;bridge[prefix+'_receipt_sha256']=c.sha(raw.encode())
        self.value.update(purpose='tilo-rootfs-native-source-compatibility',native_source_revision=self.base,
            assembly_source_revision=self.revision,provider_revision=provider,wpe_bridge=bridge)
        for name in (*c.PATHS,'os/Containerfile'):
            self.value['changes'][name]={}
            for label,rev in (('old',self.base),('new',self.revision)):
                self.value['changes'][name][label+'_blob']=self.git('rev-parse',rev+':'+name).decode().strip()
                self.value['changes'][name][label+'_sha256']=c.sha(self.git('show',rev+':'+name))
        proof=self.verify()
        inputs=dict(revision=self.revision,epoch=proof['assembly_epoch'],containerfile_sha256=c.sha(new),
            wpe_reference='immutable',wpe_receipt_sha256=bridge['assembly_receipt_sha256'])
        with patch.object(c,'SOURCE',self.base): result=c.original_native_inputs(inputs,proof)
        self.assertEqual(result['containerfile_sha256'],c.sha(old))
        self.assertEqual(result['wpe_receipt_sha256'],bridge['original_receipt_sha256'])
        c.verify_selected_wpe(proof,bridge['assembly_receipt_json'].encode(),bridge['assembly_receipt_sha256'])
        with self.assertRaises(ValueError):c.verify_selected_wpe(proof,b'{}',bridge['assembly_receipt_sha256'])
        updated['payload_sha256']='changed';raw=json.dumps(updated);bridge['assembly_receipt_json']=raw;bridge['assembly_receipt_sha256']=c.sha(raw.encode())
        with self.assertRaisesRegex(ValueError,'more than'):self.verify()
        updated['payload_sha256']='same'
        # Rootfs-only policy follow-up: recreate baseline with policy files, then
        # replay the same provider/recipe corrections as separate exact commits.
        self.git('reset','--hard',self.base)
        for name in c.POLICY_PATHS:
            f=self.repo/name;f.parent.mkdir(parents=True,exist_ok=True);f.write_bytes(b'old policy\n')
        old=old.replace(b'RUN old',b'COPY docs/security/boot-chain.md /docs/\nCOPY os/usr/libexec/tilo-security-audit /audit/\nCOPY os/scripts/verify-runtime-crypto-packages.sh /usr/local/bin/\nRUN bash /usr/local/bin/verify-runtime-crypto-packages.sh\nRUN old')
        (self.repo/'os/Containerfile').write_bytes(old);self.git('add','.');self.git('commit','-qm','policy baseline')
        self.base=self.git('rev-parse','HEAD').decode().strip()
        for name in c.PATHS:(self.repo/name).write_bytes(b'corrected\n')
        self.git('commit','-qam','provider');provider=self.git('rev-parse','HEAD').decode().strip()
        new=old.replace(b'RUN old',b'RUN fixed');(self.repo/'os/Containerfile').write_bytes(new)
        self.git('commit','-qam','recipe');recipe_rev=self.git('rev-parse','HEAD').decode().strip()
        for name in c.POLICY_PATHS:(self.repo/name).write_bytes(b'reviewed policy\n')
        self.git('commit','-qam','policy');self.revision=self.git('rev-parse','HEAD').decode().strip()
        self.value.update(native_source_revision=self.base,assembly_source_revision=self.revision,provider_revision=provider,rootfs_revision=recipe_rev)
        for name in (*c.PATHS,'os/Containerfile',*c.POLICY_PATHS):
            self.value['changes'][name]={}
            for label,rev in (('old',self.base),('new',self.revision)):
                self.value['changes'][name][label+'_blob']=self.git('rev-parse',rev+':'+name).decode().strip()
                self.value['changes'][name][label+'_sha256']=c.sha(self.git('show',rev+':'+name))
        original['audit']['compatible_recipes']['os/Containerfile']=c.sha(old)
        updated['audit']['compatible_recipes']['os/Containerfile']=c.sha(new)
        updated['audit_sha256']=c.sha((json.dumps(updated['audit'],sort_keys=True,separators=(',',':'))+'\n').encode())
        for prefix,value in (('original',original),('assembly',updated)):
            raw=json.dumps(value);bridge[prefix+'_receipt_json']=raw;bridge[prefix+'_receipt_sha256']=c.sha(raw.encode())
        self.verify()
        # A reviewed recipe-only follow-up retains policy ancestry and bridges
        # the new consumer recipe back to the original native/WPE identities.
        policy_rev=self.revision
        newest=new.replace(b'RUN fixed',b'RUN deferred-optional-test')
        (self.repo/'os/Containerfile').write_bytes(newest)
        self.git('commit','-qam','defer optional runtime test')
        self.revision=self.git('rev-parse','HEAD').decode().strip()
        self.value.update(policy_revision=policy_rev,assembly_source_revision=self.revision)
        self.value['changes']['os/Containerfile'].update(
            new_blob=self.git('rev-parse',self.revision+':os/Containerfile').decode().strip(),
            new_sha256=c.sha(newest))
        updated['audit']['compatible_recipes']['os/Containerfile']=c.sha(newest)
        updated['audit_sha256']=c.sha((json.dumps(updated['audit'],sort_keys=True,separators=(',',':'))+'\n').encode())
        raw=json.dumps(updated);bridge['assembly_receipt_json']=raw;bridge['assembly_receipt_sha256']=c.sha(raw.encode())
        proof=self.verify()
        inputs.update(revision=self.revision,epoch=proof['assembly_epoch'],containerfile_sha256=c.sha(newest),wpe_receipt_sha256=bridge['assembly_receipt_sha256'])
        with patch.object(c,'SOURCE',self.base): result=c.original_native_inputs(inputs,proof)
        self.assertEqual(result['containerfile_sha256'],c.sha(old))
        self.assertEqual(result['wpe_receipt_sha256'],bridge['original_receipt_sha256'])
        self.value['policy_revision']=recipe_rev
        with self.assertRaisesRegex(ValueError,'ancestry'):self.verify()
        self.value['policy_revision']=policy_rev
        # Additional tracked input, even with the expected parent, is rejected.
        self.git('reset','--soft',policy_rev)
        (self.repo/'Cargo.toml').write_bytes(b'extra\n');self.git('commit','-qam','unexpected followup')
        self.revision=self.git('rev-parse','HEAD').decode().strip();self.value['assembly_source_revision']=self.revision
        with self.assertRaisesRegex(ValueError,'only Containerfile'):self.verify()
        self.git('reset','--hard',policy_rev)
        self.revision=policy_rev;self.value['assembly_source_revision']=policy_rev;del self.value['policy_revision']
        self.value['changes']['os/Containerfile'].update(new_blob=self.git('rev-parse',policy_rev+':os/Containerfile').decode().strip(),new_sha256=c.sha(new))
        updated['audit']['compatible_recipes']['os/Containerfile']=c.sha(new)
        updated['audit_sha256']=c.sha((json.dumps(updated['audit'],sort_keys=True,separators=(',',':'))+'\n').encode())
        raw=json.dumps(updated);bridge['assembly_receipt_json']=raw;bridge['assembly_receipt_sha256']=c.sha(raw.encode())
        self.value['rootfs_revision']=provider
        with self.assertRaisesRegex(ValueError,'ancestry'):self.verify()
        self.value['rootfs_revision']=recipe_rev
        (self.repo/'Cargo.toml').write_bytes(b'extra\n');self.git('commit','-qam','unexpected')
        self.revision=self.git('rev-parse','HEAD').decode().strip();self.value['assembly_source_revision']=self.revision
        with self.assertRaisesRegex(ValueError,'ancestry'):self.verify()
        self.git('reset','--soft',recipe_rev);self.git('commit','-qm','policy plus unexpected source')
        self.revision=self.git('rev-parse','HEAD').decode().strip();self.value['assembly_source_revision']=self.revision
        with self.assertRaisesRegex(ValueError,'only reviewed'):self.verify()
        return


    def test_policy_input_must_stay_in_rootfs(self):
        recipe=b'FROM x AS builder\nFROM ${BASE_REF} AS rootfs\nCOPY os/scripts/verify-runtime-crypto-packages.sh /usr/local/bin/\nCOPY os/usr/libexec/tilo-security-audit /usr/libexec/\nCOPY docs/security/boot-chain.md /usr/share/\nRUN bash /usr/local/bin/verify-runtime-crypto-packages.sh\nFROM rootfs AS split\n'
        c.verify_policy_placement(recipe)
        with self.assertRaises(ValueError):c.verify_policy_placement(recipe.replace(b'FROM x AS builder',b'FROM x AS builder\nCOPY os/scripts/verify-runtime-crypto-packages.sh /tmp/'))

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
