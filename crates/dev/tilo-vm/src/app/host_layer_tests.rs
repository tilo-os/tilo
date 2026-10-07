//! Drive the host import path over the production stdio Remote transport.
use super::*;

const PEER: &str = r#"
import base64,json,sys
hash,component,returned,log,protocol=sys.argv[1:]
print(json.dumps({"tilo_device_admin":{"device":"test","label":"test","route":"direct","via":"test","session":1}}),flush=True)
received=0
for line in sys.stdin:
 request=json.loads(line)
 method=request['method']
 with open(log,'a') as file: file.write(method+'\n')
 if method=='hello': result={"protocol":int(protocol),"daemon_version":"test","root":"test","arch":"x86_64"}
 elif method=='layer.list': result=[{"hash":hash,"provenance":{"component":component}}]
 elif method=='layer.upload.begin': result={"upload":"test"}
 elif method=='layer.upload.chunk':
  received+=len(base64.b64decode(request['params']['data']))
  result={"received":received}
 elif method=='layer.import': result={"hash":returned,"existed":False}
 else: raise RuntimeError(method)
 print(json.dumps({"jsonrpc":"2.0","id":request['id'],"result":result}),flush=True)
"#;

fn import_case(cached: bool, component: &str, corrupt: bool) {
    let dir = tempfile::tempdir().unwrap();
    let staging = build::Staging::new(&Worktree {
        root: dir.path().into(),
        branch: None,
    })
    .unwrap();
    std::fs::create_dir_all(staging.tree().join("usr/bin")).unwrap();
    std::fs::write(staging.tree().join("usr/bin/shell"), b"actual binary bytes").unwrap();
    let expected = tilo_vm_protocol::layer_hash::tree_hash(&staging.tree()).unwrap();
    let listed = if cached {
        expected.clone()
    } else {
        {
            std::fs::write(staging.tree().join("usr/bin/shell"), b"older binary bytes").unwrap();
            let previous = tilo_vm_protocol::layer_hash::tree_hash(&staging.tree()).unwrap();
            std::fs::write(staging.tree().join("usr/bin/shell"), b"actual binary bytes").unwrap();
            previous
        }
    };
    let returned = if corrupt {
        "f".repeat(64)
    } else {
        expected.clone()
    };
    let log = dir.path().join("calls");
    let line = CommandLine::new("python3", &["-c", PEER])
        .arg(listed)
        .arg(component)
        .arg(returned)
        .arg(log.display().to_string())
        .arg(tilo_vm_protocol::PROTOCOL_VERSION.to_string());
    let (mut remote, _) = Remote::over_peer("test", &line).unwrap();
    let mut app = App {
        json: true,
        target: None,
        worktree: None,
        registry_path: dir.path().join("vms.toml"),
        runner: SystemRunner { cwd: None },
        receipt: Receipt::new(dir.path(), vec![]),
        warnings: Vec::new(),
        peer: None,
    };
    let result = app.import_host_layer(
        &mut remote,
        &staging,
        tilo_vm_protocol::Provenance::local("shell"),
    );
    let reused = cached && component == "shell";
    let calls = std::fs::read_to_string(log).unwrap();
    if reused {
        assert_eq!(result.unwrap(), expected);
        assert_eq!(calls, "hello\nlayer.list\n");
        assert!(
            !staging.archive().exists(),
            "a cache hit never packs the layer"
        );
    } else {
        assert!(calls.contains("layer.upload.chunk\n"));
        assert!(calls.ends_with("layer.import\n"));
        assert!(staging.archive().exists());
        if corrupt {
            assert!(result.is_err(), "a mismatched import cannot reach swap");
        } else {
            assert_eq!(result.unwrap(), expected);
        }
    }
}

#[test]
fn host_layer_cache_skips_upload_only_for_identical_component_content() {
    import_case(true, "shell", false);
    import_case(false, "shell", false);
    import_case(true, "other", false);
    import_case(false, "shell", true);
}
