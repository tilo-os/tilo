import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('hydrate',Path(__file__).resolve().parents[1]/'dev/hydrate-assembly-history.py')
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)

class PartialHistory(unittest.TestCase):
 def test_only_reviewed_old_blobs_are_hydrated_for_offline_use(self):
  with tempfile.TemporaryDirectory() as temp:
   root=Path(temp);server=root/'server';checkout=root/'checkout';server.mkdir()
   def git(repo,*args,env=None):
    return subprocess.check_output(['git','-C',str(repo),*args],env=env,stderr=subprocess.DEVNULL,text=True).strip()
   git(server,'init');git(server,'config','user.name','Fixture');git(server,'config','user.email','fixture@example.invalid')
   for name in ('os/Containerfile','os/dev/build-wpe.sh','unrelated'):
    p=server/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text('old '+name)
   git(server,'add','.');git(server,'commit','-m','old');old=git(server,'rev-parse','HEAD');missing=git(server,'rev-parse',old+':unrelated');needed=git(server,'rev-parse',old+':os/Containerfile')
   for name in ('os/Containerfile','unrelated'):(server/name).write_text('new '+name)
   git(server,'commit','-am','new');new=git(server,'rev-parse','HEAD')
   git(server,'config','uploadpack.allowFilter','true');git(server,'config','uploadpack.allowAnySHA1InWant','true')
   subprocess.run(['git','clone','--filter=blob:none','--no-local',server.as_uri(),str(checkout)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
   offline=dict(os.environ,GIT_CONFIG_COUNT='1',GIT_CONFIG_KEY_0='protocol.allow',GIT_CONFIG_VALUE_0='never')
   def exists(oid):return subprocess.run(['git','-C',str(checkout),'cat-file','-e',oid],env=offline,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0
   self.assertFalse(exists(needed));self.assertFalse(exists(missing))
   raw=json.dumps({'native_source_revision':old,'assembly_source_revision':new,'changes':{'os/Containerfile':{}}}).encode()
   receipt=h.hydrate(checkout,raw,hashlib.sha256(raw).hexdigest(),new,os.environ.copy())
   self.assertTrue(receipt['offline_blobs_verified']);self.assertTrue(exists(needed));self.assertFalse(exists(missing))
   git(checkout,'remote','set-url','origin','file:///unavailable-source')
   self.assertEqual(git(checkout,'show',old+':os/Containerfile',env=offline),'old os/Containerfile')
   with self.assertRaises(ValueError):h.hydrate(checkout,raw,'0'*64,new,offline)

if __name__=='__main__':unittest.main()
