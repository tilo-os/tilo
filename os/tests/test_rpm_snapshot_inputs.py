#!/usr/bin/env python3
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('gate', ROOT/'os/dev/rpm-snapshot-inputs.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

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


class Inputs(unittest.TestCase):
    def test_rebuild_triggers(self):
        with self.subTest(phase="dev_ssh_transaction_has_matching_core_rpm_in_snapshot"):
            specs = set(gate.parser.packages(ROOT / 'os/Containerfile.dev', 'aarch64'))
            self.assertTrue({'openssh', 'openssh-server', 'rsync'} <= specs)
        restart_fixture(self)
        with self.subTest(phase="dracut_only_change_preserves_both_architecture_sets"):
            self.write('os/Containerfile',(ROOT/'os/Containerfile').read_text(encoding='utf-8')+'\nRUN dracut --force --no-hostonly /boot/test.img\n')
            self.assertTrue(self.same())
        restart_fixture(self)
        with self.subTest(phase="package_architecture_and_unknown_package_commands_rebuild"):
            for addition in ('RUN dnf install -y new-package','# tilo-rpm-snapshot-package[aarch64]: arm-only','# tilo-rpm-snapshot-package[x86_64]: amd-only','RUN dnf install -y ${UNKNOWN_PACKAGE}'):
                with self.subTest(addition=addition):
                    self.write('os/Containerfile',(ROOT/'os/Containerfile').read_text(encoding='utf-8')+'\n'+addition+'\n')
                    self.assertFalse(self.same())
        restart_fixture(self)
        with self.subTest(phase="base_recipe_signing_changes_rebuild"):
            for path in ('os/base-snapshot.env','os/Containerfile.rpms','.forgejo/actions/install-cosign/action.yml'):
                with self.subTest(path=path):
                    self.write(path,'changed\n');self.assertFalse(self.same())

    @classmethod
    def setUpClass(cls):
        # git init/add/commit dominate the fixture cost: commit it once and give
        # every test (and merged phase) its own copy with the same commit ids.
        cls.template=tempfile.TemporaryDirectory(); cls.addClassCleanup(cls.template.cleanup)
        fixture=object.__new__(cls); fixture.repo=Path(cls.template.name)/'repo'; fixture.repo.mkdir()
        fixture.git('init','-q'); fixture.git('config','user.email','test@example.invalid'); fixture.git('config','user.name','test')
        for source in gate.parser.SOURCES:fixture.write(source,(ROOT/source).read_text(encoding='utf-8'))
        fixture.write('os/base-snapshot.env','BASE_REF=registry/base@sha256:one\n')
        fixture.write('os/Containerfile.rpms','FROM ${BASE_REF}\n')
        cls.template_base=fixture.commit()
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.repo=Path(self.temp.name)/'repo'
        shutil.copytree(Path(self.template.name)/'repo',self.repo,symlinks=True)
        self.inputs=list(gate.parser.SOURCES)+['os/base-snapshot.env','os/Containerfile.rpms','.forgejo/actions/install-cosign']
        self.base=self.template_base
    def git(self,*args):
        return subprocess.check_output(['git','-C',str(self.repo),*args],stderr=subprocess.PIPE).decode().strip()
    def write(self,path,text):
        p=self.repo/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text,encoding='utf-8')
    def commit(self):
        self.git('add','.');self.git('commit','-qm','fixture');return self.git('rev-parse','HEAD')
    def same(self):return gate.equivalent(self.repo,self.base,self.commit(),self.inputs)[0]
    def test_history_decisions(self):
        with self.subTest(phase="deleted_source_cannot_prove_equivalence"):
            (self.repo/'os/Containerfile').unlink();head=self.commit()
            with self.assertRaises(subprocess.CalledProcessError):gate.equivalent(self.repo,self.base,head,self.inputs)
        restart_fixture(self)
        with self.subTest(phase="manual_forces_unchanged_inputs"):
            self.assertEqual(self.invoke(self.base,'workflow_dispatch'),(0,'changed=true'))
        restart_fixture(self)
        with self.subTest(phase="unknown_history_builds_but_blocks_publication"):
            self.assertEqual(self.invoke('0'*40),(0,'changed=true'))
            self.assertEqual(self.invoke('0'*40,require=True),(1,'changed=true'))
        restart_fixture(self)
        with self.subTest(phase="known_unchanged_history_skips"):
            self.assertEqual(self.invoke(self.base),(0,'changed=false'))
        restart_fixture(self)
        with self.subTest(phase="pin_without_source_revision_refreshes"):
            head=self.pin(None)
            self.assertEqual(self.invoke(None,head=head),(0,'changed=true'))
    def invoke(self,base,event='push',require=False,head=None):
        selector=['--since-pin'] if base is None else ['--base',base]
        args=['gate',*selector,'--head',head or self.base,'--event',event]+(['--require-equivalent'] if require else [])
        output=self.repo/'output'
        with patch.object(gate,'ROOT',self.repo),patch.object(gate,'INPUTS',tuple(self.inputs+[gate.PIN])),patch('sys.argv',args),patch.dict(os.environ,{'GITHUB_OUTPUT':str(output)}),contextlib.redirect_stdout(io.StringIO()):
            result=gate.main()
        return result,output.read_text().splitlines()[-1]
    def test_since_pin_detection(self):
        with self.subTest(phase="snapshot_workflow_triggers_on_every_input"):
            workflow=(ROOT/'.forgejo/workflows/snapshot-rpms.yml').read_text(encoding='utf-8')
            push=workflow.split('\n  push:\n',1)[1].split('\n\n',1)[0]
            paths={line.strip()[2:].strip('"').removesuffix('/**') for line in push.splitlines() if line.strip().startswith('- ') and '/' in line}
            self.assertEqual(paths,set(gate.INPUTS))
        restart_fixture(self)
        with self.subTest(phase="since_pin_ignores_the_pin_and_intermediate_pushes"):
            pinned=self.pin(self.base)
            self.write('unrelated.txt','x\n');head=self.commit()
            self.assertEqual(self.invoke(None,head=head),(0,'changed=false'))
            self.assertNotEqual(pinned,head)
        restart_fixture(self)
        with self.subTest(phase="since_pin_sees_a_change_that_landed_before_an_earlier_push"):
            self.write('os/Containerfile',(ROOT/'os/Containerfile').read_text(encoding='utf-8')+'\nRUN dnf install -y new-package\n')
            self.commit()
            # The pin still names the older source; a later unrelated push must refresh.
            self.pin(self.base);self.write('unrelated.txt','x\n');head=self.commit()
            self.assertEqual(self.invoke(None,head=head),(0,'changed=true'))
    def pin(self,revision):
        self.write(gate.PIN,'RPMS_REF=registry.tilo.so/tilo/rpms@sha256:'+'a'*64+'\n'+(f'RPMS_SOURCE_REVISION={revision}\n' if revision else ''))
        return self.commit()
    def newer(self,source,head):
        args=['gate','--base',source,'--head',head,'--require-newer-than-pin']
        with patch.object(gate,'ROOT',self.repo),patch('sys.argv',args),contextlib.redirect_stdout(io.StringIO()):
            return gate.main()==0
    def test_promotion_ordering(self):
        with self.subTest(phase="promotion_supersedes_only_an_older_pin"):
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
        restart_fixture(self)
        with self.subTest(phase="promotion_refuses_an_unrecorded_pin_that_landed_later"):
            source=self.pin(None)
            self.write(gate.PIN,'RPMS_REF=registry.tilo.so/tilo/rpms@sha256:'+'b'*64+'\n');main=self.commit()
            self.assertFalse(self.newer(source,main))
if __name__=='__main__':unittest.main()
