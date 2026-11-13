//! A `tilo_invoke` waits on its host's word that the invocation changed,
//! not a poll interval, and one session's requests overlap: `tilo_status`
//! and `tilo_cancel` answer while a `tilo_invoke` of the same session still
//! waits.
use super::*;
use crate::mcp::{MAX_ADMITTED, MAX_WORKING};
use crate::mcp_test_hosts::{
    GatedHost, Stale, StoredByTestApp, StubHost, WithoutRevision, long_invoke, run, run_paused,
    tool,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tilo_actions::InvocationId;

/// The session a test's calls share.
fn gated_server(host: GatedHost) -> Arc<McpServer<GatedHost>> {
    Arc::new(McpServer::new(host, "workspace".into(), "owner".into()))
}

/// Open the session, as a client's connection does before its first call.
async fn initialized(server: &Arc<McpServer<GatedHost>>) {
    let opened = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"a","version":"1"}}}))
        .await;
    assert!(
        opened.is_some_and(|reply| reply.get("result").is_some()),
        "the session opens"
    );
}

/// Yield to the session's other requests until the host has been asked to
/// start `count` invocations.
async fn until_invoked(host: &GatedHost, count: usize) {
    tilo_test_env::within("the invocation to reach the host", async {
        while host.invoked().len() < count {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await;
}

/// While a `tilo_invoke` waits, the same session's `tilo_status` answers;
/// the invoke then wakes on the host's change, observing once before it
/// waits and once after, however long the action ran.
#[test]
fn status_answers_while_the_same_session_waits_on_an_invocation() {
    let host = GatedHost::default();
    let server = gated_server(host.clone());
    // Many poll intervals of the old 25 ms loop, on the paused clock.
    let ran_for = std::time::Duration::from_millis(250);
    run_paused(async {
        initialized(&server).await;
        let invocation = InvocationId::new();
        let invoking = tool(&server, "tilo_invoke", long_invoke(invocation));
        let meanwhile = async {
            until_invoked(&host, 1).await;
            // Well within the invoke's wait: an answer at all means the
            // status did not wait behind it.
            let status = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tool(
                    &server,
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
    let server = gated_server(host.clone());
    run_paused(async {
        initialized(&server).await;
        let shared = InvocationId::new();
        let other = InvocationId::new();
        let first = tool(&server, "tilo_invoke", long_invoke(shared));
        let retry = tool(&server, "tilo_invoke", long_invoke(shared));
        let separate = tool(&server, "tilo_invoke", long_invoke(other));
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
    let server = Arc::new(
        McpServer::new(host.clone(), "workspace".into(), "owner".into())
            .with_media(Some(Arc::new(StoredByTestApp))),
    );
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
        initialized(&server).await;
        listings();
        let single = tool(&server, "tilo_invoke", long_invoke(InvocationId::new())).await;
        assert_eq!(
            single["structuredContent"]["update"]["state"], "succeeded",
            "{single}"
        );
        assert_eq!(listings(), [0], "one listing, of publishers alone");
        let step = json!({"resource": "test.note:1", "action": tilo_resource::APPLY});
        let batch = tool(
            &server,
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
            "a batch on an instance the session listed lists nothing again"
        );
        let apps = tool(&server, "tilo_list_apps", json!({})).await;
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
    let server = gated_server(host.clone());
    run(async {
        initialized(&server).await;
        let reply = tool(&server, "tilo_invoke", current_invoke()).await;
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
        without_revision: Some(WithoutRevision::Stale),
        ..GatedHost::default()
    };
    let server = gated_server(host.clone());
    run(async {
        initialized(&server).await;
        let reply = tool(&server, "tilo_invoke", current_invoke()).await;
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

/// An admission the host has not answered within the call's wait goes on
/// without the call, which answers it as pending under its invocation id,
/// for the agent to invoke again with that id, rather than as a failure
/// that names no id to retry with.
#[test]
fn an_unanswered_admission_is_pending_under_its_invocation_id() {
    let host = GatedHost {
        admission_unanswered: true,
        ..GatedHost::default()
    };
    let server = gated_server(host.clone());
    run_paused(async {
        initialized(&server).await;
        let invocation = InvocationId::new();
        let reply = tool(&server, "tilo_invoke", long_invoke(invocation)).await;
        assert_eq!(reply["isError"], false, "{reply}");
        assert_eq!(
            reply["structuredContent"]["invocation_id"],
            invocation.0.to_string(),
            "{reply}"
        );
        assert!(
            reply["structuredContent"]["pending"]
                .as_str()
                .is_some_and(|pending| pending.contains("20000 ms")),
            "the admission was awaited as long as the call waits: {reply}"
        );
    });
    assert_eq!(host.invoked().len(), 1, "the admission was asked for once");
}

/// The state `tilo_status` or `tilo_cancel` of `invocation` answers.
async fn about(server: &Arc<McpServer<GatedHost>>, name: &str, invocation: InvocationId) -> Value {
    let arguments = json!({"invocation_id": invocation.0.to_string()});
    tool(server, name, arguments).await["structuredContent"]["update"]["state"].clone()
}

/// Start an invoke of each of `ids` in the session, each on its own task.
fn invoke_all(
    server: &Arc<McpServer<GatedHost>>,
    ids: &[InvocationId],
) -> tokio::task::JoinSet<Value> {
    let mut calls = tokio::task::JoinSet::new();
    for id in ids.iter().copied() {
        let server = Arc::clone(server);
        calls.spawn(async move {
            let reply = tool(&server, "tilo_invoke", long_invoke(id)).await;
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
        let count = states
            .entry(state.as_str().unwrap_or("none").to_owned())
            .or_default();
        *count = usize::saturating_add(*count, 1);
    }
    states
}

/// A `tilo_cancel` answers at once while the `tilo_invoke` it stops still
/// waits, and the invoke then answers that it was cancelled: the cancel
/// does not wait behind the very call it cancels.
#[test]
fn a_cancel_answers_at_once_while_its_invoke_waits_and_stops_it() {
    let host = GatedHost::default();
    let server = gated_server(host.clone());
    // On the paused clock the cancel's answer takes no time unless it waits
    // on a timer, as it would behind the invoke's wait.
    run_paused(async {
        initialized(&server).await;
        let invocation = InvocationId::new();
        let invoking = Box::pin(tool(&server, "tilo_invoke", long_invoke(invocation)));
        let cancelling = async {
            until_invoked(&host, 1).await;
            let asked = tokio::time::Instant::now();
            let cancel = about(&server, "tilo_cancel", invocation).await;
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
    let server = gated_server(host.clone());
    run_paused(async {
        initialized(&server).await;
        let ids: Vec<_> = (0..=MAX_WORKING).map(|_| InvocationId::new()).collect();
        let calls = invoke_all(&server, &ids);
        until_invoked(&host, MAX_WORKING).await;
        // Time for one more to reach the host, were it not held.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = host.invoked();
        assert_eq!(started.len(), MAX_WORKING, "one call waits for a turn");
        let stopped = started[0];
        assert_eq!(about(&server, "tilo_status", stopped).await, "accepted");
        assert_eq!(about(&server, "tilo_cancel", stopped).await, "cancelled");
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
    let server = gated_server(host.clone());
    run_paused(async {
        initialized(&server).await;
        let ids: Vec<_> = (0..MAX_ADMITTED).map(|_| InvocationId::new()).collect();
        let calls = invoke_all(&server, &ids);
        until_invoked(&host, MAX_WORKING).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let request = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{
            "name":"tilo_invoke","arguments":long_invoke(InvocationId::new())
        }});
        let refused = tokio::time::timeout(Duration::from_secs(1), server.handle(request))
            .await
            .expect("a call beyond the bound is refused, not queued")
            .expect("the session answers");
        assert_eq!(refused["error"]["code"], -32000, "{refused}");
        assert_eq!(host.invoked().len(), MAX_WORKING);
        assert_eq!(
            about(&server, "tilo_status", host.invoked()[0]).await,
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

/// A directory action asking for confirmation needs the revision it is
/// confirmed at; the bridge, which confirms on its client's policy, reads
/// the revision it is at and starts the action once with it.
#[test]
fn a_confirmed_action_is_given_the_revision_it_is_confirmed_at() {
    let host = GatedHost {
        at_once: true,
        without_revision: Some(WithoutRevision::NeedsConfirmation),
        ..GatedHost::default()
    };
    let server = gated_server(host.clone());
    run(async {
        initialized(&server).await;
        let reply = tool(&server, "tilo_invoke", current_invoke()).await;
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

/// An exact-phrase contract is answered by the person on their device, at
/// the revision the bridge read, and the retry names that revision, so a
/// contract that moved on since is refused rather than answered stale.
#[test]
fn phrase_retry_names_the_revision_it_read() {
    let host = GatedHost {
        at_once: true,
        stub: StubHost {
            confirmation: tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "ERASE".into(),
            },
            ..StubHost::default()
        },
        ..GatedHost::default()
    };
    let server = gated_server(host.clone());
    run(async {
        initialized(&server).await;
        let reply = tool(&server, "tilo_invoke", current_invoke()).await;
        assert_eq!(
            reply["structuredContent"]["update"]["state"], "succeeded",
            "{reply}"
        );
    });
    let started = host.stub.invoked.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(
        started[0].confirmation,
        Some(tilo_actions::remote::Confirmation::Witnessed(format!(
            "person:{}",
            started[0].invocation.0
        )))
    );
    assert_eq!(
        started[0].revision.as_deref(),
        Some("1"),
        "the phrase retry is bound to the revision its phrase was read at"
    );
}

fn current_resolve() -> Value {
    json!({"peer_id": 71, "resource": "test.note:1", "action": tilo_resource::APPLY})
}

/// A directory that moved past the revision a caller named answers what
/// happened and what to do, rather than a bare code: the caller read a
/// revision the app has since left.
#[test]
fn a_stale_read_says_what_happened_and_what_to_do() {
    let host = GatedHost {
        stale: Some(Stale::Reads),
        ..GatedHost::default()
    };
    let server = gated_server(host);
    run(async {
        initialized(&server).await;
        let mut args = current_resolve();
        args["revision"] = json!("0");
        let reply = tool(&server, "tilo_resolve", args).await;
        let said = reply["isError"].as_bool().unwrap_or(false);
        let text = reply.to_string();
        assert!(said, "{text}");
        for expected in ["changed", "now", "omit revision"] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
    });
}

/// An invoke that named a revision the directory left behind is the one
/// answer a caller cannot act on by itself: what happened, which revision
/// it named, which it is at now, and what to do next.
#[test]
fn a_stale_invoke_says_what_to_do_rather_than_a_code() {
    let host = GatedHost {
        at_once: true,
        stale: Some(Stale::Invokes),
        ..GatedHost::default()
    };
    let server = gated_server(host);
    run(async {
        initialized(&server).await;
        let mut args = current_invoke();
        args["revision"] = json!("0");
        let reply = tool(&server, "tilo_invoke", args).await;
        let text = reply.to_string();
        for expected in ["changed", "now", "omit revision", "tilo_query"] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!text.contains("invalid request"), "{text}");
    });
}
