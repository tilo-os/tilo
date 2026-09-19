import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('hydrate',Path(__file__).resolve().parents[1]/'dev/hydrate-assembly-history.py')
h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
spec=importlib.util.spec_from_file_location('compat',Path(__file__).resolve().parents[1]/'dev/native_source_compatibility.py')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)

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

 def test_dracut_cache_history_supports_complete_offline_admission(self):
  with tempfile.TemporaryDirectory() as temp:
   root=Path(temp);server=root/'server';checkout=root/'checkout';server.mkdir()
   def git(repo,*args,env=None):
    return subprocess.check_output(['git','-C',str(repo),*args],env=env,stderr=subprocess.DEVNULL)
   def oid(revision,name):
    return git(server,'rev-parse',revision+':'+name).decode().strip()
   def commit(message,updates):
    for name,raw in updates.items():
     path=server/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(raw)
    git(server,'add','.');git(server,'commit','-qm',message)
    return git(server,'rev-parse','HEAD').decode().strip()
   def encoded(value):
    return (json.dumps(value,sort_keys=True,separators=(',',':'))+'\n').encode()
   def digest(raw):return hashlib.sha256(raw).hexdigest()

   git(server,'init','-q')
   for key,value in (('user.name','Fixture'),('user.email','fixture@example.invalid'),
                     ('core.autocrlf','false'),('commit.gpgsign','false')):
    git(server,'config',key,value)
   recipe=(b'FROM base AS builder\nRUN compile\nFROM ${BASE_REF} AS rootfs\n'
           +b''.join(b'COPY '+name.encode()+b' /rootfs-input\n' for name in c.POLICY_PATHS)
           +b'RUN bash /usr/local/bin/verify-runtime-crypto-packages.sh\n'
           +b'RUN dracut original\nFROM rootfs AS split\nRUN seal\n')
   initial={name:('original '+name+'\n').encode() for name in (*c.PATHS,*c.POLICY_PATHS,*c.DRACUT_PATHS[1:-1])}
   initial.update({'os/Containerfile':recipe,'os/dev/build-wpe.sh':b'build WPE\n',
                   'os/dev/build-image.sh':b'build image\n','unrelated':b'unrelated historical secret-free fixture\n'})
   pre_native=commit('older unrelated history',initial)
   pins={'SOURCE':commit('native source',{'unrelated':b'unrelated current fixture\n'})}
   provider=commit('provider correction',{name:('corrected '+name+'\n').encode() for name in c.PATHS})
   recipe=recipe.replace(b'dracut original',b'dracut rootfs')
   rootfs=commit('rootfs correction',{'os/Containerfile':recipe})
   policy=commit('policy correction',{name:('corrected '+name+'\n').encode() for name in c.POLICY_PATHS})
   recipe=recipe.replace(b'dracut rootfs',b'dracut base')
   pins['CACHE_BASE']=commit('reviewed cache base',{'os/Containerfile':recipe})
   recipe=c.TRUST_TOOL_STAGE+recipe
   pins['CACHE_TRUST']=commit('build-only trust stage',{'os/Containerfile':recipe})
   pins['CACHE_API']=commit('cache API correction',{'os/dev/build-image.sh':b'corrected build image\n'})
   recipe=recipe.replace(b'dracut base',b'dracut viewer')
   pins['CACHE_VIEWER']=commit('viewer correction',{'os/Containerfile':recipe})
   recipe=recipe.replace(b'dracut viewer',b'dracut cache')
   pins['CACHE_SOURCE']=commit('cache correction',{'os/Containerfile':recipe})
   recipe=recipe.replace(b'dracut cache',b'dracut corrected')
   updates={name:('dracut correction '+name+'\n').encode() for name in c.DRACUT_PATHS[1:]}
   updates['os/Containerfile']=recipe
   pins['DRACUT_SOURCE']=commit('dracut closure',updates)

   def change(name,revision):
    old,new=(git(server,'show',rev+':'+name) for rev in (pins['SOURCE'],revision))
    return dict(old_blob=oid(pins['SOURCE'],name),new_blob=oid(revision,name),
                old_sha256=digest(old),new_sha256=digest(new))
   def wpe_receipt(revision):
    audit={'compatible_recipes':{'os/Containerfile':digest(git(server,'show',revision+':os/Containerfile')),
                                'os/dev/build-wpe.sh':digest(initial['os/dev/build-wpe.sh'])}}
    return encoded({'reference':'fixture/wpe@sha256:'+'4'*64,'audit':audit,'audit_sha256':digest(encoded(audit))})
   original_wpe=wpe_receipt(pins['SOURCE'])
   def bridge(revision):
    current=wpe_receipt(revision)
    return dict(original_receipt_json=original_wpe.decode(),original_receipt_sha256=digest(original_wpe),
                assembly_receipt_json=current.decode(),assembly_receipt_sha256=digest(current))
   native_digest='1'*64;reconstruction_digest='2'*64
   prior=encoded(dict(schema=1,purpose='tilo-rootfs-native-source-compatibility',
       native_source_revision=pins['SOURCE'],assembly_source_revision=pins['CACHE_BASE'],
       provider_revision=provider,rootfs_revision=rootfs,policy_revision=policy,
       native_receipt_sha256=native_digest,approved_reconstruction_sha256=reconstruction_digest,
       changes={name:change(name,pins['CACHE_BASE']) for name in (*c.PATHS,*c.POLICY_PATHS,'os/Containerfile')},
       wpe_bridge=bridge(pins['CACHE_BASE'])))
   approval=dict(schema=2,purpose='tilo-reviewed-cache-source-compatibility',
       assembly_source_revision=pins['DRACUT_SOURCE'],previous_approval_json=prior.decode(),
       previous_approval_sha256=digest(prior),wpe_bridge=bridge(pins['DRACUT_SOURCE']))
   raw=encoded(approval);approval_path=root/'approval.json';approval_path.write_bytes(raw)

   git(server,'config','uploadpack.allowFilter','true')
   git(server,'config','uploadpack.allowAnySHA1InWant','true')
   subprocess.run(['git','clone','--filter=blob:none','--no-local','--no-checkout',server.as_uri(),str(checkout)],
                  check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
   offline=dict(os.environ,GIT_CONFIG_COUNT='1',GIT_CONFIG_KEY_0='protocol.allow',GIT_CONFIG_VALUE_0='never')
   def exists(blob):
    return subprocess.run(['git','-C',str(checkout),'cat-file','-e',blob],env=offline,
                          stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0
   current_blobs={name:oid(pins['DRACUT_SOURCE'],name) for name in c.DRACUT_PATHS}
   older_tests={name:oid(pins['CACHE_SOURCE'],name) for name in c.DRACUT_PATHS[1:-1]}
   unrelated_old=oid(pre_native,'unrelated')
   for blob in (*current_blobs.values(),*older_tests.values(),unrelated_old):self.assertFalse(exists(blob))
   # Fetch only the unrelated current file needed to finish a clean checkout.
   # Every reviewed current and historical blob must come from the hydrator.
   git(checkout,'cat-file','blob',oid(pins['DRACUT_SOURCE'],'unrelated'))
   with patch.multiple(c,**pins),patch.dict(sys.modules,{'native_source_compatibility':c}):
    receipt=h.hydrate(checkout,raw,digest(raw),pins['DRACUT_SOURCE'],os.environ.copy())
    self.assertTrue(receipt['offline_blobs_verified'])
    for blob in (*current_blobs.values(),*older_tests.values()):self.assertTrue(exists(blob))
    self.assertFalse(exists(unrelated_old))
    with self.assertRaisesRegex(ValueError,'digest differs'):
     h.hydrate(checkout,raw,'0'*64,pins['DRACUT_SOURCE'],os.environ.copy())
    invalid=encoded(dict(approval,previous_approval_sha256='0'*64))
    with self.assertRaisesRegex(ValueError,'previous admission digest differs'):
     h.hydrate(checkout,invalid,digest(invalid),pins['DRACUT_SOURCE'],os.environ.copy())
    invalid=encoded(dict(approval,assembly_source_revision='f'*40))
    with self.assertRaisesRegex(ValueError,'cache correction source differs'):
     h.hydrate(checkout,invalid,digest(invalid),'f'*40,os.environ.copy())
    git(checkout,'remote','set-url','origin','file:///unavailable-source')
    git(checkout,'checkout','--force','HEAD',env=offline)
    # Execute every production ancestry, tree, blob, rootfs and WPE check with
    # transport disabled, rather than mocking compatibility verification.
    with patch.dict(os.environ,offline,clear=True):
     verified=c.verify(checkout,pins['DRACUT_SOURCE'],approval_path,digest(raw),native_digest,reconstruction_digest)
    self.assertEqual(verified['assembly_source_revision'],pins['DRACUT_SOURCE'])
    self.assertEqual(verified['native_source_revision'],pins['SOURCE'])
    self.assertEqual(verified['dracut_source_correction']['sha256'],{name:digest(updates[name]) for name in c.DRACUT_PATHS})
    self.assertTrue(verified['complete_native_inventory_required'])
    self.assertFalse(exists(unrelated_old))

if __name__=='__main__':unittest.main()
