"""Pure admission tests; no cluster, image, disk or VM operation."""
import base64
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import re
import struct
import sys
import tempfile
import tarfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

DEV=Path(__file__).resolve().parents[1]/'dev'
sys.path.insert(0, str(DEV))
spec=importlib.util.spec_from_file_location('lane',DEV/'run-supervised-install-lane.py')
lane=importlib.util.module_from_spec(spec);spec.loader.exec_module(lane)


def node(name,*,disk=False,kvm=False,ready=True):
    labels={'kubernetes.io/arch':'arm64'}
    if disk:labels['agent-sandbox.sandbox.example/disk-kernel']='e23e6ab7f708'
    return {'metadata':{'name':name,'labels':labels},'spec':{'unschedulable':False},
      'status':{'conditions':[{'type':'Ready','status':'True' if ready else 'False'}],
                'allocatable':{'devic.es/kvm':'1' if kvm else '0'}}}


def helper_elf():
    value=bytearray(156);value[:6]=b'\x7fELF\x02\x01'
    struct.pack_into('<H',value,18,183);struct.pack_into('<Q',value,32,64)
    struct.pack_into('<HH',value,54,56,1);struct.pack_into('<I',value,64,4)
    struct.pack_into('<Q',value,72,120);struct.pack_into('<Q',value,96,36)
    struct.pack_into('<III',value,120,4,20,3);value[132:136]=b'GNU\0';value[136:156]=b'b'*20
    return bytes(value)


class LaneAdmission(unittest.TestCase):
    def test_lane_timings_are_ordered_typed_and_credential_free(self):
        class Clock:
            value = 0
            def __call__(self):
                self.value += 10
                return self.value
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'timings.json';timer=lane.LaneTimer(path,Clock())
            for name in lane.TIMING_STAGES:
                with timer.stage(name):
                    pass
            receipt=timer.finish()
            persisted=json.loads(path.read_text())
            lane.validate_lane_timings(persisted)
            self.assertEqual(receipt['status'],'succeeded')
            self.assertEqual([row['name'] for row in persisted['stages']],list(lane.TIMING_STAGES))
            self.assertTrue(all(row['status']=='succeeded' for row in persisted['stages']))
            self.assertNotIn('registry',path.read_text().lower())
            hostile=dict(persisted);hostile['credential']='secret'
            with self.assertRaises(ValueError):
                lane.validate_lane_timings(hostile)

    def test_lane_timing_failure_is_persisted_at_the_failed_stage(self):
        class Clock:
            value = 0
            def __call__(self):
                self.value += 10
                return self.value
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'timings.json';timer=lane.LaneTimer(path,Clock())
            with self.assertRaisesRegex(RuntimeError,'boom'):
                with timer.stage('creator_admission'):
                    raise RuntimeError('boom')
            receipt=json.loads(path.read_text())
            self.assertEqual(receipt['status'],'failed')
            self.assertEqual(receipt['stages'],[{
              'name':'creator_admission','status':'failed',
              'started_offset_ns':10,'duration_ns':10}])
            lane.validate_lane_timings(receipt)

    def test_lane_timings_record_skipped_creator_work_on_resume(self):
        class Clock:
            value = 0
            def __call__(self):
                self.value += 10
                return self.value
        with tempfile.TemporaryDirectory() as directory:
            timer=lane.LaneTimer(Path(directory)/'timings.json',Clock())
            with timer.stage('creator_admission'):
                pass
            for name in ('input_transfer','create_install','freeze_identity'):
                timer.skip(name)
            for name in lane.TIMING_STAGES[4:]:
                with timer.stage(name):
                    pass
            receipt=timer.finish()
            self.assertEqual([row['status'] for row in receipt['stages'][1:4]],
                             ['skipped']*3)
            lane.validate_lane_timings(receipt)

    def resumable_fixture(self, directory, expires_at=4102444800):
        root=Path(directory)
        creator_node=node('disk',disk=True)
        creator_node['metadata']['uid']='disk-node-uid'
        creator={'pod':'tilo-disk-run1','pod_uid':'creator-pod-uid','namespace':'kata-system',
          'node':'disk','node_uid':'disk-node-uid','spec':{},'image_id':lane.DISK_IMAGE}
        args=SimpleNamespace(operation_dir=root,helper_revision='a'*40,
          acceptance_id='run1',context='ctx',verifier_node=None,vm_node=None)
        creation=b'{"disk_created":true}\n'
        identity={'size':64*1024**3,'sha256':'b'*64}
        (root/'creation.json').write_bytes(creation)
        (root/'raw-identity.json').write_text(json.dumps(identity))
        checkpoint=lane.checkpoint_creator(args,creator,creator_node,creation,identity,expires_at)
        return args,checkpoint,creator_node,creator,creation,identity

    def test_creator_inputs_share_one_transfer_but_retain_distinct_hashes(self):
        inputs = (b'helper archive', b'public archive', b'installer archive')
        outer, identities = lane.creator_input_archive(*inputs)
        self.assertEqual(set(identities), {
          'helpers.tar', 'public-trust.tar', 'installer-bootc-helper.tar'})
        with tarfile.open(fileobj=io.BytesIO(outer), mode='r:') as archive:
            members = archive.getmembers()
            self.assertEqual([member.name for member in members], list(identities))
            for member, expected in zip(members, inputs):
                self.assertEqual(member.mode, 0o600)
                self.assertEqual(archive.extractfile(member).read(), expected)
                self.assertEqual(identities[member.name], hashlib.sha256(expected).hexdigest())

        with patch.object(lane, 'command') as command:
            lane.stream_creator_inputs('ctx', 'kata-system', 'pod', 'disk', *inputs)
        self.assertEqual(command.call_count, 1)
        argv, payload = command.call_args.args[:2]
        self.assertEqual(argv[:8], ['kubectl', '--context', 'ctx', '-n', 'kata-system',
                                    'exec', '-i', 'pod'])
        self.assertEqual(payload, outer)
        self.assertNotIn(b'registry-auth', payload)
        self.assertEqual(argv[-3:], [identities[name] for name in identities])
        script = argv[argv.index('-ceu') + 1]
        self.assertIn('sha256sum -c -', script)
        self.assertIn('mv -T -n', script)
        self.assertIn('own_helpers=0; own_public=0; own_installer=0', script)

    def test_pending_creator_requires_an_exact_atomic_input_receipt(self):
        inputs=(b'helpers',b'public',b'installer')
        expected={'schema':1,'purpose':'tilo-supervised-creator-inputs',
                  'helpers':hashlib.sha256(inputs[0]).hexdigest(),
                  'public':hashlib.sha256(inputs[1]).hexdigest(),
                  'installer':hashlib.sha256(inputs[2]).hexdigest()}
        worker={'namespace':'kata-system','pod':'tilo-disk-run1'}
        with patch.object(lane,'command',return_value=json.dumps(expected)):
            self.assertTrue(lane.creator_inputs_ready('ctx',worker,*inputs))
        with patch.object(lane,'command',return_value='{}'):
            self.assertFalse(lane.creator_inputs_ready('ctx',worker,*inputs))
        with patch.object(lane,'command',side_effect=lane.ControllerTransportError('relay')):
            with self.assertRaises(lane.ControllerTransportError):
                lane.creator_inputs_ready('ctx',worker,*inputs)

    def test_gui_evidence_requires_desktop_and_rendered_local_browser_page(self):
        html='<!doctype html><h1>Tilo browser offline ready</h1>\n'
        good={'oobe_intro':'Create your local identity',
              'oobe_backup':'Back up your Tilo identity',
              'oobe_backup_resumed':'Back up your Tilo identity',
              'oobe_ready':'Your identity is ready',
              'tour_move':'Everything is close at hand',
              'tour_arrange':'A place for each kind of work',
              'tour_share':'Sharing stays deliberate',
              'tour_start':'Your first five minutes',
              'desktop_ocr':'cycle-tilo',
              'reboot_greeter':'Welcome back',
              'reboot_login':'cycle-tilo',
              'browser_ocr':'Tilo browser offline ready',
              'browser_fixture':'url=http://10.0.2.2:18765/index.html\n'
                                'sha256='+hashlib.sha256(html.encode()).hexdigest()+'\n'
                                'transport=qemu-user-local-gateway\n'
                                'external_references=none\n',
              'browser_fixture_html':html}
        self.assertIsNone(lane.validate_gui_evidence(good))
        for key,bad in [('oobe_backup_resumed',''),('oobe_ready',''),('tour_share',''),
                        ('desktop_ocr',''),('reboot_greeter',''),('reboot_login',''),
                        ('browser_ocr','Browser opened'),
                        ('browser_fixture',good['browser_fixture'].replace('external_references=none','external_references=https')),
                        ('browser_fixture_html',html+'tampered')]:
            changed=dict(good);changed[key]=bad
            with self.subTest(key=key),self.assertRaises(ValueError):
                lane.validate_gui_evidence(changed)

    def test_supervised_evidence_paths_match_cycle_screen_names(self):
        cycle=(DEV/'test-installer-cycle.sh').read_text()
        screen_names=set(re.findall(r'^wait_screen ([^ ]+)',cycle,re.MULTILINE))
        paths=lane.cycle_evidence_paths('fixture')
        screen_paths={key:Path(value).stem for key,value in paths.items()
                      if value.endswith('.txt') and key not in {
                          'verdict','exit_status','browser_fixture'}}
        self.assertEqual(set(screen_paths.values()), {
            '08-oobe-intro','09-oobe-backup','10-oobe-backup-resumed','11-oobe-ready',
            '12-tour-move','13-tour-arrange','14-tour-share','15-tour-start',
            '16-desktop-config','18-browser-offline','19-reboot-greeter','20-reboot-login'})
        self.assertTrue(set(screen_paths.values()).issubset(screen_names))

    def test_selects_separate_current_workers(self):
        document={'items':[node('disk',disk=True),node('verify',disk=True),node('vm',kvm=True)]}
        disk,vm=lane.select_nodes(document)
        verifier=lane.select_verifier_node(document,disk,vm)
        self.assertEqual((disk['metadata']['name'],vm['metadata']['name']),('disk','vm'))
        self.assertEqual(verifier['metadata']['name'],'verify')
        manifest=lane.disk_manifest('tilo-disk-run1',disk)
        self.assertEqual(manifest['spec']['nodeSelector']['agent-sandbox.sandbox.example/kata'],'true')
        self.assertEqual(manifest['spec']['nodeSelector']['katacontainers.io/kata-runtime'],'true')
        self.assertEqual(manifest['spec']['tolerations'][-2:], [
          {'key':'node.kubernetes.io/not-ready','operator':'Exists','effect':'NoExecute','tolerationSeconds':300},
          {'key':'node.kubernetes.io/unreachable','operator':'Exists','effect':'NoExecute','tolerationSeconds':300}])
        self.assertEqual(manifest['spec']['containers'][0]['env'],
                         [{'name':'REGISTRY_AUTH_FILE','value':'/work/registry-auth.json'}])
        self.assertEqual(lane.vm_manifest('tilo-vm-run1',vm)['spec']['containers'][0]
                         ['resources']['requests']['devic.es/kvm'],'1')

    def test_controller_loss_before_verifier_pauses_credential_free_creator(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,_,_=self.resumable_fixture(directory)
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            with patch.object(lane,'acquire_verifier',side_effect=lane.ControllerTransportError('lost')):
                with self.assertRaises(lane.ControllerTransportError):
                    lane.handoff_creator_to_verifier(Path('/root'),args,
                      {'items':[disk,verify,vm]},checkpoint,disk,vm,creator,b'{}',
                      {'size':64*1024**3,'sha256':'b'*64},b'helpers',b'public')
            paused=json.loads((Path(directory)/'paused-creator.json').read_text())
            self.assertEqual(paused['creator'],creator)
            self.assertEqual(paused['status'],'paused')
            self.assertFalse((Path(directory)/'registry-auth.json').exists())
            self.assertIn('--cleanup-paused',paused['cleanup_command'])

    def test_creator_checkpoint_refuses_credentials_and_manifest_has_bounded_owner(self):
        with tempfile.TemporaryDirectory() as directory:
            args,_,disk,creator,creation,identity=self.resumable_fixture(directory)
            manifest=lane.disk_manifest('tilo-disk-run1',disk,operation='run1',expires_at=1234)
            self.assertEqual(manifest['metadata']['labels'],{
              lane.OWNER_LABEL:lane.OWNER,lane.OPERATION_LABEL:'run1',
              lane.EXPIRES_LABEL:'1234'})
            self.assertEqual(manifest['spec']['activeDeadlineSeconds'],43200)
            (Path(directory)/'registry-auth.json').write_text('{}')
            with self.assertRaisesRegex(ValueError,'cannot retain'):
                lane.checkpoint_creator(args,creator,disk,creation,identity,1234)

    def test_resume_uses_full_creator_node_without_duplicate_creator(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,_,_=self.resumable_fixture(directory)
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            document={'items':[disk,verify,vm]}
            with patch.object(lane,'optional_same_worker',return_value=creator) as same, \
                    patch.object(lane,'create_worker') as create:
                loaded=lane.resume_creator(args,document)
            same.assert_called_once_with('ctx',creator)
            create.assert_not_called()
            self.assertTrue((Path(directory)/'paused-creator.json').exists())
            self.assertIs(loaded[4],disk)
            selected=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            with patch.object(lane,'select_verifier_node',wraps=lane.select_verifier_node) as choose, \
                    patch.object(lane,'adopt_worker',return_value=None), \
                    patch.object(lane,'create_worker',return_value=selected):
                self.assertEqual(lane.acquire_verifier(
                    args,document,checkpoint,loaded[4],loaded[5]),(selected,False))
            self.assertIs(choose.call_args.args[1],disk)

    def test_lost_verifier_create_response_adopts_exact_worker_once(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,_,_=self.resumable_fixture(directory)
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            with patch.object(lane,'adopt_worker',side_effect=[None,verifier]) as adopt, \
                    patch.object(lane,'create_worker',side_effect=lane.ControllerTransportError('lost')) as create:
                self.assertEqual(lane.acquire_verifier(
                  args,{'items':[disk,verify,vm]},checkpoint,disk,vm),(verifier,False))
            create.assert_called_once();self.assertEqual(adopt.call_count,2)
            self.assertEqual(json.loads((Path(directory)/'verifier-handoff.json').read_text())['state'],'started')

    def test_ready_verifier_resume_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,_,_=self.resumable_fixture(directory)
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            lane.write_verifier_handoff(args,checkpoint,verifier,'ready')
            with patch.object(lane,'optional_same_worker',return_value=verifier), \
                    patch.object(lane,'create_worker') as create, \
                    patch.object(lane,'delete_worker') as delete:
                self.assertEqual(lane.acquire_verifier(
                  args,{'items':[disk,verify,vm]},checkpoint,disk,vm),(verifier,True))
            create.assert_not_called();delete.assert_not_called()

    def test_verifier_receipt_failure_cleans_unrecorded_worker(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,_,_=self.resumable_fixture(directory)
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            with patch.object(lane,'adopt_worker',return_value=None), \
                    patch.object(lane,'create_worker',return_value=verifier), \
                    patch.object(lane,'write_verifier_handoff',side_effect=OSError('disk full')), \
                    patch.object(lane,'delete_worker') as delete:
                with self.assertRaisesRegex(OSError,'disk full'):
                    lane.acquire_verifier(args,{'items':[disk,verify,vm]},checkpoint,disk,vm)
            delete.assert_called_once_with('ctx',verifier)

    def test_setup_failure_deletes_verifier_and_keeps_creator_paused(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,creation,identity=self.resumable_fixture(directory)
            lane.pause_creator(args,checkpoint,'creator-checkpoint-ready')
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            with patch.object(lane,'acquire_verifier',return_value=(verifier,False)), \
                    patch.object(lane,'host_agent',side_effect=ValueError('setup failed')), \
                    patch.object(lane,'optional_same_worker',return_value=verifier), \
                    patch.object(lane,'delete_worker') as delete:
                with self.assertRaisesRegex(ValueError,'setup failed'):
                    lane.handoff_creator_to_verifier(Path('/root'),args,
                      {'items':[disk,verify,vm]},checkpoint,disk,vm,creator,creation,
                      identity,b'helpers',b'public')
            delete.assert_called_once_with('ctx',verifier)
            self.assertTrue((Path(directory)/'paused-creator.json').exists())
            self.assertFalse((Path(directory)/'verifier-handoff.json').exists())

    def test_transfer_failure_deletes_partial_verifier_for_clean_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,creation,identity=self.resumable_fixture(directory)
            lane.pause_creator(args,checkpoint,'creator-checkpoint-ready')
            vm=node('vm',kvm=True);verify=node('verify',disk=True)
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            lane.write_verifier_handoff(args,checkpoint,verifier,'started')
            with patch.object(lane,'acquire_verifier',return_value=(verifier,False)), \
                    patch.object(lane,'host_agent',return_value={'metadata':{'name':'agent'}}), \
                    patch.object(lane,'controller'),patch.object(lane,'require_same_worker'), \
                    patch.object(lane,'sparse_transfer',side_effect=ValueError('copy failed')), \
                    patch.object(lane,'optional_same_worker',return_value=verifier), \
                    patch.object(lane,'delete_worker') as delete:
                with self.assertRaisesRegex(ValueError,'copy failed'):
                    lane.handoff_creator_to_verifier(Path('/root'),args,
                      {'items':[disk,verify,vm]},checkpoint,disk,vm,creator,creation,
                      identity,b'helpers',b'public')
            delete.assert_called_once_with('ctx',verifier)
            self.assertFalse((Path(directory)/'verifier-handoff.json').exists())

    def test_creator_delete_failure_keeps_ready_verifier_and_pause_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,_,creator,_,_=self.resumable_fixture(directory)
            lane.pause_creator(args,checkpoint,'creator-checkpoint-ready')
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-uid',node='verify')
            lane.write_verifier_handoff(args,checkpoint,verifier,'ready')
            with patch.object(lane,'optional_same_worker',return_value=creator), \
                    patch.object(lane,'delete_worker',side_effect=lane.ControllerTransportError('lost')):
                with self.assertRaises(lane.ControllerTransportError):
                    lane.release_creator_after_handoff(args,checkpoint,creator,verifier)
            self.assertTrue((Path(directory)/'paused-creator.json').exists())
            self.assertEqual(json.loads((Path(directory)/'verifier-handoff.json').read_text())['state'],'ready')
            self.assertTrue((Path(directory)/'creator-release-attempt.json').exists())

    def test_resume_advances_started_release_after_creator_delete_crash(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,disk,creator,_,_=self.resumable_fixture(directory)
            lane.pause_creator(args,checkpoint,'ControllerTransportError')
            verify=node('verify',disk=True);verify['metadata']['uid']='verify-node-uid'
            vm=node('vm',kvm=True)
            verifier=dict(creator,pod='tilo-verify-run1',pod_uid='verify-pod-uid',
                          node='verify',node_uid='verify-node-uid')
            lane.write_verifier_handoff(args,checkpoint,verifier,'started')
            lane.write_release_attempt(args,checkpoint,verifier)
            document={'items':[disk,verify,vm]}
            with patch.object(lane,'optional_same_worker',side_effect=[None,verifier]) as same, \
                    patch.object(lane,'create_worker') as create, \
                    patch.object(lane,'delete_worker') as delete:
                loaded=lane.resume_creator(args,document)
            self.assertEqual(same.call_args_list[0].args,('ctx',creator))
            self.assertEqual(same.call_args_list[1].args[:2],('ctx',verifier))
            self.assertEqual(same.call_args_list[1].args[2]['metadata']['name'],'tilo-verify-run1')
            self.assertEqual(json.loads((Path(directory)/'verifier-handoff.json').read_text())['state'],
                             'released')
            self.assertTrue((Path(directory)/'creator-released.json').exists())
            self.assertFalse((Path(directory)/'paused-creator.json').exists())
            self.assertIs(loaded[4],disk)
            create.assert_not_called();delete.assert_not_called()

    def test_checkpoint_write_failure_deletes_creator(self):
        with tempfile.TemporaryDirectory() as directory:
            args,_,disk,creator,creation,identity=self.resumable_fixture(directory)
            with patch.object(lane,'checkpoint_creator',side_effect=OSError('disk full')), \
                    patch.object(lane,'delete_worker') as delete:
                with self.assertRaisesRegex(OSError,'disk full'):
                    lane.establish_creator_checkpoint(args,creator,disk,creation,identity,1234)
            delete.assert_called_once_with('ctx',creator)

    def test_adopted_worker_requires_exact_ownership_labels(self):
        manifest=lane.disk_manifest('tilo-verify-run1',node('verify',disk=True),deadline=7200,
          operation='run1',expires_at=1234)
        manifest['spec']['nodeName']='verify'
        pod={'metadata':{'name':'tilo-verify-run1','namespace':'kata-system','uid':'pod-uid','labels':{}},
             'spec':manifest['spec'],'status':{'phase':'Running','containerStatuses':[{
               'ready':True,'restartCount':0,'imageID':lane.DISK_IMAGE}]}}
        with self.assertRaisesRegex(ValueError,'ownership'):
            lane.binding(pod,{'metadata':{'uid':'node-uid'}},manifest)

    def test_resume_rejects_typed_checkpoint_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,_,_,_,_=self.resumable_fixture(directory)
            path=Path(directory)/'creator-checkpoint.json'
            hostile=json.loads(path.read_text());hostile['creator_node']='disk'
            path.write_text(json.dumps(hostile))
            with self.assertRaisesRegex(ValueError,'full node document'):
                lane.load_creator_checkpoint(args)

    def test_expired_pause_cleanup_checks_owner_uid_and_never_runs_early(self):
        with tempfile.TemporaryDirectory() as directory:
            args,checkpoint,_,creator,_,_=self.resumable_fixture(directory,expires_at=100)
            lane.pause_creator(args,checkpoint,'ControllerTransportError')
            pod={'metadata':{'uid':creator['pod_uid'],'labels':{
              lane.OWNER_LABEL:lane.OWNER,lane.OPERATION_LABEL:args.acceptance_id,
              lane.EXPIRES_LABEL:'100'}}}
            with patch.object(lane,'api_json',return_value=pod), \
                    patch.object(lane,'delete_worker') as delete:
                with self.assertRaisesRegex(ValueError,'not reached'):
                    lane.cleanup_paused_creator('ctx',directory,now=99)
                lane.cleanup_paused_creator('ctx',directory,now=100)
            delete.assert_called_once_with('ctx',creator)
            self.assertTrue((Path(directory)/'paused-creator-cleaned.json').exists())

    def test_stale_or_shared_nodes_are_rejected(self):
        with self.assertRaisesRegex(ValueError,'disk-kernel'):
            lane.select_nodes({'items':[node('plain'),node('vm',kvm=True)]})
        with self.assertRaisesRegex(ValueError,'separate'):
            lane.select_nodes({'items':[node('both',disk=True,kvm=True)]},'both','both')
        with self.assertRaisesRegex(ValueError,'ready ARM'):
            lane.select_nodes({'items':[node('disk',disk=True,ready=False),node('vm',kvm=True)]},'disk')
        disk=node('disk',disk=True);vm=node('vm',kvm=True)
        with self.assertRaisesRegex(ValueError,'verifier'):
            lane.select_verifier_node({'items':[disk,vm]},disk,vm)

    def test_disk_arguments_bind_the_exact_media_lane(self):
        value={'source':'registry.invalid/os@sha256:'+'a'*64,'target':'registry.invalid/os:dev',
          'architecture':'aarch64','revision':'b'*40,'manifest_sha256':'c'*64,
          'composefs_digest':'d'*128,'systemd_version':'259.8-1.fc44',
          'public_bundle':'/work/public-trust','output':'/work/disk-output/disk.raw',
          'generation':1,'size_bytes':64*1024**3,'offline_key_signature':True,
          'disposable_guest':True,'boot_profile':'installer-media',
          'source_live_uki':'boot/EFI/Linux/live.efi',
          'installer_bootc_directory':'/work/installer-bootc-helper',
          'installer_bootc_sha256':'e'*64}
        lane.validate_disk_args(value)
        for key,bad in [('size_bytes',1),('architecture','x86_64'),('boot_profile','installed-target')]:
            changed=dict(value);changed[key]=bad
            with self.assertRaises(ValueError):lane.validate_disk_args(changed)

    def test_installer_bootc_archive_binds_exact_layout_and_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);binary=root/'bootc';binary.write_bytes(helper_elf());binary.chmod(0o755)
            digest=hashlib.sha256(binary.read_bytes()).hexdigest()
            receipt={'schema':1,'purpose':lane.installer_bootc_helper.PURPOSE,
              'architecture':'aarch64','bootc_version':lane.installer_bootc_helper.BOOTC_VERSION,
              'source_tag':lane.installer_bootc_helper.SOURCE_TAG,
              'source_commit':lane.installer_bootc_helper.SOURCE_COMMIT,
              'source_cargo_lock_sha256':lane.installer_bootc_helper.SOURCE_CARGO_LOCK_SHA256,
              'patch_sha256':lane.installer_bootc_helper.PATCH_SHA256,
              'binary_sha256':digest,'binary_build_id':(b'b'*20).hex(),
              'rustc_version':'rustc 1.91.0','cargo_version':'cargo 1.91.0',
              'rust_toolchain':lane.installer_bootc_helper.RUST_TOOLCHAIN,
              'build_image':lane.installer_bootc_helper.BUILD_IMAGE,
              'build_target':lane.installer_bootc_helper.TARGETS['aarch64'],
              'build_command':lane.installer_bootc_helper.BUILD_COMMAND}
            (root/'receipt.json').write_text(json.dumps(receipt))
            archive=lane.installer_bootc_archive(root,digest)
            self.assertGreater(len(archive),len(binary.read_bytes()))
            (root/'unexpected').write_text('no')
            with self.assertRaisesRegex(ValueError,'exactly'):
                lane.installer_bootc_archive(root,digest)

    def test_manifests_pin_images_and_avoid_host_paths(self):
        disk=lane.disk_manifest('tilo-disk-run1',node('disk',disk=True))
        vm=lane.vm_manifest('tilo-vm-run1',node('vm',kvm=True))
        self.assertTrue(lane.digest_reference(disk['spec']['containers'][0]['image']))
        self.assertTrue(lane.digest_reference(vm['spec']['containers'][0]['image']))
        self.assertFalse(disk['spec']['automountServiceAccountToken'])
        self.assertFalse(vm['spec']['automountServiceAccountToken'])
        self.assertTrue(all('hostPath' not in volume for manifest in (disk,vm)
                            for volume in manifest['spec']['volumes']))

    def test_binding_requires_the_observed_pinned_image(self):
        manifest=lane.vm_manifest('tilo-vm-run1',node('vm',kvm=True))
        manifest['spec']['nodeName']='vm'
        pod={'metadata':{'name':'tilo-vm-run1','namespace':'agent-sandboxes','uid':'pod-uid'},
             'spec':manifest['spec'],'status':{'phase':'Running','containerStatuses':[{
               'ready':True,'restartCount':0,'imageID':lane.VM_IMAGE}]}}
        n={'metadata':{'uid':'node-uid'}}
        self.assertEqual(lane.binding(pod,n)['image_id'],lane.VM_IMAGE)
        pod['status']['containerStatuses'][0]['imageID']='registry.invalid/latest'
        with self.assertRaisesRegex(ValueError,'running worker image'):
            lane.binding(pod,n)

    def test_disk_binding_accepts_only_explicit_admission_defaults(self):
        manifest=lane.disk_manifest('tilo-disk-run1',node('disk',disk=True))
        manifest['spec']['nodeName']='disk'
        pod={'metadata':{'name':'tilo-disk-run1','namespace':'kata-system','uid':'pod-uid'},
             'spec':manifest['spec'],'status':{'phase':'Running','containerStatuses':[{
               'ready':True,'restartCount':0,'imageID':lane.DISK_IMAGE}]}}
        n={'metadata':{'uid':'node-uid'}}
        self.assertEqual(lane.binding(pod,n,manifest)['image_id'],lane.DISK_IMAGE)
        for change in ('selector','toleration'):
            changed=copy.deepcopy(pod)
            if change=='selector':
                changed['spec']['nodeSelector']['unreviewed.example/selector']='true'
            else:
                changed['spec']['tolerations'].append({'operator':'Exists'})
            with self.subTest(change=change),self.assertRaisesRegex(ValueError,'requested isolation'):
                lane.binding(changed,n,manifest)

    def test_creator_failure_evidence_is_exact_and_bounded(self):
        good = {'schema': 1, 'purpose': 'tilo-supervised-bootc-install-failure',
                'stage': 'bootc-install-to-filesystem', 'argv_sha256': 'a' * 64,
                'returncode': 1, 'stdout_base64': base64.b64encode(b'progress').decode(),
                'stderr_base64': base64.b64encode(b'failure').decode(),
                'stdout_truncated': False, 'stderr_truncated': False}
        lane.validate_creator_failure(good)
        for key, value in [('extra', True), ('returncode', 0),
                           ('stderr_base64', 'not base64!')]:
            hostile = dict(good); hostile[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                lane.validate_creator_failure(hostile)

    def test_creator_failure_collection_is_private_and_never_overwrites(self):
        document = {'schema': 1, 'purpose': 'tilo-supervised-bootc-install-failure',
                    'stage': 'bootc-install-to-filesystem', 'argv_sha256': 'a' * 64,
                    'returncode': 1, 'stdout_base64': '', 'stderr_base64': '',
                    'stdout_truncated': False, 'stderr_truncated': False}
        disk = {'namespace': 'kata-system', 'pod': 'tilo-disk-run1'}
        with tempfile.TemporaryDirectory() as directory:
            args = SimpleNamespace(context='ctx', operation_dir=Path(directory))
            result = SimpleNamespace(returncode=0, stdout=json.dumps(document).encode())
            with patch.object(lane.subprocess, 'run', return_value=result), \
                    patch.object(lane, 'command') as command:
                self.assertTrue(lane.collect_creator_failure(args, disk))
                evidence = Path(directory) / 'creator-install-failure.json'
                self.assertEqual(evidence.stat().st_mode & 0o777, 0o600)
                self.assertEqual(json.loads(evidence.read_text()), document)
                self.assertEqual(command.call_count, 1)
                with self.assertRaises(FileExistsError):
                    lane.collect_creator_failure(args, disk)
                self.assertEqual(command.call_count, 1)

    def test_creator_only_replaces_exact_empty_private_output_before_creation(self):
        disk = {'namespace': 'kata-system', 'pod': 'tilo-disk-run1'}
        with patch.object(lane, 'command') as command:
            lane.prepare_disk_output('ctx', disk)
        argv = command.call_args.args[0]
        script = argv[-1]
        self.assertIn('umask 077', script)
        self.assertLess(script.index('test ! -L /work'), script.index('chmod 0700 /work'))
        self.assertLess(script.index("stat -c '%u:%g:%F' /work"),
                        script.index('chmod 0700 /work'))
        self.assertIn("stat -c '%d' /work", script)
        self.assertLess(script.index('chmod 0700 /work'),
                        script.index("stat -c '%u:%g:%a:%F' /work"))
        output_guard = 'if test -e /work/disk-output || test -L /work/disk-output; then'
        reject_link = 'test ! -L /work/disk-output'
        exact_metadata = "stat -c '%u:%g:%a:%F' /work/disk-output"
        empty_check = 'find /work/disk-output -mindepth 1 -maxdepth 1 -print -quit'
        remove = 'rmdir -- /work/disk-output'
        create = 'mkdir -m 0700 /work/disk-output'
        self.assertLess(script.index(output_guard), script.index(reject_link))
        self.assertLess(script.index(reject_link), script.index(exact_metadata))
        self.assertLess(script.index(exact_metadata), script.index(empty_check))
        self.assertLess(script.index(empty_check), script.index(remove))
        self.assertLess(script.index(remove), script.index(create))
        self.assertNotIn('rm -', script)
        self.assertIn('test ! -e /work/disk-output\ntest ! -L /work/disk-output\n' + create,
                      script)
        self.assertEqual(script.count("0:0:700:directory"), 3)
        self.assertEqual(command.call_args.kwargs, {'timeout': 30})

    def test_creator_inspects_then_starts_idempotent_detached_job(self):
        events = []
        args = type('Args', (), {'context': 'ctx'})()
        with patch.object(lane, 'controller', side_effect=lambda *a, **k: events.append(('controller', a[4]))), \
                patch.object(lane, 'prepare_disk_output', side_effect=lambda *a: events.append(('prepare', None))):
            lane.create_on_worker(Path('/root'), args, {'pod': 'disk'}, {'pod': 'agent'})
        self.assertEqual(events, [('controller', True), ('controller', False)])

    def test_creator_streams_auth_without_local_credential_file(self):
        args=SimpleNamespace(context='ctx',host_pins=Path('/pins'),disk_args=Path('/args'),
            operation_dir=Path('/operation'),helper_revision='a'*40,acceptance_id='run1')
        disk={'namespace':'kata-system','pod':'tilo-disk-run1','pod_uid':'pod-uid',
              'node':'disk','node_uid':'node-uid'}
        agent={'metadata':{'name':'kata-deploy'}}
        result=SimpleNamespace(returncode=0,stdout=b'{}',stderr=b'')
        with patch.object(lane.subprocess,'run',return_value=result) as command:
            lane.controller(Path('/root'),args,disk,agent,False,registry_auth_bytes=b'{"auths":{"registry":{}}}')
        argv=command.call_args.args[0];payload=command.call_args.kwargs['input']
        self.assertIn('--registry-auth-stdin',argv)
        self.assertIn('--creator-job-receipt',argv)
        self.assertNotIn('registry-auth.json',str(argv))
        self.assertEqual(payload,b'{"auths":{"registry":{}}}')
        self.assertEqual(command.call_args.kwargs['timeout'],7200)
        self.assertFalse(command.call_args.kwargs['text'])

    def test_creator_child_transport_sentinel_preserves_worker(self):
        args=SimpleNamespace(context='ctx',host_pins=Path('/pins'),disk_args=Path('/args'),
            operation_dir=Path('/operation'),helper_revision='a'*40,acceptance_id='run1')
        disk={'namespace':'kata-system','pod':'tilo-disk-run1','pod_uid':'pod-uid',
              'node':'disk','node_uid':'node-uid'};agent={'metadata':{'name':'kata-deploy'}}
        result=SimpleNamespace(returncode=75,stdout='',stderr='TILO_CREATOR_CONTROLLER_TRANSPORT\n')
        with patch.object(lane.subprocess,'run',return_value=result), \
                patch.object(lane,'collect_creator_failure') as failure:
            with self.assertRaises(lane.ControllerTransportError):
                lane.controller(Path('/root'),args,disk,agent,False)
        failure.assert_not_called()

    def test_resume_pending_creator_adopts_only_bound_worker(self):
        with tempfile.TemporaryDirectory() as directory:
            args,_,disk_node,creator,_,_=self.resumable_fixture(directory)
            (Path(directory)/'worker-bindings.json').write_text(json.dumps({'creator':creator}))
            vm=node('vm',kvm=True)
            pod={'metadata':{'labels':{lane.OWNER_LABEL:lane.OWNER,
                 lane.OPERATION_LABEL:args.acceptance_id,lane.EXPIRES_LABEL:'4102444800'}}}
            with patch.object(lane,'optional_same_worker',return_value=creator), \
                    patch.object(lane,'api_json',return_value=pod):
                disk,current,selected_vm,expires=lane.resume_pending_creator(args,{'items':[disk_node,vm]})
            self.assertEqual(disk,creator)
            self.assertEqual(current,disk_node)
            self.assertEqual(selected_vm,vm)
            self.assertEqual(expires,4102444800)

    def test_legacy_controller_credential_is_removed_only_when_private(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'registry-auth.json';path.write_text('{"auths":{}}');path.chmod(0o600)
            lane.remove_legacy_registry_auth(directory)
            self.assertFalse(path.exists())
            path.write_text('{"auths":{}}');path.chmod(0o644)
            with self.assertRaisesRegex(ValueError,'residue'):
                lane.remove_legacy_registry_auth(directory)

    def test_finalized_layer_audit_must_bind_creation(self):
        creation={'source':{'image_id':'sha256:'+'a'*64,'hardware_profile':'generic-arm64'},
                  'boot':{'target_uki_sha256':'b'*64}}
        receipt={'layer_material_audit':{'status':'passed-scoped-scan','image_id':'sha256:'+'a'*64},
                 'hardware_profile':'generic-arm64','prediction':creation['boot'],
                 'selected_boot':{'source_path':'boot/EFI/Linux/live.efi'}}
        self.assertIsNone(lane.validate_finalized_verification(receipt,creation))
        receipt['layer_material_audit']['status']='rejected-private-material'
        with self.assertRaisesRegex(ValueError,'source-layer'):
            lane.validate_finalized_verification(receipt,creation)

    def test_transfer_receipt_rebinds_copy_without_losing_creator_identity(self):
        source={'device':10,'inode':20,'size':64*1024**3}
        destination={'device':30,'inode':40,'size':64*1024**3}
        identity={'size':64*1024**3,'sha256':'a'*64}
        creation=(json.dumps({'path':'/work/disk-output/disk.raw',
          'file_identity':source,'size_bytes':identity['size'],'disk_created':True},
          sort_keys=True)+'\n').encode()
        receipt=lane.transfer_receipt(creation,identity,destination)
        self.assertEqual(receipt['source_file_identity'],source)
        self.assertEqual(receipt['file_identity'],destination)
        self.assertEqual(receipt['source_creation_sha256'],hashlib.sha256(creation).hexdigest())
        with self.assertRaisesRegex(ValueError,'cannot bind'):
            lane.transfer_receipt(creation,identity,destination|{'size':1})

    def test_verifier_provisions_exact_loop_devices_before_disk_open(self):
        verifier={'namespace':'kata-system','pod':'tilo-verify-run1'}
        with patch.object(lane,'command') as command:
            lane.prepare_verifier_devices('ctx',verifier)
        argv=command.call_args.args[0]
        self.assertEqual(argv[-3:-1],['python3','-c'])
        self.assertIn('sys.path.insert(0,str(p.parent))',argv[-1])
        self.assertIn('m.ensure_loop_devices()',argv[-1])
        self.assertEqual(command.call_args.kwargs,{'timeout':30})

    def test_verifier_uses_copy_receipt_and_cleans_auth(self):
        values={'source':'registry.invalid/os@sha256:'+'a'*64,
          'target':'registry.invalid/os:dev','architecture':'aarch64','revision':'b'*40,
          'manifest_sha256':'c'*64,'generation':1,'composefs_digest':'d'*128,
          'systemd_version':'259.8-1.fc44','source_live_uki':'boot/EFI/Linux/live.efi'}
        worker={'namespace':'kata-system','pod':'tilo-verify-run1','pod_uid':'verify-pod-uid',
          'node':'verify','node_uid':'verify-node-uid','spec':{},'image_id':lane.DISK_IMAGE}
        with patch.object(lane,'detached_job',return_value=b'{}') as detached:
            lane.verify_disk('ctx',worker,values,b'{"auths":{}}')
        argv=detached.call_args.args[4]
        self.assertEqual(argv[argv.index('--copy-receipt')+1],'/work/disk-output/copy.json')
        self.assertIn('rm -f -- "$source" "$destination"',argv[2])
        self.assertNotIn(b'{"auths":{}}',repr(argv).encode())
        self.assertEqual(detached.call_args.kwargs['inputs']['registry-auth'],b'{"auths":{}}')

    def test_detached_job_adopts_success_after_transport_reconnect(self):
        worker={'namespace':'kata-system','pod':'tilo-verify-run1','pod_uid':'verify-pod-uid',
          'node':'verify','node_uid':'verify-node-uid','spec':{},'image_id':lane.DISK_IMAGE}
        argv=['python3','-c','print("ok")'];inputs={'registry-auth':b'private'}
        operation='run1-installed-verification'
        root=lane._detached_job_root(operation,worker,'disk',argv,inputs)
        contract,content=lane._detached_job_contract(operation,worker,'disk',argv,inputs,root)
        output=b'{"passed":true}\n'
        receipt={'schema':1,'purpose':'tilo-supervised-detached-job',
          'contract_sha256':hashlib.sha256(content).hexdigest(),'operation':operation,
          'worker':contract['worker'],'container':'disk','argv_sha256':contract['argv_sha256'],
          'input_identities':contract['input_identities'],'output_sha256':hashlib.sha256(output).hexdigest(),
          'output_bytes':len(output),'stdout_base64':base64.b64encode(output).decode(),
          'stderr_base64':'','stdout_truncated':False,'stderr_truncated':False,
          'returncode':0,'secrets_cleaned':True,'started_at_ns':1,'finished_at_ns':2,'status':'succeeded'}
        sleeps=[]
        with patch.object(lane,'require_same_worker') as require, \
             patch.object(lane,'_detached_job_status',side_effect=[
               lane.ControllerTransportError('relay'),{'state':'absent'},{'state':'running'},
               {'state':'finished','receipt':json.dumps(receipt)}]), \
             patch.object(lane,'stream_file') as stream, \
             patch.object(lane,'_stage_detached_input') as stage, \
             patch.object(lane,'_launch_detached_job') as launch:
            actual=lane.detached_job('ctx',worker,'disk','installed-verification',argv,
              operation='run1',inputs=inputs,timeout=60,sleeper=sleeps.append)
        self.assertEqual(actual,output)
        self.assertEqual(sleeps,[1,10,10])
        self.assertEqual(stream.call_count,2);stage.assert_called_once();launch.assert_called_once()
        self.assertEqual(require.call_count,2)

    def test_detached_job_fails_closed_when_remote_process_dies(self):
        worker={'namespace':'kata-system','pod':'tilo-verify-run1','pod_uid':'verify-pod-uid',
          'node':'verify','node_uid':'verify-node-uid','spec':{},'image_id':lane.DISK_IMAGE}
        with patch.object(lane,'require_same_worker'), \
             patch.object(lane,'_detached_job_status',return_value={'state':'dead'}):
            with self.assertRaisesRegex(ValueError,'died without a receipt'):
                lane.detached_job('ctx',worker,'disk','raw-identity',['true'],
                  operation='run1',inputs={},timeout=60,sleeper=lambda _:None)

    def test_detached_job_rejects_mismatched_receipt_and_unclean_auth(self):
        worker={'namespace':'kata-system','pod':'tilo-verify-run1','pod_uid':'verify-pod-uid',
          'node':'verify','node_uid':'verify-node-uid','spec':{},'image_id':lane.DISK_IMAGE}
        operation='run1-raw-identity';argv=['true'];inputs={}
        root=lane._detached_job_root(operation,worker,'disk',argv,inputs)
        contract,content=lane._detached_job_contract(operation,worker,'disk',argv,inputs,root)
        output=b'ok\n'
        receipt={'schema':1,'purpose':'tilo-supervised-detached-job',
          'contract_sha256':hashlib.sha256(content).hexdigest(),'operation':operation,
          'worker':dict(contract['worker'],pod_uid='wrong'),'container':'disk',
          'argv_sha256':contract['argv_sha256'],'input_identities':contract['input_identities'],
          'output_sha256':hashlib.sha256(output).hexdigest(),'output_bytes':len(output),
          'stdout_base64':base64.b64encode(output).decode(),'stderr_base64':'',
          'stdout_truncated':False,'stderr_truncated':False,'returncode':0,
          'secrets_cleaned':False,'started_at_ns':1,'finished_at_ns':2,'status':'succeeded'}
        with self.assertRaisesRegex(ValueError,'does not bind'):
            lane._validate_detached_receipt(receipt,contract,content)
        receipt['worker']=contract['worker']
        with self.assertRaisesRegex(ValueError,'did not clean'):
            lane._validate_detached_receipt(receipt,contract,content)

    def test_vm_preflight_checks_exact_runtime_before_media_transfer(self):
        worker={'namespace':'agent-sandboxes','pod':'tilo-vm-run1'}
        with patch.object(lane,'command') as command:
            lane.preflight_vm('ctx',worker)
        argv=command.call_args.args[0]
        self.assertTrue(any('qemu-system-aarch64' in item for item in argv))
        self.assertIn('PROFILE_SHA256='+lane.PROFILE_ENCODER_SHA256,argv)
        script=argv[-1]
        self.assertIn('test -c /dev/kvm',script)
        self.assertIn('bash -n /work/source/os/dev/_common.sh',script)
        self.assertIn('rm -rf /work/vm-preflight',script)
        self.assertEqual(command.call_args.kwargs,{'timeout':120})


if __name__=='__main__':unittest.main()
