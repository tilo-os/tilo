//! Bundled sample inputs so the app opens with a real diff and a real
//! three-way conflict to resolve — useful as a first-run demo and as the
//! content the headless screenshot renders.

/// A named text input with a short display label (shown in headers / pickers).
#[derive(Clone, Debug)]
pub struct Sample {
    pub label: String,
    pub text: String,
}

impl Sample {
    fn new(label: &str, text: &str) -> Self {
        Self {
            label: label.to_string(),
            text: text.to_string(),
        }
    }
}

/// The two-way diff demo: an old and a new revision of a small Rust file with
/// additions, removals, and modified lines so every row kind is represented.
pub fn diff_pair() -> (Sample, Sample) {
    let left = Sample::new(
        "rate_limiter.rs · HEAD",
        "\
use std::time::Instant;

/// Token-bucket rate limiter.
pub struct RateLimiter {
    capacity: u32,
    tokens: u32,
    last: Instant,
}

impl RateLimiter {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            tokens: capacity,
            last: Instant::now(),
        }
    }

    pub fn try_acquire(&mut self) -> bool {
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }
}
",
    );

    let right = Sample::new(
        "rate_limiter.rs · working tree",
        "\
use std::time::{Duration, Instant};

/// Token-bucket rate limiter with time-based refill.
pub struct RateLimiter {
    capacity: u32,
    tokens: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            tokens: capacity as f64,
            refill_per_sec,
            last: Instant::now(),
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec)
            .min(self.capacity as f64);
        self.last = Instant::now();
    }

    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
",
    );

    (left, right)
}

/// A longer two-way diff demo (a small module with edits scattered top to
/// bottom) so a screenshot overflows the viewport — proving the aligned
/// monospace columns, the per-line change tints, and the draggable overlay
/// scrollbar all at once. Screenshot / demo use only.
pub fn diff_pair_long() -> (Sample, Sample) {
    let left = Sample::new(
        "cache.rs · HEAD",
        "\
use std::collections::HashMap;
use std::time::Instant;

/// A tiny in-memory cache with per-entry expiry.
pub struct Cache {
    map: HashMap<String, Entry>,
    capacity: usize,
}

struct Entry {
    value: String,
    stored: Instant,
}

impl Cache {
    pub fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            capacity,
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|e| e.value.as_str())
    }

    pub fn insert(&mut self, key: String, value: String) {
        if self.map.len() >= self.capacity {
            self.evict_one();
        }
        self.map.insert(key, Entry { value, stored: Instant::now() });
    }

    fn evict_one(&mut self) {
        if let Some(k) = self.map.keys().next().cloned() {
            self.map.remove(&k);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}
",
    );

    let right = Sample::new(
        "cache.rs · working tree",
        "\
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A tiny in-memory cache with per-entry TTL expiry.
pub struct Cache {
    map: HashMap<String, Entry>,
    capacity: usize,
    ttl: Duration,
}

struct Entry {
    value: String,
    stored: Instant,
}

impl Cache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            map: HashMap::new(),
            capacity,
            ttl,
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        let e = self.map.get(key)?;
        if e.stored.elapsed() > self.ttl {
            return None;
        }
        Some(e.value.as_str())
    }

    pub fn insert(&mut self, key: String, value: String) {
        if self.map.len() >= self.capacity {
            self.evict_oldest();
        }
        self.map.insert(key, Entry { value, stored: Instant::now() });
    }

    fn evict_oldest(&mut self) {
        if let Some(k) = self
            .map
            .iter()
            .min_by_key(|(_, e)| e.stored)
            .map(|(k, _)| k.clone())
        {
            self.map.remove(&k);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
",
    );

    (left, right)
}

/// The three-way merge demo: a common base config and two divergent edits that
/// genuinely conflict on the `timeout` line and the feature list, while other
/// regions auto-resolve cleanly.
pub fn merge_triple() -> (Sample, Sample, Sample) {
    let base = Sample::new(
        "config.toml · base",
        "\
[server]
host = \"0.0.0.0\"
port = 8080
timeout = 30

[features]
search = true
export = false

[logging]
level = \"info\"
",
    );

    let ours = Sample::new(
        "config.toml · ours",
        "\
[server]
host = \"0.0.0.0\"
port = 8080
timeout = 60

[features]
search = true
export = true
sync = true

[logging]
level = \"debug\"
",
    );

    let theirs = Sample::new(
        "config.toml · theirs",
        "\
[server]
host = \"127.0.0.1\"
port = 8080
timeout = 15

[features]
search = true
export = false

[logging]
level = \"info\"
format = \"json\"
",
    );

    (base, ours, theirs)
}
