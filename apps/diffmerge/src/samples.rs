//! Bundled sample inputs for the first-run demo and headless screenshot
//! renderer, including a three-way conflict to resolve.

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
use ui::time::Instant;

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
            last: ui::time::now(),
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
use ui::time::{Duration, Instant};

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
            last: ui::time::now(),
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec)
            .min(self.capacity as f64);
        self.last = ui::time::now();
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
