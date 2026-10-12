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

const APP_PEER: &str = r"
import base64,json,os,sys,threading,time
root,session,protocol=sys.argv[1:]
def record(value):
 with open(root+'/calls','a') as file: file.write(session+':'+value+'\n')
def disconnect():
 deadline=time.monotonic()+10
 while not os.path.exists(root+'/build-start'):
  if time.monotonic()>deadline: os._exit(3)
  time.sleep(.01)
 os.close(0); os.close(1)
 open(root+'/disconnected','w').close()
 os._exit(0)
print(json.dumps({'tilo_device_admin':{'device':'test','label':'test','route':'direct','via':'test','session':int(session)}}),flush=True)
if session=='1': threading.Thread(target=disconnect,daemon=True).start()
received=bytearray()
for line in sys.stdin:
 request=json.loads(line); method=request['method']; record(method)
 if method=='hello': result={'protocol':int(protocol),'daemon_version':'test','root':'test','arch':'x86_64'}
 elif method=='layer.upload.begin': result={'upload':'owned-upload'}
 elif method=='layer.upload.chunk':
  received.extend(base64.b64decode(request['params']['data'])); result={'received':len(received)}
 elif method=='app.install':
  assert bytes(received)==b'owned package transport fixture'
  assert request['params']['slot']=='fixture-refused'
  with open(root+'/uploaded','wb') as file: file.write(received)
  print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':2,'message':'fixture slot refused','hint':'no install occurred'}}),flush=True)
  continue
 else: raise RuntimeError(method)
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
";

fn app_peer(root: &Path, session: &str) -> Remote {
    let command = CommandLine::new("python3", &["-c", APP_PEER])
        .arg(root.display().to_string())
        .arg(session)
        .arg(tilo_vm_protocol::PROTOCOL_VERSION.to_string());
    Remote::over_peer("test", &command).unwrap().0
}

#[test]
fn host_app_build_disconnect_reconnects_before_upload_and_preserves_refusal() {
    let dir = tempfile::tempdir().unwrap();
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
    let mut remote = app_peer(dir.path(), "1");
    let result = app.install_host_app_package(
        &mut remote,
        "fixture-refused",
        |_runner| {
            std::fs::write(dir.path().join("build-start"), b"").unwrap();
            let started = Instant::now();
            while !dir.path().join("disconnected").exists() {
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(10));
            }
            let package = dir.path().join("app.tapp");
            std::fs::write(&package, b"owned package transport fixture").unwrap();
            Ok(package)
        },
        |_app| Ok(app_peer(dir.path(), "2")),
    );
    let error = result.expect_err("daemon refusal must not become installation success");
    assert!(
        error.to_string().contains("fixture slot refused"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("uploaded")).unwrap(),
        b"owned package transport fixture"
    );
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    assert_eq!(
        calls,
        "1:hello\n2:hello\n2:layer.upload.begin\n2:layer.upload.chunk\n2:app.install\n"
    );
    let receipt_path = app
        .receipt
        .finish(Err(&json!({ "message": error.to_string() })))
        .unwrap();
    let receipt: Value = serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["status"], "failure");
    let phases: Vec<_> = receipt["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|phase| phase["name"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["build", "transfer", "install"]);
}

#[test]
fn host_app_build_and_reconnect_failures_never_upload() {
    for build_fails in [true, false] {
        let dir = tempfile::tempdir().unwrap();
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
        let mut remote = app_peer(dir.path(), "1");
        let result = app.install_host_app_package(
            &mut remote,
            "fixture-refused",
            |_runner| {
                if build_fails {
                    return Err(CliError::InvalidInput("build refused".into()));
                }
                let package = dir.path().join("app.tapp");
                std::fs::write(&package, b"owned package transport fixture").unwrap();
                Ok(package)
            },
            |_app| {
                assert!(!build_fails, "failed builds must not reconnect");
                Err(CliError::InvalidInput("connection refused".into()))
            },
        );
        let error = result.unwrap_err().to_string();
        assert!(error.contains(if build_fails {
            "build refused"
        } else {
            "connection refused"
        }));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            "1:hello\n"
        );
        assert!(!dir.path().join("uploaded").exists());
        assert!(!app.receipt.summary().contains("transfer"));
        assert!(!app.receipt.summary().contains("install"));
    }
}

#[test]
fn metadata_failure_refuses_host_and_remote_builds_before_admission_or_apply() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "this is not a Cargo manifest",
    )
    .unwrap();
    let log = dir.path().join("metadata-calls");
    let line = CommandLine::new("python3", &["-c", PEER])
        .arg("unused")
        .arg("shell")
        .arg("unused")
        .arg(log.display().to_string())
        .arg(tilo_vm_protocol::PROTOCOL_VERSION.to_string());
    let (mut remote, _) = Remote::over_peer("test", &line).unwrap();
    let worktree = Worktree {
        root: dir.path().into(),
        branch: None,
    };
    let mut app = App {
        json: true,
        target: None,
        worktree: Some(worktree),
        registry_path: dir.path().join("vms.toml"),
        runner: SystemRunner { cwd: None },
        receipt: Receipt::new(dir.path(), vec![]),
        warnings: Vec::new(),
        peer: None,
    };
    let vm = Resolved {
        name: "test".into(),
        entry: VmEntry::default(),
        source: Source::Argument,
    };
    let component = Manifest::parse("[[component]]\nname='shell'\npackages=['shell']\n")
        .unwrap()
        .components
        .remove("shell")
        .unwrap();
    for (attempt, remote_builder) in [false, true].into_iter().enumerate() {
        let result = if remote_builder {
            app.build_layer_remote(
                &mut remote,
                &vm,
                &component,
                &["must-not-start-builder".into()],
            )
        } else {
            app.build_layer(&mut remote, &vm, &component)
        };
        let CliError::CommandFailed {
            command, stderr, ..
        } = result.unwrap_err()
        else {
            panic!("the original Cargo metadata failure must remain observable");
        };
        assert_eq!(
            command,
            "cargo metadata --locked --no-deps --format-version 1"
        );
        assert!(stderr.contains("Cargo.toml"), "{stderr}");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "hello\n");
        let phases = app.receipt.summary();
        assert_eq!(phases.split(", ").count(), attempt + 1);
        assert!(
            phases
                .split(", ")
                .all(|phase| phase.starts_with("metadata ")),
            "{phases}"
        );
        assert!(
            !dir.path().join("target").exists(),
            "no build or staging may begin"
        );
    }
}
