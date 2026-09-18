import hashlib
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('compat',Path(__file__).resolve().parents[1]/'dev/native_source_compatibility.py')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)

class CacheAdmission(unittest.TestCase):
 def approval(self):
  return dict(schema=2,purpose='tilo-reviewed-cache-source-compatibility',assembly_source_revision=c.CACHE_SOURCE,previous_approval_json='{}',previous_approval_sha256=hashlib.sha256(b'{}').hexdigest(),wpe_bridge={})
 def test_requires_exact_source_and_closed_schema(self):
  for mutation in ({'assembly_source_revision':'a'*40},{'unexpected':True},{'schema':1}):
   a=self.approval();a.update(mutation)
   with patch.object(c,'read_approval',return_value=a),patch.object(c,'git') as git,self.assertRaises(ValueError):
    c.verify('repo',c.CACHE_SOURCE,'approval','digest','native','reconstruction')
   git.assert_not_called()
 def test_requires_exact_three_commit_ancestry_and_paths(self):
  cases=[[(c.CACHE_BASE+'\n').encode()],[(c.CACHE_API+'\n').encode(),(c.CACHE_TRUST+'\n').encode(),(c.CACHE_BASE+'\n').encode(),b'os/Containerfile\0evil\0']]
  for answers in cases:
   with patch.object(c,'read_approval',return_value=self.approval()),patch.object(c,'git',side_effect=answers),self.assertRaises(ValueError):
    c.verify('repo',c.CACHE_SOURCE,'approval','digest','native','reconstruction')
 def test_previous_approval_is_independently_hash_bound(self):
  a=self.approval();a['previous_approval_json']='{"changed":true}'
  answers=[(c.CACHE_API+'\n').encode(),(c.CACHE_TRUST+'\n').encode(),(c.CACHE_BASE+'\n').encode(),b'os/Containerfile\0',b'os/dev/build-image.sh\0',b'os/Containerfile\0']
  with patch.object(c,'read_approval',return_value=a),patch.object(c,'git',side_effect=answers),patch.object(c,'_verify_base') as base,self.assertRaises(ValueError):
   c.verify('repo',c.CACHE_SOURCE,'approval','digest','native','reconstruction')
  base.assert_not_called()

if __name__=='__main__':unittest.main()
