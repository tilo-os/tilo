//! A workspace's Browser opens in the workspace's machine, on the device that
//! takes its Linux turn: the server asks the turn of the workspace's devices
//! that run Linux, and the one that takes it asks the hosted-VM service beside
//! it to start the session, handing it what the session joins the workspace
//! with.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, the VM host and their routers stay up until the test ends"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use linux_vm::hosted::control::{self, OpenBrowser, Reply, Request};
use tilo_actions::remote::browser::{self, OpenedBrowser};
use tilo_server::linux_devices::Follow;

use super::linux_turn::{placed_with, stop};

/// What a hosted-VM service was asked to open, and how often.
#[derive(Default)]
struct Opened {
    opens: std::sync::Mutex<Vec<(String, String, String)>>,
    times: AtomicUsize,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_opens_browser_on_the_device_running_its_machine() {
    let service = tempfile::tempdir().unwrap();
    let (socket, asked) = hosted_service(service.path());
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
        opened.provider, "42",
        "and the window it keeps names the provider page actions go to"
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
