// Uses the actual manager queue/shovel/Cap'n Proto client. Only the remote
// service is a fixture; this is adapter proof, not broker authorization proof.
#[derive(Default)]
struct DirectoryCaptures {
    queries: Vec<runtime_collab::ActionDirectoryQuery>,
    resolutions: Vec<(u64, String, String, String)>,
    invocations: Vec<(u64, String, String, String, Option<String>, String)>,
    eager_invocations: usize,
    unsupported: bool,
    pending_method: Option<&'static str>,
    invoke_delay: Option<std::time::Duration>,
    status_calls: Vec<u64>,
}

fn directory_test_row() -> runtime_collab::ActionDirectoryRow {
    runtime_collab::ActionDirectoryRow {
        parent_resource: "notes/page".into(),
        resource: "notes/block".into(),
        resource_kind: "notes.block".into(),
        resource_label: "Link".into(),
        presentation: None,
        action: Some("open-link".into()),
        descriptor: None,
        has_children: false,
        visibility: None,
    }
}

macro_rules! directory_fake_methods {
    () => {
        fn query_action_directory(
            self: Rc<Self>,
            params: channel_handle::QueryActionDirectoryParams,
            mut results: channel_handle::QueryActionDirectoryResults,
        ) -> Promise<(), capnp::Error> {
            let result = (|| {
                let request = params.get()?.get_query()?;
                self.captures.lock().unwrap().directory.queries.push(
                    runtime_collab::ActionDirectoryQuery {
                        peer_id: request.get_peer_id(),
                        revision: request.get_revision()?.to_str()?.into(),
                        cursor: request.get_cursor()?.to_vec(),
                        needle: request.get_needle()?.to_str()?.into(),
                        parent_resource: request.get_parent_resource()?.to_str()?.into(),
                        max_results: request.get_max_results(),
                        max_bytes: request.get_max_bytes(),
                        max_work: request.get_max_work(),
                        max_depth: request.get_max_depth(),
                    },
                );
                let mut page = results.get().init_page();
                page.set_revision("revision-7");
                page.set_next_cursor(b"cursor-2");
                page.set_exhausted(false);
                page.set_budget_exhausted(true);
                runtime_collab::write_action_directory_row(
                    page.init_rows(1).get(0),
                    &directory_test_row(),
                )
            })();
            if self.captures.lock().unwrap().directory.pending_method == Some("query") {
                return Promise::from_future(std::future::pending());
            }
            Promise::from_future(async move { result })
        }
        fn resolve_published_action(
            self: Rc<Self>,
            params: channel_handle::ResolvePublishedActionParams,
            mut results: channel_handle::ResolvePublishedActionResults,
        ) -> Promise<(), capnp::Error> {
            let result = (|| {
                let request = params.get()?;
                self.captures.lock().unwrap().directory.resolutions.push((
                    request.get_peer_id(),
                    request.get_revision()?.to_str()?.into(),
                    request.get_resource()?.to_str()?.into(),
                    request.get_action()?.to_str()?.into(),
                ));
                runtime_collab::write_action_directory_row(
                    results.get().init_resolution().init_found(),
                    &directory_test_row(),
                )
            })();
            if self.captures.lock().unwrap().directory.pending_method == Some("resolve") {
                return Promise::from_future(std::future::pending());
            }
            Promise::from_future(async move { result })
        }
        fn invoke_resolved_published_action(
            self: Rc<Self>,
            params: channel_handle::InvokeResolvedPublishedActionParams,
            mut results: channel_handle::InvokeResolvedPublishedActionResults,
        ) -> Promise<(), capnp::Error> {
            let result = (|| {
                let request = params.get()?;
                let argument = if request.get_has_argument() {
                    Some(request.get_argument()?.to_str()?.to_owned())
                } else {
                    None
                };
                let mut captures = self.captures.lock().unwrap();
                captures.directory.invocations.push((
                    request.get_peer_id(),
                    request.get_revision()?.to_str()?.into(),
                    request.get_resource()?.to_str()?.into(),
                    request.get_action()?.to_str()?.into(),
                    argument,
                    request.get_confirmation_json()?.to_str()?.into(),
                ));
                if captures.directory.unsupported {
                    return Err(capnp::Error::unimplemented("old broker".into()));
                }
                results.get().set_id(47);
                results.get().set_update_json(
                    &serde_json::to_string(&tilo_actions::InvocationUpdate::Accepted {
                        invocation: tilo_actions::InvocationId(Uuid::from_u128(47)),
                    })
                    .unwrap(),
                );
                Ok(())
            })();
            if self.captures.lock().unwrap().directory.pending_method == Some("invoke") {
                return Promise::from_future(std::future::pending());
            }
            let delay = self.captures.lock().unwrap().directory.invoke_delay;
            Promise::from_future(async move {
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                result
            })
        }
        fn published_invocation_status(
            self: Rc<Self>,
            params: channel_handle::PublishedInvocationStatusParams,
            mut results: channel_handle::PublishedInvocationStatusResults,
        ) -> Promise<(), capnp::Error> {
            let result = (|| {
                let id = params.get()?.get_id();
                self.captures
                    .lock()
                    .unwrap()
                    .directory
                    .status_calls
                    .push(id);
                results.get().set_found(true);
                results.get().set_update_json(
                    &serde_json::to_string(&tilo_actions::InvocationUpdate::Accepted {
                        invocation: tilo_actions::InvocationId(Uuid::from_u128(u128::from(id))),
                    })
                    .unwrap(),
                );
                Ok(())
            })();
            Promise::from_future(async move { result })
        }
        fn invoke_published_action(
            self: Rc<Self>,
            _: channel_handle::InvokePublishedActionParams,
            _: channel_handle::InvokePublishedActionResults,
        ) -> Promise<(), capnp::Error> {
            self.captures.lock().unwrap().directory.eager_invocations += 1;
            Promise::err(capnp::Error::failed("eager fallback forbidden".into()))
        }
    };
}

fn directory_manager(
    unsupported: bool,
) -> (CollaborationManager, SharedCaptures, thread::JoinHandle<()>) {
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures::default())),
        op_backlog: vec![],
        presence_backlog: vec![],
        context_backlog: vec![],
        scrollback_response: None,
    };
    broker.captures.lock().unwrap().directory.unsupported = unsupported;
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    (
        CollaborationManager::from_sdk_fd_for_test(fd, "caller", Uuid::new_v4()),
        captures,
        thread,
    )
}

fn resolved_request(request_id: u64) -> CollabRequest {
    CollabRequest::InvokeResolvedPublishedAction {
        request_id,
        peer_id: 19,
        revision: "revision-7".into(),
        resource: "notes/block".into(),
        action: "open-link".into(),
        arguments: serde_json::Map::from_iter([("value".into(), serde_json::json!("exact"))]),
        confirmation: serde_json::json!({"confirm":true}),
    }
}

#[test]
fn sdk_lazy_directory_query_resolve_and_invoke_use_exact_runtime_methods() {
    let (mut manager, captures, thread) = directory_manager(false);
    let query = runtime_collab::ActionDirectoryQuery {
        peer_id: 19,
        revision: "revision-7".into(),
        cursor: b"cursor-1".to_vec(),
        needle: "link".into(),
        parent_resource: "notes/page".into(),
        max_results: 2,
        max_bytes: 8192,
        max_work: 10,
        max_depth: 2,
    };
    assert!(manager.send(CollabRequest::QueryActionDirectory {
        request_id: 11,
        query: query.clone()
    }));
    assert!(manager.send(CollabRequest::ResolvePublishedAction {
        request_id: 12,
        peer_id: 19,
        revision: "revision-7".into(),
        resource: "notes/block".into(),
        action: "open-link".into()
    }));
    assert!(manager.send(resolved_request(13)));
    let mut replies = vec![];
    tilo_test_env::wait_until("the directory replies", || {
        replies.extend(manager.poll().into_iter().filter(|event| {
            matches!(
                event,
                CollabEvent::ActionDirectoryPage { .. }
                    | CollabEvent::PublishedActionResolution { .. }
                    | CollabEvent::PublishedInvocationResult { .. }
            )
        }));
        replies.len() == 3
    });
    assert!(
        matches!(&replies[0], CollabEvent::ActionDirectoryPage {request_id:11,result:Ok(page)} if page.rows==vec![directory_test_row()] && page.revision=="revision-7" && page.next_cursor==b"cursor-2" && page.budget_exhausted)
    );
    assert!(
        matches!(&replies[1], CollabEvent::PublishedActionResolution {request_id:12,result:Ok(runtime_collab::ActionDirectoryResolution::Found { row, revision: None })} if **row==directory_test_row())
    );
    assert!(
        matches!(&replies[2], CollabEvent::PublishedInvocationResult {request_id:13,result:Ok((47,tilo_actions::InvocationUpdate::Accepted { invocation }))} if invocation.0==Uuid::from_u128(47))
    );
    let captured = captures.lock().unwrap();
    assert_eq!(captured.directory.queries, vec![query]);
    assert_eq!(
        captured.directory.resolutions,
        vec![(
            19,
            "revision-7".into(),
            "notes/block".into(),
            "open-link".into()
        )]
    );
    assert_eq!(
        captured.directory.invocations,
        vec![(
            19,
            "revision-7".into(),
            "notes/block".into(),
            "open-link".into(),
            Some(format!(
                "{}{{\"value\":\"exact\"}}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            )),
            "{\"confirm\":true}".into()
        )]
    );
    assert_eq!(captured.directory.eager_invocations, 0);
    drop(captured);
    drop(manager);
    thread.join().unwrap();
}

#[test]
fn sdk_lazy_directory_unsupported_method_never_falls_back_to_eager() {
    let (mut manager, captures, thread) = directory_manager(true);
    assert!(manager.send(resolved_request(14)));
    let mut result = None;
    tilo_test_env::wait_until("the directory replies", || {
        for event in manager.poll() {
            if let CollabEvent::PublishedInvocationResult {
                request_id: 14,
                result: reply,
            } = event
            {
                result = Some(reply);
            }
        }
        result.is_some()
    });
    assert!(result.unwrap().is_err());
    let captured = captures.lock().unwrap();
    assert_eq!(
        captured.directory.invocations.len(),
        1,
        "the actual new method must be attempted"
    );
    assert_eq!(captured.directory.eager_invocations, 0);
    drop(captured);
    drop(manager);
    thread.join().unwrap();
}

#[test]
fn sdk_lazy_directory_queue_rejects_retained_capacity_before_rpc() {
    let (mut manager, captures, thread) = directory_manager(false);
    let mut request = resolved_request(15);
    if let CollabRequest::InvokeResolvedPublishedAction { revision, .. } = &mut request {
        let mut retained = String::with_capacity(SDK_COMMAND_QUEUE_BYTES + 1);
        retained.push_str(revision);
        *revision = retained;
    }
    assert!(
        !manager.send(request),
        "short text with excessive retained capacity must still exceed the bounded queue"
    );
    assert!(captures.lock().unwrap().directory.invocations.is_empty());
    drop(manager);
    thread.join().unwrap();
}

fn pending_directory_rpc_releases_lane(method: &'static str) {
    if method == "invoke" {
        super::directory_client::set_test_admission_timeout(Duration::from_secs(2));
    }
    let (mut manager, captures, thread) = directory_manager(false);
    captures.lock().unwrap().directory.pending_method = Some(method);
    let request = match method {
        "query" => CollabRequest::QueryActionDirectory {
            request_id: 20,
            query: runtime_collab::ActionDirectoryQuery {
                peer_id: 19,
                revision: "revision-7".into(),
                cursor: vec![],
                needle: String::new(),
                parent_resource: String::new(),
                max_results: 1,
                max_bytes: 8192,
                max_work: 10,
                max_depth: 1,
            },
        },
        "resolve" => CollabRequest::ResolvePublishedAction {
            request_id: 20,
            peer_id: 19,
            revision: "revision-7".into(),
            resource: "notes/block".into(),
            action: "open-link".into(),
        },
        "invoke" => resolved_request(20),
        _ => unreachable!(),
    };
    assert!(manager.send(request));
    assert!(manager.send(CollabRequest::PublishedInvocationStatus {
        request_id: 21,
        id: 47
    }));
    let mut timeout = None;
    let mut progressed = false;
    tilo_test_env::wait_until(
        format_args!("the pending {method} RPC to time out without blocking the status lane"),
        || {
            for event in manager.poll() {
                match event {
                    CollabEvent::ActionDirectoryPage {
                        request_id: 20,
                        result: Err(error),
                    }
                    | CollabEvent::PublishedActionResolution {
                        request_id: 20,
                        result: Err(error),
                    }
                    | CollabEvent::PublishedInvocationResult {
                        request_id: 20,
                        result: Err(error),
                    } => timeout = Some(error),
                    CollabEvent::PublishedInvocationResult {
                        request_id: 21,
                        result: Ok((47, tilo_actions::InvocationUpdate::Accepted { .. })),
                    } => progressed = true,
                    _ => {}
                }
            }
            timeout.is_some() && progressed
        },
    );
    let captured = captures.lock().unwrap();
    let calls = match method {
        "query" => captured.directory.queries.len(),
        "resolve" => captured.directory.resolutions.len(),
        _ => captured.directory.invocations.len(),
    };
    let statuses = captured.directory.status_calls.clone();
    let eager = captured.directory.eager_invocations;
    drop(captured);
    drop(manager);
    thread.join().unwrap();
    assert_eq!(calls, 1, "timeout must not retry the request");
    assert_eq!(eager, 0, "timeout must not fall back to eager invocation");
    assert_eq!(statuses, vec![47]);
    let error = timeout.unwrap();
    if method == "invoke" {
        assert!(
            error.starts_with("resolved_invocation_outcome_unknown:"),
            "{error}"
        );
        assert!(error.contains("do not retry"), "{error}");
    } else {
        assert!(error.contains("timed out"), "{error}");
    }
}
#[test]
fn sdk_directory_deadline_pending_query_releases_status_lane() {
    pending_directory_rpc_releases_lane("query");
}
#[test]
fn sdk_directory_deadline_pending_resolve_releases_status_lane() {
    pending_directory_rpc_releases_lane("resolve");
}
#[test]
fn sdk_directory_deadline_pending_invoke_is_unknown_and_releases_status_lane() {
    pending_directory_rpc_releases_lane("invoke");
}

/// Admission answered after longer than any short RPC bound (a durable
/// journal on a loaded host) is a definite result, never an unknown outcome.
#[test]
fn sdk_slow_admission_answer_is_definite() {
    let (mut manager, captures, thread) = directory_manager(false);
    captures.lock().unwrap().directory.invoke_delay = Some(Duration::from_secs(3));
    assert!(manager.send(resolved_request(30)));
    let mut result = None;
    tilo_test_env::wait_until("the slow admission answer", || {
        for event in manager.poll() {
            if let CollabEvent::PublishedInvocationResult {
                request_id: 30,
                result: answer,
            } = event
            {
                result = Some(answer);
            }
        }
        result.is_some()
    });
    drop(manager);
    thread.join().unwrap();
    let (id, update) = result
        .unwrap()
        .expect("a slow admission is answered definitely");
    assert_eq!(id, 47);
    assert!(matches!(
        update,
        tilo_actions::InvocationUpdate::Accepted { .. }
    ));
}
