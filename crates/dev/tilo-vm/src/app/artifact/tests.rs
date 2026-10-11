use super::*;
use crate::hypervisor::CommandLine;

// Exercise the real stdio transport and normal RPC sequence. Real SDK/TAPP
// signatures and native closure are independently covered by tilo-vmd cohort tests.
const PEER: &str = r"
import base64,json,sys
case,log=sys.argv[1:3]
print(json.dumps({'tilo_device_admin':{'device':'test','label':'test','route':'direct','via':'test','session':1}}),flush=True)
hash=''; provenance={}; installed={}; received=0
for line in sys.stdin:
 request=json.loads(line); method=request['method']; params=request.get('params',{})
 with open(log,'a') as file: file.write(method+'\n')
 error=None
 if method=='hello': result={'protocol':9,'daemon_version':'test','root':'test','arch':'x86_64'}
 elif method=='layer.upload.begin': result={'upload':'test'}; received=0
 elif method=='layer.upload.chunk':
  received+=len(base64.b64decode(params['data'])); result={'received':received}
 elif method=='layer.import':
  provenance=params['provenance']; hash=sys.argv[3]; result={'hash':hash if case!='import-hash' else 'f'*64,'existed':False}
 elif method=='layer.verify':
  result={'hash':hash,'provenance':dict(provenance)}
  if case=='provenance': result['provenance']['source_commit']='e'*40
 elif method=='swap':
  if case in ['signature','publisher','runtime','incomplete','activation']:
   error={'code':4,'message':'cohort verification: '+case,'hint':'supply a valid cohort'}
  else:
   installed[params['component']]=params['layer'] if case!='slot-layer' else 'f'*64
   result={'slot':params['slot'],'generation':1,'changed':False,'policy':'no_restart','actions':[]}
 elif method=='slot.show':
  result={'slot':{'name':params['name'],'kind':'headless','owner':{'branch':'test'},'user':'test','generation':1,'generations':[0,1],'components':installed,'session':'active','seat':None},'files':[]}
 else: raise RuntimeError(method)
 response={'jsonrpc':'2.0','id':request['id']}
 response['error' if error else 'result']=error if error else result
 print(json.dumps(response),flush=True)
";

#[test]
fn imported_cohort_uses_verified_import_and_addressed_slot_readback() {
    for case in [
        "valid",
        "import-hash",
        "provenance",
        "signature",
        "publisher",
        "runtime",
        "incomplete",
        "activation",
        "slot-layer",
    ] {
        let (dir, request) = crate::artifact::tests::fixture(false);
        let mut prepared = prepare(&request).unwrap();
        let layer = prepared
            .layers
            .iter_mut()
            .find(|l| l.receipt.component == "apps+runtime")
            .unwrap();
        let log = dir.path().join("rpc.log");
        let line = CommandLine::new("python3", &["-c", PEER])
            .arg(case)
            .arg(log.display().to_string())
            .arg(&layer.receipt.layer_hash);
        let (mut remote, _) = Remote::over_peer("artifact-test", &line).unwrap();
        let imported = import(&mut remote, layer);
        if matches!(case, "import-hash" | "provenance") {
            assert!(imported.is_err());
            assert!(!std::fs::read_to_string(&log).unwrap().contains("swap\n"));
            continue;
        }
        assert_eq!(imported.unwrap(), layer.receipt.layer_hash);
        let activated = activate(&mut remote, "review", layer, None);
        if case == "valid" {
            assert!(activated.is_ok(), "{activated:?}");
        } else {
            assert!(activated.is_err(), "{case} was acknowledged as activated");
        }
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("layer.upload.chunk\nlayer.import\nlayer.verify\nswap\n"));
        if matches!(
            case,
            "signature" | "publisher" | "runtime" | "incomplete" | "activation"
        ) {
            assert!(
                !calls.contains("slot.show\n"),
                "failed apply must stop immediately"
            );
        } else {
            assert!(calls.ends_with("slot.show\n"));
        }
    }
}
