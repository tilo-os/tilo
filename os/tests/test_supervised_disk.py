"""Admission checks only; no installer or remote Kubernetes operations."""
import copy
import importlib.util
import io
import json
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, DEV / filename)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    return module
disk = load('disk_supervised', 'create-development-disk.py')
controller = load('controller', 'create-supervised-kata-disk.py')


class GuestAdmission(unittest.TestCase):
    def setUp(self):
        self.args = SimpleNamespace(disposable_guest=True, architecture='aarch64',
            output=Path('/scratch/disk.raw'), source='source',target='target',revision='a'*40,
            generation=1,composefs_digest='b'*64)
        self.observation = dict(node_uid='00000000-0000-0000-0000-000000000001',
            pod_uid='00000000-0000-0000-0000-000000000002',sandbox_id='a'*64,
            container_id='b'*64,kernel_sha256='c'*64)
        for obj,name,value in ((disk.sys,'platform','linux'),(disk.os,'getuid',lambda:0),
            (disk.os,'geteuid',lambda:0),(disk.platform,'machine',lambda:'aarch64'),
            (disk.Path,'read_text',lambda s:'0 0 4294967295'),
            (disk.source,'inputs',lambda *a:None),(disk.prediction.boot,'expected_inputs',lambda *a:None)):
            p=patch.object(obj,name,value);p.start();self.addCleanup(p.stop)
        p=patch.dict(disk.os.environ,{},clear=True);p.start();self.addCleanup(p.stop)

    def test_standalone_none_still_denied(self):
        with patch.object(disk,'command',return_value='none'), patch.object(disk,'_create_verified_disk') as create:
            with self.assertRaisesRegex(ValueError,'QEMU/KVM'):disk.create(self.args)
            create.assert_not_called()

    def test_supervised_records_observation_but_retains_firmware_guard(self):
        with patch.object(disk,'command') as command, patch.object(disk,'reject_firmware_access') as firmware, patch.object(disk,'_create_verified_disk',return_value={'disk_created':True}):
            result=disk.create_from_supervised_controller(self.args,self.observation)
            self.assertEqual(result['controller_observation'],self.observation)
            command.assert_not_called();firmware.assert_called_once()
        with patch.object(disk,'reject_firmware_access',side_effect=ValueError('TPM exposed')),patch.object(disk,'_create_verified_disk') as create:
            with self.assertRaisesRegex(ValueError,'TPM'):disk.create_from_supervised_controller(self.args,self.observation)
            create.assert_not_called()

    def test_supervised_rejects_remote_configuration(self):
        with patch.dict(disk.os.environ,{'CONTAINER_HOST':'ssh://host'}),patch.object(disk,'_create_verified_disk') as create:
            with self.assertRaisesRegex(ValueError,'remote'):disk.create_from_supervised_controller(self.args,self.observation)
            create.assert_not_called()

    def test_no_arbitrary_diagnostics_in_receipt(self):
        with self.assertRaisesRegex(ValueError,'exact guest identity'):disk.create_from_supervised_controller(self.args,self.observation|{'credentials':'secret'})


class ControllerAdmission(unittest.TestCase):
    def setUp(self):
        fixture=json.loads((Path(__file__).parent/'fixtures/kata-disk-environment.json').read_text())
        self.pod=fixture['pod'];self.host=fixture['host']
        self.uid=self.pod['metadata']['uid'];self.node=self.pod['spec']['nodeName']
        self.sid=self.host['sandbox']['ID'];self.cid=self.host['container']['ID']
        self.image=self.pod['spec']['containers'][0]['image']

    def test_matching_live_identity(self):
        self.assertEqual(controller.inspect_pod(self.pod,self.node,self.uid,self.image),self.cid)
        self.assertEqual(controller.inspect_host(self.host,self.pod,self.host['hashes'])['sandbox_id'],self.sid)

    def test_hotplugged_host_disk_rejected_even_absent_from_argv(self):
        self.host['fds'].append({'path':'/dev/nvme1n1','kind':0o60000})
        with self.assertRaisesRegex(ValueError,'block device'):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_actual_export_not_in_oci_rejected(self):
        self.host['mountinfo']=self.host['mountinfo'].rstrip()+'\n1 2 0:1 /etc /run/kata-containers/shared/sandboxes/'+self.sid+'/rw/passthrough/unexpected rw - ext4 /dev/root rw'
        with self.assertRaisesRegex(ValueError,'actual virtiofs'):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_host_volume_or_replaced_pod_rejected(self):
        with self.assertRaisesRegex(ValueError,'identity'):controller.inspect_pod(self.pod,self.node,'different',self.image)
        self.pod['spec']['volumes'].append({'name':'host','hostPath':{'path':'/'}})
        with self.assertRaisesRegex(ValueError,'only dedicated'):controller.inspect_pod(self.pod,self.node,self.uid,self.image)

    def test_changed_runtime_pin_rejected(self):
        with self.assertRaisesRegex(ValueError,'reviewed pins'):controller.inspect_host(self.host,self.pod,{'/kernel':'d'*64})

    def test_untrusted_image_or_observed_image_rejected(self):
        with self.assertRaisesRegex(ValueError,'pinned guest'):controller.inspect_pod(self.pod,self.node,self.uid,'registry.invalid/guest@sha256:'+'a'*64)
        self.pod['status']['containerStatuses'][0]['imageID']='registry.invalid/guest@sha256:'+'a'*64
        with self.assertRaisesRegex(ValueError,'running image'):controller.inspect_pod(self.pod,self.node,self.uid,self.image)

    def test_empty_or_wrong_mount_namespace_rejected(self):
        for inventory in ('', '1 2 0:1 / / rw - tmpfs tmpfs rw'):
            with self.subTest(inventory=inventory):
                self.host['mountinfo']=inventory
                with self.assertRaises(ValueError):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_each_required_rootfs_and_shared_volume_cannot_disappear(self):
        original=self.host['mountinfo']
        roots=['/ro/passthrough/'+identity+'/rootfs' for identity in (self.sid,self.cid)]
        shared=[m['source'] for m in self.host['container']['spec']['mounts'] if m['type']=='bind' and ('configmap' in m['source'] or m['destination'] in ('/etc/hosts','/etc/hostname','/etc/resolv.conf','/dev/termination-log'))]
        for missing in roots+shared:
            with self.subTest(missing=missing):
                self.host['mountinfo']='\n'.join(line for line in original.splitlines() if missing not in line)
                with self.assertRaises(ValueError):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_actual_server_namespace_must_match_host_exports(self):
        self.host['virtiofs']['mountinfo']='\n'.join(line for line in self.host['virtiofs']['mountinfo'].splitlines() if 'configmap/scripts' not in line)
        with self.assertRaisesRegex(ValueError,'inventory differs'):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_actual_server_root_binding_required(self):
        self.host['virtiofs']['root_identity']=[1,2]
        with self.assertRaisesRegex(ValueError,'bound'):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_controller_does_not_report_success_after_container_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);(root/'pins').write_text(json.dumps(self.host['hashes']));(root/'args').write_text('{}')
            calls=[]
            def checked(argv,payload=None):
                if controller.HOST_INSPECT in argv:return json.dumps(self.host)
                if controller.GUEST_RUN in argv:
                    calls.append('executed');return '{"disk_created":true}'
                kind,name=argv[argv.index('get')+1:argv.index('get')+3]
                if kind=='node':return json.dumps({'metadata':{'uid':'node-uid'}})
                if name=='agent':return json.dumps({'metadata':{'labels':{'name':'kata-deploy'}},'spec':{'nodeName':self.node,'hostPID':True}})
                pod=copy.deepcopy(self.pod)
                if calls:pod['status']['containerStatuses'][0]['restartCount']=1
                return json.dumps(pod)
            argv=['controller','--context','fixture','--namespace','kata-system','--pod','probe','--pod-uid',self.uid,'--node',self.node,'--node-uid','node-uid','--host-agent','agent','--guest-image',self.image,'--pins',str(root/'pins'),'--disk-args',str(root/'args'),'--evidence',str(root/'evidence')]
            output=io.StringIO()
            with patch.object(controller,'checked',checked),patch.object(sys,'argv',argv),patch.object(sys,'stdout',output):
                with self.assertRaisesRegex(ValueError,'continuously ready'):controller.main()
            self.assertEqual(calls,['executed']);self.assertEqual(output.getvalue(),'')


if __name__ == '__main__':unittest.main()
