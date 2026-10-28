use super::*;

struct Guest {
    state: &'static str,
    fail_wake: bool,
    probe_known: bool,
    log: PathBuf,
}

impl Runner for Guest {
    fn output(&mut self, command: &CommandLine) -> Result<hypervisor::Output, CliError> {
        use std::io::Write as _;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        let probe = command.args.iter().any(|arg| arg == "domstate");
        let wake = command.args.iter().any(|arg| arg == "dompmwakeup");
        if !probe && !wake {
            return Err(CliError::InvalidInput(format!(
                "unexpected power operation {command:?}"
            )));
        }
        writeln!(log, "{}", if probe { "domstate" } else { "dompmwakeup" }).unwrap();
        let success = if probe {
            self.probe_known
        } else {
            !self.fail_wake
        };
        Ok(hypervisor::Output {
            success,
            status: format!("exit status: {}", u8::from(!success)),
            stdout: if probe {
                self.state.into()
            } else {
                String::new()
            },
            stderr: if success {
                String::new()
            } else {
                "fixture refusal".into()
            },
        })
    }

    fn interactive(&mut self, _: &CommandLine) -> Result<bool, CliError> {
        Err(CliError::InvalidInput(
            "wake policy must not open an interactive command".into(),
        ))
    }
}

fn guest(root: &Path, state: &'static str) -> Guest {
    Guest {
        state,
        fail_wake: false,
        probe_known: true,
        log: root.join("calls"),
    }
}

const DAEMON: &str = r"
import json,sys
log,protocol=sys.argv[1:]
assert open(log).read()=='domstate\ndompmwakeup\n'
print(json.dumps({'tilo_device_admin':{'device':'test','label':'test','route':'direct','via':'test','session':1}}),flush=True)
for line in sys.stdin:
 request=json.loads(line)
 method=request['method']
 with open(log,'a') as file: file.write(method+'\n')
 if method=='hello':
  result={'protocol':int(protocol),'daemon_version':'test','root':'test','arch':'x86_64','awake':{'held':True,'until':123,'hold_until':456}}
 elif method=='swap':
  assert request['params']['slot']=='test-slot'
  assert request['params']['component']=='shell'
  assert request['params']['layer']=='a'*64
  result={'slot':'test-slot','generation':8,'changed':False,'policy':'no_restart','actions':[]}
 else: raise RuntimeError(method)
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
";

#[test]
fn managed_swap_wakes_before_strict_transport_and_retains_hello_lease() {
    for compatible in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut guest = guest(dir.path(), "pmsuspended");
        let entry = VmEntry {
            hypervisor: "libvirt".into(),
            ..VmEntry::default()
        };
        let vm = Vm {
            name: "test",
            entry: &entry,
        };
        let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
        let protocol = if compatible {
            tilo_vm_protocol::PROTOCOL_VERSION
        } else {
            0
        };
        let line = CommandLine::new("python3", &["-c", DAEMON])
            .arg(guest.log.display().to_string())
            .arg(protocol.to_string());
        let connected = swap_transport(adapter.as_ref(), &vm, &mut guest, || {
            Remote::over_peer("test", &line)
        });
        if compatible {
            let (mut remote, _) = connected.unwrap();
            let lease = remote.hello.awake.as_ref().unwrap();
            assert!(lease.held);
            assert_eq!(lease.hold_until, Some(456));
            let applied = remote
                .call(Swap {
                    slot: "test-slot".into(),
                    component: "shell".into(),
                    layer: "a".repeat(64),
                    info: None,
                })
                .unwrap();
            assert_eq!(applied.generation, 8);
            assert!(!applied.changed);
            assert_eq!(
                std::fs::read_to_string(&guest.log).unwrap(),
                "domstate\ndompmwakeup\nhello\nswap\n"
            );
        } else {
            assert!(matches!(connected, Err(CliError::ProtocolMismatch { .. })));
            assert_eq!(
                std::fs::read_to_string(&guest.log).unwrap(),
                "domstate\ndompmwakeup\nhello\n"
            );
        }
    }
}

#[test]
fn wake_failure_stops_before_transport() {
    let dir = tempfile::tempdir().unwrap();
    let mut guest = guest(dir.path(), "pmsuspended");
    guest.fail_wake = true;
    let entry = VmEntry {
        hypervisor: "libvirt".into(),
        ..VmEntry::default()
    };
    let vm = Vm {
        name: "test",
        entry: &entry,
    };
    let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
    let called = std::cell::Cell::new(false);
    let result = swap_transport(adapter.as_ref(), &vm, &mut guest, || {
        called.set(true);
        Ok(())
    });
    assert!(matches!(result, Err(CliError::CommandFailed { .. })));
    assert!(!called.get());
    assert_eq!(
        std::fs::read_to_string(&guest.log).unwrap(),
        "domstate\ndompmwakeup\n"
    );
}

#[test]
fn physical_swaps_never_probe_or_change_power_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut guest = guest(dir.path(), "pmsuspended");
    let entry = VmEntry {
        hypervisor: hypervisor::DEVICE.into(),
        ..VmEntry::default()
    };
    let vm = Vm {
        name: "physical",
        entry: &entry,
    };
    let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
    let result: Result<(), CliError> = swap_transport(adapter.as_ref(), &vm, &mut guest, || {
        Err(CliError::InvalidInput("physical transport refused".into()))
    });
    assert!(matches!(result, Err(CliError::InvalidInput(_))));
    assert!(!guest.log.exists(), "physical power adapter must not run");
}

#[test]
fn running_off_and_unknown_managed_states_do_not_start_or_wake() {
    for (state, known) in [
        ("running", true),
        ("shut off", true),
        ("pmsuspended", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut guest = guest(dir.path(), state);
        guest.probe_known = known;
        let entry = VmEntry {
            hypervisor: "libvirt".into(),
            ..VmEntry::default()
        };
        let vm = Vm {
            name: "test",
            entry: &entry,
        };
        let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
        let called = std::cell::Cell::new(false);
        swap_transport(adapter.as_ref(), &vm, &mut guest, || {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(called.get());
        assert_eq!(std::fs::read_to_string(&guest.log).unwrap(), "domstate\n");
    }
}

#[cfg(unix)]
const POWER: &str = r"#!/usr/bin/env python3
import os,sys
root=os.environ['TILO_SWAP_WAKE_FIXTURE']
state=root+'/state'
method=sys.argv[-2]
with open(root+'/calls','a') as file: file.write(method+'\n')
if method=='domstate': print(open(state).read())
elif method=='dompmwakeup': open(state,'w').write('running')
else: raise RuntimeError(method)
";

#[cfg(unix)]
const COMPONENT_PEER: &str = r"#!/usr/bin/env python3
import json,os,sys
root=os.environ['TILO_SWAP_WAKE_FIXTURE']
assert open(root+'/state').read()=='running'
for line in sys.stdin:
 request=json.loads(line)
 method=request['method']
 with open(root+'/calls','a') as file: file.write(method+'\n')
 if method=='hello':
  result={'protocol':int(os.environ['TILO_SWAP_WAKE_PROTOCOL']),'daemon_version':'test','root':'test','arch':'x86_64'}
 elif method=='status':
  result={'protocol':int(os.environ['TILO_SWAP_WAKE_PROTOCOL']),'daemon_version':'test','slots':[],'seat0':None,'storage':{'layers_bytes':0,'free_bytes':None,'total_bytes':None},'issues':[]}
  open(root+'/state','w').write('pmsuspended')
 elif method=='swap':
  assert request['params']['slot']=='test-slot'
  assert request['params']['component']=='shell'
  assert request['params']['layer']=='a'*64
  result={'slot':'test-slot','generation':8,'changed':False,'policy':'no_restart','actions':[]}
 else: raise RuntimeError(method)
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
";

#[cfg(unix)]
#[test]
fn component_swap_resumes_both_initial_and_preapply_connections() {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(root) = std::env::var_os("TILO_SWAP_WAKE_FIXTURE") {
        run_component_swap(&PathBuf::from(root));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (name, script) in [("virsh", POWER), ("ssh", COMPONENT_PEER)] {
        let path = root.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(root.join("state"), "pmsuspended").unwrap();
    std::fs::write(
        root.join("vms.toml"),
        "[vms.fixture]\nhypervisor = 'libvirt'\nssh = 'root@127.0.0.1'\n",
    )
    .unwrap();
    let mut paths = vec![root.to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "app::swap_wake_tests::component_swap_resumes_both_initial_and_preapply_connections",
            "--exact",
            "--nocapture",
        ])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("TILO_VM_SSH", root.join("ssh"))
        .env("TILO_SWAP_WAKE_FIXTURE", root)
        .env(
            "TILO_SWAP_WAKE_PROTOCOL",
            tilo_vm_protocol::PROTOCOL_VERSION.to_string(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(
        std::fs::read_to_string(root.join("calls")).unwrap(),
        "domstate\ndompmwakeup\nhello\nstatus\ndomstate\ndompmwakeup\nhello\nswap\n"
    );
}

#[cfg(unix)]
fn run_component_swap(root: &Path) {
    let mut app = App {
        json: true,
        target: Some("fixture".into()),
        worktree: None,
        registry_path: root.join("vms.toml"),
        runner: SystemRunner { cwd: None },
        receipt: Receipt::new(root, vec![]),
        warnings: Vec::new(),
        peer: None,
    };
    let outcome = app
        .swap(
            Some("test-slot".into()),
            "shell".into(),
            &format!("layer:{}", "a".repeat(64)),
            None,
            None,
            false,
            KernelFlags::default(),
        )
        .unwrap();
    assert_eq!(outcome.data["generation"], 8);
}
