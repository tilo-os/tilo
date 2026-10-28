"""Registry wrapper lifecycle tests; no registry or bootc command is run."""
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
from types import SimpleNamespace
import threading
import tempfile
import unittest
from unittest.mock import mock_open, patch

DEV=Path(__file__).resolve().parents[1]/'dev'
sys.path.insert(0, str(DEV))
sys.path.insert(0, str(DEV.parent.parent/'tools/tests'))
from hang import HANG_BOUND  # noqa: E402
spec=importlib.util.spec_from_file_location('wrapper',DEV/'bootc-registry-wrapper.py')
wrapper=importlib.util.module_from_spec(spec);spec.loader.exec_module(wrapper)

SOURCE='docker://registry.tilo.so/tilo/tilo-development@sha256:'+'1'*64
DIGEST='sha256:'+'1'*64


class RegistryWarmupTests(unittest.TestCase):
    def test_source_and_target_preflight(self):
        with self.subTest(phase="bound_source_requires_exact_image_manifest_and_config"):
            raw=b'{"schemaVersion":2}'
            digest='sha256:'+hashlib.sha256(raw).hexdigest()
            source='docker://registry.tilo.so/tilo/tilo-development@'+digest
            image_id='4'*64
            canonical=source.removeprefix('docker://')
            inspect=SimpleNamespace(returncode=0,stdout=json.dumps([{
                'Id':image_id,'Digest':digest,'RepoDigests':[canonical]}]).encode())
            manifest=SimpleNamespace(returncode=0,stdout=raw)
            config=SimpleNamespace(returncode=0,stdout=json.dumps({
                'Digest':digest,'Architecture':'arm64'}).encode())
            metadata=f'image="{canonical}"\nimageid="{image_id}"\nrootless=0\n'
            with patch('builtins.open',mock_open(read_data=metadata.encode())), \
                    patch.object(wrapper.subprocess,'run',side_effect=[inspect,manifest,config]) as run:
                self.assertIsNone(wrapper.require_bound_source(source,digest))
            self.assertEqual(run.call_args_list[0].args[0],[
                '/usr/bin/podman','--remote=false','image','inspect',image_id])
            self.assertEqual(run.call_args_list[1].args[0][0:3],[
                '/usr/bin/skopeo','inspect','--raw'])
            hostile=SimpleNamespace(returncode=0,stdout=json.dumps([{
                'Id':image_id,'Digest':digest,'RepoDigests':['registry.invalid/x@'+digest]}]).encode())
            with patch('builtins.open',mock_open(read_data=metadata.encode())), \
                    patch.object(wrapper.subprocess,'run',return_value=hostile), \
                    self.assertRaisesRegex(ValueError,'image identity mismatch'):
                wrapper.require_bound_source(source,digest)
            duplicate=(metadata+f'imageid="{image_id}"\n').encode()
            with patch('builtins.open',mock_open(read_data=duplicate)), \
                    self.assertRaisesRegex(ValueError,'metadata is ambiguous'):
                wrapper.require_bound_source(source,digest)
        with self.subTest(phase="private_udev_preflight_requires_pid1_mount_and_no_database"):
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
        with self.subTest(phase="prepared_target_requires_recursive_root_and_esp_mounts"):
            argv=[wrapper.INSTALLER_BOOTC,'install','to-filesystem',
                  '--root-mount-spec=','--source-imgref='+SOURCE,
                  '--target-transport=registry','/target']
            valid=('40 1 0:40 / /target rw - btrfs /dev/loop0p2 rw\n'
                   '41 40 0:41 / /target/boot rw - vfat /dev/loop0p1 rw\n')
            with tempfile.TemporaryDirectory() as tmp:
                mountinfo=Path(tmp)/'mountinfo'
                mountinfo.write_text(valid)
                self.assertIsNone(wrapper.require_install_target(argv,SOURCE,mountinfo))
                for content in (
                        valid.splitlines()[0]+'\n',
                        valid.replace(' - vfat ', ' - btrfs '),
                        valid.replace('0:41', '0:40'),
                        valid+valid.splitlines()[1]+'\n'):
                    with self.subTest(content=content):
                        mountinfo.write_text(content)
                        with self.assertRaisesRegex(ValueError,'recursively visible|ambiguous'):
                            wrapper.require_install_target(argv,SOURCE,mountinfo)
                mountinfo.write_text(valid)
                invalid = (
                    argv[:-1]+['/other'],
                    [item for item in argv if not item.startswith('--source-imgref=')],
                    [('--source-imgref=docker://registry.invalid/x@sha256:'+'2'*64)
                     if item.startswith('--source-imgref=') else item for item in argv],
                    [item for item in argv if item != '--target-transport=registry'],
                    [item if item != '--target-transport=registry'
                     else '--target-transport=containers-storage' for item in argv],
                    argv[:-1]+['--source-imgref='+SOURCE,'/target'],
                    argv[:-1]+['--source-imgref=docker://registry.invalid/x@sha256:'+'2'*64,
                               '/target'],
                    argv[:-1]+['--target-transport=registry','/target'],
                    argv[:-1]+['--target-transport=containers-storage','/target'],
                )
                for command in invalid:
                    with self.subTest(command=command), \
                            self.assertRaisesRegex(ValueError,'prepared target exactly'):
                        wrapper.require_install_target(command,SOURCE,mountinfo)
            with patch.object(wrapper,'inspect_digest',return_value=False) as inspect, \
                    patch.object(wrapper.time,'sleep') as sleep:
                self.assertFalse(wrapper.warm(SOURCE,DIGEST,sleep=sleep))
            self.assertEqual(inspect.call_count,3)
            self.assertEqual([call.args for call in sleep.call_args_list],[(2,),(5,)])

    def test_registry_inspect_and_warmup(self):
        with self.subTest(phase="inspect_uses_exact_argv_without_a_shell_and_requires_digest"):
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
        with self.subTest(phase="warmup_is_bounded_with_exact_backoff_and_digest_failure"):
            with patch.object(wrapper,'inspect_digest',side_effect=[False,False,True]) as inspect, \
                    patch.object(wrapper.time,'sleep') as sleep:
                self.assertTrue(wrapper.warm(SOURCE,DIGEST,sleep=sleep))
            self.assertEqual(inspect.call_count,3)
            self.assertEqual([call.args for call in sleep.call_args_list],[(2,),(5,)])


class PrivateSourceStoreTests(unittest.TestCase):
    class Endpoint:
        def __init__(self,value):self.value=value
        def __str__(self):return self.value
        def __fspath__(self):return self.value
        def lstat(self):
            return SimpleNamespace(st_mode=wrapper.stat.S_IFDIR|0o755,st_uid=0,st_gid=0,
                                   st_dev=42,st_ino=84)

    class Process:
        def poll(self):return None
        def wait(self,timeout=None):return 0
        def kill(self):pass

    def test_private_source_store_lifecycle(self):
        with self.subTest(phase="mounts_after_container_start_and_cleans_private_view"):
            lower=self.Endpoint('/run/parent');target=self.Endpoint('/var/lib/containers')
            readonly=SimpleNamespace(f_flag=wrapper.os.ST_RDONLY)
            calls=[]
            def invoke(argv,**kwargs):
                calls.append(argv)
                if argv[0]=='/usr/bin/findmnt':
                    return SimpleNamespace(returncode=0,stdout=b'fuse.fuse-overlayfs\n')
                return SimpleNamespace(returncode=0,stdout=b'')
            with tempfile.TemporaryDirectory() as tmp, \
                    patch.object(wrapper.os,'statvfs',return_value=readonly), \
                    patch.object(wrapper,'_private_directory'), \
                    patch.object(wrapper.subprocess,'Popen',return_value=self.Process()) as popen, \
                    patch.object(wrapper.subprocess,'run',side_effect=invoke):
                with wrapper.private_source_store_view(lower,target,Path(tmp)):
                    self.assertTrue(any(Path(tmp).iterdir()))
                self.assertEqual(list(Path(tmp).iterdir()),[])
            argv=popen.call_args.args[0]
            self.assertEqual(argv[0:3],['/usr/bin/fuse-overlayfs','-f','-o'])
            self.assertIn('lowerdir=/run/parent',argv[3])
            self.assertEqual(argv[-1],'/var/lib/containers')
            self.assertEqual(calls[-1],["/usr/bin/fusermount3","-u","/var/lib/containers"])
        with self.subTest(phase="unmount_failure_is_fatal_but_private_upper_is_removed"):
            lower=self.Endpoint('/run/parent');target=self.Endpoint('/var/lib/containers')
            readonly=SimpleNamespace(f_flag=wrapper.os.ST_RDONLY)
            def invoke(argv,**kwargs):
                if argv[0]=='/usr/bin/findmnt':
                    return SimpleNamespace(returncode=0,stdout=b'fuse.fuse-overlayfs\n')
                return SimpleNamespace(returncode=1,stdout=b'')
            with tempfile.TemporaryDirectory() as tmp, \
                    patch.object(wrapper.os,'statvfs',return_value=readonly), \
                    patch.object(wrapper,'_private_directory'), \
                    patch.object(wrapper.subprocess,'Popen',return_value=self.Process()), \
                    patch.object(wrapper.subprocess,'run',side_effect=invoke), \
                    self.assertRaisesRegex(ValueError,'did not unmount'):
                with wrapper.private_source_store_view(lower,target,Path(tmp)):
                    pass
            self.assertEqual(list(Path(tmp).iterdir()) if Path(tmp).exists() else [],[])


class WrapperLifecycleTests(unittest.TestCase):
    class Child:
        def __init__(self,returncode=0,on_wait=None):
            self.returncode=returncode;self.on_wait=on_wait;self.signals=[]
        def wait(self):
            if self.on_wait:self.on_wait()
            return self.returncode
        def poll(self):return None
        def send_signal(self,value):self.signals.append(value)

    def test_bootc_wrapper_lifecycle(self):
        with self.subTest(phase="bootc_argv_and_exit_code_propagate_exactly"):
            argv=[wrapper.INSTALLER_BOOTC,'install','to-disk','--target-imgref=x:y','/output/disk.raw']
            child=self.Child(23)
            with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                    patch.object(wrapper.subprocess,'Popen',return_value=child) as popen:
                self.assertEqual(wrapper.run_bootc(SOURCE,DIGEST,argv),23)
            popen.assert_called_once_with(argv)
        with self.subTest(phase="background_heartbeat_stops_and_joins"):
            # bootc runs until the heartbeat has checked the source at least once.
            beat=threading.Event()
            def beating(*_args,**_kwargs):
                beat.set();return True
            child=self.Child(on_wait=lambda:self.assertTrue(beat.wait(HANG_BOUND)))
            with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                    patch.object(wrapper,'inspect_digest',side_effect=beating) as inspect, \
                    patch.object(wrapper.subprocess,'Popen',return_value=child):
                self.assertEqual(wrapper.run_bootc(
                    SOURCE,DIGEST,[wrapper.INSTALLER_BOOTC,'install'],heartbeat_interval=.005),0)
            self.assertGreater(inspect.call_count,0)
            self.assertFalse(any(t.name=='tilo-registry-heartbeat' for t in threading.enumerate()))
        with self.subTest(phase="heartbeat_failure_does_not_replace_bootc_result"):
            failure=wrapper.subprocess.TimeoutExpired(['/usr/bin/skopeo'],10)
            beat=threading.Event()
            def failing(*_args,**_kwargs):
                beat.set();raise failure
            child=self.Child(17,on_wait=lambda:self.assertTrue(beat.wait(HANG_BOUND)))
            with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                    patch.object(wrapper,'inspect_digest',side_effect=failing) as inspect, \
                    patch.object(wrapper.subprocess,'Popen',return_value=child):
                self.assertEqual(wrapper.run_bootc(
                    SOURCE,DIGEST,[wrapper.INSTALLER_BOOTC,'install'],heartbeat_interval=.005),17)
            self.assertGreater(inspect.call_count,0)
            self.assertFalse(any(t.name=='tilo-registry-heartbeat' for t in threading.enumerate()))
        with self.subTest(phase="signal_is_forwarded_then_re_raised_after_cleanup"):
            handlers={}
            def set_handler(signum,handler):
                previous=handlers.get(signum,signal.SIG_DFL);handlers[signum]=handler;return previous
            child=self.Child(on_wait=lambda:handlers[signal.SIGTERM](signal.SIGTERM,None))
            with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=True), \
                    patch.object(wrapper.signal,'signal',side_effect=set_handler), \
                    patch.object(wrapper.subprocess,'Popen',return_value=child), \
                    self.assertRaises(wrapper.ForwardedSignal) as raised:
                wrapper.run_bootc(SOURCE,DIGEST,[wrapper.INSTALLER_BOOTC,'install'])
            self.assertEqual(child.signals,[signal.SIGTERM])
            self.assertEqual(raised.exception.signum,signal.SIGTERM)
        with self.subTest(phase="failed_warmup_never_starts_bootc_or_heartbeat"):
            with patch.object(wrapper,'require_bound_source'), patch.object(wrapper,'warm',return_value=False), \
                    patch.object(wrapper.subprocess,'Popen') as popen:
                self.assertEqual(wrapper.run_bootc(SOURCE,DIGEST,[wrapper.INSTALLER_BOOTC]),75)
            popen.assert_not_called()
            self.assertFalse(any(t.name=='tilo-registry-heartbeat' for t in threading.enumerate()))


if __name__=='__main__':unittest.main()
