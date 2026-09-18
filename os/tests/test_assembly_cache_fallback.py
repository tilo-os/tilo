import importlib.util
from pathlib import Path
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('fallback',Path(__file__).parents[1]/'dev/assembly-cache-fallback.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
REPO='code.podesta.ai/qos/tilo-development-assembly-cache-aarch64-v1'
ENV={'TILO_RAW_ROOTFS_ONLY':'1','TILO_PUBLIC_TRUST_MODE':'development','TILO_BUILD_ARGS':'--layers=true --cache-from '+REPO+' --cache-to '+REPO+' --build-arg EPOCH=12','PRIVATE':'unchanged'}
ERROR=b'Error: failed pushing cache to repo: writing blob: uploading layer chunked: received unexpected HTTP status: 500 Internal Server Error\n'
class Tests(unittest.TestCase):
 def test_only_export_removed(self):
  fixed=m.without_export(ENV)
  self.assertEqual(fixed['TILO_BUILD_ARGS'],'--layers=true --cache-from '+REPO+' --build-arg EPOCH=12')
  self.assertEqual(fixed['PRIVATE'],'unchanged');self.assertIn('--cache-to',ENV['TILO_BUILD_ARGS'])
 def test_only_exact_failure_retried_once_and_second_failure_propagates(self):
  calls=[];records=[]
  def run(cmd,env):calls.append(env.copy());return (125,ERROR)
  self.assertEqual(m.execute(['fixture'],ENV,run,lambda x:records.append(x.copy())),125)
  self.assertEqual(len(calls),2);self.assertFalse(records[-1]['cache_hits_verified'])
 def test_real_build_errors_never_retried(self):
  for code,tail in [(1,ERROR),(125,b'Error: building at STEP RUN: exit status 1'),(125,ERROR+b'Error: another failure\n'),(125,ERROR.replace(b'500',b'401'))]:
   calls=[]
   def run(c,e):calls.append(e);return code,tail
   self.assertEqual(m.execute(['fixture'],ENV,run),code);self.assertEqual(len(calls),1)
 def test_real_child_keeps_completed_work_and_retries_without_export(self):
  import subprocess,sys,json
  with tempfile.TemporaryDirectory() as temp:
   p=Path(temp);script=p/'child.py';cache=p/'completed';count=p/'compilations'
   script.write_text('import os,pathlib,sys\np=pathlib.Path('+repr(str(cache))+')\nif not p.exists():\n p.write_text("object")\n pathlib.Path('+repr(str(count))+').write_text("1")\nif "--cache-to" in os.environ["TILO_BUILD_ARGS"]:\n print('+repr(ERROR.decode().strip())+')\n sys.exit(125)\nassert p.read_text()=="object"\nprint("cached object reused")\n')
   launcher=p/'launch.py';launcher.write_text('import importlib.util,sys,os\ns=importlib.util.spec_from_file_location("f",'+repr(str(Path(m.__file__).resolve()))+');m=importlib.util.module_from_spec(s);s.loader.exec_module(m)\nos.environ.update('+repr(ENV)+')\nsys.exit(m.execute([sys.executable,'+repr(str(script))+'],os.environ))\n')
   result=subprocess.run([sys.executable,str(launcher)],capture_output=True,timeout=10)
   self.assertEqual(result.returncode,0,result.stderr);self.assertIn(b'cached object reused',result.stdout);self.assertEqual(count.read_text(),'1')
if __name__=='__main__':unittest.main()
