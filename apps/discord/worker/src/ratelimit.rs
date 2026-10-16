//! Discord's rate limits, per bucket and global.
//!
//! Discord names a route's bucket in `X-RateLimit-Bucket` and says how many
//! calls are left in it (`X-RateLimit-Remaining`) and when it refills
//! (`X-RateLimit-Reset-After`). A bucket is shared by routes with the same
//! hash and the same major parameter (channel, guild, or webhook and its
//! token). Calls in one bucket go one at a time: the caller holds the
//! bucket's lock for the whole call, which also keeps calls to one channel
//! in the order they were made, and waits for the refill first when the
//! bucket is empty. A 429 says how long to wait; a global one stops every
//! call until then.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tilo_net::HeaderMap;
use tokio::sync::OwnedMutexGuard;
use tokio::time::Instant;

/// What one bucket has left.
#[derive(Debug, Default)]
pub struct Bucket {
    remaining: Option<u64>,
    reset_at: Option<Instant>,
}

impl Bucket {
    /// Whether the next call would have to wait for a refill.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.remaining == Some(0) && self.reset_at.is_some_and(|at| at > Instant::now())
    }
}

/// A held bucket: the call it guards runs alone in its bucket.
pub struct Held {
    guard: OwnedMutexGuard<Bucket>,
    route: String,
}

#[derive(Default)]
pub struct RateLimits {
    /// Route key → the bucket hash Discord named for it.
    hashes: Mutex<HashMap<String, String>>,
    /// Bucket hash and major parameter (or the route key, before Discord
    /// has named the bucket) → its state.
    buckets: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Bucket>>>>,
    global_until: Mutex<Option<Instant>>,
    /// Calls that got a 429 or waited for a bucket or the global limit.
    limited: AtomicU64,
}

impl RateLimits {
    fn slot(&self, route: &str) -> Arc<tokio::sync::Mutex<Bucket>> {
        let hash = self
            .hashes
            .lock()
            .ok()
            .and_then(|hashes| hashes.get(route).cloned());
        let key = hash.map_or_else(
            || route.to_owned(),
            |hash| format!("{hash}:{}", major(route)),
        );
        let mut buckets = self.buckets.lock_recover();
        Arc::clone(buckets.entry(key).or_default())
    }

    /// Take the bucket of `route` (see [`route_key`]) and wait until it and
    /// the global limit allow a call.
    pub async fn acquire(&self, route: &str) -> Held {
        let mut guard = self.slot(route).lock_owned().await;
        let mut waited = self.wait_global().await;
        if guard.exhausted()
            && let Some(at) = guard.reset_at
        {
            waited = true;
            tokio::time::sleep_until(at).await;
        }
        if waited {
            self.limited.fetch_add(1, Ordering::Relaxed);
        }
        if guard.reset_at.is_some_and(|at| at <= Instant::now()) {
            guard.remaining = None;
            guard.reset_at = None;
        }
        Held {
            guard,
            route: route.to_owned(),
        }
    }

    /// Whether a call on `route` would wait for its bucket now, either
    /// because another call holds it or because it is empty.
    #[must_use]
    pub fn busy(&self, route: &str) -> bool {
        self.slot(route)
            .try_lock()
            .map_or(true, |bucket| bucket.exhausted())
    }

    /// How many calls a 429 or a rate-limit wait has held up so far.
    #[must_use]
    pub fn limited_total(&self) -> u64 {
        self.limited.load(Ordering::Relaxed)
    }

    /// Wait out a global limit; whether there was one.
    async fn wait_global(&self) -> bool {
        let until = self.global_until.lock().ok().and_then(|g| *g);
        match until {
            Some(until) if until > Instant::now() => {
                tokio::time::sleep_until(until).await;
                true
            }
            _ => false,
        }
    }

    /// Record what a response's headers say about `held`'s bucket.
    pub fn observe(&self, held: &mut Held, headers: &HeaderMap) {
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        if let Some(hash) = header("x-ratelimit-bucket")
            && let Ok(mut hashes) = self.hashes.lock()
        {
            hashes.insert(held.route.clone(), hash.to_owned());
        }
        if let Some(remaining) = header("x-ratelimit-remaining").and_then(|r| r.parse().ok()) {
            held.guard.remaining = Some(remaining);
        }
        if let Some(after) = header("x-ratelimit-reset-after").and_then(seconds) {
            held.guard.reset_at = Instant::now().checked_add(after);
        }
    }

    /// How long a 429 says to wait, from its body's `retry_after` or the
    /// `Retry-After` header. A global 429 also holds every other call.
    pub fn retry_after(&self, held: &mut Held, headers: &HeaderMap, body: &Value) -> Duration {
        self.limited.fetch_add(1, Ordering::Relaxed);
        let wait = body
            .get("retry_after")
            .and_then(Value::as_f64)
            .and_then(|s| Duration::try_from_secs_f64(s).ok())
            .or_else(|| {
                headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(seconds)
            })
            .unwrap_or(Duration::from_secs(1));
        let global = body.get("global").and_then(Value::as_bool) == Some(true)
            || headers.contains_key("x-ratelimit-global");
        let until = Instant::now().checked_add(wait);
        if global {
            if let Ok(mut global_until) = self.global_until.lock() {
                *global_until = until;
            }
        } else {
            held.guard.remaining = Some(0);
            held.guard.reset_at = until;
        }
        wait
    }
}

fn seconds(text: &str) -> Option<Duration> {
    text.trim()
        .parse::<f64>()
        .ok()
        .and_then(|s| Duration::try_from_secs_f64(s).ok())
}

/// The part of a route key that Discord's buckets are per: the channel,
/// guild or webhook (with its token) the route is about.
fn major(route: &str) -> String {
    route
        .split('/')
        .skip(1)
        .take(3)
        .collect::<Vec<_>>()
        .join("/")
}

/// The route of a request for rate limiting: its method and path with every
/// id that is not a major parameter (message ids, emoji, user ids) replaced,
/// because Discord counts those calls in one bucket.
#[must_use]
pub fn route_key(method: &str, path: &str) -> String {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let mut key = vec![method.to_owned()];
    let mut previous = "";
    let mut before_previous = "";
    for segment in &segments {
        let major = matches!(previous, "channels" | "guilds" | "webhooks")
            || (before_previous == "webhooks" && previous != "messages");
        let variable = segment.bytes().all(|b| b.is_ascii_digit()) || previous == "reactions";
        key.push(if major || !variable {
            (*segment).to_owned()
        } else {
            ":id".to_owned()
        });
        before_previous = previous;
        previous = segment;
    }
    key.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_keys_keep_major_parameters_only() {
        assert_eq!(
            route_key("GET", "/channels/11/messages/22"),
            "GET/channels/11/messages/:id"
        );
        assert_eq!(
            route_key("PUT", "/channels/11/messages/22/reactions/%F0%9F%91%8D/@me"),
            "PUT/channels/11/messages/:id/reactions/:id/@me"
        );
        assert_eq!(
            route_key("POST", "/webhooks/33/tok"),
            "POST/webhooks/33/tok"
        );
        assert_eq!(
            route_key("PATCH", "/webhooks/33/tok/messages/44"),
            "PATCH/webhooks/33/tok/messages/:id"
        );
        assert_eq!(
            route_key("PUT", "/guilds/1/members/2/roles/3"),
            "PUT/guilds/1/members/:id/roles/:id"
        );
        assert_eq!(major("POST/webhooks/33/tok"), "webhooks/33/tok");
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_bucket_waits_for_its_reset() {
        let limits = RateLimits::default();
        let route = route_key("POST", "/webhooks/1/t");
        let mut held = limits.acquire(&route).await;
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
        headers.insert("x-ratelimit-reset-after", "2.5".parse().unwrap());
        headers.insert("x-ratelimit-bucket", "abc".parse().unwrap());
        limits.observe(&mut held, &headers);
        drop(held);
        // The bucket is now known by its hash; the state moved with it, so
        // seed the hashed slot the way the next response would.
        let mut held = limits.acquire(&route).await;
        limits.observe(&mut held, &headers);
        drop(held);
        assert!(limits.busy(&route));
        let start = Instant::now();
        drop(limits.acquire(&route).await);
        assert!(start.elapsed() >= Duration::from_millis(2_500));
        assert!(!limits.busy(&route));
    }

    #[tokio::test(start_paused = true)]
    async fn a_global_429_holds_every_route() {
        let limits = RateLimits::default();
        let mut held = limits.acquire("GET/channels/1/messages").await;
        let wait = limits.retry_after(
            &mut held,
            &HeaderMap::new(),
            &serde_json::json!({"retry_after": 1.5, "global": true}),
        );
        drop(held);
        assert_eq!(wait, Duration::from_millis(1_500));
        let start = Instant::now();
        drop(limits.acquire("POST/channels/2/messages").await);
        assert!(start.elapsed() >= Duration::from_millis(1_500));
    }
}
