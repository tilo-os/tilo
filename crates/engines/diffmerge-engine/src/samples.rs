//! The samples `load-sample` opens: a two-way comparison and a three-way merge with conflicts, so
//! a person sees how each reads before they have two files at hand.

use serde_json::{Value as Json, json};

/// The sample a `kind` names, as the package reads its inputs.
pub fn sample(kind: &str) -> Result<String, String> {
    match kind {
        "diff" => Ok(diff_pair().to_string()),
        "merge" => Ok(merge_triple().to_string()),
        "folders" => Ok(folders().to_string()),
        other => Err(format!("`{other}` is no sample this app opens")),
    }
}

fn diff_pair() -> Json {
    json!({
        "left_label": "rate_limiter.rs (HEAD)",
        "left": "\
use ui::time::Instant;

pub struct RateLimiter {
    capacity: u32,
    tokens: u32,
    last: Instant,
    paused: bool,
}

impl RateLimiter {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            tokens: capacity,
            last: ui::time::now(),
            paused: false,
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
        "right_label": "rate_limiter.rs (working tree)",
        "right": "\
use ui::time::{Duration, Instant};

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
    })
}

fn merge_triple() -> Json {
    json!({
        "base_label": "config.toml (base)",
        "base": "\
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
        "ours_label": "config.toml (ours)",
        "ours": "\
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
        "theirs_label": "config.toml (theirs)",
        "theirs": "\
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
    })
}

/// The folder comparison a demonstration shows: no disk is read for it.
fn folders() -> Json {
    let entry = |path: &str, kind: &str, left_len: u64, right_len: u64, by_length: bool| {
        json!({
            "path": path,
            "kind": kind,
            "left_len": left_len,
            "right_len": right_len,
            "by_length": by_length,
        })
    };
    json!({
        "left_label": "release-1.4",
        "left_root": "/work/rate-limiter/release-1.4",
        "right_label": "main",
        "right_root": "/work/rate-limiter/main",
        "entries": [
            entry("Cargo.toml", "modified", 1_204, 1_261, true),
            entry("README.md", "modified", 8_120, 8_120, false),
            entry("docs/limits.md", "added", 3_040, 0, false),
            entry("src/bucket.rs", "modified", 12_880, 13_402, true),
            entry("src/limiter.rs", "modified", 20_115, 20_115, false),
            entry("src/legacy.rs", "removed", 0, 4_310, false),
            entry("tests/bucket.rs", "added", 5_602, 0, false),
            entry("tests/rate.rs", "modified", 9_310, 9_884, true),
        ],
        "truncated": false,
    })
}
