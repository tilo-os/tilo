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

/// The acceptance consumer runs as an unprivileged user in a detached worktree
/// of a root-owned CI clone: HEAD resolves, but the commit object in the shared
/// store need not be readable. Removing the loose commit object models exactly
/// that as git sees it (the open fails) without depending on who runs the test.
#[test]
fn consumer_checkout_accepts_exact_head_without_its_object_and_refuses_another_commit() {
    let dir = tempfile::tempdir().unwrap();
    let clone = dir.path().join("clone");
    let checkout = dir.path().join("src");
    let git = |cwd: &std::path::Path, args: &[&str]| -> String {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    std::fs::create_dir(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    git(&clone, &["commit", "-q", "--allow-empty", "-m", "producer"]);
    let producer = git(&clone, &["rev-parse", "HEAD"]);
    git(&clone, &["commit", "-q", "--allow-empty", "-m", "later"]);
    let later = git(&clone, &["rev-parse", "HEAD"]);
    git(
        &clone,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            checkout.to_str().unwrap(),
            &producer,
        ],
    );
    let worktree = Worktree::detect(&checkout).unwrap();
    consumer_checkout(&worktree, &producer).unwrap();

    let refused = consumer_checkout(&worktree, &later).unwrap_err().render();
    assert!(
        refused.contains("differs from requested exact source")
            && refused.contains(&producer)
            && refused.contains(&later),
        "{refused}"
    );

    let (shard, rest) = producer.split_at(2);
    std::fs::remove_file(clone.join(".git/objects").join(shard).join(rest)).unwrap();
    assert!(
        worktree.rev_parse("HEAD").is_none(),
        "the fixture must make the commit object unreadable"
    );
    consumer_checkout(&worktree, &producer).unwrap();
    assert!(consumer_checkout(&worktree, &later).is_err());
}
