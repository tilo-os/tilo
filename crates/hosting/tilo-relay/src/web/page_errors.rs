//! A page's own uncaught errors, as its door reports them.
//!
//! A page that came in through a host's door sends what it did not catch to
//! `<door>errors`: one beacon, five a page load at most. The door takes each
//! report as one log line for the site's operator and keeps nothing else: no
//! report reaches a workspace, a store or another person.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use tilo_relay_protocol::directory::PageError;

/// The longest string a line carries; longer ones are cut.
const FIELD_MAX: usize = 1024;
/// Reports a relay logs per second, and the burst it takes: pages break in
/// a crowd (a failed release is every visitor at once), and a host must not
/// spend its logs on the flood.
const RATE: f64 = 20.0;
const BURST: f64 = 50.0;

/// A relay's page error reports.
pub(super) struct PageErrors {
    bucket: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Default for PageErrors {
    fn default() -> Self {
        Self {
            bucket: Mutex::new(Bucket {
                tokens: BURST,
                at: Instant::now(),
            }),
        }
    }
}

impl PageErrors {
    /// Log `body`, the report a page on `workspace`'s site sent, unless it is
    /// no report or this relay has logged its fill for now.
    pub(super) fn report(&self, workspace: &str, body: &[u8]) {
        if !self.admit() {
            return;
        }
        let Ok(error) = PageError::parse(body) else {
            return;
        };
        // A page that stopped on its failure screen: what it told the
        // person, and the host it could not open the workspace at.
        if error.kind() == "failed" {
            tracing::warn!(
                workspace = ?cut(workspace),
                cause = ?cut(&error.message),
                host = ?cut(&error.source),
                page = ?cut(&error.page),
                build = ?cut(&error.build),
                "a page could not open its workspace",
            );
            return;
        }
        // Debug formatting: a page's message is text a field cannot break
        // the line with.
        tracing::warn!(
            workspace = ?cut(workspace),
            kind = error.kind(),
            message = ?cut(&error.message),
            source = ?cut(&error.source),
            line = error.line,
            column = error.column,
            page = ?cut(&error.page),
            build = ?cut(&error.build),
            stack = ?cut(&error.stack),
            "a page reported an uncaught error",
        );
    }

    /// One report of the relay's fill, or `false` while it is spent.
    fn admit(&self) -> bool {
        let mut bucket = self.bucket.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        bucket.tokens =
            (bucket.tokens + now.duration_since(bucket.at).as_secs_f64() * RATE).min(BURST);
        bucket.at = now;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }
}

/// `text` up to [`FIELD_MAX`] characters.
fn cut(text: &str) -> &str {
    match text.char_indices().nth(FIELD_MAX) {
        Some((end, _)) => text.get(..end).unwrap_or(text),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// What a relay logs while `run` reports.
    fn logged(run: impl FnOnce(&PageErrors)) -> String {
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let reports = PageErrors::default();
        tracing::subscriber::with_default(subscriber, || run(&reports));
        let logged = sink
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        String::from_utf8(logged).unwrap()
    }

    fn report() -> Vec<u8> {
        serde_json::json!({
            "kind": "unhandledrejection", "message": "cannot read 'name'",
            "source": "https://site.example/v/7/page.js", "line": 12, "column": 4,
            "stack": "at page.js:12", "page": "/", "build": "7",
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_page_error_is_one_line_naming_its_workspace_and_page() {
        let line = logged(|reports| reports.report("coffee", &report()));
        let lines: Vec<&str> = line.lines().collect();
        assert_eq!(lines.len(), 1, "{line}");
        for said in [
            "WARN",
            "workspace=\"coffee\"",
            "kind=\"unhandledrejection\"",
            "cannot read 'name'",
            "page=\"/\"",
            "build=\"7\"",
            "line=12",
        ] {
            assert!(lines[0].contains(said), "no {said} in {line}");
        }
    }

    #[test]
    fn a_page_that_stopped_says_its_cause_and_the_host_it_could_not_reach() {
        let failed = serde_json::json!({
            "kind": "failed", "message": "unreachable: TypeError: Failed to fetch",
            "source": "https://127.0.0.1:24100", "page": "/", "build": "7",
        })
        .to_string()
        .into_bytes();
        let line = logged(|reports| reports.report("coffee", &failed));
        let lines: Vec<&str> = line.lines().collect();
        assert_eq!(lines.len(), 1, "{line}");
        for said in [
            "WARN",
            "a page could not open its workspace",
            "workspace=\"coffee\"",
            "cause=\"unreachable: TypeError: Failed to fetch\"",
            "host=\"https://127.0.0.1:24100\"",
            "build=\"7\"",
        ] {
            assert!(lines[0].contains(said), "no {said} in {line}");
        }
    }

    #[test]
    fn nothing_that_is_not_a_report_reaches_the_log() {
        let line = logged(|reports| {
            reports.report("coffee", b"not json");
            reports.report("coffee", &vec![b' '; 8192]);
        });
        assert_eq!(line, "");
    }

    #[test]
    fn a_line_carries_no_more_than_a_field_and_the_relay_its_fill() {
        let long = serde_json::json!({"kind": "error", "message": "x".repeat(3000)})
            .to_string()
            .into_bytes();
        let line = logged(|reports| {
            reports.report("coffee", &long);
            for _ in 0..200 {
                reports.report("coffee", &report());
            }
        });
        let lines: Vec<&str> = line.lines().collect();
        let reported = lines
            .iter()
            .filter(|line| line.contains("a page reported"))
            .count();
        assert!(
            (50..=51).contains(&reported),
            "the burst, then nothing: {reported} of {} lines",
            lines.len()
        );
        assert!(
            lines[0].len() < 4096,
            "one report, one bounded line: {} bytes",
            lines[0].len()
        );
    }
}
