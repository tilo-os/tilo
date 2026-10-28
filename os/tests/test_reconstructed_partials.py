from unittest.mock import patch
import contextlib,copy,hashlib,importlib.util,json,struct,tempfile,types,unittest
from pathlib import Path
from frozen_source_fixture import NATIVE_DRIVER,NATIVE_RECIPE
from restart_fixture import restart_fixture
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('resume',ROOT/'os/dev/native_resume.py');r=importlib.util.module_from_spec(spec);spec.loader.exec_module(r)
def sha(data):return hashlib.sha256(data).hexdigest()
def canonical(value):return json.dumps(value,sort_keys=True,separators=(',',':')).encode()


class ReconstructedPartials(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
  self.source=self.root/'source';(self.source/'os/dev').mkdir(parents=True)
  (self.source/'os/Containerfile').write_bytes(NATIVE_RECIPE.encode());(self.source/'os/dev/build-image.sh').write_bytes(NATIVE_DRIVER.encode())
  self.payload=self.root/'payload';(self.payload/'partial').mkdir(parents=True);self.dest=self.root/'dest';self.dest.mkdir()
  data=bytearray(64);data[:7]=b'\x7fELF\x02\x01\x01';struct.pack_into('<H',data,16,3);struct.pack_into('<H',data,18,183)
  (self.payload/'partial/tilo-crypto').write_bytes(data);(self.payload/'partial/tilo-crypto').chmod(0o755)
  self.record={'size':64,'sha256':sha(data)}
  self.legacy=[{'receipt_sha256':'a'*64,'receipt':{'purpose':r.PARTIAL,'inventory':{'shell':{'size':1,'sha256':'b'*64}}}}]
  self.seed={'shell':{'size':1,'sha256':'b'*64}};required=['shell','tilo-crypto']
  @contextlib.contextmanager
  def mounted(identity):yield self.payload
  def file_record(path):
   value=path.read_bytes();return {'size':len(value),'sha256':sha(value)},value[:64]
  tools=types.SimpleNamespace(sha=sha,canonical=canonical,strict_json=json.loads,registry_identity=lambda *args:'sha256:'+'c'*64,
   command=lambda *args:None,inspect_local=lambda *args:'local',mounted=mounted,read_bounded=lambda path,limit:path.read_bytes())
  self.native=types.SimpleNamespace(REQUIRED=required,tools=tools,file_record=file_record)
  original=(self.source/'os/Containerfile').read_text();driver=(self.source/'os/dev/build-image.sh').read_text();derived,missing=r.derive_recipe(original,self.seed,required)
  reconstruction={'schema':1,'purpose':'tilo-native-reconstruction','source_revision':r.SOURCE,'run_id':'21554','attempt':'1',
   'original_recipe_executed_verbatim':False,'resume_helper_sha256':'d'*64,'producer_sha256':'e'*64,'checkpoints':self.legacy,'reused_inventory':self.seed,
   'source_path':'/work/repo','derived_recipe_path':'/work/stage/Containerfile.resumed','seed_image_id':'sha256:'+'f'*64,'missing_targets':missing,
   'original_recipe_sha256':sha(original.encode()),'derived_recipe_sha256':sha(derived.encode()),'original_driver_sha256':sha(driver.encode())}
  reconstruction['derived_driver_sha256']=sha(r.derive_driver(driver,reconstruction['source_path'],reconstruction['derived_recipe_path'],reconstruction['seed_image_id']).encode())
  self.receipt_sha='1'*64;inventory={'tilo-crypto':self.record}
  self.approval={'schema':1,'purpose':'tilo-reconstructed-partial-admission','source_revision':r.SOURCE,'helper_revision':r.PARTIAL_HELPER,'run_id':21554,'run_number':8166,'attempt':1,
   'reconstruction_sha256':sha(canonical(reconstruction)),'resume_helper_sha256':'d'*64,'producer_sha256':'e'*64,'receipts':{self.receipt_sha:{'inventory':inventory,'capture_script_sha256':'2'*64}}}
  self.value={'schema':1,'purpose':r.RECONSTRUCTED_PARTIAL,'source_revision':r.SOURCE,'helper_revision':r.PARTIAL_HELPER,'run_id':21554,'run_number':8166,'attempt':1,'architecture':'aarch64',
   'inventory':inventory,'capture_script_sha256':'2'*64,'reconstruction':reconstruction,'reconstruction_sha256':sha(canonical(reconstruction))}
  for key in ['installable','complete_native_builder','build_success','resume_admitted','original_recipe_executed_verbatim']:self.value[key]=False
  self.receipt={k:self.value[k] for k in ['schema','purpose','source_revision','helper_revision','run_id','run_number','inventory','reconstruction_sha256','installable','complete_native_builder','build_success','resume_admitted']}
  self.receipt.update(reference='registry.tilo.so/tilo/tilo-development-partial-native-aarch64@sha256:'+'3'*64,config_digest='sha256:'+'c'*64,
   raw_manifest_verified=True,raw_config_verified=True,payload_hashes_verified=True,temporary_public_context_removed_on_exit=True)
  self.save()
 def save(self):
  self.raw=canonical(self.value);self.receipt['metadata_sha256']=sha(self.raw);(self.payload/'partial-checkpoint.json').write_bytes(self.raw)
 def validate(self,union=None):
  union={} if union is None else union
  r.validate_reconstructed_partial(self.native,self.receipt,self.receipt_sha,self.approval,self.source,self.legacy,self.seed,self.dest,union)
  return union
 def test_admitted_chain_copy(self):
  with self.subTest(phase="admitted_chain_and_payload_copied"):
   self.assertEqual(self.validate(),self.value['inventory']);self.assertEqual((self.dest/'tilo-crypto').read_bytes(),(self.payload/'partial/tilo-crypto').read_bytes())
  restart_fixture(self)
  with self.subTest(phase="copied_bytes_rechecked_before_union_update"):
   original=r.shutil.copyfile
   def corrupt(source,destination):
    original(source,destination);destination.write_bytes(b'corrupt')
   union={}
   with patch.object(r.shutil,'copyfile',corrupt):
    with self.assertRaises(ValueError):self.validate(union)
   self.assertEqual(union,{})
  restart_fixture(self)
  with self.subTest(phase="legacy_validator_still_rejects_reconstructed_purpose"):
   with self.assertRaises(ValueError):r.validate_partial(self.native,self.receipt,{},'',self.dest,{})
 def test_substitution_rejections(self):
  with self.subTest(phase="missing_admission_and_wrong_receipt_rejected"):
   self.approval=None
   with self.assertRaises(ValueError):self.validate()
  restart_fixture(self)
  with self.subTest(phase="producer_purpose_identity_and_capture_substitutions"):
   for key,value in [('purpose',r.PARTIAL),('run_id',20866),('helper_revision','0'*40),('source_revision','0'*40),('capture_script_sha256','0'*64),('architecture','x86_64'),('build_success',True)]:
    with self.subTest(key=key):
     old=self.value[key];self.value[key]=value;self.save()
     with self.assertRaises(ValueError):self.validate()
     self.value[key]=old;self.save()
  restart_fixture(self)
  with self.subTest(phase="chain_or_derivation_substitution_rejected"):
   for key,value in [('checkpoints',[]),('reused_inventory',{}),('derived_recipe_sha256','0'*64),('producer_sha256','0'*64),('missing_targets',{})]:
    with self.subTest(key=key):
     old=self.value['reconstruction'][key];self.value['reconstruction'][key]=value
     # Even re-approving the hash does not bypass independent derivation/seed checks.
     digest=sha(canonical(self.value['reconstruction']));self.value['reconstruction_sha256']=digest;self.receipt['reconstruction_sha256']=digest;self.approval['reconstruction_sha256']=digest;self.save()
     with self.assertRaises(ValueError):self.validate()
     self.value['reconstruction'][key]=old
  restart_fixture(self)
  with self.subTest(phase="payload_architecture_overlap_symlink_and_hash_rejected"):
   with self.assertRaises(ValueError):self.validate({'tilo-crypto':{'size':1,'sha256':'0'*64}})
   path=self.payload/'partial/tilo-crypto';data=bytearray(path.read_bytes());struct.pack_into('<H',data,18,62);path.write_bytes(data)
   with self.assertRaises(ValueError):self.validate()
   path.unlink();path.symlink_to('/etc/passwd')
   with self.assertRaises(ValueError):self.validate()
  restart_fixture(self)
  with self.subTest(phase="wrong_architecture_even_with_matching_approved_file_hash"):
   path=self.payload/'partial/tilo-crypto';data=bytearray(path.read_bytes());struct.pack_into('<H',data,18,62);path.write_bytes(data)
   self.record['sha256']=sha(data);self.save()
   with self.assertRaises(ValueError):self.validate()
if __name__=='__main__':unittest.main()
