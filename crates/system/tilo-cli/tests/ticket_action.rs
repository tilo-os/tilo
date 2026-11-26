//! A ticket action reaches the local API from a fresh process, even when the CLI signing the
//! browser's certificate already holds the person's root.

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Output, Stdio};
use tilo_device_identity::login::{self, Direction, Ticket};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::DeviceCert;

const WORKSPACE: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

fn start(args: &[&str], input: &[u8]) -> Child {
    let mut child = Command::new(tilo_test_env::package_binary!("tilo"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child
}

fn ask(action: &str, body: &Value) -> Child {
    start(
        &["device", "ticket-action", action],
        &serde_json::to_vec(body).unwrap(),
    )
}

fn successful(child: Child) -> String {
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn refused(output: &Output, why: &str, ticket: &Ticket) {
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains(why), "{said}");
    assert!(!said.contains(&ticket.encode()) && !said.contains(&ticket.secret_hex()));
    assert!(output.stdout.is_empty());
}

/// Read one real CLI action. A public command first checks the socket is up and closes that
/// connection without writing. Subsequent connections must carry their arguments in the body.
fn answer(
    listener: &UnixListener,
    child: &mut Child,
    action: &str,
    status: &str,
    reply: &Value,
) -> Value {
    let (stream, mut reader, request) = tilo_test_env::wait_for(action, || {
        if let Some(status) = child.try_wait().unwrap() {
            let mut said = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut said)
                .unwrap();
            panic!("CLI exited before {action} ({status}): {said}");
        }
        let (stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return None,
            Err(error) => panic!("accepting the local API call: {error}"),
        };
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        (reader.read_line(&mut request).unwrap() != 0).then_some((stream, reader, request))
    });
    assert_eq!(
        request.trim_end(),
        format!("POST /v1/workspaces/{WORKSPACE}/resources/devices/actions/{action} HTTP/1.1")
    );
    let mut length = None;
    let mut header = String::new();
    loop {
        header.clear();
        assert_ne!(reader.read_line(&mut header).unwrap(), 0);
        if header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    #[cfg(target_os = "linux")]
    {
        let pid = rustix::net::sockopt::socket_peercred(&stream)
            .unwrap()
            .pid
            .as_raw_nonzero();
        let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        let words = argv
            .strip_suffix(&[0])
            .unwrap()
            .split(|b| *b == 0)
            .skip(1)
            .collect::<Vec<_>>();
        assert_eq!(
            words,
            [b"device".as_slice(), b"ticket-action", action.as_bytes()]
        );
        let environment = std::fs::read(format!("/proc/{pid}/environ"))
            .expect("the API caller has not loaded a private key");
        let text = body["ticket"].as_str().unwrap();
        let secret = Ticket::parse(text).unwrap().secret_hex();
        for private in [text.as_bytes(), secret.as_bytes()] {
            assert!(
                !environment
                    .windows(private.len())
                    .any(|part| part == private)
            );
        }
    }
    let bytes = reply.to_string();
    write!(&stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bytes}", bytes.len()).unwrap();
    body
}

/// Keep a signed-in laptop under this scratch home and listen on the socket named by its certificate.
fn local_api(
    home: &mut tilo_test_env::TestHome,
    root: &SigningKey,
) -> (UnixListener, login::Reached) {
    let identity = home.root().join("device-identity");
    home.env()
        .set("TILO_IDENTITY_ROOT", identity)
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let dir = tilo_identity::store::account_identity_dir_from_env().unwrap();
    tilo_identity::provisioning::import_single_device_root(
        &dir,
        root,
        "Ada",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let key = SigningKey::from_bytes(&tilo_test_env::node_key());
    let public = key.verifying_key().to_bytes();
    let keys = tilo_identity::provisioning::collaboration_identity_paths();
    let place = login::Place::here(keys).unwrap();
    tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
    let tab = login::Tab {
        device_public_key: hex::encode(public),
        label: "Laptop".into(),
    };
    let mut cert = tab
        .certificate(root.verifying_key().to_bytes(), tilo_clock::unix_now_ns())
        .unwrap();
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    login::store(
        &place,
        &login::Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://eu.tilo.test".into(),
            workspace: WORKSPACE.into(),
            workspace_name: None,
            agent: "Codex".into(),
            cert: cert.clone(),
            server_cert: cert,
            server_address: None,
        },
    )
    .unwrap();
    let daemon = tilo_api::daemon::DaemonPlace::of(place.dir(), &public).unwrap();
    let socket = UnixListener::bind(daemon.api()).unwrap();
    socket.set_nonblocking(true).unwrap();
    let issuer = login::Reached {
        endpoint: public,
        relay: None,
        direct: vec![],
    };
    (socket, issuer)
}

#[test]
fn a_key_holding_cli_reports_progress_and_signs_a_returning_browser_through_its_local_api() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_key_holding_cli_reports_progress_and_signs_a_returning_browser_through_its_local_api",
        || {
            let mut home = tilo_test_env::TestHome::new().unwrap();
            let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
            let (socket, issuer) = local_api(&mut home, &root);
            let setup = Ticket::new(issuer.clone());
            let progress =
                json!({"ticket": setup.encode(), "step": "tools-added", "agent": "Codex"});
            let mut reporter = ask("reported", &progress);
            assert_eq!(
                answer(
                    &socket,
                    &mut reporter,
                    "reported",
                    "200 OK",
                    &json!({"result": {"secret": setup.secret_hex()}})
                ),
                progress
            );
            assert_eq!(successful(reporter), "{}");

            let sign_in = Ticket::in_direction(Direction::SignIn, issuer);
            let text = sign_in.encode();
            let browser = SigningKey::from_bytes(&tilo_test_env::node_key())
                .verifying_key()
                .to_bytes();
            let mut returning = start(&["device", "add", &text], &[]);
            let pending = json!({"result": {"device": hex::encode(browser), "label": "Firefox", "capability": "a browser of yours"}});
            assert_eq!(
                answer(&socket, &mut returning, "pending", "200 OK", &pending),
                json!({"ticket": text})
            );
            let approval = answer(
                &socket,
                &mut returning,
                "approve",
                "200 OK",
                &json!({"result": {"signed": true}}),
            );
            assert_eq!(approval["ticket"], text);
            assert_eq!(approval["label"], "Laptop");
            let signed: DeviceCert = serde_json::from_value(approval["cert"].clone()).unwrap();
            tilo_user_identity::verify_device_cert(&signed).unwrap();
            assert_eq!(signed.user_public_key, root.verifying_key().to_bytes());
            assert_eq!(signed.device_public_key, browser);
            assert!(successful(returning).contains("Signed the browser in"));

            let mut declined = ask("reported", &progress);
            let denied = json!({"error": {"code": "forbidden", "message": format!("Declined {} {}", setup.encode(), setup.secret_hex())}});
            assert_eq!(
                answer(&socket, &mut declined, "reported", "403 Forbidden", &denied),
                progress
            );
            refused(&declined.wait_with_output().unwrap(), "Declined", &setup);
            let wrong = ask("pending", &json!({"ticket": setup.encode()}))
                .wait_with_output()
                .unwrap();
            refused(&wrong, "another device action", &setup);
            let unknown = ask("pending", &json!({"ticket": text, "unexpected": text}))
                .wait_with_output()
                .unwrap();
            refused(&unknown, "invalid device ticket action input", &sign_in);
            let args = ["device", "ticket-action", "pending"];
            let mut malformed = serde_json::to_vec(&json!({"ticket": text})).unwrap();
            malformed.pop();
            let invalid = start(&args, &malformed).wait_with_output().unwrap();
            refused(&invalid, "invalid device ticket action input", &sign_in);
            let mut oversized = serde_json::to_vec(&json!({"ticket": text})).unwrap();
            oversized.resize(64 * 1024 + 1, b' ');
            let large = start(&args, &oversized).wait_with_output().unwrap();
            refused(&large, "device ticket action input is too large", &sign_in);
            assert_eq!(
                socket.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        },
    );
}
