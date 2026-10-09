//! A `tilo_invoke` waits on its host's word that the invocation changed,
//! not a poll interval, and one session's requests overlap: `tilo_status`
//! and `tilo_cancel` answer while a `tilo_invoke` of the same session still
//! waits, over HTTP, stdio and the daemon's native socket alike.
use super::*;
use crate::mcp::{MAX_ADMITTED, MAX_WORKING};
use std::collections::HashSet;
use std::io::{BufRead as _, Write as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tilo_actions::InvocationId;
use tilo_actions::remote::Completer;

/// A host whose invocations finish when the test says so, or when they are
/// cancelled, and which says when one changed, as the broker's hosts do:
/// only to the caller that may observe it.
#[derive(Clone, Default)]
pub(crate) struct GatedHost {
    stub: StubHost,
    owner: Arc<StdMutex<Option<Caller>>>,
    finished: Arc<StdMutex<HashSet<InvocationId>>>,
    cancelled: Arc<StdMutex<HashSet<InvocationId>>>,
    waiting: Arc<StdMutex<Vec<Completer<()>>>>,
    observes: Arc<AtomicUsize>,
    waits: Arc<AtomicUsize>,
    /// Every invocation finishes as soon as it starts.
    at_once: bool,
    /// Refuse an invocation naming no revision as an app built before
    /// revisionless resolution does: as a moved revision.
    stale_without_revision: bool,
    queries: Arc<StdMutex<Vec<CatalogueQuery>>>,
}

impl GatedHost {
    /// Finish `id` and wake whoever waits on a change.
    pub(crate) fn finish(&self, id: InvocationId) {
        self.finished.lock().unwrap().insert(id);
        self.wake();
    }

    fn wake(&self) {
        for waiter in self.waiting.lock().unwrap().drain(..) {
            waiter.complete(());
        }
    }

    pub(crate) fn invoked(&self) -> Vec<InvocationId> {
        self.stub
            .invoked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.invocation)
            .collect()
    }

    fn owns(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.owner.lock().unwrap().as_ref() == Some(caller) && self.invoked().contains(id)
    }
}

impl ActionHost for GatedHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.queries.lock().unwrap().push(query.clone());
        self.stub.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if self.stale_without_revision && request.revision.is_none() {
            return Pending::ready(Err(ActionHostError::InvalidRequest {
                reason: "the action changed since that revision; list it again".into(),
            }));
        }
        self.owner.lock().unwrap().replace(caller.clone());
        self.stub.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.observes.fetch_add(1, Ordering::SeqCst);
        if !self.owns(caller, id) {
            return None;
        }
        let update = if self.cancelled.lock().unwrap().contains(id) {
            InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            }
        } else if self.at_once || self.finished.lock().unwrap().contains(id) {
            InvocationUpdate::Succeeded {
                invocation: *id,
                output: tilo_actions::ActionOutput::default(),
            }
        } else {
            InvocationUpdate::Accepted { invocation: *id }
        };
        Some(InvocationState::new(*id, update))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if !self.owns(caller, id) {
            return None;
        }
        self.waits.fetch_add(1, Ordering::SeqCst);
        let (waiter, change) = tilo_actions::remote::pending();
        self.waiting.lock().unwrap().push(waiter);
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        if !self.owns(caller, id) {
            return Err(ActionHostError::UnknownInvocation);
        }
        self.cancelled.lock().unwrap().insert(*id);
        self.wake();
        Ok(())
    }
}

fn gated_endpoint(host: GatedHost) -> Arc<Endpoint<GatedHost>> {
    Endpoint::new(host, "room".into(), "owner".into(), None, None)
}

async fn gated_session(endpoint: &Arc<Endpoint<GatedHost>>) -> String {
    let response = endpoint
        .dispatch(&Method::POST, "/mcp", &HeaderMap::new(), &initialize("a"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get(SESSION_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

async fn call(
    endpoint: &Arc<Endpoint<GatedHost>>,
    session: &str,
    name: &str,
    arguments: Value,
) -> Value {
    let request = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}});
    let response = endpoint
        .dispatch(
            &Method::POST,
            "/mcp",
            &headers(&[(SESSION_HEADER, session)]),
            request.to_string().as_bytes(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body(response).await["result"].clone()
}

fn long_invoke(invocation: InvocationId) -> Value {
    json!({
        "peer_id": 71, "revision": "1", "resource": "test.note:1",
        "action": tilo_resource::APPLY, "arguments": {},
        "invocation_id": invocation.0.to_string(), "wait_ms": 20_000
    })
}

/// Yield to the session's other requests until the host has been asked to
/// start `count` invocations.
async fn until_invoked(host: &GatedHost, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while host.invoked().len() < count {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the invocation reaches the host");
}

/// While a `tilo_invoke` waits, the same session's `tilo_status` answers;
/// the invoke then wakes on the host's change, observing once before it
/// waits and once after, however long the action ran.
#[test]
fn status_answers_while_the_same_session_waits_on_an_invocation() {
    let host = GatedHost::default();
    let endpoint = gated_endpoint(host.clone());
    // Many poll intervals of the old 25 ms loop.
    let ran_for = std::time::Duration::from_millis(250);
    run(async {
        let session = gated_session(&endpoint).await;
        let invocation = InvocationId::new();
        let invoking = call(&endpoint, &session, "tilo_invoke", long_invoke(invocation));
        let meanwhile = async {
            until_invoked(&host, 1).await;
            let status = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                call(
                    &endpoint,
                    &session,
                    "tilo_status",
                    json!({"invocation_id": invocation.0.to_string()}),
                ),
            )
            .await
            .expect("tilo_status does not wait behind the session's tilo_invoke");
            tokio::time::sleep(ran_for).await;
            let observed = host.observes.load(Ordering::SeqCst);
            host.finish(invocation);
            (status, observed)
        };
        let (invoked, (status, observed)) = tokio::join!(invoking, meanwhile);
        assert_eq!(
            status["structuredContent"]["update"]["state"], "accepted",
            "{status}"
        );
        assert_eq!(
            observed, 2,
            "the invoke observed once and then waited; tilo_status observed once"
        );
        assert_eq!(
            invoked["structuredContent"]["update"]["state"], "succeeded",
            "{invoked}"
        );
        assert_eq!(
            host.observes.load(Ordering::SeqCst),
            3,
            "woken once by the change, the invoke observed once more"
        );
        assert_eq!(host.waits.load(Ordering::SeqCst), 2);
    });
}

/// Overlapping calls naming one invocation id run one after the other:
/// the second reaches the host only once the first has its answer, so a
/// retry racing its original cannot start or retry it a second time.
/// Calls naming different ids overlap.
#[test]
fn overlapping_calls_with_one_invocation_id_run_one_after_the_other() {
    let host = GatedHost::default();
    let endpoint = gated_endpoint(host.clone());
    run(async {
        let session = gated_session(&endpoint).await;
        let shared = InvocationId::new();
        let other = InvocationId::new();
        let first = call(&endpoint, &session, "tilo_invoke", long_invoke(shared));
        let retry = call(&endpoint, &session, "tilo_invoke", long_invoke(shared));
        let separate = call(&endpoint, &session, "tilo_invoke", long_invoke(other));
        let meanwhile = async {
            until_invoked(&host, 2).await;
            // Time for the retry to reach the host, were it not held.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let before = host.invoked();
            host.finish(other);
            host.finish(shared);
            before
        };
        let (first, retry, separate, before) = tokio::join!(first, retry, separate, meanwhile);
        let mut started = before.clone();
        started.sort_by_key(|id| id.0);
        let mut expected = vec![shared, other];
        expected.sort_by_key(|id| id.0);
        assert_eq!(
            started, expected,
            "the other id overlapped the first; the retry was held"
        );
        for reply in [&first, &retry, &separate] {
            assert_eq!(
                reply["structuredContent"]["update"]["state"], "succeeded",
                "{reply}"
            );
        }
        assert_eq!(
            host.invoked(),
            [before, vec![shared]].concat(),
            "the retry reached the host after the first answered"
        );
    });
}

/// A call learns which instances run from one listing of who publishes,
/// asking for no rows: the host need not assemble its whole catalogue to
/// answer it. The instance's app comes from that same listing, so recording
/// which app produced an answer's content lists nothing again; a later
/// call on the instance that listing named lists nothing at all.
#[test]
fn a_call_lists_live_instances_once_and_asks_for_no_rows() {
    let host = GatedHost {
        at_once: true,
        ..GatedHost::default()
    };
    let endpoint = gated_endpoint(host.clone());
    endpoint.serve_media(Some(Arc::new(StoredByTestApp)));
    let listings = || {
        host.queries
            .lock()
            .unwrap()
            .drain(..)
            .filter(|query| query.provider.is_none())
            .map(|query| query.max_results)
            .collect::<Vec<_>>()
    };
    run(async {
        let session = gated_session(&endpoint).await;
        listings();
        let single = call(
            &endpoint,
            &session,
            "tilo_invoke",
            long_invoke(InvocationId::new()),
        )
        .await;
        assert_eq!(
            single["structuredContent"]["update"]["state"], "succeeded",
            "{single}"
        );
        assert_eq!(listings(), [0], "one listing, of publishers alone");
        let step = json!({"resource": "test.note:1", "action": tilo_resource::APPLY});
        let batch = call(
            &endpoint,
            &session,
            "tilo_invoke",
            json!({"peer_id": 71, "revision": "1", "calls": [step, step, step]}),
        )
        .await;
        assert!(
            batch["structuredContent"].get("stopped").is_none(),
            "{batch}"
        );
        assert!(
            listings().is_empty(),
            "a batch on an instance the session listed lists nothing again (TIL-588)"
        );
        let apps = call(&endpoint, &session, "tilo_list_apps", json!({})).await;
        assert_eq!(
            apps["structuredContent"]["apps"][0]["peer_id"], 71,
            "{apps}"
        );
        assert_eq!(listings(), [0]);
    });
}

/// The revisions an invoke was started with, and how many directory pages
/// of the instance were read on the way.
fn revisions_and_reads(host: &GatedHost) -> (Vec<Option<String>>, usize) {
    let revisions = host
        .stub
        .invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.revision.clone())
        .collect();
    let reads = host
        .queries
        .lock()
        .unwrap()
        .iter()
        .filter(|query| query.provider.as_deref() == Some("71"))
        .count();
    (revisions, reads)
}

fn current_invoke() -> Value {
    json!({"peer_id": 71, "resource": "test.note:1", "action": tilo_resource::APPLY})
}

/// An invoke that names no revision passes none on: the host resolves the
/// directory as it is now when it admits the action, so the bridge reads no
/// page of the app's directory first, which was a round trip to the app of
/// its own.
#[test]
fn an_omitted_revision_reads_no_directory_page_first() {
    let host = GatedHost {
        at_once: true,
        ..GatedHost::default()
    };
    let endpoint = gated_endpoint(host.clone());
    run(async {
        let session = gated_session(&endpoint).await;
        let reply = call(&endpoint, &session, "tilo_invoke", current_invoke()).await;
        assert_eq!(
            reply["structuredContent"]["update"]["state"], "succeeded",
            "{reply}"
        );
    });
    assert_eq!(revisions_and_reads(&host), (vec![None], 0));
}

/// An app built before revisionless resolution refuses an invoke naming no
/// revision as a moved one; the bridge then reads the revision it is at and
/// starts the action once with it.
#[test]
fn an_app_that_needs_a_revision_is_given_the_current_one() {
    let host = GatedHost {
        at_once: true,
        stale_without_revision: true,
        ..GatedHost::default()
    };
    let endpoint = gated_endpoint(host.clone());
    run(async {
        let session = gated_session(&endpoint).await;
        let reply = call(&endpoint, &session, "tilo_invoke", current_invoke()).await;
        assert_eq!(
            reply["structuredContent"]["update"]["state"], "succeeded",
            "{reply}"
        );
    });
    assert_eq!(
        revisions_and_reads(&host),
        (vec![Some("1".to_owned())], 1),
        "one read of the revision, one start with it"
    );
}

/// The state `tilo_status` or `tilo_cancel` of `invocation` answers.
async fn about(
    endpoint: &Arc<Endpoint<GatedHost>>,
    session: &str,
    tool: &str,
    invocation: InvocationId,
) -> Value {
    let arguments = json!({"invocation_id": invocation.0.to_string()});
    call(endpoint, session, tool, arguments).await["structuredContent"]["update"]["state"].clone()
}

/// Start an invoke of each of `ids` in one session, each on its own task.
fn invoke_all(
    endpoint: &Arc<Endpoint<GatedHost>>,
    session: &str,
    ids: &[InvocationId],
) -> tokio::task::JoinSet<Value> {
    let mut calls = tokio::task::JoinSet::new();
    for id in ids.iter().copied() {
        let (endpoint, session) = (Arc::clone(endpoint), session.to_owned());
        calls.spawn(async move {
            let reply = call(&endpoint, &session, "tilo_invoke", long_invoke(id)).await;
            reply["structuredContent"]["update"]["state"].clone()
        });
    }
    calls
}

/// How many of the calls ended in each state, once all have.
async fn states(
    mut calls: tokio::task::JoinSet<Value>,
) -> std::collections::BTreeMap<String, usize> {
    let mut states = std::collections::BTreeMap::new();
    while let Some(state) = calls.join_next().await {
        let state = state.unwrap();
        *states
            .entry(state.as_str().unwrap_or("none").to_owned())
            .or_default() += 1;
    }
    states
}

/// A `tilo_cancel` answers at once while the `tilo_invoke` it stops still
/// waits, and the invoke then answers that it was cancelled: the cancel
/// does not wait behind the very call it cancels.
#[test]
fn a_cancel_answers_at_once_while_its_invoke_waits_and_stops_it() {
    let host = GatedHost::default();
    let endpoint = gated_endpoint(host.clone());
    run(async {
        let session = gated_session(&endpoint).await;
        let invocation = InvocationId::new();
        let invoking = call(&endpoint, &session, "tilo_invoke", long_invoke(invocation));
        let cancelling = async {
            until_invoked(&host, 1).await;
            let asked = Instant::now();
            let cancel = about(&endpoint, &session, "tilo_cancel", invocation).await;
            (cancel, asked.elapsed())
        };
        let (invoked, (cancel, took)) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(invoking, cancelling)
        })
        .await
        .expect("the cancel stops the invoke well before its wait ends");
        assert!(
            took < Duration::from_millis(100),
            "the cancel answered after {took:?}"
        );
        assert_eq!(cancel, "cancelled");
        assert_eq!(
            invoked["structuredContent"]["update"]["state"], "cancelled",
            "{invoked}"
        );
    });
}

/// One session's calls work at once up to [`MAX_WORKING`]: that many
/// invokes all reach the host while none has finished, one more waits for
/// a turn, and `tilo_status` and `tilo_cancel` still answer beside them.
/// The cancelled call's turn goes to the one that waited.
#[test]
fn a_session_works_its_calls_at_once_up_to_its_bound() {
    let host = GatedHost::default();
    let endpoint = gated_endpoint(host.clone());
    run(async {
        let session = gated_session(&endpoint).await;
        let ids: Vec<_> = (0..=MAX_WORKING).map(|_| InvocationId::new()).collect();
        let calls = invoke_all(&endpoint, &session, &ids);
        until_invoked(&host, MAX_WORKING).await;
        // Time for one more to reach the host, were it not held.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = host.invoked();
        assert_eq!(started.len(), MAX_WORKING, "one call waits for a turn");
        let stopped = started[0];
        assert_eq!(
            about(&endpoint, &session, "tilo_status", stopped).await,
            "accepted"
        );
        assert_eq!(
            about(&endpoint, &session, "tilo_cancel", stopped).await,
            "cancelled"
        );
        until_invoked(&host, MAX_WORKING + 1).await;
        for id in &ids {
            host.finish(*id);
        }
        assert_eq!(
            states(calls).await,
            [
                ("cancelled".to_owned(), 1),
                ("succeeded".to_owned(), MAX_WORKING)
            ]
            .into(),
        );
    });
}

/// Beyond [`MAX_ADMITTED`] calls working or waiting, a session refuses one
/// more at once rather than queue it, and `tilo_status` still answers.
#[test]
fn a_session_refuses_calls_beyond_those_it_admits() {
    let host = GatedHost::default();
    let endpoint = gated_endpoint(host.clone());
    run(async {
        let session = gated_session(&endpoint).await;
        let ids: Vec<_> = (0..MAX_ADMITTED).map(|_| InvocationId::new()).collect();
        let calls = invoke_all(&endpoint, &session, &ids);
        until_invoked(&host, MAX_WORKING).await;
        // Time for every call to take its place.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let request = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{
            "name":"tilo_invoke","arguments":long_invoke(InvocationId::new())
        }});
        let refused = tokio::time::timeout(
            Duration::from_secs(1),
            endpoint.dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[(SESSION_HEADER, &session)]),
                request.to_string().as_bytes(),
            ),
        )
        .await
        .expect("a call beyond the bound is refused, not queued");
        let refused = body(refused).await;
        assert_eq!(refused["error"]["code"], -32000, "{refused}");
        assert_eq!(host.invoked().len(), MAX_WORKING);
        assert_eq!(
            about(&endpoint, &session, "tilo_status", host.invoked()[0]).await,
            "accepted"
        );
        for id in &ids {
            host.finish(*id);
        }
        assert_eq!(
            states(calls).await,
            [("succeeded".to_owned(), MAX_ADMITTED)].into()
        );
    });
}

/// Wait, a bounded while, until `condition` holds.
fn wait_until(condition: impl Fn() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// One MCP session over a newline-framed connection whose other end is
/// `client`, used as an agent overlaps its calls: two invokes wait at once,
/// a `tilo_status` and a `tilo_cancel` answer while they wait — the cancel
/// in under 100 ms, stopping its invoke — and each reply comes back whole,
/// on a line of its own, as soon as it is ready rather than in the order
/// asked. Closing the client's side ends the connection.
pub(crate) fn overlapping_lines(host: &GatedHost, client: std::os::unix::net::UnixStream) {
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut lines = std::io::BufReader::new(client.try_clone().unwrap()).lines();
    let mut reply = || -> Value {
        let line = lines.next().expect("a reply line").unwrap();
        serde_json::from_str(&line).expect("each line is one whole JSON reply")
    };
    let send = |id: u64, method: &str, params: Value| {
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        writeln!(&client, "{request}").unwrap();
    };
    let tool = |name: &str, invocation: InvocationId| {
        let arguments = if name == "tilo_invoke" {
            long_invoke(invocation)
        } else {
            json!({"invocation_id": invocation.0.to_string()})
        };
        json!({"name": name, "arguments": arguments})
    };
    let state = |reply: &Value| reply["result"]["structuredContent"]["update"]["state"].clone();
    send(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"lines","version":"1"}}),
    );
    assert_eq!(reply()["id"], 1);
    let (cancelled, finished) = (InvocationId::new(), InvocationId::new());
    send(2, "tools/call", tool("tilo_invoke", cancelled));
    send(3, "tools/call", tool("tilo_invoke", finished));
    wait_until(
        || host.invoked().len() == 2,
        "both invokes reach the host while neither has finished",
    );
    send(4, "tools/call", tool("tilo_status", cancelled));
    let status = reply();
    assert_eq!(
        status["id"], 4,
        "the status answers while both invokes wait"
    );
    assert_eq!(state(&status), "accepted", "{status}");
    let asked = Instant::now();
    send(5, "tools/call", tool("tilo_cancel", cancelled));
    let mut answered = std::collections::BTreeMap::new();
    while !answered.contains_key(&5) {
        let next = reply();
        answered.insert(next["id"].as_u64().unwrap(), next);
    }
    let took = asked.elapsed();
    assert!(
        took < Duration::from_millis(100),
        "the cancel answered after {took:?}"
    );
    assert_eq!(state(&answered[&5]), "cancelled", "{}", answered[&5]);
    if !answered.contains_key(&2) {
        let next = reply();
        answered.insert(next["id"].as_u64().unwrap(), next);
    }
    assert_eq!(state(&answered[&2]), "cancelled", "{}", answered[&2]);
    host.finish(finished);
    let done = reply();
    assert_eq!(done["id"], 3, "{done}");
    assert_eq!(state(&done), "succeeded", "{done}");
    client.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(lines.next().is_none(), "no reply is left over");
}
