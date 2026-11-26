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
        // A readiness connection closes without sending an HTTP request. Darwin refuses to
        // set a socket timeout once its peer is gone. Peek without consuming a request byte.
        let size = tilo_test_env::wait_for("the local API request or its readiness probe", || {
            use rustix::net::{RecvFlags, recv};
            match recv(
                &stream,
                &mut [0u8; 1],
                RecvFlags::PEEK | RecvFlags::DONTWAIT,
            ) {
                Ok((_, size)) => Some(size),
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => None,
                Err(error) => panic!("reading the local API connection: {error}"),
            }
        });
        if size == 0 {
            return None;
        }
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
    holds_root: bool,
) -> (UnixListener, login::Reached) {
    let identity = home.root().join("device-identity");
    home.env()
        .set("TILO_IDENTITY_ROOT", identity)
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    if holds_root {
        let dir = tilo_identity::store::identity_dir_from_env().unwrap();
        tilo_identity::provisioning::import_single_device_root(
            &dir,
            root,
            "Ada",
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
    }
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
            let (socket, issuer) = local_api(&mut home, &root, true);
            let setup = Ticket::new(issuer.clone());
            for step in ["logged-in", "tools-added", "check", "restart", "entered"] {
                let progress = json!({"ticket": setup.encode(), "step": step, "agent": "Codex"});
                let mut reporter = ask("reported", &progress);
                assert_eq!(
                    answer(
                        &socket,
                        &mut reporter,
                        "reported",
                        "200 OK",
                        &json!({"status": "ok", "path": "devices#reported", "result": {}, "secret": setup.secret_hex()})
                    ),
                    progress
                );
                assert_eq!(successful(reporter), "{}");
            }

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

            let progress = json!({"ticket": setup.encode(), "step": "failed", "detail": "The agent is offline."});
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

#[cfg(target_os = "linux")]
#[test]
fn the_daemon_admits_a_fresh_caller_and_closes_its_connection_after_it_reads_its_key() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const CLIENT_SOCKET: &str = "TILO_TEST_TICKET_ACTION_CLIENT_SOCKET";
    const TEST: &str =
        "the_daemon_admits_a_fresh_caller_and_closes_its_connection_after_it_reads_its_key";
    const ADMITTED: &str = "the API reached the current-device callback";

    if let Some(socket) = std::env::var_os(CLIENT_SOCKET) {
        let socket = std::path::PathBuf::from(socket);
        let keys =
            tilo_device_identity::IdentityPaths::under(socket.parent().unwrap().join("keys"));
        let request = || -> std::io::Result<Vec<u8>> {
            // Each attempt establishes its connection before testing what the server answers.
            let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            stream.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
            stream
                .write_all(b"GET /v1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply)?;
            Ok(reply)
        };
        assert!(String::from_utf8_lossy(&request().unwrap()).contains(ADMITTED));
        let _key = tilo_device_identity::persist::load_device_key(&keys).unwrap();
        assert_eq!(
            rustix::process::dumpable_behavior().unwrap(),
            rustix::process::DumpableBehavior::NotDumpable
        );
        match request() {
            Ok(reply) => assert!(
                reply.is_empty(),
                "the daemon should close before serving HTTP"
            ),
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ),
                "{error}"
            ),
        }
        return;
    }

    tilo_test_env::in_own_process(module_path!(), TEST, || {
        let directory = tilo_test_env::runtime_tempdir().unwrap();
        let keys = tilo_device_identity::IdentityPaths::under(directory.path().join("keys"));
        tilo_device_identity::persist::adopt_device_key(&keys, &tilo_test_env::node_key()).unwrap();
        let socket = directory.path().join("api.sock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let server = runtime.spawn(tilo_api::direct::serve_current(listener, move || {
            seen.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(ADMITTED.to_owned()))
        }));
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("ticket_action::{TEST}"), "--nocapture"])
            .env(CLIENT_SOCKET, &socket)
            .output_locked()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the first connection reached the daemon's callback"
        );
        server.abort();
    });
}

/// Answer the CLI's own status probe as the holder of this exact enrollment.
fn entered(socket: &UnixListener, device: &login::Device, child: &mut Child) {
    let (mut stream, _) = tilo_test_env::wait_for("the holder's status", || {
        if let Some(status) = child.try_wait().unwrap() {
            let mut said = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut said)
                .unwrap();
            panic!("CLI exited before asking the holder ({status}): {said}");
        }
        match socket.accept() {
            Ok(pair) => Some(pair),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("{error}"),
        }
    });
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], tilo_api::daemon::PEER_STATUS);
    let status = json!({
        "session": "connected", "person": device.user(), "host": device.host_origin,
        "device_public_key": hex::encode(device.cert.device_public_key),
        "admitted_workspace": device.workspace,
    });
    writeln!(
        stream,
        "{}",
        json!({"jsonrpc":"2.0", "id":request["id"], "result": status})
    )
    .unwrap();
}

#[test]
fn hosted_setup_resumes_only_the_original_certified_login_after_its_host_accepts_the_receipt() {
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::PermissionsExt as _;
    const TEST: &str =
        "hosted_setup_resumes_only_the_original_certified_login_after_its_host_accepts_the_receipt";
    tilo_test_env::in_own_process(module_path!(), TEST, || {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let (api, _) = local_api(&mut home, &root, false);
        let place = login::Place::here(tilo_identity::provisioning::collaboration_identity_paths())
            .unwrap();
        let mut device = login::load_public(&place).unwrap().unwrap();
        let host = SigningKey::from_bytes(&tilo_test_env::node_key());
        let transport = SigningKey::from_bytes(&tilo_test_env::node_key())
            .verifying_key()
            .to_bytes();
        device.server_cert.device_public_key = host.verifying_key().to_bytes();
        device.server_cert.user_public_key = host.verifying_key().to_bytes();
        tilo_user_identity::sign_device_cert(&mut device.server_cert, &host).unwrap();
        device.server_address = Some(login::ServerAddress {
            endpoint_addr: iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(&transport).unwrap(),
            ),
            relay_url: None,
            web: None,
        });
        login::store(&place, &device).unwrap();
        let issuer_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        issuer_socket.set_nonblocking(true).unwrap();
        let issuer = login::Reached {
            endpoint: transport,
            relay: None,
            direct: vec![issuer_socket.local_addr().unwrap()],
        };
        let original = Ticket::new(issuer).with_issuer_device(device.server_cert.device_public_key);
        let holder =
            tilo_api::daemon::DaemonPlace::of(place.dir(), &device.cert.device_public_key).unwrap();
        let status = UnixListener::bind(holder.socket()).unwrap();
        status.set_nonblocking(true).unwrap();
        tilo_api::daemon::folders::Folders {
            auto_own: false,
            ..Default::default()
        }
        .save(place.dir())
        .unwrap();

        // Real tool discovery and registration, isolated from the account's installed agents.
        let bin = home.root().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let codex = bin.join("codex");
        std::fs::write(
            &codex,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/registrations\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
        let registrations = home.home().join("registrations");
        let version = "v0.3.1-assistant.1";
        let archive = "ab".repeat(32);
        home.env()
            .remove("CLAUDECODE")
            .remove("CLAUDE_CODE_ENTRYPOINT")
            .set("CODEX_SANDBOX", "1")
            .set("PATH", &bin)
            .set("TILO_VERSION", version)
            .set("TILO_HOSTED_ARCHIVE_SHA", &archive)
            .set("TILO_HOSTED_SETUP", "1")
            .set("TILO_HOSTED_RESUME", "1");
        let resume = |ticket: &Ticket| start(&["login", &ticket.encode(), "--agent", "Codex"], &[]);
        let denied = |case: &str, mut child: Child, ticket: &Ticket| {
            tilo_test_env::wait_for(format!("setup to refuse {case}"), || {
                child.try_wait().unwrap()
            });
            let output = child.wait_with_output().unwrap();
            assert!(!output.status.success(), "unexpected success for {case}");
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!said.contains(&ticket.encode()) && !said.contains(&ticket.secret_hex()));
            assert!(
                !registrations.exists(),
                "Tilo ran the tool for {case} before the host accepted the original setup"
            );
        };

        let marker = place.dir().join("hosted-cli");
        let write_marker = |line: &str| {
            tilo_files::native::atomic::write_private(&marker, line.as_bytes()).unwrap();
        };
        let digest = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
        let physical = hex::encode(device.cert.device_public_key);
        let pending_for = |ticket: &Ticket| {
            let ticket_sha = digest(ticket.encode().as_bytes());
            format!("pending {version} {archive} {ticket_sha} {physical}\n")
        };
        let pending = pending_for(&original);
        let login_file = place.dir().join("login.json");
        let cert_file = place
            .keys()
            .device_cert_for_user(&device.cert.user_public_key);
        let identity = format!(
            "{version} {} {}",
            digest(&std::fs::read(&login_file).unwrap()),
            digest(&std::fs::read(&cert_file).unwrap()),
        );
        denied("missing marker", resume(&original), &original);
        for (case, invalid) in [
            ("wrong release", pending.replace(version, "v0.3.0")),
            ("wrong archive", pending.replace(&archive, &"cd".repeat(32))),
            (
                "wrong ticket digest",
                pending.replace(&digest(original.encode().as_bytes()), &"ee".repeat(32)),
            ),
            (
                "wrong marked device",
                pending.replace(&physical, &"ff".repeat(32)),
            ),
            (
                "completed marker",
                format!(
                    "{identity}\n\n{}",
                    pending.replacen("pending ", "original ", 1)
                ),
            ),
        ] {
            write_marker(&invalid);
            denied(case, resume(&original), &original);
        }
        write_marker(&pending);
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
        denied("public marker", resume(&original), &original);
        let elsewhere = home.root().join("another marker");
        std::fs::rename(&marker, &elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &marker).unwrap();
        denied("symlink marker", resume(&original), &original);
        std::fs::remove_file(&marker).unwrap();
        write_marker(&pending);
        home.env().remove("TILO_HOSTED_RESUME");
        let fresh_denied = |case: &str| {
            denied(case, resume(&original), &original);
            assert_eq!(
                issuer_socket.recv(&mut [0; 1]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "the CLI contacted the issuer to certify an ineligible device"
            );
        };
        fresh_denied("already enrolled device");
        let saved_cert = home.root().join("retained-cert");
        let saved_login = home.root().join("retained-login");
        std::fs::rename(&cert_file, &saved_cert).unwrap();
        fresh_denied("login without certificate");
        std::fs::rename(&login_file, &saved_login).unwrap();
        std::fs::rename(&saved_cert, &cert_file).unwrap();
        fresh_denied("certificate without login");
        std::fs::rename(&cert_file, &saved_cert).unwrap();
        std::fs::write(&cert_file, b"incomplete certificate").unwrap();
        fresh_denied("incomplete certificate");
        std::fs::remove_file(&cert_file).unwrap();
        let cert_dir = cert_file.parent().unwrap();
        let cert_permissions = std::fs::metadata(cert_dir).unwrap().permissions();
        fresh_denied("empty certificate directory");
        std::fs::remove_dir(cert_dir).unwrap();
        write_marker(&format!("{pending}\n{identity}\n"));
        fresh_denied("prior enrollment in marker");
        write_marker(&pending);
        let saved_identity = tilo_identity::store::identity_dir_from_env().unwrap();
        assert!(!saved_identity.exists());
        std::fs::create_dir_all(&saved_identity).unwrap();
        fresh_denied("existing identity directory");
        std::fs::remove_dir(&saved_identity).unwrap();
        let saved_key = home.root().join("retained-key");
        let key_file = &place.keys().device_privkey;
        std::fs::rename(key_file, &saved_key).unwrap();
        fresh_denied("missing physical key");
        assert!(!key_file.exists(), "an absent physical key was replaced");
        std::fs::rename(&saved_key, key_file).unwrap();
        let desktop_path = home.runtime().join("desktop-only.sock");
        let desktop = UnixListener::bind(&desktop_path).unwrap();
        desktop.set_nonblocking(true).unwrap();
        home.env().set(tilo_api::client::DESKTOP_ENV, &desktop_path);
        fresh_denied("active desktop");
        assert_eq!(
            desktop.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        home.env().remove(tilo_api::client::DESKTOP_ENV);
        std::fs::create_dir(cert_dir).unwrap();
        std::fs::set_permissions(cert_dir, cert_permissions).unwrap();
        std::fs::rename(&saved_cert, &cert_file).unwrap();
        std::fs::rename(&saved_login, &login_file).unwrap();
        home.env().set("TILO_HOSTED_RESUME", "1");

        let mut moved = original.clone();
        moved.issuer.endpoint = device.cert.device_public_key;
        for (case, other) in [
            ("different issuer endpoint", moved),
            (
                "different issuer device",
                original
                    .clone()
                    .with_issuer_device(device.cert.device_public_key),
            ),
        ] {
            write_marker(&pending_for(&other));
            denied(case, resume(&other), &other);
        }
        write_marker(&pending);
        assert_eq!(
            status.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            api.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let ok = json!({"status":"ok", "path":"devices#reported", "result":{}});
        for (http, reply) in [
            (
                "403 Forbidden",
                json!({"error":{"code":"forbidden","message":"This setup was declined."}}),
            ),
            (
                "202 Accepted",
                json!({"status":"pending", "invocation":"in progress"}),
            ),
            ("200 OK", json!({"status":"ok", "result":{"pending":true}})),
        ] {
            let mut child = resume(&original);
            entered(&status, &device, &mut child);
            let asked = answer(&api, &mut child, "reported", http, &reply);
            assert_eq!(
                asked,
                json!({"ticket":original.encode(),"agent":"Codex","step":"logged-in"})
            );
            if http == "202 Accepted" {
                assert!(
                    !registrations.exists(),
                    "a pending receipt triggered registration"
                );
                let settled =
                    json!({"error":{"code":"forbidden","message":"The setup was declined."}});
                assert_eq!(
                    answer(&api, &mut child, "reported", "403 Forbidden", &settled),
                    asked,
                    "the retry reports the same login step"
                );
            }
            denied(http, child, &original);
        }

        // With a valid receipt, the same login continues to registration and confirmed entry.
        // The installer retains its daemon generation (empty without a holder) and public
        // login/certificate hashes after an enrolled setup exits before it completes.
        write_marker(&format!("{pending}\n{identity}\n"));
        let mut child = resume(&original);
        entered(&status, &device, &mut child);
        let progress = |step: &str, child: &mut Child| {
            assert_eq!(
                answer(&api, child, "reported", "200 OK", &ok),
                json!({"ticket":original.encode(),"agent":"Codex","step":step})
            );
        };
        progress("logged-in", &mut child);
        progress("restart", &mut child);
        entered(&status, &device, &mut child);
        progress("entered", &mut child);
        assert!(successful(child).contains("Entered "));
        let registered = std::fs::read_to_string(&registrations).unwrap();
        assert_eq!(registered.lines().count(), 1);
        assert!(
            registered.starts_with("mcp add tilo -- ") && registered.trim_end().ends_with(" mcp")
        );
        std::fs::remove_file(&registrations).unwrap();
        assert_eq!(login::load_public(&place).unwrap().unwrap(), device);

        let replacement = SigningKey::from_bytes(&tilo_test_env::node_key());
        tilo_device_identity::persist::write_device_key(&place.keys().device_privkey, &replacement)
            .unwrap();
        denied("substituted physical key", resume(&original), &original);
        assert_eq!(
            tilo_device_identity::persist::load_device_key(place.keys())
                .unwrap()
                .verifying_key(),
            replacement.verifying_key()
        );
        std::fs::remove_file(place.dir().join("login.json")).unwrap();
        std::fs::remove_file(&place.keys().device_privkey).unwrap();
        denied(
            "missing retained login and key",
            resume(&original),
            &original,
        );
        assert!(!place.keys().device_privkey.exists());
        assert_eq!(
            api.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    });
}
