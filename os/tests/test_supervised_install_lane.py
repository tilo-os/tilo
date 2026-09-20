"""Pure admission tests; no cluster, image, disk or VM operation."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

DEV=Path(__file__).resolve().parents[1]/'dev'
spec=importlib.util.spec_from_file_location('lane',DEV/'run-supervised-install-lane.py')
lane=importlib.util.module_from_spec(spec);spec.loader.exec_module(lane)


def node(name,*,disk=False,kvm=False,ready=True):
    labels={'kubernetes.io/arch':'arm64'}
    if disk:labels['agent-sandbox.sandbox.example/disk-kernel']='e23e6ab7f708'
    return {'metadata':{'name':name,'labels':labels},'spec':{'unschedulable':False},
      'status':{'conditions':[{'type':'Ready','status':'True' if ready else 'False'}],
                'allocatable':{'devic.es/kvm':'1' if kvm else '0'}}}


class LaneAdmission(unittest.TestCase):
    def test_gui_evidence_requires_desktop_and_rendered_local_browser_page(self):
        html='<!doctype html><h1>Tilo browser offline ready</h1>\n'
        good={'desktop_ocr':'cycle-tilo',
              'browser_ocr':'Tilo browser offline ready',
              'browser_fixture':'url=http://10.0.2.2:18765/index.html\n'
                                'sha256='+hashlib.sha256(html.encode()).hexdigest()+'\n'
                                'transport=qemu-user-local-gateway\n'
                                'external_references=none\n',
              'browser_fixture_html':html}
        self.assertIsNone(lane.validate_gui_evidence(good))
        for key,bad in [('desktop_ocr',''),('browser_ocr','Browser opened'),
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
          'source_live_uki':'boot/EFI/Linux/live.efi'}
        lane.validate_disk_args(value)
        for key,bad in [('size_bytes',1),('architecture','x86_64'),('boot_profile','installed-target')]:
            changed=dict(value);changed[key]=bad
            with self.assertRaises(ValueError):lane.validate_disk_args(changed)

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


if __name__=='__main__':unittest.main()
