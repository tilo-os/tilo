#!/usr/bin/env python3
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('gate', ROOT/'os/dev/rpm-snapshot-inputs.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

class Inputs(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.repo=Path(self.temp.name)
        self.inputs=list(gate.parser.SOURCES)+['os/base-snapshot.env','os/Containerfile.rpms','.forgejo/actions/install-cosign']
        self.git('init','-q'); self.git('config','user.email','test@example.invalid'); self.git('config','user.name','test')
        for source in gate.parser.SOURCES:self.write(source,(ROOT/source).read_text(encoding='utf-8'))
        self.write('os/base-snapshot.env','BASE_REF=registry/base@sha256:one\n')
        self.write('os/Containerfile.rpms','FROM ${BASE_REF}\n')
        self.base=self.commit()
    def git(self,*args):
        return subprocess.check_output(['git','-C',str(self.repo),*args],stderr=subprocess.PIPE).decode().strip()
    def write(self,path,text):
        p=self.repo/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text,encoding='utf-8')
    def commit(self):
        self.git('add','.');self.git('commit','-qm','fixture');return self.git('rev-parse','HEAD')
    def same(self):return gate.equivalent(self.repo,self.base,self.commit(),self.inputs)[0]
    def test_dracut_only_change_preserves_both_architecture_sets(self):
        self.write('os/Containerfile',(ROOT/'os/Containerfile').read_text(encoding='utf-8')+'\nRUN dracut --force --no-hostonly /boot/test.img\n')
        self.assertTrue(self.same())
    def test_package_architecture_and_unknown_package_commands_rebuild(self):
        for addition in ('RUN dnf install -y new-package','# tilo-rpm-snapshot-package[aarch64]: arm-only','# tilo-rpm-snapshot-package[x86_64]: amd-only','RUN dnf install -y ${UNKNOWN_PACKAGE}'):
            with self.subTest(addition=addition):
                self.write('os/Containerfile',(ROOT/'os/Containerfile').read_text(encoding='utf-8')+'\n'+addition+'\n')
                self.assertFalse(self.same())
    def test_base_recipe_signing_changes_rebuild(self):
        for path in ('os/base-snapshot.env','os/Containerfile.rpms','.forgejo/actions/install-cosign/action.yml'):
            with self.subTest(path=path):
                self.write(path,'changed\n');self.assertFalse(self.same())
    def test_deleted_source_cannot_prove_equivalence(self):
        (self.repo/'os/Containerfile').unlink();head=self.commit()
        with self.assertRaises(subprocess.CalledProcessError):gate.equivalent(self.repo,self.base,head,self.inputs)
    def invoke(self,base,event='push',require=False,head=None):
        selector=['--since-pin'] if base is None else ['--base',base]
        args=['gate',*selector,'--head',head or self.base,'--event',event]+(['--require-equivalent'] if require else [])
        output=self.repo/'output'
        with patch.object(gate,'ROOT',self.repo),patch.object(gate,'INPUTS',tuple(self.inputs+[gate.PIN])),patch('sys.argv',args),patch.dict(os.environ,{'GITHUB_OUTPUT':str(output)}),contextlib.redirect_stdout(io.StringIO()):
            result=gate.main()
        return result,output.read_text().splitlines()[-1]
    def test_manual_forces_unchanged_inputs(self):self.assertEqual(self.invoke(self.base,'workflow_dispatch'),(0,'changed=true'))
    def test_unknown_history_builds_but_blocks_publication(self):
        self.assertEqual(self.invoke('0'*40),(0,'changed=true'))
        self.assertEqual(self.invoke('0'*40,require=True),(1,'changed=true'))
    def test_known_unchanged_history_skips(self):self.assertEqual(self.invoke(self.base),(0,'changed=false'))
    def test_snapshot_workflow_triggers_on_every_input(self):
        workflow=(ROOT/'.forgejo/workflows/snapshot-rpms.yml').read_text(encoding='utf-8')
        push=workflow.split('\n  push:\n',1)[1].split('\n\n',1)[0]
        paths={line.strip()[2:].strip('"').removesuffix('/**') for line in push.splitlines() if line.strip().startswith('- ') and '/' in line}
        self.assertEqual(paths,set(gate.INPUTS))
    def pin(self,revision):
        self.write(gate.PIN,'RPMS_REF=code.podesta.ai/qos/rpms@sha256:'+'a'*64+'\n'+(f'RPMS_SOURCE_REVISION={revision}\n' if revision else ''))
        return self.commit()
    def test_since_pin_ignores_the_pin_and_intermediate_pushes(self):
        pinned=self.pin(self.base)
        self.write('unrelated.txt','x\n');head=self.commit()
        self.assertEqual(self.invoke(None,head=head),(0,'changed=false'))
        self.assertNotEqual(pinned,head)
    def test_since_pin_sees_a_change_that_landed_before_an_earlier_push(self):
        self.write('os/Containerfile',(ROOT/'os/Containerfile').read_text(encoding='utf-8')+'\nRUN dnf install -y new-package\n')
        self.commit()
        # The pin still names the older source; a later unrelated push must refresh.
        self.pin(self.base);self.write('unrelated.txt','x\n');head=self.commit()
        self.assertEqual(self.invoke(None,head=head),(0,'changed=true'))
    def newer(self,source,head):
        args=['gate','--base',source,'--head',head,'--require-newer-than-pin']
        with patch.object(gate,'ROOT',self.repo),patch('sys.argv',args),contextlib.redirect_stdout(io.StringIO()):
            return gate.main()==0
    def test_promotion_supersedes_only_an_older_pin(self):
        older=self.pin(None)
        self.write('unrelated.txt','x\n');source=self.commit()
        # Unrecorded pin: newer only while main still holds the pin source carried.
        self.assertTrue(self.newer(source,source))
        main=self.pin(older)
        self.assertTrue(self.newer(source,main))
        main=self.pin(source)
        self.assertFalse(self.newer(source,main))
        self.write('later.txt','x\n');later=self.commit()
        main=self.pin(later)
        self.assertFalse(self.newer(source,main))
    def test_promotion_refuses_an_unrecorded_pin_that_landed_later(self):
        source=self.pin(None)
        self.write(gate.PIN,'RPMS_REF=code.podesta.ai/qos/rpms@sha256:'+'b'*64+'\n');main=self.commit()
        self.assertFalse(self.newer(source,main))
    def test_pin_without_source_revision_refreshes(self):
        head=self.pin(None)
        self.assertEqual(self.invoke(None,head=head),(0,'changed=true'))

if __name__=='__main__':unittest.main()
