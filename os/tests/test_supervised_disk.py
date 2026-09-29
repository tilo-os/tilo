"""Admission checks only; no installer or remote Kubernetes operations."""
import base64
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, DEV / filename)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    return module
disk = load('disk_supervised', 'create-development-disk.py')
controller = load('controller', 'create-supervised-kata-disk.py')


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

    def test_guest_admission(self):
        with self.subTest(phase="standalone_none_still_denied"):
            with patch.object(disk,'command',return_value='none'), patch.object(disk,'_create_verified_disk') as create:
                with self.assertRaisesRegex(ValueError,'QEMU/KVM'):disk.create(self.args)
                create.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="supervised_records_observation_but_retains_firmware_guard"):
            with patch.object(disk,'command') as command, patch.object(disk,'reject_firmware_access') as firmware, \
                    patch.object(disk,'ensure_loop_devices'), patch.object(disk,'ensure_fuse_device'), \
                    patch.object(disk,'_create_verified_disk',return_value={'disk_created':True}):
                result=disk.create_from_supervised_controller(self.args,self.observation)
                self.assertEqual(result['controller_observation'],self.observation)
                command.assert_not_called();firmware.assert_called_once()
            with patch.object(disk,'reject_firmware_access',side_effect=ValueError('TPM exposed')), \
                    patch.object(disk,'ensure_loop_devices'),patch.object(disk,'ensure_fuse_device'), \
                    patch.object(disk,'_create_verified_disk') as create:
                with self.assertRaisesRegex(ValueError,'TPM'):disk.create_from_supervised_controller(self.args,self.observation)
                create.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="supervised_rejects_remote_configuration"):
            with patch.dict(disk.os.environ,{'CONTAINER_HOST':'ssh://host'}),patch.object(disk,'_create_verified_disk') as create:
                with self.assertRaisesRegex(ValueError,'remote'):disk.create_from_supervised_controller(self.args,self.observation)
                create.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="no_arbitrary_diagnostics_in_receipt"):
            with self.assertRaisesRegex(ValueError,'exact guest identity'):disk.create_from_supervised_controller(self.args,self.observation|{'credentials':'secret'})


class ControllerAdmission(unittest.TestCase):
    def setUp(self):
        fixture=json.loads((Path(__file__).parent/'fixtures/kata-disk-environment.json').read_text())
        self.pod=fixture['pod'];self.host=fixture['host']
        self.uid=self.pod['metadata']['uid'];self.node=self.pod['spec']['nodeName']
        self.sid=self.host['sandbox']['ID'];self.cid=self.host['container']['ID']
        self.image=self.pod['spec']['containers'][0]['image']

    def test_live_identity_and_views(self):
        with self.subTest(phase="matching_live_identity"):
            self.assertEqual(controller.inspect_pod(self.pod,self.node,self.uid,self.image),self.cid)
            self.assertEqual(controller.inspect_host(self.host,self.pod,self.host['hashes'])['sandbox_id'],self.sid)
        restart_fixture(self)
        with self.subTest(phase="local_emptydirs_require_three_matching_fresh_views"):
            self.local_fixture()
            self.assertEqual(controller.inspect_host(self.host,self.pod,self.host['hashes'])['sandbox_id'],self.sid)
            self.host.pop('local_volumes')
            with self.assertRaisesRegex(ValueError,'missing exact local'):
                controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="local_substitutions_fail_closed"):
            self.local_fixture()
            original=copy.deepcopy(self.host);pod=copy.deepcopy(self.pod)
            for change in ('inode','symlink','missing','destination','foreign-source','memory','subpath','nested'):
                with self.subTest(change=change):
                    self.host=copy.deepcopy(original);self.pod=copy.deepcopy(pod)
                    row=self.host['local_volumes'][0]
                    if change=='inode':row['chains'][1][2]['inode']+=1
                    elif change=='symlink':row['chains'][0][1]['kind']=0o120000
                    elif change=='missing':row['chains'][2][2]=None
                    elif change=='destination':row['destination']='/etc'
                    elif change=='foreign-source':row['source']=row['source'].replace(self.uid,'foreign-pod')
                    elif change=='memory':self.pod['spec']['volumes'][-4]['emptyDir']['medium']='Memory'
                    elif change=='subpath':self.pod['spec']['containers'][0]['volumeMounts'][-4]['subPath']='escape'
                    elif change=='nested':self.host['virtiofs']['mountinfo']+='\n1 2 0:201 / /passthrough/'+self.sid+'/rootfs/local/local-storage rw - tmpfs tmpfs rw'
                    with self.assertRaises(ValueError):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="hotplugged_host_disk_rejected_even_absent_from_argv"):
            self.host['fds'].append({'path':'/dev/nvme1n1','kind':0o60000})
            with self.assertRaisesRegex(ValueError,'block device'):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="actual_export_not_in_oci_rejected"):
            self.host['mountinfo']=self.host['mountinfo'].rstrip()+'\n1 2 0:1 /etc /run/kata-containers/shared/sandboxes/'+self.sid+'/rw/passthrough/unexpected rw - ext4 /dev/root rw'
            with self.assertRaisesRegex(ValueError,'actual virtiofs'):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="host_volume_or_replaced_pod_rejected"):
            with self.assertRaisesRegex(ValueError,'identity'):controller.inspect_pod(self.pod,self.node,'different',self.image)
            self.pod['spec']['volumes'].append({'name':'host','hostPath':{'path':'/'}})
            with self.assertRaisesRegex(ValueError,'only dedicated'):controller.inspect_pod(self.pod,self.node,self.uid,self.image)

    def local_fixture(self):
        # Actual preflight shape: named emptyDirs have no passthrough mounts;
        # runtime-rs instead creates them in pause-rootfs/local/<volume>.
        # Identity numbers below are synthetic; live admission needs the new
        # host collector's fresh three-view lstat observation, not this fixture.
        self.host['local_volumes'] = []
        self.pod['spec']['containers'][0].setdefault('volumeMounts', [])
        for i, (name, destination) in enumerate((('storage','/var/lib/containers'),
                ('scratch','/var/tmp'),('run','/run'),('tmp','/tmp'))):
            name='local-'+name
            self.pod['spec']['volumes'].append({'name':name,'emptyDir':{}})
            self.pod['spec']['containers'][0]['volumeMounts'].append({'name':name,'mountPath':destination})
            source='/var/lib/kubelet/pods/'+self.uid+'/volumes/kubernetes.io~empty-dir/'+name
            self.host['container']['spec']['mounts'].append({'type':'bind','source':source,'destination':destination})
            chain=[{'kind':0o40000,'device':201,'inode':n} for n in (100,200,300+i)]
            self.host['local_volumes'].append({'source':source,'destination':destination,'name':name,
                                              'chains':[copy.deepcopy(chain) for _ in range(3)]})

    def test_runtime_image_and_namespace(self):
        with self.subTest(phase="changed_runtime_pin_rejected"):
            with self.assertRaisesRegex(ValueError,'reviewed pins'):controller.inspect_host(self.host,self.pod,{'/kernel':'d'*64})
        restart_fixture(self)
        with self.subTest(phase="untrusted_image_or_observed_image_rejected"):
            with self.assertRaisesRegex(ValueError,'pinned guest'):controller.inspect_pod(self.pod,self.node,self.uid,'registry.invalid/guest@sha256:'+'a'*64)
            self.pod['status']['containerStatuses'][0]['imageID']='registry.invalid/guest@sha256:'+'a'*64
            with self.assertRaisesRegex(ValueError,'running image'):controller.inspect_pod(self.pod,self.node,self.uid,self.image)
        restart_fixture(self)
        with self.subTest(phase="empty_or_wrong_mount_namespace_rejected"):
            for inventory in ('', '1 2 0:1 / / rw - tmpfs tmpfs rw'):
                with self.subTest(inventory=inventory):
                    self.host['mountinfo']=inventory
                    with self.assertRaises(ValueError):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="each_required_rootfs_and_shared_volume_cannot_disappear"):
            original=self.host['mountinfo']
            roots=['/ro/passthrough/'+identity+'/rootfs' for identity in (self.sid,self.cid)]
            shared=[m['source'] for m in self.host['container']['spec']['mounts'] if m['type']=='bind' and ('configmap' in m['source'] or m['destination'] in ('/etc/hosts','/etc/hostname','/etc/resolv.conf','/dev/termination-log'))]
            for missing in roots+shared:
                with self.subTest(missing=missing):
                    self.host['mountinfo']='\n'.join(line for line in original.splitlines() if missing not in line)
                    with self.assertRaises(ValueError):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="actual_server_namespace_must_match_host_exports"):
            self.host['virtiofs']['mountinfo']='\n'.join(line for line in self.host['virtiofs']['mountinfo'].splitlines() if 'configmap/scripts' not in line)
            with self.assertRaisesRegex(ValueError,'inventory differs'):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="actual_server_root_binding_required"):
            self.host['virtiofs']['root_identity']=[1,2]
            with self.assertRaisesRegex(ValueError,'bound'):controller.inspect_host(self.host,self.pod,self.host['hashes'])
        restart_fixture(self)
        with self.subTest(phase="controller_does_not_report_success_after_container_restart"):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory);(root/'pins').write_text(json.dumps(self.host['hashes']));(root/'args').write_text('{}')
                calls=[]
                def checked(argv,payload=None):
                    if controller.HOST_INSPECT in argv:return json.dumps(self.host)
                    if controller.GUEST_LAUNCH in argv:
                        calls.append('executed')
                        binding=json.loads(payload)['binding']
                        return json.dumps({'schema':1,'purpose':'tilo-supervised-creator-job',
                                           'state':'launched','binding':binding,'launched_at':1})
                    kind,name=argv[argv.index('get')+1:argv.index('get')+3]
                    if kind=='node':return json.dumps({'metadata':{'uid':'node-uid'}})
                    if name=='agent':return json.dumps({'metadata':{'labels':{'name':'kata-deploy'}},'spec':{'nodeName':self.node,'hostPID':True}})
                    pod=copy.deepcopy(self.pod)
                    if calls:pod['status']['containerStatuses'][0]['restartCount']=1
                    return json.dumps(pod)
                argv=['controller','--context','fixture','--namespace','kata-system','--pod','probe','--pod-uid',self.uid,'--node',self.node,'--node-uid','node-uid','--host-agent','agent','--guest-image',self.image,'--pins',str(root/'pins'),'--disk-args',str(root/'args'),'--evidence',str(root/'evidence'),'--creation-receipt',str(root/'creation.json'),'--helper-revision','a'*40,'--operation','run1']
                output=io.StringIO()
                with patch.object(controller,'checked',checked),patch.object(controller,'helper_bundle',return_value=(b'zip','b'*64)), \
                        patch.object(controller,'poll_creator_job',return_value={'creation_sha256':'a'*64}), \
                        patch.object(controller,'remote_creator_receipt',return_value=b'{"purpose":"tilo-development-unaccepted-test-disk","disk_created":true}'), \
                        patch.object(sys,'argv',argv),patch.object(sys,'stdout',output):
                    with self.assertRaisesRegex(ValueError,'continuously ready'):controller.main()
                self.assertEqual(calls,['executed']);self.assertEqual(output.getvalue(),'')

    def test_creator_launch_and_receipt(self):
        with self.subTest(phase="private_registry_auth_rejects_symlink_and_reads_regular_file"):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory); regular=root/'auth.json'; regular.write_text('{"auths":{}}')
                regular.chmod(0o600)
                self.assertEqual(controller.private_input(regular,limit=1024),b'{"auths":{}}')
                link=root/'redirected'; link.symlink_to(regular)
                with self.assertRaises((ValueError,OSError)):
                    controller.private_input(link,limit=1024)
        restart_fixture(self)
        with self.subTest(phase="kubectl_client_timeout_is_reconnectable_before_creator_launch"):
            result=SimpleNamespace(returncode=1,stdout='',stderr=(
                'net/http: request canceled while waiting for connection '
                '(Client.Timeout exceeded while awaiting headers)'))
            with patch.object(controller.subprocess,'run',return_value=result):
                with self.assertRaises(controller.ControllerTransportError):
                    controller.checked(['kubectl','--context','fixture','get','pods'])
        restart_fixture(self)
        with self.subTest(phase="registry_auth_stdin_reaches_detached_creator_launch"):
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory); pins=root/'pins'; args=root/'args'
                pins.write_text('{}');args.write_text('{}')
                binding={'operation':'run1'}; launches=[]
                def checked(argv,payload=None):
                    if controller.HOST_INSPECT in argv:return '{}'
                    if controller.GUEST_LAUNCH in argv:
                        launches.append(json.loads(payload))
                        return json.dumps({'schema':1,'purpose':'tilo-supervised-creator-job',
                                           'state':'launched','binding':binding,'launched_at':1})
                    kind,name=argv[argv.index('get')+1:argv.index('get')+3]
                    if kind=='node':return json.dumps({'metadata':{'uid':'node-uid'}})
                    if name=='agent':return json.dumps({'metadata':{'labels':{'name':'kata-deploy'}},
                                                         'spec':{'nodeName':self.node,'hostPID':True}})
                    return json.dumps(self.pod)
                argv=['controller','--context','fixture','--namespace','kata-system','--pod','probe',
                      '--pod-uid',self.uid,'--node',self.node,'--node-uid','node-uid',
                      '--host-agent','agent','--guest-image',self.image,'--pins',str(pins),
                      '--disk-args',str(args),'--evidence',str(root/'evidence'),
                      '--creation-receipt',str(root/'creation.json'),'--helper-revision','a'*40,
                      '--operation','run1','--registry-auth-stdin']
                auth=b'{"auths":{"registry.test":{"auth":"x"}}}'
                with patch.object(controller,'checked',checked), \
                        patch.object(controller,'inspect_pod',return_value='c'*64), \
                        patch.object(controller,'inspect_host',return_value={}), \
                        patch.object(controller,'helper_bundle',return_value=(b'zip','b'*64)), \
                        patch.object(controller,'creator_job_binding',return_value=(binding,'d'*64)), \
                        patch.object(controller,'validate_creator_job_status'), \
                        patch.object(controller,'poll_creator_job',return_value={'creation_sha256':'a'*64}), \
                        patch.object(controller,'remote_creator_receipt',return_value=b'{"purpose":"tilo-development-unaccepted-test-disk","disk_created":true}'), \
                        patch.object(sys,'argv',argv),patch.object(sys,'stdin',SimpleNamespace(buffer=io.BytesIO(auth))), \
                        patch.object(sys,'stdout',io.StringIO()):
                    controller.main()
                self.assertEqual(base64.b64decode(launches[0]['registry_auth']),auth)
        restart_fixture(self)
        with self.subTest(phase="creation_receipt_is_private_and_atomic"):
            with tempfile.TemporaryDirectory() as directory:
                path=Path(directory)/'evidence'/'creation.json'
                receipt={'purpose':'tilo-development-unaccepted-test-disk','disk_created':True}
                self.assertEqual(controller.persist_receipt(path,json.dumps(receipt)),receipt)
                self.assertEqual(json.loads(path.read_text()),receipt)
                self.assertEqual(path.stat().st_mode & 0o777,0o600)
                with self.assertRaisesRegex(ValueError,'completed creation'):
                    controller.persist_receipt(path,'{"disk_created":false}')


class DetachedCreatorJob(unittest.TestCase):
    def binding(self):
        return controller.creator_job_binding(
            'run1', 'pod-uid', 'node-uid', 'c'*64,
            'registry.test/tools@sha256:'+'a'*64,
            {'output':'/work/disk-output/disk.raw'}, {'kernel_sha256':'b'*64},
            'd'*40, 'e'*64)[0]

    def status(self, state, **extra):
        value={'schema':1,'purpose':'tilo-supervised-creator-job','state':state,
               'binding':self.binding()}
        value.update(extra); return value

    def process(self):
        return {'pid':7,'start':'123','argv_sha256':'a'*64,
                'cgroup_sha256':'b'*64,'request_sha256':'c'*64}

    def test_detached_job_polling(self):
        with self.subTest(phase="helper_bundle_is_reproducible"):
            revision='d'*40
            def checked(argv, payload=None):
                if 'rev-parse' in argv:return revision+'\n'
                if 'status' in argv:return ''
                if 'ls-tree' in argv:return 'os/dev/b.py\nos/dev/a.py\n'
                raise AssertionError(argv)
            def git_show(argv, **kwargs):
                return SimpleNamespace(stdout={'os/dev/a.py':b'a\n','os/dev/b.py':b'b\n'}[argv[-1].split(':',1)[1]])
            with patch.object(controller,'checked',side_effect=checked), \
                    patch.object(controller.subprocess,'run',side_effect=git_show):
                first=controller.helper_bundle(revision)
                second=controller.helper_bundle(revision)
            self.assertEqual(first,second)
            with zipfile.ZipFile(io.BytesIO(first[0])) as archive:
                self.assertEqual([item.filename for item in archive.infolist()],['b.py','a.py'])
                self.assertTrue(all(item.date_time==(1980,1,1,0,0,0) for item in archive.infolist()))
        with self.subTest(phase="poll_reconnects_after_transport_loss_then_accepts_bound_success"):
            running=self.status('running',process=self.process())
            success=self.status('succeeded',process=self.process(),log_bytes=12,log_truncated=False,
                                creation_sha256='d'*64)
            pauses=[]
            with patch.object(controller,'remote_creator_status',side_effect=[
                    controller.ControllerTransportError('relay reset'),running,success]):
                actual=controller.poll_creator_job(['kubectl'],'ns','pod','disk',self.binding(),
                                                    clock=lambda:0,wall_clock=lambda:10,
                                                    sleep=lambda delay:pauses.append(delay))
            self.assertEqual(actual,success)
            self.assertEqual(pauses,[2,10])
        with self.subTest(phase="orphaned_launch_and_dead_pid_fail_closed"):
            launched=self.status('launched',launched_at=1)
            with patch.object(controller,'remote_creator_status',return_value=launched):
                with self.assertRaisesRegex(ValueError,'did not establish'):
                    controller.poll_creator_job(['kubectl'],'ns','pod','disk',self.binding(),
                                                clock=lambda:0,wall_clock=lambda:62,sleep=lambda _:None)
            result=SimpleNamespace(returncode=3,stdout='TILO_CREATOR_PROCESS_GONE\n',stderr='')
            with patch.object(controller.subprocess,'run',return_value=result):
                with self.assertRaisesRegex(ValueError,'process vanished'):
                    controller.remote_creator_status(['kubectl'],'ns','pod','disk',self.binding())
        with self.subTest(phase="failed_status_is_distinguished_and_its_bound_log_is_retrieved"):
            failed=self.status('failed',process=self.process(),log_bytes=32,log_truncated=False)
            with patch.object(controller,'remote_creator_status',return_value=failed):
                with self.assertRaises(controller.CreatorJobFailed):
                    controller.poll_creator_job(['kubectl'],'ns','pod','disk',self.binding(),
                                                clock=lambda:0,wall_clock=lambda:10,sleep=lambda _:None)
            log=b'creator job failed: NameError\n  create-development-disk.py:1 run\n'
            with patch.object(controller,'checked',return_value=base64.b64encode(log).decode()):
                self.assertEqual(controller.remote_creator_failure_log(
                    ['kubectl'],'ns','pod','disk',self.binding()),log)
            self.assertIn("status.get('binding')!=expected",controller.REMOTE_CREATOR_FAILURE_LOG)
        with self.subTest(phase="remote_receipt_binds_detached_request_before_adoption"):
            binding=self.binding()
            receipt={'disk_created':True,'path':'/work/disk-output/disk.raw',
                     'size_bytes':64*1024**3,'controller_job':{'binding':binding},
                     'controller_helper':{'revision':binding['helper_revision'],
                                          'bundle_sha256':binding['helper_bundle_sha256']}}
            raw=(json.dumps(receipt,sort_keys=True)+'\n').encode()
            status={'creation_sha256':hashlib.sha256(raw).hexdigest()}
            with patch.object(controller,'checked',return_value=base64.b64encode(raw).decode()):
                self.assertEqual(controller.remote_creator_receipt(['kubectl'],'ns','pod','disk',status,binding),raw)
            receipt['controller_job']={'binding':{}}
            hostile=(json.dumps(receipt,sort_keys=True)+'\n').encode()
            with patch.object(controller,'checked',return_value=base64.b64encode(hostile).decode()):
                with self.assertRaisesRegex(ValueError,'does not bind'):
                    controller.remote_creator_receipt(['kubectl'],'ns','pod','disk',
                                                       {'creation_sha256':hashlib.sha256(hostile).hexdigest()},binding)

    def test_detached_job_credentials(self):
        with self.subTest(phase="credentials_are_memory_only_and_terminal_cleanup_is_in_job"):
            self.assertIn("credential=pathlib.Path('/tmp')", controller.GUEST_LAUNCH)
            self.assertIn("row[4]=='/tmp'", controller.GUEST_LAUNCH)
            self.assertIn("auth.unlink();auth.parent.rmdir();auth=None", controller.GUEST_JOB)
            self.assertIn("try:auth.unlink()", controller.GUEST_JOB)
            self.assertNotIn('registry_auth', controller.GUEST_JOB)
        with self.subTest(phase="guest_job_cleans_credential_after_terminal_failure"):
            if not (hasattr(os, 'O_NOFOLLOW') and Path('/proc/self').exists() and (os.geteuid() == 0)):
                self.skipTest('requires the root-owned Linux Kata file contract')
            with tempfile.TemporaryDirectory() as directory:
                job=Path(directory)/'job';job.mkdir(mode=0o700)
                credentials=Path(directory)/'credentials';credentials.mkdir(mode=0o700)
                auth=credentials/'registry-auth.json';auth.write_bytes(b'{"auths":{"registry":"secret"}}');auth.chmod(0o600)
                request={'binding':self.binding(),'observation':{},'args':{},'helpers':base64.b64encode(b'not a zip').decode(),
                         'helper_revision':'d'*40,'helper_bundle_sha256':'e'*64}
                path=job/'request.json';path.write_text(json.dumps(request));path.chmod(0o600)
                # A hang bound: the job fsyncs its status twice, which a busy
                # runner's disk can stretch past 15 s.
                completed=subprocess.run([sys.executable,'-c',controller.GUEST_JOB,str(job),str(auth)],
                                         capture_output=True,timeout=120)
                self.assertEqual(completed.returncode,0)
                self.assertFalse(auth.exists())
                self.assertFalse(credentials.exists())
                status=json.loads((job/'status.json').read_text())
                self.assertEqual(status['state'],'failed')
                log=(job/'log.txt').read_text()
                self.assertIn('creator job failed: BadZipFile',log)
                self.assertNotIn('secret',log)
                controller.validate_creator_job_status(status,self.binding())


if __name__ == '__main__':unittest.main()
