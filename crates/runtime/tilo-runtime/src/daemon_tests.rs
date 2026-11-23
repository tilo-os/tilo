use super::{Mode, parse_args};
use std::{ffi::OsString, io, time::Duration};
use tilo_mcp::lines::{
    Frame, MAX_REQUEST_BYTES, mcp_answer, read_frame, serve_lines, spawn_reader, write_reply,
};

tilo_test_env::scenario! {
    fn the_daemons_sockets_activate_open_and_answer_as_requests_finish;

    phase default_admission_activates_a_live_owner_and_retries_after_its_exit;
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Endpoint {
            active: Arc<AtomicBool>,
            stop: Arc<AtomicBool>,
            thread: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Endpoint {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::SeqCst);
                if let Some(thread) = self.thread.take() {
                    thread.join().unwrap();
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(directory.path().join("native.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (serving, stopping) = (active.clone(), stop.clone());
        let endpoint = Endpoint {
            active,
            stop,
            thread: Some(std::thread::spawn(move || {
                while !stopping.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                                .unwrap();
                            let mut request = String::new();
                            BufReader::new(stream.try_clone().unwrap())
                                .read_line(&mut request)
                                .unwrap();
                            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                            assert_eq!(
                                request,
                                serde_json::json!({"native":"activate", "app_id":"so.tilo.fixture"})
                            );
                            writeln!(
                                stream,
                                "{}",
                                serde_json::json!({"activated": serving.load(Ordering::SeqCst)})
                            )
                            .unwrap();
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("activation endpoint failed: {error}"),
                    }
                }
            })),
        };
        let grace = Duration::from_millis(60);
        let first = super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
            .unwrap()
            .unwrap();
        endpoint.active.store(true, Ordering::SeqCst);
        assert!(
            super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
                .unwrap()
                .is_none(),
            "a second healthy launch attaches without replacing its owner"
        );
        endpoint.active.store(false, Ordering::SeqCst);
        let refusal =
            super::default_launch_guard(directory.path(), "so.tilo.fixture", grace).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
        assert!(refusal.to_string().contains("owner was retained"));
        drop(first);
        // The stale PID left in the lock file never counts as a live owner.
        let retry = super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
            .unwrap()
            .expect("owner exit releases the kernel lease for a fresh launch");
        drop(retry);
        drop(endpoint);
    }

    phase a_result_another_process_was_handed_opens_through_the_action_provider;
    {
        let host = PageHost::default();
        host.offered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
            std::sync::Arc::new(host.clone()),
            tilo_actions::remote::Caller::local_person("owner").in_workspace("workspace"),
        ));
        let runtime = tilo_rt::try_app().unwrap();
        runtime.handle().block_on(search.refresh()).unwrap();
        let entry = search.search("travel", 1)[0].id;

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("native.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let serving = {
            let (host, search) = (host.clone(), std::sync::Arc::clone(&search));
            std::thread::spawn(move || {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().unwrap();
                    super::serve_connection(
                        &stream.try_clone().unwrap(),
                        stream,
                        &std::sync::Arc::new(tilo_mcp::McpServer::for_person(
                            host.clone(),
                            "workspace".into(),
                            "owner",
                        )),
                        &super::Status {
                            version: 1,
                            pid: 1,
                            workspace: "workspace".into(),
                        },
                        &std::sync::OnceLock::from(std::sync::Arc::clone(&search)),
                        None,
                        None,
                    )
                    .unwrap();
                }
            })
        };

        // What the app a Spotlight click woke sends: the daemon opens it.
        tilo_host_search::open(&socket, &entry.to_string()).unwrap();
        let invoked = host.invoked.lock().unwrap().clone();
        assert_eq!(invoked.len(), 1);
        assert_eq!(
            invoked[0].target.address,
            tilo_actions::ActionAddress::new("page:1", "open")
        );

        // Access is revoked before the next walk: the daemon re-checks and refuses.
        host.offered
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let refused = tilo_host_search::open(&socket, &entry.to_string()).unwrap_err();
        assert!(refused.to_string().contains("NotOffered"), "{refused}");
        assert_eq!(host.invoked.lock().unwrap().len(), 1, "nothing more ran");
        serving.join().unwrap();
    }

    phase native_socket_serves_parallel_requests;
    // The daemon's native socket answers one session's MCP requests as they
    // finish, not in the order asked: a `tilo_status` and one that cancels
    // answer while two invokes of the same connection wait.
    {
        use tilo_mcp::test_hosts::{HeldHost, overlapping_lines};
        let host = HeldHost::default();
        let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
            std::sync::Arc::new(host.clone()),
            tilo_actions::remote::Caller::local_person("owner").in_workspace("workspace"),
        ));
        let (client, served) = std::os::unix::net::UnixStream::pair().unwrap();
        let serving = {
            let host = host.clone();
            std::thread::spawn(move || {
                super::serve_connection(
                    &served.try_clone().unwrap(),
                    served,
                    &std::sync::Arc::new(tilo_mcp::McpServer::for_person(
                        host,
                        "workspace".into(),
                        "owner",
                    )),
                    &super::Status {
                        version: 1,
                        pid: 1,
                        workspace: "workspace".into(),
                    },
                    &std::sync::OnceLock::from(std::sync::Arc::clone(&search)),
                    None,
                    None,
                )
            })
        };
        overlapping_lines(&host, &client);
        serving.join().unwrap().unwrap();
    }

    phase the_proxy_delivers_each_reply_before_the_stream_ends;
    // The `--connect` proxy hands each reply to the client as it arrives, while
    // the connection stays open: a client that waits for one answer before it
    // asks the next must not wait for a reply that is sitting in a buffer.
    {
        /// A writer that records what reached its destination: only flushed bytes.
        #[derive(Default)]
        struct Client {
            pending: Vec<u8>,
            delivered: Vec<u8>,
        }
        impl io::Write for Client {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.pending.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.delivered.append(&mut self.pending);
                Ok(())
            }
        }
        /// The daemon's side: one reply, then a stream that is still open.
        struct OpenStream(Vec<Vec<u8>>);
        impl io::Read for OpenStream {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let Some(chunk) = self.0.pop() else {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "still open"));
                };
                buffer[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }
        let mut client = Client::default();
        let reply = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n".to_vec();
        let mut daemon = OpenStream(vec![reply[16..].to_vec(), reply[..16].to_vec()]);
        let still_open = tilo_mcp::session_api::relay(&mut daemon, &mut client).unwrap_err();
        assert_eq!(still_open.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            client.delivered, reply,
            "the whole reply reached the client while the stream is open"
        );
    }
}

tilo_test_env::scenario! {
    #[expect(
        clippy::naive_bytecount,
        reason = "counts the newlines of one short test reply"
    )]
    fn the_stdio_host_frames_bounded_lines_and_answers_as_requests_finish;

    phase bounded_framing_recovers_after_an_oversized_line;
    {
        let mut bytes = vec![b'x'; MAX_REQUEST_BYTES + 1];
        bytes.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        let mut input = std::io::BufReader::new(bytes.as_slice());

        assert_eq!(read_frame(&mut input).unwrap(), Frame::TooLong);
        let Frame::Line(next) = read_frame(&mut input).unwrap() else {
            panic!("expected the complete request after the oversized line");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&next).unwrap()["method"],
            "ping"
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
    }

    phase line_at_the_one_mib_limit_is_accepted;
    {
        let mut bytes = vec![b' '; MAX_REQUEST_BYTES];
        bytes.push(b'\n');
        let mut input = std::io::BufReader::new(bytes.as_slice());
        let Frame::Line(line) = read_frame(&mut input).unwrap() else {
            panic!("line at limit must be accepted");
        };
        assert_eq!(line.len(), MAX_REQUEST_BYTES);
    }

    phase framing_joins_lines_split_across_buffer_refills;
    {
        // A three-byte buffer forces every line to span several `fill_buf` calls,
        // including a refill that starts exactly at a newline.
        let bytes = b"abcdef\r\n\nxy\nlast";
        let mut input = std::io::BufReader::with_capacity(3, bytes.as_slice());
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Frame::Line(b"abcdef".to_vec())
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Line(Vec::new()));
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Line(b"xy".to_vec()));
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Frame::Line(b"last".to_vec())
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
    }

    phase protocol_writer_emits_one_json_line_and_flushes;
    {
        #[derive(Default)]
        struct Output {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut output = Output::default();
        write_reply(
            &mut output,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}),
        )
        .unwrap();
        assert_eq!(output.flushes, 1);
        assert_eq!(output.bytes.last(), Some(&b'\n'));
        assert_eq!(
            output.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        serde_json::from_slice::<serde_json::Value>(&output.bytes).unwrap();
    }

    phase parallel_requests_complete_out_of_order;
    // The stdio host answers one session's requests as they finish, not in
    // the order asked: a `tilo_status` and one that cancels answer while two
    // invokes wait, and every reply is one whole line.
    {
        use tilo_mcp::test_hosts::{HeldHost, overlapping_lines};
        let host = HeldHost::default();
        let server = std::sync::Arc::new(tilo_mcp::McpServer::for_person(
            host.clone(),
            "workspace".into(),
            "owner",
        ));
        let (client, end) = std::os::unix::net::UnixStream::pair().unwrap();
        let input = spawn_reader(end.try_clone().unwrap()).unwrap();
        let serving = std::thread::spawn(move || {
            let mut writer = end;
            serve_lines(
                tilo_rt::try_app().unwrap(),
                input,
                &mut writer,
                |request| mcp_answer(&server, request),
                Some(&mut || Ok(())),
            )
        });
        overlapping_lines(&host, &client);
        serving.join().unwrap().unwrap();
    }
}

tilo_test_env::scenario! {
    fn the_command_line_names_one_mode_and_reads_the_names_it_had;

    phase run_app_reads_the_workspace_flag_it_had_another_name_for;
    // A launcher outside this repository passes the flag by the name it had
    // before the workspace was called a workspace.
    {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        let workspace = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
        let named = args(&[
            "--profile",
            "/tmp/profile",
            "--run-app",
            "so.tilo.photos",
            "--workspace",
            workspace,
            "--",
            "/bin/true",
        ])
        .unwrap();
        let Mode::RunApp {
            workspace: Some(id),
            ..
        } = &named.mode
        else {
            panic!("the launch was parsed as {:?}", named.mode);
        };
        assert_eq!(id, workspace);

        let old = args(&[
            "--profile",
            "/tmp/profile",
            "--run-app",
            "so.tilo.photos",
            "--workspace",
            workspace,
            "--",
            "/bin/true",
        ])
        .unwrap();
        assert_eq!(old.mode, named.mode);

        assert!(
            args(&[
                "--profile",
                "/tmp/profile",
                "--run-app",
                "so.tilo.photos",
                "--workspace",
                "not-a-workspace",
                "--",
                "/bin/true",
            ])
            .is_err()
        );
    }

    phase the_status_probe_reads_the_field_name_it_had;
    // The status probe answers the served workspace as `workspace` and reads
    // `workspace` as the same.
    {
        let workspace = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
        let old: super::Status = serde_json::from_str(&format!(
            "{{\"version\":1,\"pid\":42,\"workspace\":\"{workspace}\"}}"
        ))
        .unwrap();
        assert_eq!(old.workspace, workspace);
        let new: super::Status = serde_json::from_str(&format!(
            "{{\"version\":1,\"pid\":42,\"workspace\":\"{workspace}\"}}"
        ))
        .unwrap();
        assert_eq!(new.workspace, old.workspace);
        // Launchers and scripts read the answer's `workspace`.
        assert_eq!(
            serde_json::to_value(&new).unwrap()["workspace"],
            serde_json::json!(workspace)
        );
    }

    phase the_daemon_is_plain_tilo_runtime;
    {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        let dev = args(&["--profile", "/tmp/profile", "--dev"]).unwrap();
        assert_eq!((dev.mode, dev.dev), (Mode::Daemon, true));
        assert!(!args(&["--profile", "/tmp/profile"]).unwrap().dev);
        assert_eq!(args(&[]).unwrap().mode, Mode::Daemon, "plain tilo-runtime");
        assert!(args(&["--profile", "/tmp/profile", "--connect", "--dev"]).is_err());
        assert!(args(&["--profile", "/tmp/profile", "--dev", "--dev"]).is_err());
    }

    phase connect_asks_for_a_workspace_by_its_id;
    // `--connect --workspace ID` asks the desktop's API for that workspace: it
    // is a client's request of the person, so it goes with `--connect` alone,
    // never with a standalone runtime of the client's own.
    {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        let workspace = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
        let asked = args(&["--connect", "--workspace", workspace]).unwrap();
        assert_eq!(asked.mode, Mode::Connect);
        assert_eq!(asked.workspace.as_deref(), Some(workspace));
        assert_eq!(args(&["--connect"]).unwrap().workspace, None);
        for invalid in [
            vec!["--connect", "--workspace"],
            vec!["--connect", "--workspace", "not-a-workspace"],
            vec![
                "--connect",
                "--workspace",
                workspace,
                "--workspace",
                workspace,
            ],
            vec!["--connect", "--standalone", "--workspace", workspace],
            vec![
                "--profile",
                "/tmp/profile",
                "--connect",
                "--workspace",
                workspace,
            ],
            vec![
                "--profile",
                "/tmp/profile",
                "--workspace",
                workspace,
            ],
        ] {
            assert!(args(&invalid).is_err(), "{invalid:?}");
        }
    }
}

/// A client that names a profile chose a standalone runtime itself; one that
/// names none is routed by its login (`session_api::route`).
#[test]
fn standalone_is_the_clients_own_explicit_choice_for_connect() {
    let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
    assert!(args(&["--connect", "--standalone"]).unwrap().standalone);
    assert!(
        args(&["--profile", "/tmp/profile", "--connect"])
            .unwrap()
            .standalone,
        "a named profile is a standalone runtime's"
    );
    assert!(args(&["--profile", "/tmp/profile", "--standalone"]).is_err());
    assert!(args(&["--connect", "--standalone", "--standalone"]).is_err());
}

/// One page resource with an `open` view action, offered by publisher `7`,
/// and every invocation the host was asked to run.
#[derive(Clone, Default)]
struct PageHost {
    offered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    invoked: std::sync::Arc<std::sync::Mutex<Vec<tilo_actions::remote::InvokeRequest>>>,
}

impl tilo_actions::remote::ActionProvider for PageHost {
    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::{CatalogueProvider, CatalogueResource, CatalogueRow};
        let rows: Vec<CatalogueRow> = if self.offered.load(std::sync::atomic::Ordering::SeqCst) {
            vec![CatalogueRow {
                offered: None,
                provider: "7".into(),
                app_id: "so.tilo.notes".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: "page:1".into(),
                    kind: "notes.page".into(),
                    label: "Travel plans".into(),
                    presentation: None,
                },
                action: Some(tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new("page:1", "open"),
                    resource_kind: "notes.page".into(),
                    resource_label: "Travel plans".into(),
                    action_label: "Open note".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: tilo_actions::OperationKind::View,
                    contract: tilo_actions::ActionContract::view(),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }),
                has_children: false,
                visibility: None,
                surface: None,
            }]
        } else {
            Vec::new()
        };
        let mut page = tilo_actions::remote::CataloguePage::from_rows("1".into(), rows, &query);
        if query.provider.is_none() {
            page.providers = vec![CatalogueProvider {
                provider: "7".into(),
                app_id: "so.tilo.notes".into(),
                workspace: "workspace".into(),
                document_id: String::new(),
                browsable: true,
            }];
        }
        tilo_actions::remote::Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
            invocation,
            status: tilo_actions::remote::ReceiptStatus::Accepted,
        }))
    }

    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

/// The server writes the workspace and the agent key into the client's `initialize` and nothing
/// else: a probe before it and every line after it reach the API as sent.
#[test]
fn the_server_writes_the_workspace_and_key_into_the_initialize_alone() {
    let sent = concat!(
        r#"{"jsonrpc":"2.0","id":0,"method":"server/discover"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{"experimental":{"so.tilo":{"actsAs":"agent"}}},"clientInfo":{"name":"Claude Code"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"workspace":"x"}}"#,
        "\n",
    );
    let mut relayed = Vec::new();
    let asking = tilo_mcp::session_api::Asking {
        workspace: Some("w-1".into()),
        key: Some("tilo_ak_0123".into()),
        session_name: Some("Office Hours".into()),
    };
    let total =
        tilo_mcp::session_api::relay_asking(&mut sent.as_bytes(), &mut relayed, &asking).unwrap();
    assert_eq!(total, sent.len() as u64, "every byte read is relayed");
    let lines: Vec<serde_json::Value> = relayed
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["method"], "server/discover");
    assert_eq!(
        lines[1].pointer(tilo_mcp::session_api::KEY),
        Some(&serde_json::json!("tilo_ak_0123")),
        "the agent key rides in the initialize"
    );
    assert_eq!(
        lines[1].pointer(tilo_mcp::session_api::WORKSPACE),
        Some(&serde_json::json!("w-1"))
    );
    assert_eq!(
        lines[1].pointer("/params/capabilities/experimental/so.tilo/actsAs"),
        Some(&serde_json::json!("agent")),
        "what the client asked is kept"
    );
    assert_eq!(
        lines[2],
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"workspace":"x"}})
    );
    let hello =
        tilo_mcp::session_api::parse_hello(&serde_json::to_vec(&lines[1]).unwrap()).unwrap();
    assert_eq!(hello.workspace.as_deref(), Some("w-1"), "the API reads it");
    assert_eq!(
        hello.key.as_deref(),
        Some("tilo_ak_0123"),
        "the API reads the key"
    );
    assert_eq!(
        hello.request.pointer(tilo_mcp::session_api::KEY),
        None,
        "the key is kept out of the request the API goes on to answer"
    );
    assert_eq!(
        hello.session_name.as_deref(),
        Some("Office Hours"),
        "the session the client named rides in the initialize, and the API reads it"
    );
}

/// A device no person's certificate names yet answers on its native endpoint at once: its own
/// actions are answered, and one that needs a person is refused saying the device has none yet.
/// Nothing waits for a claim.
#[test]
fn an_unclaimed_device_answers_its_own_actions_at_once_and_says_it_has_no_person_yet() {
    use io::{BufRead as _, Write as _};
    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let place = tilo_device_identity::login::Place::at(dir.path().join("login"), keys);
    let native = std::sync::Arc::new(super::Native::new(
        crate::device::unclaimed::DeviceOnly::new(key.clone(), place),
        crate::seats::peer_uid,
        tilo_api::daemon::admission::here(|| rustix::process::geteuid().as_raw()),
    ));
    let listener =
        tilo_files::native::private::bind_socket(&dir.path().join("native.sock")).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let stream = std::os::unix::net::UnixStream::connect(dir.path().join("native.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = io::BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut ask = |request: serde_json::Value| {
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the device answers at once");
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };

    let status = ask(serde_json::json!({"native": "status"}));
    assert_eq!(
        status["workspace"], "",
        "no workspace is served yet: {status}"
    );

    let info = ask(
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "tilo_do", "arguments": {"path": "devices#info"}}}),
    );
    let text = info["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert_ne!(info["result"]["isError"], true, "{info}");
    assert!(
        text.contains(&hex::encode(key.verifying_key().to_bytes())),
        "the device says what it is: {info}"
    );

    let person = ask(
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "tilo_do", "arguments": {"path": "notes#create"}}}),
    );
    let text = person["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("this device has no person yet"),
        "what needs a person says the device has none: {person}"
    );
}

/// A device nobody set up founds no person by itself: it stays its device alone until its own
/// account starts one (`devices#found`), as a laptop's first command does through the place every
/// client finds the daemon at. Another account may not, and a device with a person starts no
/// other.
#[test]
fn a_device_founds_its_person_only_when_its_own_account_asks() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    const OWN: u32 = 990;
    const OTHER: u32 = 1000;

    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let root = home.root().to_owned();
    let keys = tilo_device_identity::IdentityPaths::under(root.join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let device = hex::encode(key.verifying_key().to_bytes());
    let place = tilo_device_identity::login::Place::at(root.join("login"), keys);
    let admission: &'static tilo_api::daemon::admission::Admission = Box::leak(Box::new(
        tilo_api::daemon::admission::Admission::new(OWN, false),
    ));
    let native = std::sync::Arc::new(super::Native::new(
        crate::device::unclaimed::DeviceOnly::new(key, place),
        test_peer,
        admission,
    ));
    let sockets = root.join("sockets");
    std::fs::create_dir_all(&sockets).unwrap();
    let listener =
        tilo_files::native::private::bind_socket(&sockets.join(tilo_paths::machine::DAEMON_NATIVE))
            .unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let daemon = tilo_api::daemon::DaemonPlace::at(&sockets, &root.join("login"));
    let ask = |uid: u32, request: Value| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(daemon.native()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the device answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let found = |uid: u32| {
        let answer = ask(
            uid,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": "devices#found"}}}),
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (answer["result"]["isError"] == true, text)
    };

    let fresh = ask(OWN, json!({"native": "account#who"}));
    assert_eq!(
        fresh["person"],
        Value::Null,
        "nothing founds a person by itself: {fresh}"
    );
    assert_eq!(fresh["claimed"], false, "{fresh}");

    let (refused, why) = found(OTHER);
    assert!(refused, "another account started a person: {why}");
    assert!(why.contains("own account"), "{why}");
    assert_eq!(ask(OWN, json!({"native": "account#who"}))["claimed"], false);

    // The laptop's first command: through the place every client finds the daemon at.
    PEER.store(OWN, Ordering::SeqCst);
    daemon.found().unwrap();
    let who = ask(OWN, json!({"native": "account#who"}));
    let person = who["person"].as_str().expect("a person now").to_owned();
    assert_eq!(who["claimed"], true, "{who}");
    assert_eq!(who["device"], device, "{who}");

    let (refused, why) = found(OWN);
    assert!(refused, "a device with a person started another: {why}");
    assert!(
        why.contains(tilo_device_identity::login::ALREADY_A_PERSONS),
        "{why}"
    );
    daemon
        .found()
        .expect("a command on a device with a person goes on as that person");
    assert_eq!(
        ask(OWN, json!({"native": "account#who"}))["person"],
        person,
        "the person stays the one first started"
    );
}

/// The account a test's next native connection comes from, as the daemon reads it.
static PEER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn test_peer(_: &std::os::unix::net::UnixStream) -> io::Result<u32> {
    Ok(PEER.load(std::sync::atomic::Ordering::SeqCst))
}

/// Under the system unit every account reaches the native endpoint, and the account is what it is
/// answered as: root says who signed in (and nobody else may), an account signed in is its person
/// and is served, and any other account is answered the device's own actions alone. `account#who`
/// tells each which it is.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one sign-in and sign-out, read top to bottom"
)]
fn the_system_daemon_answers_each_account_as_who_signed_in() {
    use ed25519_dalek::Signer as _;
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    use tilo_api::daemon::admission::{Admission, ROOT};
    const DAEMON: u32 = 990;
    const ADA: u32 = 1000;
    const BOB: u32 = 1001;

    // A device bound to Ada: her root certified its key.
    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    keys.ensure_directories().unwrap();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let ada = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let person = ada.verifying_key().to_bytes();
    let device = key.verifying_key().to_bytes();
    tilo_device_identity::persist::write_public_key(&keys.user_pubkey, &person).unwrap();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person,
        device_public_key: device,
        device_label: "desk".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1_000,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &ada).unwrap();
    tilo_device_identity::persist::write_device_cert(&keys.device_cert, &cert).unwrap();
    let proof_of = |uid: u32| {
        let digest = tilo_identity::local_person::proof_digest(&person, &device, uid).unwrap();
        hex::encode(ada.sign(&digest).to_bytes())
    };

    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
    );
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(DAEMON, true)));
    let native = std::sync::Arc::new(super::Native::new(answers.clone(), test_peer, admission));
    // The person's services run, answering as the workspace `home`.
    native.serve_the_person(super::Endpoint {
        session_factory: std::sync::Arc::new(move || {
            Ok(tilo_mcp::McpServer::for_agent(
                answers.clone(),
                String::new(),
                "agent",
                "",
            ))
        }),
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: "home".into(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
        agents: None,
    });
    let socket = dir.path().join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    // One request on a connection of `uid`'s, and its answer.
    let ask = |uid: u32, request: Value| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let who = |uid: u32| ask(uid, json!({"native": "account#who"}));
    // A seat action called as any action is: whether it failed, and what it answered.
    let act = |uid: u32, path: &str, args: Value| {
        let answer = ask(
            uid,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}}),
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (answer["result"]["isError"] == true, text)
    };
    let status = |uid: u32| ask(uid, json!({"native": "status"}));
    // A seat action whose answer may carry a descriptor: its text, and the descriptor.
    let act_handed = |uid: u32, path: &str, args: Value| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}});
        writeln!(&stream, "{request}").unwrap();
        let (mut line, fd) = runtime_portal::fd_transport::recv_with_fd(&stream).unwrap();
        while line.last() != Some(&b'\n') {
            let mut rest = Vec::new();
            io::BufReader::new(&stream)
                .read_until(b'\n', &mut rest)
                .unwrap();
            line.extend(rest);
        }
        let answer: Value = serde_json::from_slice(&line).unwrap();
        assert_ne!(answer["result"]["isError"], true, "{answer}");
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (text, fd)
    };

    // Before anyone signed in, Ada's account is a stranger: no person, and the device alone.
    let unknown = who(ADA);
    assert_eq!(unknown["person"], Value::Null, "{unknown}");
    assert_eq!(unknown["device"], hex::encode(device), "{unknown}");
    assert_eq!(unknown["claimed"], true, "{unknown}");
    assert_eq!(
        status(ADA)["workspace"],
        "",
        "a stranger is not served the person"
    );

    // Only root says who signed in: neither Ada herself nor the daemon's own account may.
    let signing_in = json!({"person": hex::encode(person), "uid": ADA, "proof": proof_of(ADA)});
    for by in [ADA, DAEMON] {
        let (failed, why) = act(by, "seats#signed-in", signing_in.clone());
        assert!(failed, "{by} said who signed in: {why}");
    }
    // A proof for another account says nothing about Bob's.
    let (failed, why) = act(
        ROOT,
        "seats#signed-in",
        json!({"person": hex::encode(person), "uid": BOB, "proof": proof_of(ADA)}),
    );
    assert!(failed, "{why}");
    // Nor is a person this device does not serve taken on root's word.
    let (failed, why) = act(
        ROOT,
        "seats#signed-in",
        json!({"person": hex::encode([3; 32]), "uid": ADA, "proof": proof_of(ADA)}),
    );
    assert!(failed, "{why}");
    // A person who arrives has a workspace found for them, unless their session hands over the
    // ones their desktop kept, and root is handed the link her shell is admitted by, with its
    // answer, for the session it starts.
    let shells = std::sync::Arc::new(Linked::default());
    native.seats.serve(std::sync::Arc::clone(&shells) as _);
    let mut importing = signing_in.clone();
    importing["importing"] = json!(true);
    let (signed_in, link) = act_handed(ROOT, "seats#signed-in", importing);
    assert!(signed_in.contains(&hex::encode(person)), "{signed_in}");
    assert_eq!(
        shells.arrivals.load(Ordering::SeqCst),
        0,
        "the desktop's workspace is hers"
    );
    shells.reaches(link.expect("the answer carries her shell's link"));
    let (signed_in, link) = act_handed(ROOT, "seats#signed-in", signing_in);
    assert!(signed_in.contains(&hex::encode(person)), "{signed_in}");
    assert_eq!(
        shells.arrivals.load(Ordering::SeqCst),
        1,
        "she is found a workspace"
    );
    shells.reaches(link.expect("each sign-in carries a link of its own"));
    // A daemon of the system's starts no shell itself: root starts each with the link it got.
    let started = ask(
        ADA,
        json!({"native": "shells#start", "args": ["-c", "exit 0"], "env": {}}),
    );
    assert!(started["error"].is_string(), "{started}");

    // Ada is her person now, and served; Bob still a stranger; root the device, no person.
    assert_eq!(who(ADA)["person"], hex::encode(person));
    assert_eq!(
        status(ADA)["workspace"],
        "home",
        "a signed-in account is served"
    );
    assert_eq!(who(BOB)["person"], Value::Null);
    assert_eq!(status(BOB)["workspace"], "");
    assert_eq!(who(ROOT)["person"], Value::Null);

    // She leaves: only root says so, and she is a stranger again.
    let leaving = json!({"uid": ADA});
    assert!(act(ADA, "seats#left", leaving.clone()).0);
    let (failed, left) = act(ROOT, "seats#left", leaving);
    assert!(!failed && left.contains("true"), "{left}");
    assert_eq!(who(ADA)["person"], Value::Null);
    assert_eq!(status(ADA)["workspace"], "");
}

/// An app that is its own computer's daemon runs it in its process, and may stop it and start it
/// again there: each start answers on its native endpoint at once, and each stop closes it and
/// lets go of the device, so the next start takes the device again.
#[test]
fn a_daemon_in_this_process_stops_and_starts_again_on_its_device() {
    use io::{BufRead as _, Write as _};
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = tempfile::tempdir().unwrap();
    let private = |path: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    };
    let (home, run, profile) = (
        dir.path().join("home"),
        dir.path().join("run"),
        dir.path().join("profile"),
    );
    for path in [&home, &run, &profile] {
        private(path);
    }
    // A device nobody set up founds no person by itself: it waits for one, answering its own
    // actions.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home)
        .set("XDG_RUNTIME_DIR", &run)
        .set("TILO_IDENTITY_ROOT", dir.path().join("identity"))
        .remove("CREDENTIALS_DIRECTORY")
        .remove("TILO_RUNTIME_PROFILE");
    for start in ["first", "again"] {
        let running = super::Running::start(&profile).unwrap();
        let socket = running.native_socket().to_owned();
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = io::BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        writeln!(
            writer,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": "devices#info"}}})
        )
        .unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the device answers at once");
        let info: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_ne!(info["result"]["isError"], true, "{start} start: {info}");
        writeln!(writer, "{}", serde_json::json!({"native": "account#who"})).unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the device says who asks");
        let who: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            who["person"],
            serde_json::Value::Null,
            "{start} start: {who}"
        );
        assert_eq!(who["claimed"], false, "{start} start: {who}");
        // `tilo status` asks the same endpoint what the device holds, as a connection's first
        // line too.
        let status = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        status
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            &status,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": tilo_api::daemon::PEER_STATUS})
        )
        .unwrap();
        let mut line = String::new();
        io::BufReader::new(&status)
            .read_line(&mut line)
            .expect("the device says what it holds");
        let peer: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            peer["result"]["pid"],
            std::process::id(),
            "{start} start: {peer}"
        );
        running.stop().unwrap();
        assert!(
            !socket.exists(),
            "the {start} start's endpoint is gone once it stopped"
        );
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_err(),
            "nothing answers there after the {start} start stopped"
        );
    }
}

/// A person's shells as a test serves them: each sign-in counted unless it imports, and each
/// link a pair whose other end the test keeps; a shell is started as `/bin/sh` with what it asks.
#[derive(Default)]
struct Linked {
    arrivals: std::sync::atomic::AtomicUsize,
    ends: std::sync::Mutex<Vec<std::os::unix::net::UnixStream>>,
}

impl Linked {
    /// Whether `link` is the other end of the newest link made.
    fn reaches(&self, link: std::os::fd::OwnedFd) {
        use io::{Read as _, Write as _};
        let mut link = std::os::unix::net::UnixStream::from(link);
        let mut end = self.ends.lock().unwrap().pop().unwrap();
        end.write_all(b"shell").unwrap();
        let mut read = [0; 5];
        link.read_exact(&mut read).unwrap();
        assert_eq!(
            &read, b"shell",
            "the handed descriptor is the link the daemon made"
        );
    }
}

impl crate::seats::Shells for Linked {
    fn arrived(&self, importing: bool) -> io::Result<std::os::unix::net::UnixStream> {
        if !importing {
            self.arrivals
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        self.ends.lock().unwrap().push(ours);
        Ok(theirs)
    }

    fn start(
        &self,
        args: Vec<std::ffi::OsString>,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> io::Result<std::process::Child> {
        std::process::Command::new("/bin/sh")
            .args(args)
            .env_clear()
            .envs(env)
            .spawn()
    }
}

/// A daemon that runs as its person's own account starts their shell when that account asks, and
/// answers how it ended; no other account starts one, and a request that asks for one founds
/// nothing.
#[test]
fn the_persons_own_daemon_starts_their_shell_when_their_account_asks() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    use tilo_api::daemon::admission::Admission;
    const DAEMON: u32 = 1000;
    const OTHER: u32 = 1001;

    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
    );
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(DAEMON, false)));
    let native = std::sync::Arc::new(super::Native::new(answers, test_peer, admission));
    // The person's endpoint founds their workspace as a connection is admitted to it.
    let founded = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    native.serve_the_person(super::Endpoint::<crate::device::unclaimed::DeviceOnly> {
        session_factory: {
            let founded = std::sync::Arc::clone(&founded);
            std::sync::Arc::new(move || {
                founded.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("no workspace in this test"))
            })
        },
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: String::new(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
    });
    native.seats.serve(std::sync::Arc::new(Linked::default()));
    let socket = dir.path().join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    // A connection of `uid`'s.
    let connect = |uid: u32| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        (io::BufReader::new(stream.try_clone().unwrap()), stream)
    };
    // One request on `connection`, and its answer.
    let ask = |connection: &mut (
        io::BufReader<std::os::unix::net::UnixStream>,
        std::os::unix::net::UnixStream,
    ),
               request: &Value| {
        writeln!(connection.1, "{request}").unwrap();
        let mut line = String::new();
        connection
            .0
            .read_line(&mut line)
            .expect("the daemon answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };

    let start =
        json!({"native": "shells#start", "args": ["-c", "exit \"$CODE\""], "env": {"CODE": "7"}});
    let mut person = connect(DAEMON);
    let started = ask(&mut person, &start);
    let pid = started["pid"].clone();
    assert!(pid.is_u64(), "{started}");
    let ended = ask(&mut person, &json!({"native": "shells#wait", "pid": pid}));
    assert_eq!(
        ended["code"], 7,
        "the shell ran as asked, and its end is told: {ended}"
    );
    assert_eq!(
        founded.load(Ordering::SeqCst),
        0,
        "starting a shell founds nothing"
    );

    // A shell is waited for once, and only one this daemon started.
    let again = ask(
        &mut connect(DAEMON),
        &json!({"native": "shells#wait", "pid": pid}),
    );
    assert!(again["error"].is_string(), "{again}");
    // Another account starts no shell of this person's.
    let refused = ask(&mut connect(OTHER), &start);
    assert!(refused["error"].is_string(), "{refused}");
}
