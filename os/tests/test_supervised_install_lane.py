"""Pure admission tests; no cluster, image, disk or VM operation."""
import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import sys
import tempfile
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
    def test_gui_evidence_requires_desktop_and_rendered_local_browser_page(self):
        html='<!doctype html><h1>Tilo browser offline ready</h1>\n'
        good={'oobe_intro':'Create your local identity',
              'oobe_backup':'Back up your Tilo identity',
              'oobe_ready':'Your identity is ready',
              'tour_move':'Everything is close at hand',
              'tour_arrange':'A place for each kind of work',
              'tour_share':'Sharing stays deliberate',
              'tour_start':'Your first five minutes',
              'desktop_ocr':'cycle-tilo',
              'browser_ocr':'Tilo browser offline ready',
              'browser_fixture':'url=http://10.0.2.2:18765/index.html\n'
                                'sha256='+hashlib.sha256(html.encode()).hexdigest()+'\n'
                                'transport=qemu-user-local-gateway\n'
                                'external_references=none\n',
              'browser_fixture_html':html}
        self.assertIsNone(lane.validate_gui_evidence(good))
        for key,bad in [('oobe_ready',''),('tour_share',''),('desktop_ocr',''),('browser_ocr','Browser opened'),
                        ('browser_fixture',good['browser_fixture'].replace('external_references=none','external_references=https')),
                        ('browser_fixture_html',html+'tampered')]:
            changed=dict(good);changed[key]=bad
            with self.subTest(key=key),self.assertRaises(ValueError):
                lane.validate_gui_evidence(changed)

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
              'patch_sha256':lane.installer_bootc_helper.PATCH_SHA256,
              'binary_sha256':digest,'binary_build_id':(b'b'*20).hex(),
              'rustc_version':'rustc 1.91.0','cargo_version':'cargo 1.91.0',
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
                'stage': 'bootc-install-to-disk', 'argv_sha256': 'a' * 64,
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
                    'stage': 'bootc-install-to-disk', 'argv_sha256': 'a' * 64,
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

    def test_creator_prepares_fresh_private_output_before_creation(self):
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
        self.assertLess(script.index('test ! -e /work/disk-output'),
                        script.index('mkdir -m 0700 /work/disk-output'))
        self.assertEqual(script.count("0:0:700:directory"), 2)
        self.assertEqual(command.call_args.kwargs, {'timeout': 30})

    def test_creator_inspects_then_prepares_then_executes(self):
        events = []
        args = type('Args', (), {'context': 'ctx'})()
        with patch.object(lane, 'controller', side_effect=lambda *a: events.append(('controller', a[4]))), \
                patch.object(lane, 'prepare_disk_output', side_effect=lambda *a: events.append(('prepare', None))):
            lane.create_on_worker(Path('/root'), args, {'pod': 'disk'}, {'pod': 'agent'})
        self.assertEqual(events, [('controller', True), ('prepare', None), ('controller', False)])

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
        worker={'namespace':'kata-system','pod':'tilo-verify-run1'}
        with patch.object(lane,'command',side_effect=[b'{}',b'']) as command:
            lane.verify_disk('ctx',worker,values,b'{"auths":{}}')
        argv=command.call_args_list[0].args[0]
        self.assertEqual(argv[argv.index('--copy-receipt')+1],'/work/disk-output/copy.json')
        self.assertIn("trap 'rm -f -- /work/registry-auth.json'",argv[argv.index('sh')+2])

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
