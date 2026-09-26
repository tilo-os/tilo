"""Focused protocol tests for the direct supervised disk transfer."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


DEV=Path(__file__).resolve().parents[1]/'dev'
sys.path.insert(0,str(DEV))
spec=importlib.util.spec_from_file_location('lane',DEV/'run-supervised-install-lane.py')
lane=importlib.util.module_from_spec(spec);spec.loader.exec_module(lane)


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


class Controller(unittest.TestCase):
    def test_controller_never_exposes_capability(self):
        with self.subTest(phase="control_argv_and_receipt_never_contain_capability"):
            secret='f'*64;identity={'size':1024,'sha256':'a'*64}
            source={'namespace':'kata-system','pod':'source','pod_uid':'source-uid','node':'node-a'}
            target={'namespace':'kata-system','pod':'target','pod_uid':'target-uid','node':'node-b'}
            bindings=[{'pod_uid':'source-uid','pod_ip':'10.0.0.1'},
                      {'pod_uid':'target-uid','pod_ip':'10.0.0.2'},
                      {'pod_uid':'source-uid','pod_ip':'10.0.0.1'},
                      {'pod_uid':'target-uid','pod_ip':'10.0.0.2'}]
            receipt={'schema':1,'purpose':'tilo-direct-sparse-transfer','operation':'op',
              'source_pod_uid':'source-uid','target_pod_uid':'target-uid','size':1024,
              'sha256':'a'*64,'path':'/work/output/disk.raw','data_bytes':12,'extent_frames':2}
            class Process:
                def __init__(self,ready=False):
                    self.stdout=io.StringIO(json.dumps({'schema':1,'status':'ready','port':23456})+'\n' if ready else '')
                    self.returncode=0
                def communicate(self,timeout=None):return json.dumps(receipt),''
                def poll(self):return 0
            calls=[]
            def control(argv,token):calls.append((argv,token));return Process(len(calls)==1)
            with patch.object(lane,'transfer_pod_binding',side_effect=bindings), \
                 patch.object(lane.secrets,'token_hex',return_value=secret), \
                 patch.object(lane,'_ready_line',return_value=23456), \
                 patch.object(lane,'_control_process',side_effect=control):
                self.assertEqual(lane.sparse_transfer('ctx',source,target,target_container='disk',
                  target_directory='/work/output',target_name='disk.raw',operation='op',identity=identity),receipt)
            self.assertEqual([token for _,token in calls],[secret,secret])
            self.assertNotIn(secret,json.dumps([argv for argv,_ in calls])+json.dumps(receipt))
        restart_fixture(self)
        with self.subTest(phase="transfer_binding_rejects_stale_uid_or_invalid_ip"):
            worker={'namespace':'ns','pod':'pod','pod_uid':'old','node':'node'}
            pod={'metadata':{'uid':'new'},'status':{'podIP':'10.0.0.1'}}
            with patch.object(lane,'api_json',side_effect=[pod,{}]), \
                 patch.object(lane,'binding',return_value={'different':True}), \
                 self.assertRaisesRegex(ValueError,'identity changed'):
                lane.transfer_pod_binding('ctx',worker)


@unittest.skipUnless(sys.platform=='linux' and os.geteuid()==0,'requires root Linux metadata')
class Protocol(unittest.TestCase):
    def setUp(self):
        self.root=Path(tempfile.mkdtemp(prefix='tilo-direct-',dir='/root'))
        self.source=self.root/'source.raw';self.size=32*1024*1024
        with self.source.open('wb') as stream:
            stream.truncate(self.size);stream.write(b'TILO')
            stream.seek(24*1024*1024);stream.write(b'END')
        self.source.chmod(0o400)
        self.digest=hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.token='a'*64

    def tearDown(self):
        shutil.rmtree(self.root,ignore_errors=True)

    def receiver(self,token=None,source_ip='127.0.0.1',name='disk.raw',script=None,directory='target'):
        argv=[sys.executable,'-c',script or lane.DIRECT_RECEIVER_SCRIPT,str(self.root/directory),name,
              '127.0.0.1',source_ip,'operation','source-uid','target-uid',str(self.size),self.digest]
        process=subprocess.Popen(argv,stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE,text=True)
        process.stdin.write((token or self.token)+'\n');process.stdin.close();process.stdin=None
        return process,argv

    def sender(self,port,token=None,source=None):
        argv=[sys.executable,'-c',lane.DIRECT_SENDER_SCRIPT,str(source or self.source),'127.0.0.1',str(port),
              'operation','source-uid','target-uid',str(self.size),self.digest,'127.0.0.1']
        process=subprocess.Popen(argv,stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE,text=True)
        process.stdin.write((token or self.token)+'\n');process.stdin.close();process.stdin=None
        return process,argv

    def ready(self,receiver):
        value=json.loads(receiver.stdout.readline());self.assertEqual(value['status'],'ready')
        return value['port']

    def test_protocol_transfers(self):
        with self.subTest(phase="success_preserves_holes_and_receipt_never_contains_capability"):
            receiver,receiver_argv=self.receiver();sender,sender_argv=self.sender(self.ready(receiver))
            sender_out,sender_err=sender.communicate(timeout=20)
            receiver_out,receiver_err=receiver.communicate(timeout=20)
            self.assertEqual((sender.returncode,receiver.returncode),(0,0),(sender_err,receiver_err))
            sent=json.loads(sender_out);received=json.loads(receiver_out);self.assertEqual(sent,received)
            target=self.root/'target/disk.raw'
            self.assertEqual(hashlib.sha256(target.read_bytes()).hexdigest(),self.digest)
            self.assertEqual(target.stat().st_mode&0o777,0o400)
            self.assertLess(target.stat().st_blocks*512,self.size)
            evidence=json.dumps(received)+json.dumps(receiver_argv)+json.dumps(sender_argv)+sender_out+receiver_out
            self.assertNotIn(self.token,evidence)
        restart_fixture(self)
        with self.subTest(phase="two_hop_creator_verifier_vm_chain"):
            first_receiver,_=self.receiver(name='verifier.raw',directory='verifier')
            first_sender,_=self.sender(self.ready(first_receiver))
            first_sender.communicate(timeout=20);first_receiver.communicate(timeout=20)
            self.assertEqual((first_sender.returncode,first_receiver.returncode),(0,0))
            verifier=self.root/'verifier/verifier.raw'
            self.assertEqual(verifier.stat().st_mode&0o777,0o400)

            second_receiver,_=self.receiver(name='installer.raw',directory='vm')
            second_sender,_=self.sender(self.ready(second_receiver),source=verifier)
            second_sender.communicate(timeout=20);second_receiver.communicate(timeout=20)
            self.assertEqual((second_sender.returncode,second_receiver.returncode),(0,0))
            installed=self.root/'vm/installer.raw'
            self.assertEqual(installed.stat().st_mode&0o777,0o400)
            self.assertEqual(hashlib.sha256(installed.read_bytes()).hexdigest(),self.digest)
            self.assertLess(installed.stat().st_blocks*512,self.size)

    def test_protocol_failures_fail_closed(self):
        with self.subTest(phase="wrong_token_and_source_ip_fail_without_partial_target"):
            for case in ('token','ip'):
                with self.subTest(case=case):
                    shutil.rmtree(self.root/'target',ignore_errors=True)
                    receiver,_=self.receiver(source_ip='127.0.0.2' if case=='ip' else '127.0.0.1')
                    sender,_=self.sender(self.ready(receiver),token='b'*64 if case=='token' else self.token)
                    sender.communicate(timeout=20);receiver.communicate(timeout=20)
                    self.assertNotEqual(receiver.returncode,0)
                    self.assertFalse((self.root/'target/disk.raw').exists())
                    self.assertFalse(list((self.root/'target').glob('.incoming-*')))
        restart_fixture(self)
        with self.subTest(phase="truncation_duplicate_and_replay_fail_closed"):
            receiver,_=self.receiver();port=self.ready(receiver)
            sock=socket.create_connection(('127.0.0.1',port))
            handshake={'version':1,'operation':'operation','source_uid':'source-uid','target_uid':'target-uid',
                       'size':self.size,'sha256':self.digest,'capability':self.token}
            encoded=json.dumps(handshake).encode();sock.sendall(struct.pack('>I',len(encoded))+encoded)
            sock.sendall(b'D'+struct.pack('>QQ',0,4096)+b'partial');sock.close()
            receiver.communicate(timeout=20);self.assertNotEqual(receiver.returncode,0)
            self.assertFalse((self.root/'target/disk.raw').exists())
            self.assertFalse(list((self.root/'target').glob('.incoming-*')))

            receiver,_=self.receiver();port=self.ready(receiver)
            sock=socket.create_connection(('127.0.0.1',port))
            sock.sendall(struct.pack('>I',len(encoded))+encoded)
            sock.sendall(b'D'+struct.pack('>QQ',self.size,1)+b'X');sock.close()
            receiver.communicate(timeout=20);self.assertNotEqual(receiver.returncode,0)
            self.assertFalse((self.root/'target/disk.raw').exists())
            self.assertFalse(list((self.root/'target').glob('.incoming-*')))

            (self.root/'target/disk.raw').write_bytes(b'existing')
            duplicate,_=self.receiver();duplicate.communicate(timeout=20)
            self.assertNotEqual(duplicate.returncode,0)
            self.assertEqual((self.root/'target/disk.raw').read_bytes(),b'existing')
        restart_fixture(self)
        with self.subTest(phase="failure_after_atomic_link_removes_unacknowledged_target"):
            marker="os.unlink(temp_name,dir_fd=dir_fd);os.fsync(dir_fd)"
            injected=lane.DIRECT_RECEIVER_SCRIPT.replace(marker,"raise RuntimeError('injected after link')")
            receiver,_=self.receiver(script=injected);sender,_=self.sender(self.ready(receiver))
            sender.communicate(timeout=20);receiver.communicate(timeout=20)
            self.assertNotEqual(receiver.returncode,0)
            self.assertFalse((self.root/'target/disk.raw').exists())
            self.assertFalse(list((self.root/'target').glob('.incoming-*')))


if __name__=='__main__':unittest.main()
