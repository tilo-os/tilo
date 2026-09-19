import hashlib
import subprocess
import tempfile
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('compat',Path(__file__).resolve().parents[1]/'dev/native_source_compatibility.py')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)

class CacheAdmission(unittest.TestCase):
 def approval(self):
  return dict(schema=2,purpose='tilo-reviewed-cache-source-compatibility',assembly_source_revision=c.CACHE_SOURCE,previous_approval_json='{}',previous_approval_sha256=hashlib.sha256(b'{}').hexdigest(),wpe_bridge={})
 def test_only_exact_inert_trust_stage_can_extend_recipe_prefix(self):
  old=b'FROM base AS builder\nRUN original\nFROM ${BASE_REF} AS rootfs\nRUN old\nFROM rootfs AS split\nRUN tail\n'
  new=c.TRUST_TOOL_STAGE+old.replace(b'RUN old',b'RUN fixed')
  c.verify_cache_recipe(old,new)
  for modified in (new.replace(b'RUN original',b'RUN changed'),new.replace(c.TRUST_TOOL_STAGE,c.TRUST_TOOL_STAGE+b'RUN extra\n'),new.replace(b'AS public-trust-tools',b'AS other')):
   with self.assertRaises(ValueError):c.verify_cache_recipe(old,modified)
 def test_requires_exact_source_and_closed_schema(self):
  for mutation in ({'assembly_source_revision':'a'*40},{'unexpected':True},{'schema':1}):
   a=self.approval();a.update(mutation)
   with patch.object(c,'read_approval',return_value=a),patch.object(c,'git') as git,self.assertRaises(ValueError):
    c.verify('repo',c.CACHE_SOURCE,'approval','digest','native','reconstruction')
   git.assert_not_called()
 def test_requires_exact_four_commit_ancestry_and_paths(self):
  cases=[[(c.CACHE_BASE+'\n').encode()],[(c.CACHE_VIEWER+'\n').encode(),(c.CACHE_API+'\n').encode(),(c.CACHE_TRUST+'\n').encode(),(c.CACHE_BASE+'\n').encode(),b'os/Containerfile\0evil\0']]
  for answers in cases:
   with patch.object(c,'read_approval',return_value=self.approval()),patch.object(c,'git',side_effect=answers),self.assertRaises(ValueError):
    c.verify('repo',c.CACHE_SOURCE,'approval','digest','native','reconstruction')
 def test_previous_approval_is_independently_hash_bound(self):
  a=self.approval();a['previous_approval_json']='{"changed":true}'
  answers=[(c.CACHE_VIEWER+'\n').encode(),(c.CACHE_API+'\n').encode(),(c.CACHE_TRUST+'\n').encode(),(c.CACHE_BASE+'\n').encode(),b'os/Containerfile\0',b'os/dev/build-image.sh\0',b'os/Containerfile\0',b'os/Containerfile\0']
  with patch.object(c,'read_approval',return_value=a),patch.object(c,'git',side_effect=answers),patch.object(c,'_verify_base') as base,self.assertRaises(ValueError):
   c.verify('repo',c.CACHE_SOURCE,'approval','digest','native','reconstruction')
  base.assert_not_called()

class DracutAdmission(unittest.TestCase):
 def test_real_git_exact_child_paths_and_rootfs_isolation(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp)
   def git(*args):
    return subprocess.check_output(['git','-C',str(root),*args],stderr=subprocess.DEVNULL).decode().strip()
   git('init','-q');git('config','user.name','Fixture');git('config','user.email','fixture@example.invalid');git('config','core.autocrlf','false')
   recipe=b'FROM base AS builder\nRUN compile\nFROM ${BASE_REF} AS rootfs\nRUN dracut old\nFROM rootfs AS split\n'
   for name in c.DRACUT_PATHS[:-1]:
    path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(recipe if name=='os/Containerfile' else b'old test\n')
   git('add','.');git('commit','-qm','parent');parent=git('rev-parse','HEAD')
   for name in c.DRACUT_PATHS:
    (root/name).write_bytes(recipe.replace(b'dracut old',b'dracut corrected') if name=='os/Containerfile' else b'corrected test\n')
   git('add','.');git('commit','-qm','local root correction');child=git('rev-parse','HEAD')
   with patch.object(c,'CACHE_SOURCE',parent),patch.object(c,'DRACUT_SOURCE',child):
    result=c.verify_dracut_successor(root,child)
    self.assertEqual(set(result['sha256']),set(c.DRACUT_PATHS))
    with self.assertRaises(ValueError):c.verify_dracut_successor(root,parent)
   # Even an otherwise admitted exact child cannot change producer instructions.
   git('reset','--soft',parent)
   (root/'os/Containerfile').write_bytes(recipe.replace(b'RUN compile',b'RUN different compiler'))
   git('add','.');git('commit','-qm','producer changed');bad=git('rev-parse','HEAD')
   with patch.object(c,'CACHE_SOURCE',parent),patch.object(c,'DRACUT_SOURCE',bad),self.assertRaisesRegex(ValueError,'non-rootfs'):
    c.verify_dracut_successor(root,bad)
   git('reset','--soft',parent);(root/'Cargo.toml').write_bytes(b'extra input\n')
   git('add','.');git('commit','-qm','extra input');bad=git('rev-parse','HEAD')
   with patch.object(c,'CACHE_SOURCE',parent),patch.object(c,'DRACUT_SOURCE',bad),self.assertRaisesRegex(ValueError,'paths'):
    c.verify_dracut_successor(root,bad)


if __name__=='__main__':unittest.main()
