//! Real TLS/helper and trusted sheet regressions. The independent native TLS
//! fixture proves broker authority; the pinned `OpenDrop` suite proves external
//! production receiver compatibility. Neither configures an interface.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;

use crate::shell::portal_dialog::PortalDialogController;
use crate::shell::state::{Capability, FsMode, GRANT_SESSION, WorkspaceTree};
use ui::a11y::Action;
use vello::kurbo::Rect;

struct Receiver {
    port: u16,
    pins: Vec<String>,
    requests: Arc<Mutex<Vec<String>>>,
    archives: Arc<Mutex<Vec<Vec<u8>>>>,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Receiver {
    fn start(rotate: bool, stall_ask: bool) -> Self {
        let mut pins = Vec::new();
        let configs = (0..2)
            .map(|_| {
                let material =
                    rcgen::generate_simple_self_signed(vec!["airdrop-fixture.invalid".into()])
                        .unwrap();
                pins.push(hex::encode(Sha256::digest(material.cert.der())));
                let key = rustls::pki_types::PrivatePkcs8KeyDer::from(
                    material.signing_key.serialize_der(),
                );
                Arc::new(
                    rustls::ServerConfig::builder()
                        .with_no_client_auth()
                        .with_single_cert(vec![material.cert.der().clone()], key.into())
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let archives = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let state = (
            Arc::clone(&requests),
            Arc::clone(&archives),
            Arc::clone(&connections),
            Arc::clone(&stop),
        );
        let worker = std::thread::spawn(move || {
            while !state.3.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let index = state.2.fetch_add(1, Ordering::AcqRel);
                        let config = Arc::clone(&configs[usize::from(rotate && index != 0)]);
                        receive(socket, config, &state.0, &state.1, &state.3, stall_ask);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture listener: {error}"),
                }
            }
        });
        Self {
            port,
            pins,
            requests,
            archives,
            connections,
            stop,
            worker: Some(worker),
        }
    }

    fn paths(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
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
    config: Arc<rustls::ServerConfig>,
    requests: &Mutex<Vec<String>>,
    archives: &Mutex<Vec<Vec<u8>>>,
    stop: &AtomicBool,
    stall_ask: bool,
) {
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let connection = rustls::ServerConnection::new(config).unwrap();
    let mut stream = BufReader::new(rustls::StreamOwned::new(connection, socket));
    loop {
        let mut line = String::new();
        match stream.read_line(&mut line) {
            Ok(0) | Err(_) => break, // TLS-only inspection/pin rejection closes without HTTP.
            Ok(_) => {}
        }
        let path = line.split_whitespace().nth(1).unwrap().to_owned();
        requests.lock().unwrap().push(path.clone());
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
        if path == "/Upload" {
            archives.lock().unwrap().push(body);
        }
        if stall_ask && path == "/Ask" {
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            break;
        }
        let body: &[u8] = if path == "/Discover" {
            b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ReceiverComputerName</key><string>Authorized lab</string></dict></plist>"
        } else {
            b""
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        stream.get_mut().write_all(response.as_bytes()).unwrap();
        stream.get_mut().write_all(body).unwrap();
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

struct Fixture {
    // Workers stop before these environment/settings guards are released.
    home: tilo_test_env::TestHome,
    _settings: crate::shell::test_support::InstalledSettings,
    root: tempfile::TempDir,
    path: std::path::PathBuf,
    scope: AirDropScope,
    service: NativeAirDropTransfer,
    controller: PortalDialogController,
    attended: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> Self {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        home.env().set("TILO_USER_ID", "airdrop-local-person");
        home.env().set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/airdrop-fixture",
        );
        let settings = crate::shell::test_support::install_settings(Arc::new(
            settings_store::SessionSettings::open(settings_store::SessionPaths::beside(
                home.config().join("settings.json"),
            ))
            .unwrap(),
        ));
        settings_store::update_section(
            "continuity",
            serde_json::json!({
                "enabled": true, "file_transfer": true
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("approved.txt");
        std::fs::write(&path, b"approved original bytes").unwrap();
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        workspace.grant_app(
            FILES_APP_ID,
            &Capability::Filesystem {
                path: root.path().into(),
                mode: FsMode::Read,
                scope: GRANT_SESSION,
            },
        );
        let scope = AirDropScope {
            peer_id: 41,
            channel: "local-files-channel".into(),
            workspace: workspace.room().to_string(),
            person: "airdrop-local-person".into(),
        };
        let queue = PortalDialogQueue::default();
        let attended = Arc::new(AtomicBool::new(true));
        let presence = Arc::clone(&attended);
        let service = NativeAirDropTransfer::new(
            tree.shared(),
            queue.clone(),
            Arc::new(move || presence.load(Ordering::Acquire)),
            true,
        )
        .with_process_timeout(tilo_test_env::hang_bound());
        Self {
            home,
            _settings: settings,
            root,
            path,
            scope,
            service,
            controller: PortalDialogController::new(queue),
            attended,
        }
    }

    fn start(&self, receiver: &Receiver) -> [u8; 16] {
        self.service
            .start(
                AirDropRequest {
                    scope: self.scope.clone(),
                    address: "127.0.0.1".into(),
                    port: receiver.port,
                    source_path: self.path.as_os_str().as_encoded_bytes().to_vec(),
                },
                SecureNearbySource::open_beneath(self.root.path(), &self.path).unwrap(),
            )
            .unwrap()
    }

    fn sheet(&mut self) -> String {
        wait(|| self.controller.pump() || self.controller.view().is_some());
        let view = self.controller.view().unwrap();
        assert!(self.controller.text_field().unwrap().value().is_empty());
        assert!(view.detail().contains("approved.txt"));
        let layout = self
            .controller
            .layout(Rect::new(0.0, 0.0, 1920.0, 1080.0))
            .unwrap();
        assert!(layout.launcher_name.unwrap().y0 > layout.sheet.y0 + 118.0);
        view.handle().into()
    }

    fn enter(&mut self, handle: &str, pin: &str) {
        assert!(self.controller.perform_accessibility_action(
            &format!("portal-sheet:name#{handle}"),
            &Action::SetValue(pin.into())
        ));
    }

    fn approve(&mut self, handle: &str) -> bool {
        self.controller.perform_accessibility_action(
            &format!("portal-sheet:confirm#{handle}"),
            &Action::Activate,
        )
    }

    fn phase(&self, id: [u8; 16]) -> Option<NearbyTransferPhase> {
        self.service
            .statuses(self.scope.peer_id, &self.scope.channel)
            .iter()
            .find(|status| status.id == id)
            .map(|status| status.phase)
    }

    fn settled(&self, id: [u8; 16], phase: NearbyTransferPhase) {
        wait(|| self.phase(id) == Some(phase) && !self.service.active.load(Ordering::Acquire));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.disconnect(self.scope.peer_id);
        wait(|| !self.service.active.load(Ordering::Acquire));
    }
}

fn wait(condition: impl FnMut() -> bool) {
    tilo_test_env::wait_until("the observable production operation", condition);
}

fn extracted(archive: &[u8]) -> Vec<u8> {
    // Independent gzip decoder; inspect the standard newc header, not the
    // sender's encoder. The separate OpenDrop oracle performs real extraction.
    let mut child = Command::new("/usr/bin/python3")
        .args([
            "-I",
            "-c",
            "import gzip,sys;sys.stdout.buffer.write(gzip.decompress(sys.stdin.buffer.read()))",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(archive).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let bytes = output.stdout;
    assert_eq!(&bytes[..6], b"070701");
    let size = usize::from_str_radix(std::str::from_utf8(&bytes[54..62]).unwrap(), 16).unwrap();
    let name_size =
        usize::from_str_radix(std::str::from_utf8(&bytes[94..102]).unwrap(), 16).unwrap();
    let name_end = 110_usize.checked_add(name_size).unwrap();
    assert_eq!(&bytes[110..name_end], b"./approved.txt\0");
    let payload = name_end.checked_add(3).unwrap() & !3;
    bytes[payload..payload.checked_add(size).unwrap()].to_vec()
}

#[test]
fn verified_airdrop_requires_independent_pin_and_sends_exact_staged_bytes() {
    let mut fixture = Fixture::new();
    let receiver = Receiver::start(false, false);
    let id = fixture.start(&receiver);
    let handle = fixture.sheet();
    assert!(
        !fixture
            .controller
            .view()
            .unwrap()
            .detail()
            .contains(&receiver.pins[0])
    );
    assert!(receiver.connections.load(Ordering::Acquire) > 0);
    assert!(receiver.paths().is_empty(), "inspection sends no HTTP");
    fixture.enter(&handle, &"00".repeat(32));
    assert!(!fixture.approve(&handle));
    assert!(fixture.controller.entry_error().is_some());
    assert!(fixture.controller.view().is_some());
    let access = fixture
        .controller
        .accessibility(Rect::new(0.0, 0.0, 1920.0, 1080.0))
        .unwrap();
    assert!(
        access
            .children
            .iter()
            .any(|node| node.state.required && node.state.invalid)
    );
    assert!(
        access
            .children
            .iter()
            .any(|node| node.live == Some(ui::a11y::Politeness::Assertive))
    );
    assert!(fixture.controller.state_json()["error"].is_string());
    assert!(
        fixture
            .controller
            .layout(Rect::new(0.0, 0.0, 1920.0, 200.0))
            .is_none(),
        "clipped security review cannot be painted as approvable"
    );
    assert!(
        fixture
            .controller
            .layout(Rect::new(0.0, 0.0, 300.0, 1080.0))
            .is_none()
    );
    assert!(
        receiver.paths().is_empty(),
        "wrong independent pin cannot approve even Discover"
    );
    assert!(
        fixture
            .service
            .statuses(42, &fixture.scope.channel)
            .is_empty()
    );
    assert!(
        fixture
            .service
            .statuses(41, "another-workspace-channel")
            .is_empty()
    );

    std::fs::write(&fixture.path, b"later unapproved edits").unwrap();
    fixture.enter(&handle, &receiver.pins[0]);
    assert!(fixture.approve(&handle));
    assert!(
        !fixture.controller.perform_accessibility_action(
            &format!("portal-sheet:confirm#{handle}"),
            &Action::Activate
        ),
        "approval is consumed once"
    );
    fixture.settled(id, NearbyTransferPhase::Completed);
    assert_eq!(receiver.paths(), ["/Discover", "/Ask", "/Upload"]);
    assert_eq!(
        extracted(&receiver.archives.lock().unwrap()[0]),
        b"approved original bytes"
    );
}

#[test]
fn verified_airdrop_rejects_certificate_rotation_and_requires_fresh_verification() {
    let mut fixture = Fixture::new();
    let receiver = Receiver::start(true, false);
    let id = fixture.start(&receiver);
    let handle = fixture.sheet();
    fixture.enter(&handle, &receiver.pins[0]);
    assert!(fixture.approve(&handle));
    fixture.settled(id, NearbyTransferPhase::Failed);
    assert!(
        receiver.paths().is_empty(),
        "changed leaf fails before HTTP"
    );
    let next = fixture.start(&receiver);
    let handle = fixture.sheet();
    fixture.enter(&handle, &receiver.pins[0]);
    assert!(!fixture.approve(&handle));
    assert!(receiver.paths().is_empty());
    fixture.enter(&handle, &receiver.pins[1]);
    assert!(fixture.approve(&handle));
    fixture.settled(next, NearbyTransferPhase::Completed);
    assert_eq!(receiver.paths(), ["/Discover", "/Ask", "/Upload"]);
}

#[test]
fn verified_airdrop_decline_lock_policy_and_source_revocation_send_nothing() {
    for decision in 0..5 {
        let mut fixture = Fixture::new();
        let receiver = Receiver::start(false, false);
        let id = fixture.start(&receiver);
        let handle = fixture.sheet();
        match decision {
            0 => {
                assert!(fixture.controller.cancel());
            }
            1 => fixture.attended.store(false, Ordering::Release),
            2 => {
                settings_store::update_section(
                    "continuity",
                    serde_json::json!({
                        "enabled": true, "file_transfer": false
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                )
                .unwrap();
            }
            3 => {
                fixture.service.membership.write().desktop.workspaces[0]
                    .sandbox
                    .clear();
            }
            4 => {
                std::fs::rename(&fixture.path, fixture.root.path().join("old.txt")).unwrap();
                std::fs::write(&fixture.path, b"substituted inode").unwrap();
            }
            _ => unreachable!(),
        }
        fixture.settled(id, NearbyTransferPhase::Cancelled);
        fixture.controller.pump();
        assert!(fixture.controller.view().is_none());
        assert!(!fixture.approve(&handle));
        assert!(receiver.paths().is_empty());
    }
}

#[test]
fn verified_airdrop_disconnect_cancels_scope_and_running_helper_is_stoppable() {
    let mut fixture = Fixture::new();
    let receiver = Receiver::start(false, true);
    let id = fixture.start(&receiver);
    let handle = fixture.sheet();
    fixture.enter(&handle, &receiver.pins[0]);
    assert!(fixture.approve(&handle));
    wait(|| receiver.paths().contains(&"/Ask".into()));
    assert!(fixture.service.cancel(id));
    fixture.settled(id, NearbyTransferPhase::Cancelled);
    assert_eq!(receiver.paths(), ["/Discover", "/Ask"]);
    assert!(receiver.archives.lock().unwrap().is_empty());
    drop(receiver);
    let receiver = Receiver::start(false, false);
    fixture.start(&receiver);
    let handle = fixture.sheet();
    fixture.service.disconnect(fixture.scope.peer_id);
    wait(|| !fixture.service.active.load(Ordering::Acquire));
    fixture.controller.pump();
    assert!(fixture.controller.view().is_none());
    assert!(
        fixture
            .service
            .statuses(fixture.scope.peer_id, &fixture.scope.channel)
            .is_empty()
    );
    assert!(!fixture.approve(&handle));
    assert!(receiver.paths().is_empty());
}

#[test]
fn verified_airdrop_requires_local_person_and_lab_admission() {
    let mut fixture = Fixture::new();
    let receiver = Receiver::start(false, false);
    fixture.home.env().set("TILO_AIRDROP_LAB", "0");
    let default_service = NativeAirDropTransfer::session(fixture.service.membership.clone());
    assert!(!default_service.available_to(&fixture.scope.workspace, Some(&fixture.scope.person)));
    assert!(
        !fixture
            .service
            .available_to(&fixture.scope.workspace, Some("other-person"))
    );
    assert!(!fixture.service.available_to(&fixture.scope.workspace, None));
    assert!(
        !fixture
            .service
            .available_to("not-a-room", Some(&fixture.scope.person))
    );
    let mut scope = fixture.scope.clone();
    scope.person = "other-person".into();
    assert!(
        fixture
            .service
            .start(
                AirDropRequest {
                    scope,
                    address: "127.0.0.1".into(),
                    port: receiver.port,
                    source_path: fixture.path.as_os_str().as_encoded_bytes().to_vec(),
                },
                SecureNearbySource::open_beneath(fixture.root.path(), &fixture.path).unwrap()
            )
            .is_err()
    );
    assert_eq!(receiver.connections.load(Ordering::Acquire), 0);
}

#[test]
fn verified_airdrop_staging_keeps_the_approved_inode_alive_until_attempt_end() {
    let fixture = Fixture::new();
    let source = SecureNearbySource::open_beneath(fixture.root.path(), &fixture.path).unwrap();
    let inode = source.metadata().unwrap().ino();
    let descriptor = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| std::fs::read_link(path).is_ok_and(|target| target == fixture.path))
        .expect("approved source descriptor");
    let staged = StagedFile::new(source, &|| true, tilo_test_env::hang_bound()).unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    std::fs::write(&fixture.path, b"replacement").unwrap();
    let retained =
        std::fs::metadata(&descriptor).expect("source stays open after staging and unlink");
    assert_eq!(retained.ino(), inode);
    assert_eq!(retained.nlink(), 0);
    assert_ne!(std::fs::metadata(&fixture.path).unwrap().ino(), inode);
    drop(staged);
    assert!(
        std::fs::metadata(descriptor).is_err(),
        "source authority is closed with the attempt"
    );
}

#[test]
fn verified_airdrop_helper_receives_only_scoped_descriptors_and_no_ambient_credentials() {
    let mut fixture = Fixture::new();
    fixture
        .home
        .env()
        .set("TILO_AIRDROP_TEST_SECRET", "fictional-credential");
    let source = SecureNearbySource::open_beneath(fixture.root.path(), &fixture.path).unwrap();
    let staged = StagedFile::new(source, &|| true, tilo_test_env::hang_bound()).unwrap();
    let private = File::open(&fixture.path).unwrap();
    // SAFETY: fcntl duplicates our fixture file into a fresh owned descriptor.
    let private_fd = unsafe { libc::fcntl(private.as_raw_fd(), libc::F_DUPFD, 64) };
    assert!(private_fd >= 64);
    // SAFETY: this freshly duplicated descriptor belongs exclusively here.
    let private = unsafe { File::from_raw_fd(private_fd) };
    // Deliberately make a separate fixture descriptor inheritable so the
    // production close_range boundary, rather than File's default, closes it.
    assert_eq!(unsafe { libc::fcntl(private_fd, libc::F_SETFD, 0) }, 0);
    let script = format!(
        r#"
import fcntl,json,os
try:
    os.fstat({private_fd})
    private_open = True
except OSError:
    private_open = False
print(json.dumps({{"ambient": "TILO_AIRDROP_TEST_SECRET" in os.environ,
                  "private_open": private_open,
                  "mode": fcntl.fcntl(9, fcntl.F_GETFL) & os.O_ACCMODE,
                  "seals": fcntl.fcntl(9, fcntl.F_GET_SEALS)}}))
"#
    );
    let mut command = command_base(&script);
    inherit_descriptors(&mut command, Some(staged.file.as_raw_fd()), None);
    let reply = supervise(command, &|| true, tilo_test_env::hang_bound()).unwrap();
    assert!(private.metadata().is_ok());
    assert_eq!(reply["ambient"], false);
    assert_eq!(reply["private_open"], false);
    assert_eq!(reply["mode"], libc::O_RDONLY);
    assert_eq!(
        reply["seals"],
        libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
    );
    assert!(staged.file.metadata().unwrap().is_file());
}
