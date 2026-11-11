//! A workspace's Browser opens in the workspace's VM: the relay asks the placer
//! which host runs that VM, waits while none has room, then asks that host's
//! member device over the device plane to open Browser, handing it what the
//! session joins the workspace with. The member asks the hosted-VM service
//! beside it to start the session in the VM.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the member and their routers stay up until the test ends"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use legacy_vm::hosted::control::{self, OpenBrowser, Reply, Request};
use tilo_relay::host::HostMode;
use tilo_relay::placed_browsers::{OpenError, PlacedBrowsers};
use tilo_relay::placed_shells::{Member, Members, PlacedShells};
use tilo_runtime::standalone::take_work;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestUser, device_endpoint, host_on};

/// The VM host the placer names, as the operator's `StatefulSet` calls it.
const VM_HOST: &str = "tilo-vm-host-0";

/// What a hosted-VM service was asked to open, and how often.
#[derive(Default)]
struct Opened {
    opens: std::sync::Mutex<Vec<(String, String, String)>>,
    times: AtomicUsize,
}

/// The `host:port` an endpoint of this process listens on, as a relay's
/// members file names a member.
fn address_of(endpoint: &iroh::Endpoint) -> String {
    let socket = endpoint
        .bound_sockets()
        .into_iter()
        .find(std::net::SocketAddr::is_ipv4)
        .expect("the endpoint listens on IPv4");
    format!("127.0.0.1:{}", socket.port())
}

/// A hosted-VM service that opens the first browser session asked for and
/// attaches to it after, and what it was asked.
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
                        provider: "42".into(),
                    }
                }
                Request::StopBrowser { .. } => Reply::Done,
                other => panic!("Browser's service is asked about it: {other:?}"),
            };
            let _ = control::FRAMING.write_value(&mut stream, &reply);
        }
    });
    (socket, asked)
}

/// A placer with no room for the first ask, naming [`VM_HOST`] after.
fn placer() -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(Some(Request::Place { vm })) =
                control::FRAMING.read_value::<_, Request>(&mut stream)
            else {
                continue;
            };
            assert_eq!(
                (vm.environment.as_str(), vm.size),
                ("default", legacy_vm::hosted::size::VmSize::Small),
                "the relay places Browser in the workspace's VM, where its shells run"
            );
            let reply = if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::Busy(legacy_vm::hosted::service::BUSY.into())
            } else {
                Reply::Placed {
                    host: VM_HOST.into(),
                }
            };
            let _ = control::FRAMING.write_value(&mut stream, &reply);
        }
    });
    (address, asked)
}

/// A workspace whose VM host runs its VM, and the relay's placed browsers
/// asking it.
struct Placed {
    workspace: String,
    browsers: PlacedBrowsers,
    asked: Arc<Opened>,
    hosted: Hosted,
    /// The member and its profile, up as long as the arrangement is.
    _member: take_work::Member,
    _profile: tempfile::TempDir,
    _service: tempfile::TempDir,
}

async fn placed() -> Placed {
    let operator = TestUser::fresh("operator");
    let desk = operator.enrol("host", DeviceCapabilities::primary());
    let vm_host = operator.enrol(VM_HOST, DeviceCapabilities::member());
    let hosted = host_on(&operator, &desk, HostMode::Device).await;
    let workspace = hosted
        .host
        .workspaces()
        .create("Browser")
        .await
        .unwrap()
        .workspace;

    // The member serves the workspace's Browser on the
    // hosted-VM service this process runs for it.
    let service = tempfile::tempdir().unwrap();
    let (socket, asked) = hosted_service(service.path());
    let profile = tempfile::tempdir().unwrap();
    let member_endpoint = device_endpoint(&vm_host).await;
    let member_address = address_of(&member_endpoint);
    let member = take_work::Member::open(
        profile.path(),
        (vm_host.sk.clone(), vm_host.cert.clone()),
        member_endpoint,
        take_work::Relay::new(
            &hex::encode(desk.device_pubkey()),
            &address_of(hosted.router.endpoint()),
        )
        .unwrap(),
        Some(socket),
    )
    .unwrap();

    let (placer, _) = placer();
    let members = Members::from([(
        VM_HOST.to_owned(),
        Member {
            device: hex::encode(vm_host.device_pubkey()),
            address: member_address,
        },
    )]);
    let shells = Arc::new(PlacedShells::new(
        placer,
        members,
        "acct-1".into(),
        legacy_vm::hosted::size::VmSize::Small,
    ));
    shells.ask_from(
        hosted.host.workspaces().plane(),
        hosted.router.endpoint().clone(),
    );
    Placed {
        workspace,
        browsers: PlacedBrowsers::new(shells).with_patience(Duration::from_secs(60)),
        asked,
        hosted,
        _member: member,
        _profile: profile,
        _service: service,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_opens_browser_where_its_vm_host_is() {
    let placed = placed().await;
    let workspace = placed.workspace.clone();
    let opened =
        tokio::task::block_in_place(|| placed.browsers.open(&workspace, "{\"record\":\"atlas\"}"))
            .unwrap();
    assert_eq!(opened.host, VM_HOST);
    assert_eq!(opened.opened_ms, Some(900), "Browser was opened for it");
    assert_eq!(
        opened.provider, "42",
        "and the window it holds names the provider page actions go to"
    );

    // Opening again attaches to the session already running.
    let browsers = &placed.browsers;
    let again = tokio::task::block_in_place(|| browsers.open(&workspace, "{\"record\":\"atlas\"}"));
    assert_eq!(
        again.unwrap().opened_ms,
        None,
        "the running session is attached to, not opened again"
    );
    let opens = placed.asked.opens.lock().unwrap().clone();
    assert_eq!(placed.asked.times.load(Ordering::SeqCst), 2);
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
        "the member asked the service for the caller's workspace, with the material word for word"
    );

    // Ending it goes to the same host's member, and deletes what it wrote.
    tokio::task::block_in_place(|| placed.browsers.stop(&workspace)).unwrap();

    // A workspace the placer never finds room for is told so, not waited on
    // forever.
    let impatient = PlacedBrowsers::new(Arc::new(PlacedShells::new(
        "127.0.0.1:1".into(),
        Members::default(),
        "acct-1".into(),
        legacy_vm::hosted::size::VmSize::Small,
    )))
    .with_patience(Duration::from_millis(50));
    let refused = tokio::task::block_in_place(|| impatient.open(&workspace, "{}"));
    assert!(
        matches!(refused, Err(OpenError::Error(_))),
        "the placer cannot be asked: {refused:?}"
    );

    let Placed { hosted, .. } = placed;
    hosted.stop().await;
}
