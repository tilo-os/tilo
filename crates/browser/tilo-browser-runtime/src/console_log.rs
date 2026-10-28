//! Warnings and errors the page's Rust code reports through `tracing` reach
//! the browser's console.
//!
//! The page installs no other subscriber, and without one every
//! `tracing::warn!` and `tracing::error!` is dropped: what went wrong in a
//! window could not be read anywhere. This one writes those two levels
//! (nothing below them) as one line each, `target: message key=value …`,
//! with `console.warn` and `console.error`. It holds no spans, and formats
//! only events it was asked for, so what is not logged costs as before.

use std::fmt::Write as _;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

/// Have the page's warnings and errors reach the console, unless something
/// already took the global subscriber.
pub(crate) fn install() {
    let _ = tracing::subscriber::set_global_default(Console);
}

struct Console;

impl Subscriber for Console {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if self.enabled(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= Level::WARN
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // Never asked for: no span is enabled.
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let mut line = Line(String::new(), String::new());
        event.record(&mut line);
        let Line(message, mut fields) = line;
        let mut text = format!("{}: {message}", metadata.target());
        if !fields.is_empty() {
            fields.insert(0, ' ');
            text.push_str(&fields);
        }
        if *metadata.level() == Level::ERROR {
            web_sys::console::error_1(&text.into());
        } else {
            web_sys::console::warn_1(&text.into());
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// An event's message, and its other fields as `key=value`.
struct Line(String, String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
            return;
        }
        if !self.1.is_empty() {
            self.1.push(' ');
        }
        let _ = write!(self.1, "{}={value:?}", field.name());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
            return;
        }
        if !self.1.is_empty() {
            self.1.push(' ');
        }
        let _ = write!(self.1, "{}={value}", field.name());
    }
}
