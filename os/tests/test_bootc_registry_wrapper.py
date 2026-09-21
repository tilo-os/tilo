"""Registry wrapper lifecycle tests; no registry or bootc command is run."""
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import signal
from types import SimpleNamespace
import threading
import time
import unittest
from unittest.mock import mock_open, patch

DEV=Path(__file__).resolve().parents[1]/'dev'
spec=importlib.util.spec_from_file_location('wrapper',DEV/'bootc-registry-wrapper.py')
wrapper=importlib.util.module_from_spec(spec);spec.loader.exec_module(wrapper)

SOURCE='docker://code.sandbox.example/qos/tilo-development@sha256:'+'1'*64
DIGEST='sha256:'+'1'*64


class RegistryWarmupTests(unittest.TestCase):
    def test_bound_source_requires_exact_image_manifest_and_config(self):
        raw=b'{"schemaVersion":2}'
        digest='sha256:'+hashlib.sha256(raw).hexdigest()
        source='docker://code.sandbox.example/qos/tilo-development@'+digest
        image_id='4'*64
        canonical=source.removeprefix('docker://')
        inspect=SimpleNamespace(returncode=0,stdout=json.dumps([{
            'Id':image_id,'Digest':digest,'RepoDigests':[canonical]}]).encode())
        manifest=SimpleNamespace(returncode=0,stdout=raw)
        config=SimpleNamespace(returncode=0,stdout=json.dumps({
            'Digest':digest,'Architecture':'arm64'}).encode())
        metadata=f'image="{canonical}"\nimageid="{image_id}"\n'
        with patch('builtins.open',mock_open(read_data=metadata)), \
                patch.object(wrapper.subprocess,'run',side_effect=[inspect,manifest,config]) as run:
            self.assertIsNone(wrapper.require_bound_source(source,digest))
        self.assertEqual(run.call_args_list[0].args[0],[
            '/usr/bin/podman','--remote=false','image','inspect',image_id])
        self.assertEqual(run.call_args_list[1].args[0][0:3],[
            '/usr/bin/skopeo','inspect','--raw'])
        hostile=SimpleNamespace(returncode=0,stdout=json.dumps([{
            'Id':image_id,'Digest':digest,'RepoDigests':['registry.invalid/x@'+digest]}]).encode())
        with patch('builtins.open',mock_open(read_data=metadata)), \
                patch.object(wrapper.subprocess,'run',return_value=hostile), \
                self.assertRaisesRegex(ValueError,'image identity mismatch'):
            wrapper.require_bound_source(source,digest)

    def test_private_udev_preflight_requires_pid1_mount_and_no_database(self):
        directory=SimpleNamespace(st_mode=wrapper.stat.S_IFDIR|0o755,st_dev=42)
        readonly=SimpleNamespace(f_flag=wrapper.os.ST_RDONLY)
        writable=SimpleNamespace(f_flag=0)
        with patch.object(wrapper.os,'stat',side_effect=[directory,directory]), \
                patch.object(wrapper.os,'statvfs',return_value=readonly), \
                patch.object(wrapper.os.path,'lexists',return_value=False):
            self.assertIsNone(wrapper.require_private_udev())
        wrong=SimpleNamespace(st_mode=wrapper.stat.S_IFDIR|0o755,st_dev=43)
        with patch.object(wrapper.os,'stat',side_effect=[directory,wrong]), \
                patch.object(wrapper.os,'statvfs',return_value=readonly), \
                patch.object(wrapper.os.path,'lexists',return_value=False), \
                self.assertRaisesRegex(ValueError,'differs from PID 1'):
            wrapper.require_private_udev()
        with patch.object(wrapper.os,'stat',side_effect=[directory,directory]), \
                patch.object(wrapper.os,'statvfs',return_value=writable), \
                patch.object(wrapper.os.path,'lexists',return_value=False), \
                self.assertRaisesRegex(ValueError,'is writable'):
            wrapper.require_private_udev()
        with patch.object(wrapper.os,'stat',side_effect=[directory,directory]), \
                patch.object(wrapper.os,'statvfs',return_value=readonly), \
                patch.object(wrapper.os.path,'lexists',return_value=True), \
                self.assertRaisesRegex(ValueError,'stale database'):
            wrapper.require_private_udev()

    def test_inspect_uses_exact_argv_without_a_shell_and_requires_digest(self):
        result=SimpleNamespace(returncode=0,stdout=(DIGEST+'\n').encode())
        with patch.dict(os.environ,{'REGISTRY_AUTH_FILE':'/private/auth.json'},clear=True), \
                patch.object(wrapper.subprocess,'run',return_value=result) as run:
            self.assertTrue(wrapper.inspect_digest(SOURCE,DIGEST))
        self.assertEqual(run.call_args.args[0],[
            '/usr/bin/skopeo','inspect','--format','{{.Digest}}','--authfile',
            '/private/auth.json',SOURCE])
        self.assertNotIn('shell',run.call_args.kwargs)
        with self.assertRaisesRegex(ValueError,'exact immutable source'):
            wrapper.inspect_digest(SOURCE,'sha256:'+'2'*64)

    def test_warmup_is_bounded_with_exact_backoff_and_digest_failure(self):
        with patch.object(wrapper,'inspect_digest',side_effect=[False,False,True]) as inspect, \
                patch.object(wrapper.time,'sleep') as sleep:
            self.assertTrue(wrapper.warm(SOURCE,DIGEST,sleep=sleep))
        self.assertEqual(inspect.call_count,3)
        self.assertEqual([call.args for call in sleep.call_args_list],[(2,),(5,)])
        with patch.object(wrapper,'inspect_digest',return_value=False) as inspect, \
                patch.object(wrapper.time,'sleep') as sleep:
            self.assertFalse(wrapper.warm(SOURCE,DIGEST,sleep=sleep))
        self.assertEqual(inspect.call_count,3)
        self.assertEqual([call.args for call in sleep.call_args_list],[(2,),(5,)])


class WrapperLifecycleTests(unittest.TestCase):
    class Child:
        def __init__(self,returncode=0,on_wait=None):
            self.returncode=returncode;self.on_wait=on_wait;self.signals=[]
        def wait(self):
            if self.on_wait:self.on_wait()
            return self.returncode
        def poll(self):return None
        def send_signal(self,value):self.signals.append(value)

    def test_bootc_argv_and_exit_code_propagate_exactly(self):
        argv=['/usr/bin/bootc','install','to-disk','--target-imgref=x:y','/output/disk.raw']
        child=self.Child(23)
        with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                patch.object(wrapper.subprocess,'Popen',return_value=child) as popen:
            self.assertEqual(wrapper.run_bootc(SOURCE,DIGEST,argv),23)
        popen.assert_called_once_with(argv)

    def test_background_heartbeat_stops_and_joins(self):
        child=self.Child(on_wait=lambda:time.sleep(.035))
        with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                patch.object(wrapper,'inspect_digest',return_value=True) as inspect, \
                patch.object(wrapper.subprocess,'Popen',return_value=child):
            self.assertEqual(wrapper.run_bootc(
                SOURCE,DIGEST,['/usr/bin/bootc','install'],heartbeat_interval=.005),0)
        self.assertGreater(inspect.call_count,0)
        self.assertFalse(any(t.name=='tilo-registry-heartbeat' for t in threading.enumerate()))

    def test_heartbeat_failure_does_not_replace_bootc_result(self):
        child=self.Child(17,on_wait=lambda:time.sleep(.02))
        failure=wrapper.subprocess.TimeoutExpired(['/usr/bin/skopeo'],10)
        with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                patch.object(wrapper,'inspect_digest',side_effect=failure) as inspect, \
                patch.object(wrapper.subprocess,'Popen',return_value=child):
            self.assertEqual(wrapper.run_bootc(
                SOURCE,DIGEST,['/usr/bin/bootc','install'],heartbeat_interval=.005),17)
        self.assertGreater(inspect.call_count,0)
        self.assertFalse(any(t.name=='tilo-registry-heartbeat' for t in threading.enumerate()))

    def test_signal_is_forwarded_then_re_raised_after_cleanup(self):
        handlers={}
        def set_handler(signum,handler):
            previous=handlers.get(signum,signal.SIG_DFL);handlers[signum]=handler;return previous
        child=self.Child(on_wait=lambda:handlers[signal.SIGTERM](signal.SIGTERM,None))
        with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                patch.object(wrapper.signal,'signal',side_effect=set_handler), \
                patch.object(wrapper.os,'kill') as kill, \
                patch.object(wrapper.subprocess,'Popen',return_value=child), \
                self.assertRaisesRegex(AssertionError,'signal propagation returned'):
            wrapper.run_bootc(SOURCE,DIGEST,['/usr/bin/bootc','install'])
        self.assertEqual(child.signals,[signal.SIGTERM])
        kill.assert_called_once_with(os.getpid(),signal.SIGTERM)

    def test_failed_warmup_never_starts_bootc_or_heartbeat(self):
        with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=False), \
                patch.object(wrapper.subprocess,'Popen') as popen:
            self.assertEqual(wrapper.run_bootc(SOURCE,DIGEST,['/usr/bin/bootc']),75)
        popen.assert_not_called()
        self.assertFalse(any(t.name=='tilo-registry-heartbeat' for t in threading.enumerate()))


if __name__=='__main__':unittest.main()
