"""Admission checks only; no installer or remote Kubernetes operations."""
import base64
import copy
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import sys
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
        self.uid='00000000-0000-0000-0000-000000000002'
        self.sid='a'*64;self.cid='b'*64
        self.pod={'metadata':{'uid':self.uid},'spec':{'nodeName':'node','runtimeClassName':'kata-qemu-runtime-rs',
            'automountServiceAccountToken':False,'containers':[{'name':'probe'}],
            'volumes':[{'name':'scratch','emptyDir':{'medium':'Memory'}}]},
            'status':{'phase':'Running','containerStatuses':[{'ready':True,'containerID':'containerd://'+self.cid}]}}
        config=b'/opt/kata/share/defaults/kata-containers/runtime-rs/runtimes/qemu-runtime-rs/configuration-qemu-runtime-rs.toml'
        obj={'Labels':{'io.kubernetes.pod.uid':self.uid},'Runtime':{'Name':'io.containerd.kata-qemu-runtime-rs.v2','Options':{'value':base64.b64encode(bytes((18,len(config)))+config).decode()}},'spec':{'linux':{},'annotations':{},'mounts':[]}}
        self.host={'sandbox':copy.deepcopy(obj)|{'ID':self.sid},'container':copy.deepcopy(obj)|{'ID':self.cid},
            'hashes':{'/kernel':'c'*64},'argv':['qemu','-kernel','/kernel','-blockdev','driver=file,filename=/opt/kata/share/kata-containers/kata-ubuntu-noble.image'],
            'fds':[{'path':'/dev/kvm','kind':0o20000}], 'mountinfo':''}

    def test_matching_live_identity(self):
        self.assertEqual(controller.inspect_pod(self.pod,'node',self.uid),self.cid)
        self.assertEqual(controller.inspect_host(self.host,self.pod,self.host['hashes'])['sandbox_id'],self.sid)

    def test_hotplugged_host_disk_rejected_even_absent_from_argv(self):
        self.host['fds'].append({'path':'/dev/nvme1n1','kind':0o60000})
        with self.assertRaisesRegex(ValueError,'block device'):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_actual_export_not_in_oci_rejected(self):
        self.host['mountinfo']='1 2 0:1 /etc /run/kata-containers/shared/sandboxes/'+self.sid+'/rw/passthrough/unexpected rw - ext4 /dev/root rw'
        with self.assertRaisesRegex(ValueError,'actual virtiofs'):controller.inspect_host(self.host,self.pod,self.host['hashes'])

    def test_host_volume_or_replaced_pod_rejected(self):
        with self.assertRaisesRegex(ValueError,'identity'):controller.inspect_pod(self.pod,'node','different')
        self.pod['spec']['volumes'].append({'name':'host','hostPath':{'path':'/'}})
        with self.assertRaisesRegex(ValueError,'only dedicated'):controller.inspect_pod(self.pod,'node',self.uid)

    def test_changed_runtime_pin_rejected(self):
        with self.assertRaisesRegex(ValueError,'reviewed pins'):controller.inspect_host(self.host,self.pod,{'/kernel':'d'*64})


if __name__ == '__main__':unittest.main()
