//! The resource against a recording engine: each verb reaches the engine as
//! the certified Files route in the workspace in front, and each row it acts
//! on is one the reading listed.

use super::*;

use runtime_collab::{
    NearbyDestinationHandle, NearbyTransferDestination, NearbyTransferSnapshot,
    NearbyTransferStatus, NearbyTransferTarget,
};

/// The device's one destination and one target, and the transfer handles it
/// issues, fixed so a row's handle is one a test can name.
const TARGET: [u8; 16] = [0x11; 16];
const DESTINATION: [u8; 16] = [0x22; 16];
const TRANSFER: [u8; 16] = [0x33; 16];

#[derive(Default)]
struct Fake {
    calls: Vec<(NearbyTransferRequestContext, NearbyTransferCommand)>,
    phase: Option<NearbyTransferPhase>,
    source: Vec<u8>,
    airdrop: bool,
}

impl Fake {
    fn calls(&self) -> Vec<&NearbyTransferCommand> {
        self.calls.iter().map(|(_, command)| command).collect()
    }
}

struct Recording {
    fake: Mutex<Fake>,
}

impl NearbyTransferServices for Recording {
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: NearbyTransferCommand,
    ) -> Result<NearbyTransferReply, String> {
        let mut fake = self.fake.lock_recover();
        fake.calls.push((context.clone(), command.clone()));
        let reply = match command {
            NearbyTransferCommand::Snapshot => {
                let transfers = fake.phase.map_or_else(Vec::new, |phase| {
                    vec![NearbyTransferStatus {
                        handle: NearbyTransferHandle::from_issued_bytes(TRANSFER),
                        name: "notes.txt".to_owned(),
                        device_label: "Ada's laptop".to_owned(),
                        direction: NearbyTransferDirection::Sending,
                        phase,
                        transferred_bytes: 512,
                        total_bytes: 1024,
                    }]
                });
                NearbyTransferReply::Snapshot {
                    snapshot: NearbyTransferSnapshot {
                        targets: vec![NearbyTransferTarget {
                            handle: NearbyTargetHandle::from_issued_bytes(TARGET),
                            label: "Ada's laptop".to_owned(),
                            online: true,
                        }],
                        destinations: vec![NearbyTransferDestination {
                            handle: NearbyDestinationHandle::from_issued_bytes(DESTINATION),
                            label: "Documents".to_owned(),
                            remembered_for_session: false,
                        }],
                        transfers,
                    },
                }
            }
            NearbyTransferCommand::AirDropAvailability => {
                NearbyTransferReply::AirDropAvailability {
                    available: fake.airdrop,
                }
            }
            NearbyTransferCommand::Send { source_path, .. }
            | NearbyTransferCommand::SendAirDrop { source_path, .. } => {
                fake.phase = Some(NearbyTransferPhase::AwaitingConsent);
                fake.source = source_path;
                NearbyTransferReply::Queued {
                    handle: NearbyTransferHandle::from_issued_bytes(TRANSFER),
                }
            }
            NearbyTransferCommand::Decide { accept, .. } => {
                fake.phase = Some(if accept {
                    NearbyTransferPhase::Completed
                } else {
                    NearbyTransferPhase::Cancelled
                });
                NearbyTransferReply::Accepted
            }
            NearbyTransferCommand::Cancel { .. } => {
                fake.phase = Some(NearbyTransferPhase::Cancelled);
                NearbyTransferReply::Accepted
            }
            NearbyTransferCommand::Retry { .. } => {
                fake.phase = Some(NearbyTransferPhase::Transferring);
                NearbyTransferReply::Accepted
            }
        };
        drop(fake);
        Ok(reply)
    }

    fn disconnect(&self, _peer_id: u64) {}
}

/// A nearby send publishing `workspace`, sharing through `shared`.
fn nearby(fake: &Arc<Recording>, workspace: &str, shared: Arc<Mutex<Vec<String>>>) -> NearbySend {
    NearbySend::new(
        Arc::clone(fake) as Arc<dyn NearbyTransferServices>,
        "Ada",
        {
            let workspace = workspace.to_owned();
            move || workspace.clone()
        },
        move |resource| {
            shared.lock_recover().push(resource.to_owned());
            Ok(())
        },
    )
}

fn recording(airdrop: bool) -> Arc<Recording> {
    Arc::new(Recording {
        fake: Mutex::new(Fake {
            airdrop,
            ..Fake::default()
        }),
    })
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("{other} is not an arguments object"),
    }
}

#[test]
fn the_reading_lists_the_devices_in_reach_the_folders_here_and_what_moved() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let nearby = nearby(&fake, "workspace-a", Arc::clone(&shared));

    let reading = nearby.read().expect("the engine shows what it holds");
    assert_eq!(reading.devices.len(), 1);
    assert_eq!(reading.devices[0].name, "Ada's laptop");
    assert_eq!(reading.devices[0].kind, DeviceKind::Device);
    assert!(reading.devices[0].reachable);
    assert_eq!(reading.devices[0].handle, hex(&TARGET));
    assert_eq!(reading.destinations.len(), 1);
    assert_eq!(reading.destinations[0].name, "Documents");
    assert!(reading.transfers.is_empty());

    // Every call arrives as the certified Files route, in the workspace in
    // front.
    let fake = fake.fake.lock_recover();
    let (context, _) = &fake.calls[0];
    assert_eq!(context.app_id(), FILES_APP_ID);
    assert_eq!(context.workspace_id(), "workspace-a");
    assert_eq!(context.person(), Some("Ada"));
    drop(fake);
}

#[test]
fn a_send_then_an_accept_hands_the_file_over_and_the_row_says_so() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);

    let reading = nearby.read().expect("a reading");
    let device = reading.devices[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/Documents/notes.txt"],
                "device": device,
            })),
        )
        .expect("the send is offered");
    {
        let fake = fake.fake.lock_recover();
        let sent = fake
            .calls()
            .into_iter()
            .find_map(|command| match command {
                NearbyTransferCommand::Send {
                    target,
                    source_path,
                } => Some((*target, source_path.clone())),
                _ => None,
            })
            .expect("the engine is asked to send");
        assert_eq!(sent.0, NearbyTargetHandle::from_issued_bytes(TARGET));
        assert_eq!(sent.1, b"/home/ada/Documents/notes.txt");
        drop(fake);
    }

    let reading = nearby.read().expect("a reading");
    assert_eq!(reading.transfers.len(), 1);
    let row = &reading.transfers[0];
    assert_eq!(row.state, TransferState::Offered);
    assert_eq!(row.name, "notes.txt");
    assert_eq!(row.progress, 50);

    let destination = reading.destinations[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "accept",
            &arguments(serde_json::json!({
                "transfer": row.handle,
                "destination": destination,
            })),
        )
        .expect("the offer is kept");
    {
        let fake = fake.fake.lock_recover();
        let decided = fake
            .calls()
            .into_iter()
            .find_map(|command| match command {
                NearbyTransferCommand::Decide {
                    handle,
                    accept,
                    destination,
                    ..
                } => Some((*handle, *accept, *destination)),
                _ => None,
            })
            .expect("the engine is asked to decide");
        assert_eq!(decided.0, NearbyTransferHandle::from_issued_bytes(TRANSFER));
        assert!(decided.1);
        assert_eq!(
            decided.2,
            Some(NearbyDestinationHandle::from_issued_bytes(DESTINATION))
        );
        drop(fake);
    }
    let reading = nearby.read().expect("a reading");
    assert_eq!(reading.transfers[0].state, TransferState::Done);
    assert_eq!(reading.transfers[0].destination, "Documents");
}

#[test]
fn an_accept_without_a_folder_takes_the_one_the_engine_remembers() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let row = {
        let reading = nearby.read().expect("a reading");
        let device = reading.devices[0].handle.clone();
        nearby
            .invoke(
                RESOURCE,
                "send",
                &arguments(serde_json::json!({
                    "items": ["/home/ada/notes.txt"],
                    "device": device,
                })),
            )
            .expect("a send");
        nearby.read().expect("a reading").transfers[0]
            .handle
            .clone()
    };
    nearby
        .invoke(
            RESOURCE,
            "accept",
            &arguments(serde_json::json!({ "transfer": row })),
        )
        .expect("the remembered folder takes it");
    let fake = fake.fake.lock_recover();
    assert!(fake.calls().into_iter().any(|command| matches!(
        command,
        NearbyTransferCommand::Decide {
            accept: true,
            destination: Some(_),
            ..
        }
    )));
    drop(fake);
}

#[test]
fn a_decline_turns_the_offer_down() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let device = nearby.read().expect("a reading").devices[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": device,
            })),
        )
        .expect("a send");
    let row = nearby.read().expect("a reading").transfers[0]
        .handle
        .clone();
    nearby
        .invoke(
            RESOURCE,
            "decline",
            &arguments(serde_json::json!({ "transfer": row })),
        )
        .expect("the offer is turned down");
    {
        let calls = fake.fake.lock_recover();
        assert!(calls.calls().into_iter().any(|command| matches!(
            command,
            NearbyTransferCommand::Decide {
                accept: false,
                destination: None,
                ..
            }
        )));
        drop(calls);
    }
    assert_eq!(
        nearby.read().expect("a reading").transfers[0].state,
        TransferState::Declined
    );
}

#[test]
fn a_cancel_stops_a_transfer_and_a_retry_offers_it_again() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let device = nearby.read().expect("a reading").devices[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": device,
            })),
        )
        .expect("a send");
    let row = nearby.read().expect("a reading").transfers[0]
        .handle
        .clone();
    nearby
        .invoke(
            RESOURCE,
            "cancel",
            &arguments(serde_json::json!({ "transfer": row.as_str() })),
        )
        .expect("the transfer stops");
    assert_eq!(
        nearby.read().expect("a reading").transfers[0].state,
        TransferState::Declined
    );
    nearby
        .invoke(
            RESOURCE,
            "retry-transfer",
            &arguments(serde_json::json!({ "transfer": row })),
        )
        .expect("the transfer is offered again");
    assert_eq!(
        nearby.read().expect("a reading").transfers[0].state,
        TransferState::Sending
    );
    let fake = fake.fake.lock_recover();
    assert!(fake.calls().into_iter().any(|command| matches!(
        command,
        NearbyTransferCommand::Cancel { .. } | NearbyTransferCommand::Retry { .. }
    )));
    drop(fake);
}

#[test]
fn the_lab_receiver_is_a_device_kind_and_a_send_to_it_names_its_address() {
    let fake = recording(true);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared).with_lab_receiver(LabReceiver {
        name: "Lab receiver".to_owned(),
        address: "10.0.0.9".to_owned(),
        port: 4443,
    });
    let reading = nearby.read().expect("a reading");
    let lab = reading
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::LabReceiver)
        .expect("the lab receiver is listed");
    assert_eq!(lab.name, "Lab receiver");
    assert!(lab.reachable);
    assert_eq!(lab.address, "10.0.0.9");
    assert_eq!(lab.port, 4443);
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": lab.handle,
            })),
        )
        .expect("the lab receiver takes it");
    let fake = fake.fake.lock_recover();
    let handed = fake
        .calls()
        .into_iter()
        .find_map(|command| match command {
            NearbyTransferCommand::SendAirDrop {
                source_path,
                address,
                port,
            } => Some((source_path.clone(), address.clone(), *port)),
            _ => None,
        })
        .expect("the engine is asked to hand it to the address");
    assert_eq!(handed.0, b"/home/ada/notes.txt");
    assert_eq!(handed.1, "10.0.0.9");
    assert_eq!(handed.2, 4443);
    drop(fake);
}

#[test]
fn a_lab_receiver_that_is_not_here_refuses_the_send() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared).with_lab_receiver(LabReceiver {
        name: "Lab receiver".to_owned(),
        address: "10.0.0.9".to_owned(),
        port: 4443,
    });
    let reading = nearby.read().expect("a reading");
    let lab = reading
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::LabReceiver)
        .expect("the lab receiver is listed");
    assert!(!lab.reachable);
    let refused = nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": lab.handle,
            })),
        )
        .expect_err("an absent receiver takes nothing");
    assert!(
        matches!(refused, ApiError::Refused { .. }),
        "{refused:?} is not a refusal"
    );
}

#[test]
fn share_opens_the_invitation_sheet_for_the_resource_it_names() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", Arc::clone(&shared));
    nearby
        .invoke(
            RESOURCE,
            "share",
            &arguments(serde_json::json!({ "resource": "notes:note:42" })),
        )
        .expect("the sheet opens");
    assert_eq!(
        *shared.lock_recover(),
        ["notes:note:42".to_owned()],
        "the sheet names the resource it shares"
    );
}

#[test]
fn a_handle_the_reading_did_not_list_is_refused_and_never_reaches_the_engine() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let before = fake.fake.lock_recover().calls.len();
    let refused = nearby
        .invoke(
            RESOURCE,
            "cancel",
            &arguments(serde_json::json!({ "transfer": "not-a-handle" })),
        )
        .expect_err("a handle no reading listed names nothing");
    assert!(
        matches!(refused, ApiError::InvalidArguments(_)),
        "{refused:?}"
    );
    assert_eq!(fake.fake.lock_recover().calls.len(), before);
}

#[test]
fn a_device_no_reading_listed_is_refused() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let refused = nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": hex(&[0x44; 16]),
            })),
        )
        .expect_err("a device that was never in reach takes nothing");
    assert!(matches!(refused, ApiError::Refused { .. }), "{refused:?}");
}

/// A file handed to a receiver that is not one of the person's devices, verified through the
/// question it puts to the person: a real helper run against a receiver in this process.
#[cfg(target_os = "linux")]
mod a_receiver_that_is_not_one_of_yours {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "a test fixture: a failed step is the assertion"
    )]

    use std::io::{BufRead, BufReader, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use sha2::{Digest as _, Sha256};
    use tilo_service::{AskReceiver, Question};
    use tilo_test_env::SpawnUnderEnvLock as _;

    use super::*;

    /// What the file is called on both ends.
    const NAME: &str = "contract.txt";
    const DEADLINE: Duration = Duration::from_secs(60);
    const PASS: Duration = Duration::from_millis(10);

    /// Whether the receiver takes what it is offered or refuses it.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Accept,
        Refuse,
    }

    /// An `AirDrop` receiver of another person, in this process: an HTTPS endpoint that answers
    /// discovery, takes or refuses the ask, and records what it is posted.
    struct Receiver {
        port: u16,
        /// The leaf certificate digest it serves, as its trusted console would print it.
        pin: String,
        requests: Arc<Mutex<Vec<String>>>,
        uploads: Arc<Mutex<Vec<Vec<u8>>>>,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Receiver {
        fn start(answer: Answer) -> Self {
            let material =
                rcgen::generate_simple_self_signed(vec!["airdrop-fixture.invalid".to_owned()])
                    .unwrap();
            let cert = material.cert.der().clone();
            let pin = hex::encode(Sha256::digest(&cert));
            let key =
                rustls::pki_types::PrivatePkcs8KeyDer::from(material.signing_key.serialize_der());
            let config = Arc::new(
                rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key.into())
                .unwrap(),
            );
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let uploads = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let state = (
                Arc::clone(&requests),
                Arc::clone(&uploads),
                Arc::clone(&stop),
            );
            let worker = std::thread::spawn(move || {
                while !state.2.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((socket, _)) => receive(socket, &config, &state.0, &state.1, answer),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture listener: {error}"),
                    }
                }
            });
            Self {
                port,
                pin,
                requests,
                uploads,
                stop,
                worker: Some(worker),
            }
        }

        fn paths(&self) -> Vec<String> {
            self.requests.lock_recover().clone()
        }

        fn uploads(&self) -> Vec<Vec<u8>> {
            self.uploads.lock_recover().clone()
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn receive(
        socket: TcpStream,
        config: &rustls::ServerConfig,
        requests: &Mutex<Vec<String>>,
        uploads: &Mutex<Vec<Vec<u8>>>,
        answer: Answer,
    ) {
        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
        let connection = rustls::ServerConnection::new(Arc::new(config.clone())).unwrap();
        let mut stream = BufReader::new(rustls::StreamOwned::new(connection, socket));
        loop {
            let mut line = String::new();
            match stream.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            requests.lock_recover().push(path.clone());
            let mut length = 0;
            let mut chunked = false;
            loop {
                line.clear();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if line
                    .to_ascii_lowercase()
                    .contains("transfer-encoding: chunked")
                {
                    chunked = true;
                }
            }
            let body = if chunked {
                chunks(&mut stream)
            } else {
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                body
            };
            let (status, reason, reply): (u16, &str, &[u8]) = match path.as_str() {
                "/Ask" if answer == Answer::Refuse => (403, "Forbidden", b""),
                "/Upload" => {
                    uploads.lock_recover().push(body);
                    (200, "OK", b"")
                }
                "/Discover" => (
                    200,
                    "OK",
                    b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ReceiverComputerName</key><string>Somebody else</string></dict></plist>",
                ),
                _ => (200, "OK", b""),
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                reply.len()
            );
            stream.get_mut().write_all(response.as_bytes()).unwrap();
            stream.get_mut().write_all(reply).unwrap();
            stream.get_mut().flush().unwrap();
        }
    }

    fn chunks(stream: &mut impl BufRead) -> Vec<u8> {
        let mut body = Vec::new();
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            let length = usize::from_str_radix(line.trim(), 16).unwrap();
            if length == 0 {
                stream.read_line(&mut String::new()).unwrap();
                return body;
            }
            let mut chunk = vec![0; length];
            stream.read_exact(&mut chunk).unwrap();
            body.extend(chunk);
            let mut ending = [0; 2];
            stream.read_exact(&mut ending).unwrap();
            assert_eq!(&ending, b"\r\n");
        }
    }

    /// A port nothing serves: the receiver the person named is not there.
    fn a_port_nobody_serves() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// The receivers, under a Files grant over `folder`, asking through the questions returned.
    fn receivers(folder: &Path) -> (AirDrops, AskReceiver) {
        let grant = folder.to_path_buf();
        let air_drop = AirDrops::new(Host {
            enabled: true,
            open: Box::new(move |_workspace, path| {
                if !path.starts_with(&grant) {
                    return Err("not beneath the Files grant".to_owned());
                }
                let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                Ok((file, name))
            }),
            permitted: Box::new(|| true),
            changed: Box::new(|| {}),
            language: Box::new(|| None),
        });
        let (asks, questions) = tilo_service::asks::channel();
        air_drop.ask_with(asks);
        (air_drop, questions)
    }

    /// Run `done` until it holds, or the deadline passes: whether it held.
    fn until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(PASS);
        }
    }

    /// How the attempt stands, as the transfer list shows it.
    fn phase(air_drop: &AirDrops) -> Option<NearbyTransferPhase> {
        air_drop
            .attempts()
            .into_iter()
            .find(|attempt| attempt.name == NAME)
            .map(|attempt| attempt.phase)
    }

    /// The receiver's verification, once its certificate is in.
    fn asked(questions: &AskReceiver) -> (Question, VerifyQuestion) {
        let mut asked = None;
        assert!(
            until(|| {
                asked = questions.pending().into_iter().next();
                asked.is_some()
            }),
            "the receiver's verification is asked"
        );
        let asked = asked.unwrap();
        assert_eq!(asked.kind, VERIFY_QUESTION);
        let details = serde_json::from_value(asked.details.clone()).unwrap();
        (asked, details)
    }

    /// Hand `file` to the receiver on `port`.
    fn send(air_drop: &AirDrops, file: &Path, port: u16) {
        air_drop
            .start(
                None,
                "workspace-a",
                file.as_os_str().as_encoded_bytes(),
                "127.0.0.1",
                port,
            )
            .expect("the file is handed over");
    }

    /// Answer `asked` with the fingerprint the person typed.
    fn verify(questions: &AskReceiver, asked: &Question, fingerprint: &str) {
        let answer = tilo_service::Answer {
            values: vec![zeroize::Zeroizing::new(fingerprint.to_owned())],
            ..tilo_service::Answer::default()
        };
        assert!(
            questions.answer(&asked.handle, answer),
            "the question was up"
        );
    }

    /// What the receiver was posted, decoded as the one file the sender archived.
    fn extracted(upload: &[u8]) -> (String, Vec<u8>) {
        let mut child = std::process::Command::new("/usr/bin/python3")
            .args([
                "-I",
                "-c",
                "import gzip,sys;sys.stdout.buffer.write(gzip.decompress(sys.stdin.buffer.read()))",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        child.stdin.take().unwrap().write_all(upload).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "the upload is one gzip archive");
        let bytes = output.stdout;
        assert_eq!(&bytes[..6], b"070701", "a newc cpio archive");
        let size = usize::from_str_radix(std::str::from_utf8(&bytes[54..62]).unwrap(), 16).unwrap();
        let name_size =
            usize::from_str_radix(std::str::from_utf8(&bytes[94..102]).unwrap(), 16).unwrap();
        let name_end = 110_usize.checked_add(name_size).unwrap();
        assert_eq!(&bytes[name_end - 1], &0, "the name ends its field");
        // The archive names its one entry by the path it lands at, which the sender roots at "./".
        let name = String::from_utf8(bytes[110..name_end - 1].to_vec()).unwrap();
        let name = name.strip_prefix("./").unwrap_or(&name).to_owned();
        let payload = name_end.checked_add(3).unwrap() & !3;
        (
            name,
            bytes[payload..payload.checked_add(size).unwrap()].to_vec(),
        )
    }

    /// The bytes the tests send, and the file holding them under the Files grant.
    fn the_file(folder: &Path) -> (PathBuf, Vec<u8>) {
        let bytes: Vec<u8> = (0..40_000_u32).map(|index| (index % 251) as u8).collect();
        let file = folder.join(NAME);
        std::fs::write(&file, &bytes).expect("the file Files sends");
        (file, bytes)
    }

    #[test]
    fn a_file_reaches_the_receiver_who_took_it_and_only_after_its_fingerprint() {
        assert!(
            Path::new("/usr/bin/python3").is_file(),
            "the AirDrop wire helper needs the system's own interpreter"
        );
        let folder = tempfile::tempdir().expect("the person's files");
        let (file, bytes) = the_file(folder.path());
        let receiver = Receiver::start(Answer::Accept);
        let (air_drop, questions) = receivers(folder.path());

        send(&air_drop, &file, receiver.port);
        let (question, verification) = asked(&questions);
        assert_eq!(verification.name, NAME, "the sheet names the file");
        assert_eq!(verification.bytes, 40_000, "and its size");
        assert_eq!(
            verification.endpoint,
            format!("AirDrop · 127.0.0.1:{}", receiver.port),
            "and the receiver by its endpoint"
        );

        assert_eq!(
            verification.certificate, receiver.pin,
            "what the sheet holds typing to"
        );

        // A fingerprint that is not the receiver's sends nothing, and the question stands again.
        verify(&questions, &question, &"00".repeat(32));
        assert_eq!(asked(&questions).0, question, "the attempt still waits");
        assert_eq!(phase(&air_drop), Some(NearbyTransferPhase::AwaitingConsent));
        assert!(receiver.paths().is_empty(), "nothing was sent");

        // The fingerprint its console printed sends the staged bytes once.
        verify(&questions, &question, &receiver.pin);
        assert!(questions.pending().is_empty(), "the question is answered");
        assert!(
            until(|| phase(&air_drop) == Some(NearbyTransferPhase::Completed)),
            "the attempt is done"
        );
        assert_eq!(receiver.paths(), ["/Discover", "/Ask", "/Upload"]);
        let uploads = receiver.uploads();
        assert_eq!(uploads.len(), 1, "one upload");
        assert_eq!(
            extracted(&uploads[0]),
            (NAME.to_owned(), bytes),
            "the receiver holds exactly the bytes the person approved"
        );
    }

    #[test]
    fn a_receiver_who_declines_the_ask_gets_no_bytes_and_files_says_so() {
        let folder = tempfile::tempdir().expect("the person's files");
        let (file, _bytes) = the_file(folder.path());
        let receiver = Receiver::start(Answer::Refuse);
        let (air_drop, questions) = receivers(folder.path());

        send(&air_drop, &file, receiver.port);
        let (question, _) = asked(&questions);
        verify(&questions, &question, &receiver.pin);
        assert!(
            until(|| phase(&air_drop) == Some(NearbyTransferPhase::Failed)),
            "the attempt failed"
        );
        assert_eq!(receiver.paths(), ["/Discover", "/Ask"]);
        assert!(receiver.uploads().is_empty(), "the receiver took nothing");
    }

    #[test]
    fn a_person_who_declines_the_verification_sends_nothing() {
        let folder = tempfile::tempdir().expect("the person's files");
        let (file, _bytes) = the_file(folder.path());
        let receiver = Receiver::start(Answer::Accept);
        let (air_drop, questions) = receivers(folder.path());

        send(&air_drop, &file, receiver.port);
        let (question, _) = asked(&questions);
        assert!(
            questions.decline(&question.handle),
            "the person declines it"
        );
        assert!(
            until(|| phase(&air_drop) == Some(NearbyTransferPhase::Cancelled)),
            "the attempt is cancelled"
        );
        assert!(questions.pending().is_empty());
        assert!(receiver.paths().is_empty(), "nothing was sent");
    }

    #[test]
    fn a_receiver_that_is_not_there_fails_before_any_question() {
        let folder = tempfile::tempdir().expect("the person's files");
        let (file, _bytes) = the_file(folder.path());
        let (air_drop, questions) = receivers(folder.path());

        send(&air_drop, &file, a_port_nobody_serves());
        assert!(
            until(|| {
                assert!(
                    questions.pending().is_empty(),
                    "nothing is asked about a receiver that never answered"
                );
                phase(&air_drop) == Some(NearbyTransferPhase::Failed)
            }),
            "the attempt failed"
        );
    }
}
