#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use discord_wire::event::{code_sha256, event_id, source};
use discord_wire::op::{DiscordOp, FetchAssetOp, MemberOp, OutAttachment, SendOp};
use fake_discord::{CHANNEL_GENERAL, FakeDiscord, TOKEN, User};
use serde_json::{Map, Value, json};
use tilo_connector::framing::{read_frame_async, write_frame_async};
use tilo_connector::protocol::{Bytes, FromWorker, Outcome, ToWorker};
use tokio::sync::mpsc;

type Store = Arc<Mutex<HashMap<(String, String), String>>>;

/// What the worker sent, kept for the test to look through.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "every field shows in a failing assertion's output"
)]
enum Seen {
    Result(u64, Outcome),
    Event(String, String, Map<String, Value>),
    Blob(Vec<u8>),
    Status(bool, String),
    Checkpoint(Value),
    /// Queue depth and rate-limited total.
    Metrics(u64, u64),
    StoreSecret(String, String),
    NeedSecret(String, String),
}

struct Host {
    fake: FakeDiscord,
    to_worker: mpsc::UnboundedSender<ToWorker>,
    seen: Arc<Mutex<Vec<Seen>>>,
    store: Store,
    next_request: Mutex<u64>,
    worker: tokio::task::JoinHandle<Result<(), String>>,
}

impl Host {
    async fn start(read_only: bool) -> Self {
        let fake = FakeDiscord::start().await.expect("fake discord");
        Self::with(fake, read_only, None)
    }

    fn with(fake: FakeDiscord, read_only: bool, checkpoint: Option<Value>) -> Self {
        let vars: HashMap<&str, String> = [
            ("DISCORD_API_BASE", fake.api_base()),
            ("DISCORD_CDN_BASE", fake.cdn_base()),
            ("DISCORD_ALLOW_LOOPBACK", "1".to_owned()),
            (
                "DISCORD_READ_ONLY",
                if read_only { "1" } else { "0" }.to_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let config =
            discord_worker::Config::from_vars(|name| vars.get(name).cloned()).expect("config");
        let (host_end, worker_end) = tokio::io::duplex(1 << 20);
        let (worker_read, worker_write) = tokio::io::split(worker_end);
        let worker = tokio::spawn(discord_worker::run(
            config,
            tokio::io::BufReader::new(worker_read),
            worker_write,
        ));
        let (host_read, mut host_write) = tokio::io::split(host_end);
        let (to_worker, mut outgoing) = mpsc::unbounded_channel::<ToWorker>();
        tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if write_frame_async(&mut host_write, &message).await.is_err() {
                    return;
                }
            }
        });
        let store: Store = Arc::new(Mutex::new(HashMap::new()));
        store.lock().unwrap().insert(
            ("bot-token".to_owned(), "default".to_owned()),
            TOKEN.to_owned(),
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let (store, seen, to_worker) = (store.clone(), seen.clone(), to_worker.clone());
            tokio::spawn(async move {
                let mut input = tokio::io::BufReader::new(host_read);
                while let Ok(Some(message)) = read_frame_async::<_, FromWorker>(&mut input).await {
                    let entry = match message {
                        FromWorker::NeedSecret { purpose, item } => {
                            let held = store
                                .lock()
                                .unwrap()
                                .get(&(purpose.clone(), item.clone()))
                                .cloned();
                            let _ = to_worker.send(ToWorker::Secret {
                                purpose: purpose.clone(),
                                item: item.clone(),
                                bytes: held.map(|s| Bytes(s.into_bytes())),
                            });
                            Seen::NeedSecret(purpose, item)
                        }
                        FromWorker::StoreSecret {
                            purpose,
                            item,
                            bytes,
                        } => {
                            store.lock().unwrap().insert(
                                (purpose.clone(), item.clone()),
                                String::from_utf8(bytes.0.clone()).unwrap(),
                            );
                            Seen::StoreSecret(purpose, item)
                        }
                        FromWorker::Result { request, outcome } => Seen::Result(request, outcome),
                        FromWorker::Event {
                            source,
                            id,
                            payload,
                            ..
                        } => Seen::Event(source, id, payload),
                        FromWorker::Blob { bytes } => Seen::Blob(bytes.0.clone()),
                        FromWorker::Status { healthy, message } => Seen::Status(healthy, message),
                        FromWorker::Checkpoint { state } => Seen::Checkpoint(state),
                        FromWorker::Metrics {
                            queue_depth,
                            rate_limited_total,
                            ..
                        } => Seen::Metrics(queue_depth, rate_limited_total),
                        FromWorker::Ready { .. } => continue,
                    };
                    seen.lock().unwrap().push(entry);
                }
            });
        }
        to_worker
            .send(ToWorker::Start {
                version: tilo_connector::protocol::PROTOCOL_VERSION,
                checkpoint,
            })
            .unwrap();
        Self {
            fake,
            to_worker,
            seen,
            store,
            next_request: Mutex::new(1),
            worker,
        }
    }

    async fn connected(&self) {
        assert!(
            self.fake
                .eventually(tilo_test_env::hang_bound(), |f| f.connected().then_some(()))
                .await
                .is_some(),
            "the worker never connected"
        );
    }

    fn send_op(&self, op: &DiscordOp) -> u64 {
        let request = {
            let mut next = self.next_request.lock().unwrap();
            let request = *next;
            *next += 1;
            request
        };
        self.to_worker
            .send(ToWorker::Perform {
                request,
                op: serde_json::to_value(op).unwrap(),
            })
            .unwrap();
        request
    }

    async fn outcome(&self, request: u64) -> Outcome {
        self.wait(|seen| match seen {
            Seen::Result(r, outcome) if *r == request => Some(outcome.clone()),
            _ => None,
        })
        .await
        .unwrap_or_else(|| panic!("no result for request {request}"))
    }

    async fn perform(&self, op: &DiscordOp) -> Outcome {
        let request = self.send_op(op);
        self.outcome(request).await
    }

    async fn wait<T>(&self, mut find: impl FnMut(&Seen) -> Option<T>) -> Option<T> {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        loop {
            let found = self.seen.lock().unwrap().iter().find_map(&mut find);
            if found.is_some() {
                return found;
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn event(&self, wanted_source: &str, wanted_id: &str) -> Map<String, Value> {
        self.wait(|seen| match seen {
            Seen::Event(s, id, payload) if s == wanted_source && id == wanted_id => {
                Some(payload.clone())
            }
            _ => None,
        })
        .await
        .unwrap_or_else(|| panic!("no {wanted_source} event {wanted_id}"))
    }

    fn count(&self, matches: impl Fn(&Seen) -> bool) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| matches(s))
            .count()
    }

    /// A webhook in `channel` whose token the host holds.
    fn webhook(&self, channel: &str) -> String {
        let (id, token) = self.fake.create_webhook(channel);
        self.store
            .lock()
            .unwrap()
            .insert(("webhook".to_owned(), channel.to_owned()), token);
        id
    }

    /// The worker killed, as a host restart does (no `Stop`, nothing
    /// flushed), and a new one started from the last checkpoint the host
    /// kept, against the same Discord.
    async fn restarted(self) -> Self {
        let checkpoint = self
            .seen
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|seen| match seen {
                Seen::Checkpoint(state) => Some(state.clone()),
                _ => None,
            });
        assert!(
            checkpoint
                .as_ref()
                .is_some_and(|c| c["session_id"].is_string()),
            "the host kept a session to resume: {checkpoint:?}"
        );
        self.worker.abort();
        let _ = self.worker.await;
        Self::with(self.fake, false, checkpoint)
    }

    async fn stop(self) {
        let _ = self.to_worker.send(ToWorker::Stop);
        let ended = tokio::time::timeout(tilo_test_env::hang_bound(), self.worker).await;
        assert!(matches!(ended, Ok(Ok(Ok(())))), "worker ended: {ended:?}");
    }
}

fn send(webhook: &str, content: &str) -> DiscordOp {
    DiscordOp::Send(SendOp {
        channel: CHANNEL_GENERAL.to_owned(),
        webhook: webhook.to_owned(),
        thread: None,
        username: "Ada (Tilo)".to_owned(),
        avatar_url: None,
        avatar_blob: None,
        content: content.to_owned(),
        attachments: Vec::new(),
        created_ms: now_ms() - 1_000,
        tilo: format!("tilo-{content}"),
    })
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn message_id(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done { result } => result["message_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no message id in {result}"))
            .to_owned(),
        other => panic!("not done: {other:?}"),
    }
}

#[tokio::test]
async fn identifies_with_the_bridge_intents_and_reports_messages() {
    let host = Host::start(false).await;
    host.connected().await;
    assert_eq!(host.fake.intents(), Some(34_311));
    let ada = User::new("3001", "ada");
    let id = host.fake.post(CHANNEL_GENERAL, &ada, "hello from Discord");
    let payload = host
        .event(source::MESSAGE_CREATED, &event_id::message_created(&id))
        .await;
    assert_eq!(payload["content"], "hello from Discord");
    assert_eq!(payload["channel_id"], CHANNEL_GENERAL);
    assert!(
        host.wait(
            |s| matches!(s, Seen::Status(true, m) if m == "connected to Discord").then_some(())
        )
        .await
        .is_some()
    );
    // Slash commands are registered for the guild.
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .commands()
                .as_array()
                .is_some_and(|c| c.len() == 2)
                .then_some(()))
            .await
            .is_some()
    );
    host.stop().await;
}

#[tokio::test]
async fn link_codes_are_hashed_on_arrival_and_answered_privately() {
    let host = Host::start(false).await;
    host.connected().await;
    let ada = User::new("3001", "ada");
    let interaction = host.fake.slash(&ada, "link", Some("ab12 cd34"));
    let payload = host
        .event(source::COMMAND, &event_id::command(&interaction))
        .await;
    assert_eq!(payload["name"], "link");
    assert_eq!(payload["code_sha256"], code_sha256("ab12 cd34"));
    assert!(!Value::Object(payload).to_string().contains("ab12"));
    let reply = host
        .fake
        .eventually(tilo_test_env::hang_bound(), |f| {
            f.interaction_replies().into_iter().next()
        })
        .await
        .expect("an interaction reply");
    assert_eq!(reply["reply"]["data"]["flags"], 64);
    host.stop().await;
}

#[tokio::test]
async fn a_dropped_gateway_resumes_the_session() {
    let host = Host::start(false).await;
    host.connected().await;
    host.fake.drop_gateway();
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| (f.resumes() == 1
                && f.connected())
            .then_some(()))
            .await
            .is_some(),
        "no resume"
    );
    assert_eq!(host.fake.identifies(), 1);
    let id = host
        .fake
        .post(CHANNEL_GENERAL, &User::new("3001", "ada"), "after");
    host.event(source::MESSAGE_CREATED, &event_id::message_created(&id))
        .await;
    host.stop().await;
}

#[tokio::test]
async fn a_restarted_worker_hears_discord_again_and_reads_what_it_missed() {
    let host = Host::start(false).await;
    host.connected().await;
    let ada = User::new("3001", "ada");
    let first = host.fake.post(CHANNEL_GENERAL, &ada, "before the restart");
    host.event(source::MESSAGE_CREATED, &event_id::message_created(&first))
        .await;
    let host = host.restarted().await;
    // Said while no worker was there, then after the new one is up: both
    // reach the room.
    let missed = host
        .fake
        .post(CHANNEL_GENERAL, &ada, "while the worker restarted");
    let payload = host
        .event(source::MESSAGE_CREATED, &event_id::message_created(&missed))
        .await;
    assert_eq!(payload["content"], "while the worker restarted");
    let after = host.fake.post(CHANNEL_GENERAL, &ada, "after the restart");
    let payload = host
        .event(source::MESSAGE_CREATED, &event_id::message_created(&after))
        .await;
    assert_eq!(payload["content"], "after the restart");
    host.stop().await;
}

#[tokio::test]
async fn backfill_after_refused_resume_reemits_with_same_ids() {
    let host = Host::start(false).await;
    host.connected().await;
    let ada = User::new("3001", "ada");
    let first = host.fake.post(CHANNEL_GENERAL, &ada, "seen live");
    host.event(source::MESSAGE_CREATED, &event_id::message_created(&first))
        .await;
    // The worker noted the message as seen before it emitted the event, so
    // the dropped session's backfill starts after it.
    host.fake.refuse_next_resume();
    let missed = host
        .fake
        .post_unseen(CHANNEL_GENERAL, &ada, "missed while away");
    host.fake.drop_gateway();
    let payload = host
        .event(source::MESSAGE_CREATED, &event_id::message_created(&missed))
        .await;
    assert_eq!(payload["content"], "missed while away");
    assert_eq!(host.fake.identifies(), 2);
    let backfills = host
        .fake
        .requests_to("GET", &format!("/channels/{CHANNEL_GENERAL}/messages"));
    assert!(
        backfills
            .iter()
            .any(|r| r.query("after") == Some(first.as_str()))
    );
    // The message seen live is not reported a second time.
    let id = event_id::message_created(&first);
    assert_eq!(
        host.count(|s| matches!(s, Seen::Event(_, i, _) if *i == id)),
        1
    );
    host.stop().await;
}

#[tokio::test]
async fn sends_through_the_webhook_without_mass_pings() {
    let host = Host::start(false).await;
    host.connected().await;
    let webhook = host.webhook(CHANNEL_GENERAL);
    let outcome = host.perform(&send(&webhook, "hi @everyone")).await;
    let id = message_id(&outcome);
    let message = host.fake.message(&id).expect("posted");
    assert_eq!(message["content"], "hi @everyone");
    let posted = host
        .fake
        .requests_to("POST", &format!("/webhooks/{webhook}"));
    assert_eq!(posted.len(), 1);
    assert_eq!(
        posted[0].json["allowed_mentions"],
        json!({"parse": ["users"]})
    );
    // A retry of the same op finds the message instead of posting again.
    let again = host.perform(&send(&webhook, "hi @everyone")).await;
    assert_eq!(message_id(&again), id);
    assert_eq!(
        host.fake
            .requests_to("POST", &format!("/webhooks/{webhook}"))
            .len(),
        1
    );
    host.stop().await;
}

#[tokio::test]
async fn sends_attachments_the_host_sent_ahead() {
    let host = Host::start(false).await;
    host.connected().await;
    let webhook = host.webhook(CHANNEL_GENERAL);
    let bytes = b"file body".to_vec();
    let blob = blake3::hash(&bytes).to_hex().to_string();
    host.to_worker
        .send(ToWorker::Blob {
            blake3: blob.clone(),
            bytes: Bytes(bytes.clone()),
        })
        .unwrap();
    let DiscordOp::Send(mut op) = send(&webhook, "see file") else {
        unreachable!()
    };
    op.attachments = vec![OutAttachment {
        blob,
        name: "notes.txt".to_owned(),
        size: 9,
    }];
    message_id(&host.perform(&DiscordOp::Send(op)).await);
    let posted = host
        .fake
        .requests_to("POST", &format!("/webhooks/{webhook}"));
    assert_eq!(posted[0].files, vec![("notes.txt".to_owned(), bytes)]);
    host.stop().await;
}

#[tokio::test]
async fn a_burst_survives_429s_without_dead_letters() {
    let host = Host::start(false).await;
    host.connected().await;
    let webhook = host.webhook(CHANNEL_GENERAL);
    host.fake
        .rate_limit("POST", &format!("/webhooks/{webhook}"), 3, 100);
    let requests: Vec<u64> = (0..10)
        .map(|n| host.send_op(&send(&webhook, &format!("burst {n}"))))
        .collect();
    let mut shown = String::new();
    for request in requests {
        let id = message_id(&host.outcome(request).await);
        shown.push_str(host.fake.message(&id).unwrap()["content"].as_str().unwrap());
        shown.push('\n');
    }
    for n in 0..10 {
        assert!(shown.contains(&format!("burst {n}")), "lost burst {n}");
    }
    // The 429s show in the metrics the host turns into its metrics file.
    assert!(
        host.wait(|s| matches!(s, Seen::Metrics(0, limited) if *limited >= 3).then_some(()))
            .await
            .is_some(),
        "no metrics counting the 429s"
    );
    host.stop().await;
}

#[tokio::test]
async fn coalesces_queued_sends_while_the_webhook_bucket_is_empty() {
    let host = Host::start(false).await;
    host.connected().await;
    let webhook = host.webhook(CHANNEL_GENERAL);
    // Two calls per second, as a flooded webhook sees.
    host.fake
        .limit("POST", &format!("/webhooks/{webhook}"), 2, 1_000);
    let requests: Vec<u64> = (0..12)
        .map(|n| host.send_op(&send(&webhook, &format!("line {n}"))))
        .collect();
    let mut ids = Vec::new();
    for request in &requests {
        ids.push(message_id(&host.outcome(*request).await));
    }
    let messages = host.fake.messages_in(CHANNEL_GENERAL);
    assert!(
        messages.len() < 12,
        "nothing was merged: {} messages",
        messages.len()
    );
    // Order holds: the texts read back in the order they were queued.
    let mut all = Vec::new();
    for message in &messages {
        all.extend(
            message["content"]
                .as_str()
                .unwrap()
                .lines()
                .map(str::to_owned),
        );
    }
    let expected: Vec<String> = (0..12).map(|n| format!("line {n}")).collect();
    assert_eq!(all, expected);
    // Every merged op names the message it went into.
    for (n, id) in ids.iter().enumerate() {
        let content = host.fake.message(id).unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(content.lines().any(|l| l == format!("line {n}")));
    }
    host.stop().await;
}

#[tokio::test]
async fn a_refused_call_is_refused_and_not_retried() {
    let host = Host::start(false).await;
    host.connected().await;
    host.fake.fail(
        "PUT",
        &format!("/guilds/{}/bans/", fake_discord::GUILD),
        1,
        403,
        50_013,
        "Missing Permissions",
    );
    let outcome = host
        .perform(&DiscordOp::Ban(MemberOp {
            user: "3001".to_owned(),
            reason: "spam".to_owned(),
        }))
        .await;
    assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
    assert_eq!(
        host.fake
            .requests_to("PUT", &format!("/guilds/{}/bans/", fake_discord::GUILD))
            .len(),
        1
    );
    host.stop().await;
}

#[tokio::test]
async fn a_revoked_token_turns_unhealthy_and_ops_wait() {
    let host = Host::start(false).await;
    host.connected().await;
    host.fake.revoke_token("a.new.token");
    let outcome = host
        .perform(&DiscordOp::Ban(MemberOp {
            user: "3001".to_owned(),
            reason: String::new(),
        }))
        .await;
    assert!(
        matches!(outcome, Outcome::Unavailable { .. }),
        "{outcome:?}"
    );
    assert!(
        host.wait(|s| matches!(s, Seen::Status(false, _)).then_some(()))
            .await
            .is_some()
    );
    host.stop().await;
}

#[tokio::test]
async fn create_webhook_hands_the_token_to_the_store_only() {
    let host = Host::start(false).await;
    host.connected().await;
    let outcome = host
        .perform(&DiscordOp::CreateWebhook(
            discord_wire::op::CreateWebhookOp {
                channel: CHANNEL_GENERAL.to_owned(),
                tilo: "room".to_owned(),
            },
        ))
        .await;
    let Outcome::Done { result } = outcome else {
        panic!("{outcome:?}")
    };
    let webhook = result["webhook_id"].as_str().unwrap().to_owned();
    let token = host.fake.webhook_token(&webhook).unwrap();
    assert!(!result.to_string().contains(&token));
    assert_eq!(
        host.store
            .lock()
            .unwrap()
            .get(&("webhook".to_owned(), CHANNEL_GENERAL.to_owned())),
        Some(&token)
    );
    // Asked again (an op runs at least once), it adopts the same webhook.
    let again = host
        .perform(&DiscordOp::CreateWebhook(
            discord_wire::op::CreateWebhookOp {
                channel: CHANNEL_GENERAL.to_owned(),
                tilo: "room".to_owned(),
            },
        ))
        .await;
    let Outcome::Done { result } = again else {
        panic!()
    };
    assert_eq!(result["webhook_id"], webhook.as_str());
    host.stop().await;
}

#[tokio::test]
async fn fetch_asset_sends_the_bytes_then_the_result() {
    let host = Host::start(false).await;
    host.connected().await;
    let url = host
        .fake
        .put_cdn("avatars/3001/abc.png", "image/png", b"png bytes");
    let outcome = host
        .perform(&DiscordOp::FetchAsset(FetchAssetOp {
            url: url.clone(),
            max_bytes: 1_000,
            attachment: None,
        }))
        .await;
    let Outcome::Done { result } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(result["blob"], blake3::hash(b"png bytes").to_hex().as_str());
    assert_eq!(result["content_type"], "image/png");
    assert_eq!(
        host.count(|s| matches!(s, Seen::Blob(b) if b == b"png bytes")),
        1
    );
    let too_big = host
        .perform(&DiscordOp::FetchAsset(FetchAssetOp {
            url,
            max_bytes: 3,
            attachment: None,
        }))
        .await;
    assert!(matches!(too_big, Outcome::Refused { .. }), "{too_big:?}");
    let elsewhere = host
        .perform(&DiscordOp::FetchAsset(FetchAssetOp {
            url: "https://example.com/a.png".to_owned(),
            max_bytes: 1_000,
            attachment: None,
        }))
        .await;
    assert!(
        matches!(elsewhere, Outcome::Refused { .. }),
        "{elsewhere:?}"
    );
    host.stop().await;
}

#[tokio::test]
async fn read_only_changes_nothing() {
    let host = Host::start(true).await;
    host.connected().await;
    let webhook = host.webhook(CHANNEL_GENERAL);
    let outcome = host.perform(&send(&webhook, "nope")).await;
    assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
    let ada = User::new("3001", "ada");
    let interaction = host.fake.slash(&ada, "link", Some("abcd"));
    host.event(source::COMMAND, &event_id::command(&interaction))
        .await;
    // A reply would be decided with the event, before the worker reads its
    // next frame: no reply is started in read-only mode.
    assert!(host.fake.interaction_replies().is_empty());
    assert!(host.fake.requests_to("PUT", "/applications/").is_empty());
    assert!(host.fake.messages_in(CHANNEL_GENERAL).is_empty());
    assert!(
        host.wait(|s| matches!(s, Seen::Status(true, m) if m.contains("read-only")).then_some(()))
            .await
            .is_some()
    );
    host.stop().await;
}
