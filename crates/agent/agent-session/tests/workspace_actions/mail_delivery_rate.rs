//! Mail brought into a hosted workspace one message at a time, as `tilo migrate start thunderbird
//! -w` brings it: a key delivers each with `mail/mail.messages#deliver-message` through the API
//! server. A message delivered into a mailbox holding thousands costs about what one delivered
//! into an empty mailbox does, and the key's listings between deliveries answer at once: nothing a
//! delivery does grows with what Mail already holds, so an import never slows down as it goes and
//! never holds up the workspace's other calls.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key};
use super::branch_workspace::Key;
use super::*;
use serde_json::{Value, json};

/// Where `tilo migrate` delivers a message (thunderbird.rs `DELIVER`), as resource and action.
const MESSAGES: &str = "mail/mail.messages";
const DELIVER: &str = "deliver-message";

/// How many messages are delivered in all: enough for the mailbox's messages to fill a part of
/// its chain and start the next, so every size of part a delivery lands in is met.
const DELIVERED: usize = 300;

/// How many messages one timed window takes: the first after the mailbox is made, against the
/// slowest of every later stretch of as many.
const WINDOW: usize = 20;

/// How many messages are delivered between two of the key's listings.
const LISTED_EVERY: usize = 10;

/// What one listing may take between deliveries. Listing the mail reads a few pages of its
/// messages, which takes about half a second in a test build on a busy machine; a listing that
/// waited behind the deliveries took many seconds.
const LISTING_BUDGET: Duration = Duration::from_millis(2_000);

/// A message as the importer delivers one: its words about as long as a real message's, so the
/// mailbox's parts fill as a real mailbox's do (about a hundred and thirty messages a part).
fn message(index: usize) -> Value {
    let body = format!(
        "Message {index}. {}",
        "The quarterly numbers are in and the venue is booked for the offsite. ".repeat(56)
    );
    json!({
        "from": format!("Sender {} <sender{}@example.test>", index % 37, index % 37),
        "to": "ada@example.test",
        "subject": format!("Report {index}"),
        "body": body,
        "date": "2026-10-12T09:00:00Z",
        "folder": "inbox",
        "read": index.is_multiple_of(3),
        "message_id": format!("<m{index}@example.test>"),
    })
}

/// Deliver message `index` as the importer does, asking again while the workspace is not ready
/// for the mailbox's newest part.
async fn deliver(key: &Key<'_>, path: &str, index: usize) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (status, answer) = key
            .post(
                path,
                &message(index),
                &[
                    (tilo_api::v1::INVOCATION_HEADER, "migrate-thunderbird"),
                    (
                        tilo_api::v1::INVOCATION_TITLE_HEADER,
                        "Bring Thunderbird in",
                    ),
                ],
            )
            .await;
        if matches!(
            answer["error"]["code"].as_str(),
            Some("not_ready" | "unanswered")
        ) {
            assert!(
                Instant::now() < deadline,
                "message {index} never became ready: {answer}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        }
        assert!(
            (200..300).contains(&status) && answer["status"] == "ok",
            "message {index}: {status} {answer}"
        );
        return;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_delivered_into_a_full_mailbox_costs_what_one_into_an_empty_one_does() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let computer = enrol(&ada, "Ada's computer", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &computer, "Ada's import").await;
    let key = Key {
        api: &on_server.api,
        token,
    };
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": "Ada's mail"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");
    let deliver_at = format!(
        "{base}/resources/{}/actions/{DELIVER}",
        tilo_api::client::encode_path(MESSAGES)
    );
    let lists = [
        format!("{base}/resources?path=tasks&limit=40"),
        format!("{base}/resources?path=mail&limit=40"),
    ];

    // The first message makes the mailbox; every one after it is timed, and every few messages
    // the key lists its tasks or its mail. A write waits a moment after a key's read for the
    // listing's next page, by design, so the message after a listing is left out of the timing.
    deliver(&key, &deliver_at, 0).await;
    let mut took = Vec::with_capacity(DELIVERED);
    let mut slowest_listing = Duration::ZERO;
    let mut listed = false;
    for index in 1..DELIVERED {
        let started = Instant::now();
        deliver(&key, &deliver_at, index).await;
        if !std::mem::take(&mut listed) {
            took.push(started.elapsed());
        }
        if index % LISTED_EVERY == 0 {
            let path = &lists[(index / LISTED_EVERY) % lists.len()];
            let started = Instant::now();
            let (status, answer) = key.get(path).await;
            slowest_listing = slowest_listing.max(started.elapsed());
            assert_eq!(status, 200, "{path}: {answer}");
            listed = true;
        }
    }
    let window = |at: usize| took[at..at + WINDOW].iter().sum::<Duration>();
    let empty = window(0);
    let (worst_at, worst) = (WINDOW..=took.len() - WINDOW)
        .map(|at| (at, window(at)))
        .max_by_key(|(_, took)| *took)
        .unwrap();
    eprintln!(
        "{WINDOW} messages took {empty:?} into an empty mailbox, {worst:?} at worst (the \
         {worst_at}th timed on); {DELIVERED} in {:?}; slowest listing {slowest_listing:?}",
        took.iter().sum::<Duration>()
    );
    assert!(
        worst <= empty * 2 + Duration::from_millis(250),
        "a delivery grows with what the mailbox holds: {WINDOW} messages took {empty:?} into an \
         empty mailbox, {worst:?} later on (the {worst_at}th timed on)"
    );
    assert!(
        slowest_listing < LISTING_BUDGET,
        "a listing between deliveries took {slowest_listing:?}"
    );
}
