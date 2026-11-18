//! Webhooks: a workspace's events posted to a URL, signed, in order, retried
//! until the URL takes them.
//!
//! Each webhook reads its workspace's events as the key that made it, with
//! the same watch the event stream holds ([`super::events::Watch`]), from
//! where it last delivered: the cursor is kept with the webhook, so a
//! restart neither loses events nor posts delivered ones again. A delivery is
//! `POST <url>` with the event as its body and
//!
//! - `Tilo-Webhook-Id`, the webhook;
//! - `Tilo-Delivery`, unique per event and webhook, the same on every retry;
//! - `Tilo-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">`
//!   under the webhook's secret.
//!
//! A `2xx` within [`ANSWER_WITHIN`] delivers it; anything else is tried again
//! after [`RETRIES`], and an event the URL never takes is given up on (and
//! logged) so the ones after it still go. Revoking the key ends its
//! webhooks.

use super::answers::Event;
use crate::Server;
use crate::store::Webhook;
use hmac::{KeyInit as _, Mac as _};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// How long a URL has to answer one delivery.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);
/// The waits before each retry: 8 attempts over about two and a half hours.
pub const RETRIES: [Duration; 7] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
    Duration::from_secs(1800),
    Duration::from_secs(3600),
    Duration::from_secs(3600),
];
/// How long a look at the workspace waits for something to happen.
const LOOK_MS: u64 = 25_000;
/// How long a webhook waits before looking again after its workspace could
/// not be read.
const UNREADABLE: Duration = Duration::from_secs(30);

/// The signature header's value for `body` at `time` (Unix seconds).
#[must_use]
pub fn signature(secret: &str, time: u64, body: &[u8]) -> String {
    // An HMAC takes a key of any length, so this never refuses one.
    let Ok(mut mac) = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(time.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("t={time},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// Whether `url` is one a webhook may post to: `https`, or plain `http` to
/// this machine.
#[must_use]
pub fn deliverable(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    })
}

/// The webhooks being delivered, by id.
#[derive(Default)]
pub struct Deliveries {
    running: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

impl Deliveries {
    /// Deliver every webhook the server keeps.
    pub fn start_all(server: &Arc<Server>) {
        for hook in server.store.webhooks(None) {
            Self::start(server, hook);
        }
    }

    /// Deliver `hook` from where it last delivered.
    pub fn start(server: &Arc<Server>, hook: Webhook) {
        let id = hook.id.clone();
        let task = tokio::spawn(deliver(Arc::downgrade(server), hook));
        let replaced = crate::lock(&server.v1.deliveries.running).insert(id, task.abort_handle());
        if let Some(replaced) = replaced {
            replaced.abort();
        }
    }

    /// Stop delivering `id`.
    pub fn stop(&self, id: &str) {
        let running = crate::lock(&self.running).remove(id);
        if let Some(running) = running {
            running.abort();
        }
    }
}

/// Deliver `hook`'s events until it, its key or the server is gone.
async fn deliver(server: Weak<Server>, hook: Webhook) {
    let mut watch = None;
    loop {
        let Some(live) = server.upgrade() else { return };
        let Some(grant) = live
            .store
            .grant(&hook.grant)
            .filter(crate::store::Grant::live)
        else {
            return;
        };
        if watch.is_none() {
            match live.api_session(&grant).await {
                Ok(session) => {
                    watch = Some(super::events::Watch::new(
                        session,
                        hook.workspace.clone(),
                        hook.path.clone(),
                        live.store
                            .webhooks(None)
                            .into_iter()
                            .find(|kept| kept.id == hook.id)
                            .and_then(|kept| kept.cursor),
                    ));
                }
                Err(refused) => {
                    tracing::info!(webhook = %hook.id, reason = %refused.body.message, "a webhook's workspace cannot be read now");
                    drop(live);
                    tokio::time::sleep(UNREADABLE).await;
                    continue;
                }
            }
        }
        let http = live.http.clone();
        drop(live);
        let Some(reading) = watch.as_mut() else {
            continue;
        };
        let events = match reading.next(LOOK_MS).await {
            Ok(events) => events,
            Err(refused) => {
                tracing::info!(webhook = %hook.id, reason = %refused.body.message, "a webhook's workspace cannot be read now");
                watch = None;
                tokio::time::sleep(UNREADABLE).await;
                continue;
            }
        };
        for (index, event) in events.iter().enumerate() {
            if hook.events.is_empty() || hook.events.contains(&event.kind) {
                post(&http, &hook, event, index).await;
            }
        }
        if !events.is_empty()
            && let (Some(server), Some(cursor)) = (server.upgrade(), reading.cursor())
            && let Err(error) = server.store.webhook_delivered(&hook.id, cursor)
        {
            tracing::warn!(webhook = %hook.id, %error, "where a webhook delivered up to was not kept");
        }
    }
}

/// Post `event` (the `index`th of its batch) to `hook`'s URL until it takes
/// it or the retries run out.
async fn post(http: &tilo_net::Client, hook: &Webhook, event: &Event, index: usize) {
    let body = serde_json::to_vec(event).unwrap_or_default();
    let delivery = format!("{}-{index}", event.id);
    for wait in std::iter::once(Duration::ZERO).chain(RETRIES) {
        tokio::time::sleep(wait).await;
        let time = tilo_clock::unix_now_ms() / 1000;
        let answered = http
            .post(&hook.url)
            .header("content-type", "application/json")
            .header("tilo-webhook-id", hook.id.as_str())
            .header("tilo-delivery", delivery.as_str())
            .header("tilo-signature", signature(&hook.secret, time, &body))
            .body(body.clone())
            .timeout(ANSWER_WITHIN)
            .send()
            .await;
        match answered {
            Ok(answer) if answer.status().is_success() => return,
            Ok(answer) => {
                tracing::info!(webhook = %hook.id, status = %answer.status(), "a webhook refused a delivery; it is tried again");
            }
            Err(error) => {
                tracing::info!(webhook = %hook.id, %error, "a webhook was not reached; it is tried again");
            }
        }
    }
    tracing::warn!(webhook = %hook.id, delivery, "a webhook never took a delivery; it was given up on");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_an_hmac_of_the_time_and_body() {
        // HMAC-SHA256("key", "1700000000.{}"), as Python's hmac computes it.
        let signed = signature("key", 1_700_000_000, b"{}");
        assert_eq!(
            signed,
            "t=1700000000,v1=9d713ed406bb7076d4123f0dc2c39d2df5c654ed4b0cd56b52c8b4c940bd63ae"
        );
        assert_ne!(signed, signature("other", 1_700_000_000, b"{}"));
        assert_ne!(signed, signature("key", 1_700_000_001, b"{}"));
    }

    #[test]
    fn a_webhook_posts_over_https_or_to_this_machine() {
        assert!(deliverable("https://example.com/hook"));
        assert!(deliverable("http://127.0.0.1:8080/hook"));
        assert!(!deliverable("http://example.com/hook"));
        assert!(!deliverable("file:///etc/passwd"));
    }
}
