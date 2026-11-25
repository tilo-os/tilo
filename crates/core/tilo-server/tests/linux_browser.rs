//! A workspace's Browser opens in the workspace's machine, on the device that
//! takes its Linux turn: the server asks the turn of the workspace's devices
//! that run Linux, and the one that takes it asks the hosted-VM service beside
//! it to start the session, handing it what the session joins the workspace
//! with. The machine's daemon is then asked, as a device of the workspace, to
//! launch Browser and say where its window is.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the VM host and their routers stay up until the test ends"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use linux_vm::hosted::control::{self, OpenBrowser, Reply, Request};
use tilo_actions::api::PlacedAction;
use tilo_actions::remote::browser::{self, OpenedBrowser};
use tilo_actions::remote::environment_window::{self as vm, Holds, Launch, Session, Shown, Window};
use tilo_actions::remote::{
    ActionProvider as _, Admits, Admitted, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    Outcome, Pending,
};
use tilo_peer_state::actions::{ActionService, RemoteDevice};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_runtime::device::environments::Follow;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use super::linux_turn::{Placed, placed_with, stop};
use crate::common::device_endpoint;

/// What a hosted-VM service was asked to open, and how often, and the device it says the VM's
/// daemon is.
#[derive(Default)]
struct Opened {
    opens: Mutex<Vec<(String, String, String)>>,
    times: AtomicUsize,
    device: Mutex<[u8; 32]>,
}

/// A hosted-VM service that opens the first browser session asked for and
/// attaches to it after, naming the VM's daemon as the device it was told, and what it was
/// asked.
fn hosted_service(dir: &std::path::Path) -> (std::path::PathBuf, Arc<Opened>) {
    let socket = dir.join("hosted.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let asked = Arc::new(Opened::default());
    let counted = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(Some(request)) = control::FRAMING.read_value::<_, Request>(&mut stream) else {
                continue;
            };
            let reply = match request {
                Request::OpenBrowser {
                    browser:
                        OpenBrowser {
                            workspace,
                            account,
                            material,
                            ..
                        },
                } => {
                    counted
                        .opens
                        .lock()
                        .unwrap()
                        .push((workspace, account, material));
                    let opened = counted.times.fetch_add(1, Ordering::SeqCst) == 0;
                    Reply::Opened {
                        opened_ms: opened.then_some(900),
                        device: hex::encode(*counted.device.lock().unwrap()),
                    }
                }
                Request::StopBrowser { .. } => Reply::Done,
                // The VM host asks which VMs it runs and how
                // much room it has.
                Request::Status => Reply::Status {
                    vms: Vec::new(),
                    load: linux_vm::hosted::control::HostLoad::default(),
                },
                other => panic!("Browser's service is asked about it: {other:?}"),
            };
            let _ = control::FRAMING.write_value(&mut stream, &reply);
        }
    });
    (socket, asked)
}

/// What a workspace VM's daemon answers the host about its sessions ([`vm`]): Browser is
/// launched on a record, its window is up under the provider `42`, and a session is ended.
#[derive(Default)]
struct Sessions {
    launched: Mutex<Vec<Launch>>,
    stopped: Mutex<Vec<Holds>>,
}

impl Admits for Sessions {
    const PROVIDER: &'static str = vm::PROVIDER;
    const APP: &'static str = vm::PROVIDER;

    fn rows(&self) -> Vec<PlacedAction> {
        Vec::new()
    }

    fn start(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let InvokeArgument::Json(named) = &request.argument else {
            return Err("the machine's actions take their arguments as an object".into());
        };
        let named = serde_json::Value::Object(named.clone());
        let answer = match request.target.address.action.as_str() {
            vm::LAUNCH => {
                let launch: Launch =
                    serde_json::from_value(named).map_err(|error| error.to_string())?;
                self.launched.lock().unwrap().push(launch);
                serde_json::json!({})
            }
            vm::STOP => {
                let Session { holds } =
                    serde_json::from_value(named).map_err(|error| error.to_string())?;
                self.stopped.lock().unwrap().push(holds);
                serde_json::json!({})
            }
            vm::WINDOW => serde_json::to_value(Window {
                browser: Shown {
                    running: true,
                    provider: Some("42".into()),
                    used_ms: 0,
                },
                apps: Shown::default(),
            })
            .unwrap(),
            other => return Err(format!("the machine has no action `{other}`")),
        };
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput {
            value: Some(answer),
            ..tilo_actions::ActionOutput::default()
        })))
    }
}

/// A workspace's VM, as the host that placed the workspace reaches it: a device the workspace's
/// roster lists beside the host, whose daemon answers the VM's sessions on the device action
/// service. Nothing dials into a VM: its daemon dialled the host when it logged in, and the host
/// asks it back over that link.
struct Machine {
    device: [u8; 32],
    sessions: Arc<Sessions>,
    _link: RemoteDevice,
}

async fn machine(placed: &Placed) -> Machine {
    // The VM is a device of the person the workspace is, listed beside the host; it signs its
    // own copy's first rows, as every device signs only what it writes.
    let device = placed.operator.enrol("vm", DeviceCapabilities::member());
    let host = placed.hosted.host.workspaces().device_cert().clone();
    let roster = vec![device.cert.clone(), host];
    let now = tilo_clock::wall();
    let mut peer = StatePeer::new(
        &placed.workspace,
        device.cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(&placed.workspace, &device.cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &device.sk,
        now,
    )
    .unwrap();
    let sessions = Arc::new(Sessions::default());
    let answers = ActionService::new(device.sk.clone(), Arc::clone(&sessions) as _);
    answers.hold(Replica::new(peer));
    // The daemon asks its host over a link it dials, which it answers the host's asks on.
    let endpoint = device_endpoint(&device).await;
    let link = answers.remote(
        &endpoint,
        &placed.hosted.router.endpoint().addr(),
        &placed.workspace,
    );
    let answered = link
        .query(
            &Caller::visitor(device.device_pubkey()),
            CatalogueQuery::default(),
        )
        .await;
    assert!(answered.is_some(), "the machine reaches its host");
    Machine {
        device: device.device_pubkey(),
        sessions,
        _link: link,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_opens_browser_on_the_device_running_its_machine() {
    let service = tempfile::tempdir().unwrap();
    let device = [9; 32];
    let (socket, asked) = hosted_service(service.path());
    *asked.device.lock().unwrap() = device;
    let placed = placed_with(1, Some(socket)).await;
    let open = |material: &str| {
        let asked = tilo_actions::remote::browser::OpenBrowser {
            workspace: placed.workspace.clone(),
            account: "acct-1".into(),
            material: material.to_owned(),
        };
        let ran = tokio::task::block_in_place(|| {
            placed.shells.run(
                &placed.workspace,
                browser::target(),
                &asked,
                Follow::default(),
            )
        })
        .unwrap();
        serde_json::from_value::<OpenedBrowser>(ran.output.unwrap()).unwrap()
    };
    let opened = open("{\"record\":\"atlas\"}");
    assert_eq!(opened.opened_ms, Some(900), "Browser was opened for it");
    assert_eq!(
        opened.device,
        hex::encode(device),
        "and it names the VM's device, where its sessions are launched and read"
    );
    assert_eq!(
        open("{\"record\":\"atlas\"}").opened_ms,
        None,
        "the running session is attached to, not opened again"
    );
    assert_eq!(asked.times.load(Ordering::SeqCst), 2);
    let opens = asked.opens.lock().unwrap().clone();
    assert_eq!(
        opens.first().map(|(workspace, account, material)| (
            workspace.as_str(),
            account.as_str(),
            material.as_str()
        )),
        Some((
            placed.workspace.as_str(),
            "acct-1",
            "{\"record\":\"atlas\"}"
        )),
        "the service was asked for the caller's workspace, with the material word for word"
    );
    stop(placed).await;
}

/// The Browser sessions a server's daemon hands its host are its environments: a session the
/// host starts through them opens in the workspace's machine on the device that takes its
/// Linux turn, billed to the account the server pays with.
#[tokio::test(flavor = "multi_thread")]
async fn the_browser_a_daemon_hands_its_host_starts_on_the_device_running_the_machine() {
    let service = tempfile::tempdir().unwrap();
    let (socket, asked) = hosted_service(service.path());
    let placed = placed_with(1, Some(socket)).await;
    // The workspace's VM, whose daemon the machine's service names once it started it.
    let machine = machine(&placed).await;
    *asked.device.lock().unwrap() = machine.device;
    let sessions = placed
        .shells
        .sessions()
        .expect("a server that pays for machines hands its host Browser sessions");
    assert_eq!(sessions.account, "acct-1");

    // The host serves a ticket for the placement, where it is reached: the VM's daemon logs in
    // with it and makes its own key.
    let host = *placed.hosted.router.endpoint().addr().id.as_bytes();
    let material = tilo_server_protocol::hosted::Material {
        workspace: placed.workspace.clone(),
        workspace_id: tilo_server::WorkspaceId::parse(&placed.workspace).unwrap(),
        record: "atlas".into(),
        ticket: tilo_device_identity::login::Ticket::new(tilo_device_identity::login::Reached {
            endpoint: host,
            relay: None,
            direct: Vec::new(),
        }),
        user_public_key: [7; 32],
        account: "acct-1".into(),
    };
    let window = tokio::task::block_in_place(|| sessions.sessions.start(&material))
        .expect("the device running the machine opened it");
    assert_eq!(
        window.provider, "42",
        "the window names the provider page actions go to"
    );
    assert_eq!(
        *machine.sessions.launched.lock().unwrap(),
        [Launch {
            workspace: placed.workspace.clone(),
            holds: Holds::Browser,
            record: Some("atlas".into()),
            app: None,
        }],
        "the VM's daemon was asked to launch Browser on the record, in the workspace"
    );
    let opens = asked.opens.lock().unwrap().clone();
    let [(workspace, account, handed)] = opens.as_slice() else {
        panic!("the machine's service is asked once: {opens:?}");
    };
    assert_eq!(
        (workspace, account.as_str()),
        (&placed.workspace, "acct-1"),
        "for the workspace, billed to the server's account"
    );
    let handed: serde_json::Value = serde_json::from_str(handed).expect("the material is JSON");
    let ticket: tilo_device_identity::login::Ticket =
        serde_json::from_value(handed["ticket"].clone()).expect("a join ticket is there");
    assert_eq!(
        ticket, material.ticket,
        "the VM's daemon joins with the ticket the host serves, reached where the host is"
    );
    for key in ["secret", "device", "key", "server"] {
        assert!(
            handed.get(key).is_none(),
            "the VM is handed no key and no certificate: {key} in {handed}"
        );
    }
    tokio::task::block_in_place(|| sessions.sessions.stop(&placed.workspace));
    assert_eq!(
        *machine.sessions.stopped.lock().unwrap(),
        [Holds::Browser],
        "ending the workspace's Browser ends its session in the VM"
    );
    drop(sessions);
    drop(machine);
    stop(placed).await;
}
