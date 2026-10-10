from pathlib import Path
import hashlib,json,os,re,subprocess,time,uuid
BUILD_LOG=Path('/tmp/dictation-browser-composition-native-build-remote.log')
# The exact owned review path is supplied as a script argument by the controller.
import sys
OUT=Path(sys.argv[1]);assert OUT.parent==Path('/tmp') and OUT.name.startswith('tilo-dictation-browser-composition-')
def digest(p):
 with Path(p).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
b=json.loads((OUT/'build-receipt.json').read_text());assert b['build_exit']==0
HOST=OUT/'runtime/chromium/tilo-chromium-host';SRC=OUT/'source';TESTS=SRC/'apps/browser/chromium/tests'
assert digest(HOST)==b['host_sha256'];assert not (HOST.parent/'chrome-sandbox').exists()
for p,h in b['source_hashes'].items():assert digest(SRC/p)==h,p
selected=['test_unrelated_custom_composition_end_cannot_release_the_original_field','test_removed_custom_composition_editor_allows_only_a_fresh_successor','test_composition_before_first_private_prepare_is_refused_and_recovers','test_preexisting_composition_blocks_successor_focus_until_it_ends','test_password_otp_and_composition_are_refused','test_cancel_and_retired_cancel_cannot_affect_a_successor','test_focus_change_and_return_permanently_retires_original_lease','test_document_open_cannot_reuse_removed_composition_observer']
records=[]
for name,fixture,args,expected in [('guarded-lifecycle',TESTS/'guarded_text_input.py',['GuardedTextInput.'+x for x in selected],len(selected)),('actual-native-composition',TESTS/'native_composition.py',[],4)]:
 cmd=['bwrap','--unshare-all','--die-with-parent','--new-session','--ro-bind','/usr','/usr','--ro-bind','/lib','/lib','--ro-bind','/lib64','/lib64','--ro-bind','/etc','/etc','--proc','/proc','--dev','/dev','--tmpfs','/tmp','--ro-bind',str(OUT),str(OUT),'--chdir',str(SRC),'--clearenv','--setenv','LANG','C.UTF-8','--setenv','LC_ALL','C.UTF-8','--setenv','TZ','UTC','--setenv','HOME','/tmp','--setenv','TILO_TEST_PRIVATE_NAMESPACE','1','--setenv','PYTHONDONTWRITEBYTECODE','1','--setenv','PYTHONPATH',str(TESTS),'--','/usr/bin/python3',str(fixture),str(HOST),*args,'-v']
 log=OUT/(name+'.log');start=time.monotonic()
 with log.open('wb') as f:run=subprocess.run(cmd,stdout=f,stderr=subprocess.STDOUT,timeout=120)
 text=log.read_text();print(name+' exit='+str(run.returncode)+'\n'+text[-6500:],flush=True)
 matches=re.findall(r'Ran (\d+) tests? in ([0-9.]+)s',text)
 r={'fixture':name,'fixture_sha256':digest(fixture),'exit':run.returncode,'expected_tests':expected,'executed_tests':int(matches[-1][0]) if matches else None,'seconds':time.monotonic()-start,'log_sha256':digest(log)};records.append(r)
 (OUT/'native-test-receipt.json').write_text(json.dumps({'build':b,'records':records,'private_PID_and_network_namespace':True,'mic':False,'model':False,'production_pins_changed':False},indent=2)+'\n')
 assert run.returncode==0 and matches and int(matches[-1][0])==expected and 'skipped' not in text,'Native qualification failed; preserve assertions and logs'
for p,h in b['source_hashes'].items():assert digest(SRC/p)==h,p
assert digest(HOST)==b['host_sha256']
print('ALL_SELECTED_NATIVE_TESTS_PASS',sum(x['executed_tests'] for x in records),flush=True)
