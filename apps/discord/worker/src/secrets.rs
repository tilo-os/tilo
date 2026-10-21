//! Secrets the host holds and hands over on request: the bot token and one
//! webhook token per channel. They live only in memory here (zeroed when
//! dropped) and are never logged or written.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tilo_connector::protocol::{Bytes, FromWorker};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

pub const BOT_TOKEN: &str = "bot-token";
pub const TOKEN_ITEM: &str = "default";
/// Webhook tokens, one per channel id.
pub const WEBHOOK: &str = "webhook";

const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

pub type Secret = Zeroizing<String>;
type Key = (String, String);
type Waiter = oneshot::Sender<Option<Secret>>;

pub struct Secrets {
    out: mpsc::UnboundedSender<FromWorker>,
    cache: Mutex<HashMap<Key, Secret>>,
    waiting: Mutex<HashMap<Key, Vec<Waiter>>>,
}

impl Secrets {
    pub fn new(out: mpsc::UnboundedSender<FromWorker>) -> Self {
        Self {
            out,
            cache: Mutex::new(HashMap::new()),
            waiting: Mutex::new(HashMap::new()),
        }
    }

    /// The secret, asking the host for it when it is not held yet. `None`
    /// when the host has none or does not answer.
    pub async fn get(&self, purpose: &str, item: &str) -> Option<Secret> {
        if let Some(held) = self.cached(purpose, item) {
            return Some(held);
        }
        let key = (purpose.to_owned(), item.to_owned());
        let (answer, answered) = oneshot::channel();
        let first = {
            let mut waiting = self.waiting.lock().ok()?;
            let queue = waiting.entry(key).or_default();
            queue.push(answer);
            let first = queue.len() == 1;
            drop(waiting);
            first
        };
        if first {
            let _ = self.out.send(FromWorker::NeedSecret {
                purpose: purpose.to_owned(),
                item: item.to_owned(),
            });
        }
        tokio::time::timeout(ANSWER_TIMEOUT, answered)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
    }

    /// The secret if it is held already, without asking the host.
    pub fn cached(&self, purpose: &str, item: &str) -> Option<Secret> {
        self.cache
            .lock()
            .ok()?
            .get(&(purpose.to_owned(), item.to_owned()))
            .cloned()
    }

    /// The host's answer to a `NeedSecret`.
    pub fn deliver(&self, purpose: &str, item: &str, bytes: Option<&Bytes>) {
        let key = (purpose.to_owned(), item.to_owned());
        let secret = bytes
            .and_then(|b| std::str::from_utf8(&b.0).ok())
            .map(|text| Zeroizing::new(text.trim().to_owned()))
            .filter(|text| !text.is_empty());
        if let (Some(secret), Ok(mut cache)) = (&secret, self.cache.lock()) {
            cache.insert(key.clone(), secret.clone());
        }
        let waiters = self
            .waiting
            .lock()
            .ok()
            .and_then(|mut waiting| waiting.remove(&key))
            .unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(secret.clone());
        }
    }

    /// Drop a secret Discord refused, so the next use asks the host again
    /// (after the administrator has set a new one).
    pub fn forget(&self, purpose: &str, item: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(&(purpose.to_owned(), item.to_owned()));
        }
    }

    /// Keep a secret the worker created (a webhook token) and hand it to the
    /// host's store.
    pub fn store(&self, purpose: &str, item: &str, value: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                (purpose.to_owned(), item.to_owned()),
                Zeroizing::new(value.to_owned()),
            );
        }
        let _ = self.out.send(FromWorker::StoreSecret {
            purpose: purpose.to_owned(),
            item: item.to_owned(),
            bytes: Bytes(value.as_bytes().to_vec()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_request_answers_every_waiter_and_is_cached() {
        let (out, mut sent) = mpsc::unbounded_channel();
        let secrets = std::sync::Arc::new(Secrets::new(out));
        let a = tokio::spawn({
            let secrets = std::sync::Arc::clone(&secrets);
            async move { secrets.get(BOT_TOKEN, TOKEN_ITEM).await }
        });
        let b = tokio::spawn({
            let secrets = std::sync::Arc::clone(&secrets);
            async move { secrets.get(BOT_TOKEN, TOKEN_ITEM).await }
        });
        let Some(FromWorker::NeedSecret { purpose, item }) = sent.recv().await else {
            panic!("asks the host");
        };
        assert_eq!((purpose.as_str(), item.as_str()), (BOT_TOKEN, TOKEN_ITEM));
        // Both waiters registered before the answer.
        let key = (BOT_TOKEN.to_owned(), TOKEN_ITEM.to_owned());
        tilo_test_env::within("both waiters register", async {
            while secrets
                .waiting
                .lock()
                .unwrap()
                .get(&key)
                .map_or(0, Vec::len)
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        secrets.deliver(BOT_TOKEN, TOKEN_ITEM, Some(&Bytes(b"tok\n".to_vec())));
        assert_eq!(a.await.unwrap().as_deref().map(String::as_str), Some("tok"));
        assert_eq!(b.await.unwrap().as_deref().map(String::as_str), Some("tok"));
        assert!(sent.try_recv().is_err(), "asked once");
        assert!(secrets.get(BOT_TOKEN, TOKEN_ITEM).await.is_some(), "cached");
        secrets.forget(BOT_TOKEN, TOKEN_ITEM);
        secrets.deliver(BOT_TOKEN, TOKEN_ITEM, None);
        assert!(secrets.cache.lock().unwrap().is_empty());
    }
}
