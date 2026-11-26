//! A web call with a key by reference, against services the test starts on
//! this computer and a key that is made up. The key reaches its own service
//! and no other, where its item says it rides; its person is asked about
//! each call and their allow sends that very call once; and the key is in no
//! answer, no refusal, no log line, nothing the server keeps for the
//! question and no file in the clear.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, the server handle a test binds is kept until the test ends, and a chain is told in one piece"
)]

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use test_http::{Reply, Request, Responder};
use tilo_actions::InvocationId;
use tilo_actions::remote::ActionCallError::{self, NotPermitted, Refused, UnknownAction};
use tilo_actions::remote::person_vault;
use tilo_actions::remote::{InvokeArgument, InvokeRequest};

use super::super::tests::{admit, caller, governed};
use super::turn_use_tests::on_turn;
use super::vault_tests::{ask, files_holding};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{HostedTurn, OpenAsk, TurnUse, UseAnswer};
use crate::workspaces::tests::{governor, test_host};

/// A made-up key: no service ever issued it, and it opens nothing.
const KEY: &str = "made-up-key-for-tests-0123456789abcdef";
/// What stands in an answer where the key stood.
const WITHHELD: &str = "[withheld]";
/// The most of an answer a call hands back.
const MOST: usize = 256 * 1024;

/// A server with one workspace and Ada in it on a device of her own. The
/// server may call this computer, where the test's services are.
macro_rules! stage {
    ($dir:ident, $workspaces:ident, $workspace:ident, $workspace_id:ident, $service:ident, $owner:ident, $ada:ident, $as_ada:ident) => {
        let $dir = tempfile::tempdir().unwrap();
        let (key, $owner) = governor();
        let server = test_host($dir.path(), &key, &$owner).await;
        let $workspaces = server.workspaces();
        $workspaces.call_private_model_addresses(true);
        let ($workspace, $workspace_id, $service) = governed($workspaces).await;
        let $ada = admit($workspaces, &$workspace, &$owner, 1).await;
        let $as_ada = caller(&$ada);
    };
}

/// The call to `url` with the vault item `key` names, and `more` of it.
fn call(url: &str, key: &Value, more: Value) -> InvokeRequest {
    let mut argument = serde_json::Map::new();
    argument.insert("url".into(), json!(url));
    argument.insert("key".into(), key.clone());
    if let Value::Object(more) = more {
        argument.extend(more);
    }
    InvokeRequest {
        invocation: InvocationId::new(),
        target: person_vault::call_target(),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The ask a call answered with, when it asked.
fn asked(taken: Result<TurnUse, ActionCallError>) -> OpenAsk {
    match taken {
        Ok(TurnUse::Asks(open)) => *open,
        other => panic!("the person is asked first: {other:?}"),
    }
}

/// What a call that was made answered.
fn made(taken: Result<TurnUse, ActionCallError>) -> Value {
    match taken {
        Ok(TurnUse::Done(answer)) => answer,
        other => panic!("the call is made: {other:?}"),
    }
}

/// Why a call was refused.
fn refusal(taken: Result<TurnUse, ActionCallError>) -> String {
    match taken {
        Err(Refused { reason }) => reason,
        other => panic!("the call is refused: {other:?}"),
    }
}

/// The agent asks for the call `asking` makes, its person allows it, and the
/// agent makes the same call again: what it is answered then.
async fn allowed(
    workspaces: &Workspaces,
    turn: &HostedTurn<'_>,
    asking: impl Fn() -> InvokeRequest,
) -> Result<TurnUse, ActionCallError> {
    let open = asked(workspaces.use_for_turn(turn, &asking(), json!({})).await);
    let answered = workspaces
        .answer_use(turn, &open.ask, &open.detail, UseAnswer::Once)
        .await
        .expect("an allow is taken");
    assert_eq!(answered, Some(json!({"allowed": "once"})));
    workspaces.use_for_turn(turn, &asking(), json!({})).await
}

/// How a service looks for the made-up key in a request.
type Shown = fn(&Request) -> bool;

/// Whether `request` shows the made-up key as a bearer token.
fn as_bearer(request: &Request) -> bool {
    request.header("authorization") == Some(format!("Bearer {KEY}").as_str())
}

/// Whether `request` shows it in the header the service names.
fn in_its_header(request: &Request) -> bool {
    request.header("x-api-key") == Some(KEY) && request.header("authorization").is_none()
}

/// Whether `request` shows it after the word the service wants before it.
fn after_a_word(request: &Request) -> bool {
    request.header("authorization") == Some(format!("Token {KEY}").as_str())
}

/// Whether `request` shows a basic sign-in that does not spell the key out.
fn as_basic(request: &Request) -> bool {
    request
        .header("authorization")
        .is_some_and(|shown| shown.starts_with("Basic ") && !shown.contains(KEY))
}

/// Whether `request` shows it as the parameter the service names.
fn in_the_address(request: &Request) -> bool {
    request.target.ends_with(&format!("api_key={KEY}")) && request.header("authorization").is_none()
}

/// A service that answers whoever shows the made-up key the way `shown`
/// looks for it, and turns everyone else away. It repeats what it was sent
/// where a careless service would, and sends one address on to `elsewhere`.
fn a_service(shown: Shown, elsewhere: String) -> Responder {
    Responder::handle(move |request| {
        if !shown(request) {
            return Some(Reply::status(401).body(r#"{"error":"that key is unknown"}"#));
        }
        let path = request.target.split('?').next().unwrap_or_default();
        Some(match path {
            "/v1/balance" => Reply::status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"balance":1234}"#),
            "/v1/echo" => {
                let mut said = format!("{} {}\n", request.method, request.target);
                for (name, value) in &request.headers {
                    let _ = writeln!(said, "{name}: {value}");
                }
                said.push_str(&request.body_text());
                Reply::status(200)
                    .header("X-Seen", request.header("authorization").unwrap_or("none"))
                    .header("Set-Cookie", &format!("session={KEY}"))
                    .body(said)
            }
            "/v1/moved" => {
                Reply::status(302).header("Location", &format!("{elsewhere}/taken?key={KEY}"))
            }
            "/v1/long" => Reply::status(200).body(format!(
                "{}{KEY}{}",
                "a".repeat(MOST - 5),
                "b".repeat(1_000)
            )),
            _ => Reply::status(404),
        })
    })
}

/// Every log line written on this thread, at every level.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// Keep every log line this thread writes from now on, until the guard is
/// dropped.
fn logged() -> (Lines, tracing::subscriber::DefaultGuard) {
    let lines = Lines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(lines.clone())
        .finish();
    (lines, tracing::subscriber::set_default(subscriber))
}

/// The item `kept` answered, by its reference.
fn reference(kept: Result<Value, ActionCallError>) -> Value {
    kept.expect("the person keeps a key")["reference"].clone()
}

/// A call asks its person first and sends nothing. What the server keeps for
/// the question names the key by reference. Their allow sends that very call
/// once, when the agent makes it again, and the next one asks anew: also
/// after "Always", which lets nothing stand. The key is in no answer, no log
/// line and no file in the clear, and its use is written down without it.
#[tokio::test]
async fn a_call_asks_its_person_first_and_their_allow_sends_that_call_once() {
    stage!(
        dir,
        workspaces,
        workspace,
        workspace_id,
        service,
        owner,
        ada,
        as_ada
    );
    let hers = (&ada, &as_ada);
    let paying = a_service(as_bearer, "http://127.0.0.1:9".into());
    let item = reference(
        ask(
            &service,
            workspace_id,
            hers,
            person_vault::ADD,
            json!({"kind": "key", "belongsTo": [paying.url()], "secret": KEY}),
        )
        .await,
    );
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    let offered = workspaces.calls_offered(&turn).await;
    assert_eq!(offered.len(), 1, "the turn is offered the call");
    assert_eq!(offered[0].provider, person_vault::PROVIDER);
    assert_eq!(workspaces.uses_offered(&turn).await.len(), 2);

    let (lines, guard) = logged();
    tracing::info!("the line that shows the log is read");
    let balance = || {
        call(
            &format!("{}/v1/balance", paying.url()),
            &item,
            json!({"reason": "To read this month's numbers."}),
        )
    };
    let open = asked(
        workspaces
            .use_for_turn(&turn, &balance(), json!({"part": "p0"}))
            .await,
    );
    assert!(!open.takes_always(), "no Always is offered for a call");
    assert_eq!(open.reason, "To read this month's numbers.");
    assert_eq!(open.agent, "scout");
    let shown = open.detail.to_string();
    assert!(
        shown.contains("/v1/balance"),
        "the person sees where it goes: {shown}"
    );
    let id = item.as_str().unwrap().trim_start_matches("vault.item:");
    assert!(
        !shown.contains(id),
        "the item's address is withheld: {shown}"
    );
    assert!(
        paying.drain_requests().is_empty(),
        "nothing is sent unasked"
    );
    assert_eq!(
        workspaces.use_for_turn(&turn, &balance(), json!({})).await,
        Ok(TurnUse::Waits),
        "the same call asks once"
    );

    // Allowed: nothing is sent by the answer, and the same call goes through.
    let answered = workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
        .await
        .unwrap();
    assert_eq!(answered, Some(json!({"allowed": "once"})));
    assert!(
        paying.drain_requests().is_empty(),
        "an allow sends nothing by itself"
    );
    // Another call is not the one its person was shown.
    let other = call(&format!("{}/v1/echo", paying.url()), &item, json!({}));
    let asked_anew = asked(workspaces.use_for_turn(&turn, &other, json!({})).await);
    workspaces.use_not_asked(&asked_anew);
    assert!(paying.drain_requests().is_empty());

    let answer = made(workspaces.use_for_turn(&turn, &balance(), json!({})).await);
    assert_eq!(answer["status"], 200);
    assert_eq!(answer["body"], r#"{"balance":1234}"#);
    assert_eq!(answer["truncated"], false);
    assert_eq!(answer["headers"]["content-type"], "application/json");
    assert!(!answer.to_string().contains(KEY));
    let seen = paying.drain_requests();
    assert_eq!(seen.len(), 1, "sent once");
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].target, "/v1/balance");
    assert!(as_bearer(&seen[0]), "with the key its service knows");

    // The allow is spent: the next one asks again, and a no sends nothing.
    let again = asked(workspaces.use_for_turn(&turn, &balance(), json!({})).await);
    assert_eq!(
        workspaces
            .answer_use(&turn, &again.ask, &again.detail, UseAnswer::No)
            .await,
        Ok(None)
    );
    let third = asked(workspaces.use_for_turn(&turn, &balance(), json!({})).await);
    // "Always" lets one call through, as an allow does, and nothing stands.
    assert_eq!(
        workspaces
            .answer_use(&turn, &third.ask, &third.detail, UseAnswer::Always)
            .await,
        Ok(Some(json!({"allowed": "once"})))
    );
    made(workspaces.use_for_turn(&turn, &balance(), json!({})).await);
    asked(workspaces.use_for_turn(&turn, &balance(), json!({})).await);
    assert_eq!(paying.drain_requests().len(), 1, "one more, and no third");

    // What the item remembers: when, where and for whom, and no leave.
    let remembers = ask(
        &service,
        workspace_id,
        hers,
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    .unwrap();
    let uses = remembers["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 2, "{remembers}");
    assert_eq!(uses[0]["by"], "Scout");
    assert_eq!(uses[0]["places"], json!([paying.url()]));
    // And what for: the call's own word, its method and path, and that its
    // person allowed that one call. An "Always" answered anyway let one
    // call through, and is written down as that.
    let word = person_vault::use_word(&person_vault::call_target());
    for used in uses {
        assert_eq!(used["using"], json!(word), "{remembers}");
        assert_eq!(used["what"], "GET /v1/balance", "{remembers}");
        assert_eq!(used["allowed"], "once", "{remembers}");
    }
    assert!(!remembers.to_string().contains(KEY), "{remembers}");
    assert!(
        remembers
            .get("always")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "no Always stands: {remembers}"
    );
    assert!(!remembers.to_string().contains(KEY));

    drop(guard);
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(
        logged.contains("the line that shows the log is read"),
        "the log was read: {} bytes",
        logged.len()
    );
    assert!(!logged.contains(KEY), "a log line holds the key");
    let holding = files_holding(dir.path(), KEY.as_bytes());
    assert!(
        holding.is_empty(),
        "in the clear on the server's disk: {holding:?}"
    );
}

/// A key is sent to the service its item belongs to and to no other: not to
/// another service, not to another port of the same computer, and not by a
/// call its person allowed for the right one. An item that is no key is
/// refused. Nobody is asked about a call that would be refused, and nothing
/// reaches the other service.
#[tokio::test]
async fn a_key_is_sent_to_its_own_service_and_to_no_other() {
    stage!(
        dir,
        workspaces,
        workspace,
        workspace_id,
        service,
        owner,
        ada,
        as_ada
    );
    let hers = (&ada, &as_ada);
    let paying = a_service(as_bearer, String::new());
    let other = a_service(as_bearer, String::new());
    let key_for = |place: String, kind: &'static str| {
        ask(
            &service,
            workspace_id,
            hers,
            person_vault::ADD,
            json!({"kind": kind, "belongsTo": [place], "secret": KEY}),
        )
    };
    let item = reference(key_for(paying.url(), "key").await);
    let value = reference(key_for(paying.url(), "value").await);
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    let elsewhere = format!("{}/v1/balance", other.url());
    assert_eq!(
        refusal(
            workspaces
                .use_for_turn(&turn, &call(&elsewhere, &item, json!({})), json!({}))
                .await
        ),
        "That item belongs somewhere else"
    );
    // Allowed for its own service, the allow does not carry over.
    let own = || call(&format!("{}/v1/balance", paying.url()), &item, json!({}));
    let open = asked(workspaces.use_for_turn(&turn, &own(), json!({})).await);
    workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
        .await
        .unwrap();
    assert_eq!(
        refusal(
            workspaces
                .use_for_turn(&turn, &call(&elsewhere, &item, json!({})), json!({}))
                .await
        ),
        "That item belongs somewhere else"
    );
    for (url, why) in [
        (
            "https://api.example.test/v1/balance".to_owned(),
            "That item belongs somewhere else",
        ),
        (
            format!("http://localhost:{}/v1/balance", paying.port()),
            "That item belongs somewhere else",
        ),
        (
            "mailto:ada@example.test".to_owned(),
            "A web call names the web address it calls",
        ),
    ] {
        assert_eq!(
            refusal(
                workspaces
                    .use_for_turn(&turn, &call(&url, &item, json!({})), json!({}))
                    .await
            ),
            why,
            "{url}"
        );
    }
    assert_eq!(
        refusal(
            workspaces
                .use_for_turn(&turn, &own_with(&paying, &value), json!({}))
                .await
        ),
        "That item is not the kind of thing this needs"
    );
    let nobody = json!(format!("vault.item:{}", "0".repeat(32)));
    assert_eq!(
        refusal(
            workspaces
                .use_for_turn(&turn, &own_with(&paying, &nobody), json!({}))
                .await
        ),
        "That item is not kept here"
    );
    assert!(matches!(
        workspaces
            .use_for_turn(&turn, &own_with(&paying, &json!(KEY)), json!({}))
            .await,
        Err(ActionCallError::InvalidRequest { reason }) if !reason.contains(KEY)
    ));
    assert!(
        other.drain_requests().is_empty(),
        "the other service saw nothing"
    );
    assert!(
        paying.drain_requests().is_empty(),
        "and nothing was sent yet"
    );
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody is asked about a call that would be refused"
    );
    // The allow for its own service still stands, and is spent there.
    assert_eq!(
        made(workspaces.use_for_turn(&turn, &own(), json!({})).await)["status"],
        200
    );
    assert_eq!(paying.drain_requests().len(), 1);
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());
}

/// The call for the balance at `at` with the item `key` names.
fn own_with(at: &Responder, key: &Value) -> InvokeRequest {
    call(&format!("{}/v1/balance", at.url()), key, json!({}))
}

/// The key rides where its item says its service takes it, and nowhere the
/// agent says: the header or the parameter the key rides in, and what signs
/// a request in or says who it is for, are not the agent's to set. Only a
/// key says where it rides.
#[tokio::test]
async fn a_key_rides_where_its_item_says_and_nowhere_the_agent_says() {
    stage!(
        dir,
        workspaces,
        workspace,
        workspace_id,
        service,
        owner,
        ada,
        as_ada
    );
    let hers = (&ada, &as_ada);
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    let ways: [(&str, Shown, Value); 5] = [
        ("", as_bearer, json!({"Authorization": "Bearer mine"})),
        (
            "basic",
            as_basic,
            json!({"authorization": "Basic bWluZQ=="}),
        ),
        (
            "header:X-Api-Key",
            in_its_header,
            json!({"x-api-key": "mine"}),
        ),
        (
            "header:Authorization:Token",
            after_a_word,
            json!({"Host": "elsewhere.example.test"}),
        ),
        (
            "query:api_key",
            in_the_address,
            json!({"X-Forwarded-Host": "elsewhere.example.test"}),
        ),
    ];
    for (sent_in, shown, not_the_agents) in ways {
        let at = a_service(shown, String::new());
        let mut adding = json!({"kind": "key", "belongsTo": [at.url()], "secret": KEY});
        if !sent_in.is_empty() {
            adding["sentIn"] = json!(sent_in);
        }
        if sent_in == "basic" {
            adding["username"] = json!("ada");
        }
        let item = reference(ask(&service, workspace_id, hers, person_vault::ADD, adding).await);
        let said = refusal(
            workspaces
                .use_for_turn(
                    &turn,
                    &call(
                        &format!("{}/v1/balance", at.url()),
                        &item,
                        json!({"headers": not_the_agents}),
                    ),
                    json!({}),
                )
                .await,
        );
        assert!(said.contains("is not yours to set"), "{sent_in}: {said}");
        if sent_in.starts_with("query") {
            let taken = format!("{}/v1/balance?api_key=mine", at.url());
            let said = refusal(
                workspaces
                    .use_for_turn(&turn, &call(&taken, &item, json!({})), json!({}))
                    .await,
            );
            assert!(said.contains("is not yours to set"), "{said}");
        }
        assert!(
            at.drain_requests().is_empty(),
            "{sent_in}: nothing was sent"
        );
        assert!(
            workspaces.open_use_asks(&workspace).is_empty(),
            "{sent_in}: nobody was asked"
        );

        let answer = made(
            allowed(workspaces, &turn, || {
                call(
                    &format!("{}/v1/balance?page=2", at.url()),
                    &item,
                    json!({"headers": {"Accept": "application/json"}}),
                )
            })
            .await,
        );
        assert_eq!(
            answer["status"], 200,
            "{sent_in}: the service knew the key: {answer}"
        );
        assert!(!answer.to_string().contains(KEY));
        let seen = at.drain_requests();
        assert_eq!(seen.len(), 1, "{sent_in}");
        assert_eq!(seen[0].header("accept"), Some("application/json"));
        assert!(
            seen[0].target.starts_with("/v1/balance?page=2"),
            "{}",
            seen[0].target
        );
    }

    // Only a key says where it rides, and only where a key can ride.
    for unkept in [
        json!({"kind": "login", "belongsTo": ["https://example.test"], "sentIn": "basic", "secret": "a password"}),
        json!({"kind": "key", "belongsTo": ["https://example.test"], "sentIn": "header:Host", "secret": KEY}),
        json!({"kind": "key", "belongsTo": ["https://example.test"], "sentIn": "somewhere", "secret": KEY}),
    ] {
        let refused = ask(
            &service,
            workspace_id,
            hers,
            person_vault::ADD,
            unkept.clone(),
        )
        .await;
        assert!(
            matches!(&refused, Err(Refused { reason }) if !reason.contains(KEY) && !reason.contains("a password")),
            "{unkept}: {refused:?}"
        );
    }
    // A key too short to be struck out of an answer is not sent.
    let short = a_service(as_bearer, String::new());
    let item = reference(
        ask(
            &service,
            workspace_id,
            hers,
            person_vault::ADD,
            json!({"kind": "key", "belongsTo": [short.url()], "secret": "0123"}),
        )
        .await,
    );
    let said = refusal(
        workspaces
            .use_for_turn(&turn, &own_with(&short, &item), json!({}))
            .await,
    );
    assert!(said.contains("too short"), "{said}");
    assert!(short.drain_requests().is_empty());
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());
}

/// What a service repeats of the key comes back without it: in the body, in
/// a header, in the address it sends the caller on to (which is not
/// followed), and across the cut of an answer too long to hand back whole.
/// A session the service opens is not handed back. A service that hangs up
/// is said to be unreachable in words that name neither the key nor the
/// address.
#[tokio::test]
async fn what_a_service_repeats_comes_back_without_the_key() {
    stage!(
        dir,
        workspaces,
        workspace,
        workspace_id,
        service,
        owner,
        ada,
        as_ada
    );
    let hers = (&ada, &as_ada);
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    let (lines, guard) = logged();
    tracing::info!("the line that shows the log is read");
    let elsewhere = Responder::handle(|_| Some(Reply::status(200).body("taken")));
    let ways: [(&str, Shown, String); 3] = [
        ("", as_bearer, format!("authorization: Bearer {WITHHELD}")),
        (
            "basic",
            as_basic,
            format!("authorization: Basic {WITHHELD}"),
        ),
        (
            "query:api_key",
            in_the_address,
            format!("POST /v1/echo?api_key={WITHHELD}"),
        ),
    ];
    for (sent_in, shown, repeated) in ways {
        let at = a_service(shown, elsewhere.url());
        let mut adding = json!({"kind": "key", "belongsTo": [at.url()], "secret": KEY});
        if !sent_in.is_empty() {
            adding["sentIn"] = json!(sent_in);
        }
        let item = reference(ask(&service, workspace_id, hers, person_vault::ADD, adding).await);
        let answer = made(
            allowed(workspaces, &turn, || {
                call(
                    &format!("{}/v1/echo", at.url()),
                    &item,
                    json!({"method": "post", "body": "a note", "headers": {"Content-Type": "text/plain"}}),
                )
            })
            .await,
        );
        let body = answer["body"].as_str().unwrap();
        assert!(
            body.to_ascii_lowercase()
                .contains(&repeated.to_ascii_lowercase()),
            "{sent_in}: {body}"
        );
        assert!(body.ends_with("a note"), "{body}");
        assert!(!answer.to_string().contains(KEY), "{sent_in}: {answer}");
        assert!(answer["headers"].get("set-cookie").is_none(), "{answer}");
        if sent_in.is_empty() {
            assert_eq!(answer["headers"]["x-seen"], format!("Bearer {WITHHELD}"));
        }
        let seen = at.drain_requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].body_text(), "a note");

        let moved = made(
            allowed(workspaces, &turn, || {
                call(&format!("{}/v1/moved", at.url()), &item, json!({}))
            })
            .await,
        );
        assert_eq!(moved["status"], 302);
        assert_eq!(
            moved["headers"]["location"],
            format!("{}/taken?key={WITHHELD}", elsewhere.url())
        );
        assert!(!moved.to_string().contains(KEY));

        let long = made(
            allowed(workspaces, &turn, || {
                call(&format!("{}/v1/long", at.url()), &item, json!({}))
            })
            .await,
        );
        assert_eq!(long["truncated"], true);
        let handed = long["body"].as_str().unwrap();
        assert!(
            handed.ends_with(&format!("aaaa{WITHHELD}")),
            "the key across the cut is struck whole"
        );
        assert!(handed.len() <= MOST.saturating_add(WITHHELD.len()));
        assert!(
            !handed.contains(&KEY[..8]),
            "not handed back by its beginning"
        );
    }
    assert!(
        elsewhere.drain_requests().is_empty(),
        "no call follows where a service sends it on"
    );

    let gone = Responder::handle(|_| None);
    let item = reference(
        ask(
            &service,
            workspace_id,
            hers,
            person_vault::ADD,
            json!({"kind": "key", "belongsTo": [gone.url()], "sentIn": "query:api_key", "secret": KEY}),
        )
        .await,
    );
    let said = refusal(allowed(workspaces, &turn, || own_with(&gone, &item)).await);
    assert_eq!(said, "The service could not be reached");

    drop(guard);
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(logged.contains("the line that shows the log is read"));
    assert!(!logged.contains(KEY), "a log line holds the key");
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());
}

/// Only a turn this server carries out makes a call with a key: not a person
/// asking for themselves, not an agent that anything narrows without a rule
/// that names the call, and another member's turn holds the workspace's
/// reference, its use asking his person. And no device's request makes one
/// at all.
#[tokio::test]
async fn only_a_hosted_turn_makes_a_call() {
    stage!(
        dir,
        workspaces,
        workspace,
        workspace_id,
        service,
        owner,
        ada,
        as_ada
    );
    let hers = (&ada, &as_ada);
    let paying = a_service(as_bearer, String::new());
    let item = reference(
        ask(
            &service,
            workspace_id,
            hers,
            person_vault::ADD,
            json!({"kind": "key", "belongsTo": [paying.url()], "secret": KEY}),
        )
        .await,
    );
    let scout = on_turn(workspaces, &ada, &workspace, "scout");

    // A person asking for themselves is no turn this server carries out.
    let herself = HostedTurn {
        workspace: workspace_id,
        caller: &as_ada,
        bound: None,
    };
    assert!(workspaces.calls_offered(&herself).await.is_empty());
    assert_eq!(
        workspaces
            .use_for_turn(&herself, &own_with(&paying, &item), json!({}))
            .await,
        Err(UnknownAction)
    );
    // An agent that something narrows holds no rule that names the call.
    let narrowed = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: Some(&[]),
    };
    assert!(workspaces.calls_offered(&narrowed).await.is_empty());
    assert_eq!(
        workspaces
            .use_for_turn(&narrowed, &own_with(&paying, &item), json!({}))
            .await,
        Err(NotPermitted)
    );
    // Another member's turn holds the workspace's reference too: the item is
    // the workspace's, and its use asks his person, not hers.
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let his = on_turn(workspaces, &bo, &workspace, "rover");
    let his_asker = hex::encode(bo.user_public_key);
    let theirs = HostedTurn {
        workspace: workspace_id,
        caller: &his,
        bound: None,
    };
    let open = asked(
        workspaces
            .use_for_turn(&theirs, &own_with(&paying, &item), json!({}))
            .await,
    );
    assert_eq!(
        open.person, his_asker,
        "the workspace's item asks his person"
    );
    // No device's request makes a call: the person's is no action the
    // vault has, and her agent's asks her first.
    let asking = json!({"url": format!("{}/v1/balance", paying.url()), "key": item});
    assert_eq!(
        ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            person_vault::WEB_CALL,
            asking.clone()
        )
        .await,
        Err(UnknownAction)
    );
    let hers = ask(
        &service,
        workspace_id,
        (&ada, &scout),
        person_vault::WEB_CALL,
        asking,
    )
    .await;
    assert!(
        matches!(hers, Err(ActionCallError::ConfirmationRequired { .. })),
        "{hers:?}"
    );
    assert!(paying.drain_requests().is_empty(), "nothing was sent");
    assert_eq!(
        workspaces.open_use_asks(&workspace).len(),
        1,
        "his person was asked about it"
    );
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());
}
