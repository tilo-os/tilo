//! What an agent remembers across messages.
//!
//! The agent records each finished task of this process: what was asked,
//! the operations it ran with their outcomes, and what it replied.
//!
//! Rendered for the model as plain text after the stable prompt prefix: the
//! most recent exchanges in full (with each action's argument summary, which
//! is what lets it find and undo its own writing), older ones summarised,
//! under a hard character cap that drops the oldest first. Memory is display
//! input only: it names what happened, it grants nothing.

use std::collections::VecDeque;
use std::fmt::Write as _;

/// Most exchanges kept from this process's own tasks.
pub const MEMORY_EXCHANGES: usize = 8;
/// Most characters of rendered memory (about 2k tokens).
pub const MEMORY_CHARS: usize = 8_000;
/// Most recent exchanges rendered in full; older ones are summarised.
const FULL_EXCHANGES: usize = 4;

/// One earlier exchange: what was asked, what the agent did, what it said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub request: String,
    /// One line per operation: what, on what, with which arguments, outcome.
    pub actions: Vec<String>,
    /// Operations left out of `actions`.
    pub more_actions: usize,
    pub reply: String,
}

/// The agent's rolling conversation memory.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    pub(crate) exchanges: VecDeque<Exchange>,
    /// Exchanges older than those kept, known only to exist.
    omitted: usize,
}

/// Most bytes of an action label.
const MAX_LABEL_BYTES: usize = 120;

/// The longest prefix of `text` within `max` bytes, cut on a character
/// boundary.
pub fn head(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

/// An action's label as people read it: the action, then what it acts on.
pub fn action_label(action_label: &str, resource_label: &str) -> String {
    let label = if resource_label.is_empty() {
        action_label.to_owned()
    } else {
        format!("{action_label}: {resource_label}")
    };
    head(&label, MAX_LABEL_BYTES).to_owned()
}

fn clip(text: &str, most: usize) -> String {
    head(text.trim(), most).to_owned()
}

impl Memory {
    /// Record one finished task of this process.
    pub fn remember(&mut self, request: &str, actions: Vec<String>, reply: &str) {
        let more_actions = actions.len().saturating_sub(16);
        self.exchanges.push_back(Exchange {
            request: clip(request, 1_200),
            actions: actions
                .into_iter()
                .take(16)
                .map(|action| clip(&action, 400))
                .collect(),
            more_actions,
            reply: clip(reply, 1_200),
        });
        while self.exchanges.len() > MEMORY_EXCHANGES {
            self.exchanges.pop_front();
            self.omitted += 1;
        }
    }

    /// The memory as the model reads it; empty when there is none. Recent
    /// exchanges in full, older ones summarised, oldest dropped first to fit
    /// [`MEMORY_CHARS`].
    pub fn render(&self) -> String {
        let mut skip = 0;
        loop {
            let text = self.render_from(skip);
            if text.len() <= MEMORY_CHARS || skip >= self.exchanges.len() {
                return text;
            }
            skip += 1;
        }
    }

    fn render_from(&self, skip: usize) -> String {
        let mut s = String::new();
        let kept: Vec<&Exchange> = self.exchanges.iter().skip(skip).collect();
        if kept.is_empty() {
            return s;
        }
        s.push_str(
            "Earlier in this conversation (oldest first). \"You did\" is your own action \
             log: the exact operations you ran, with their arguments and outcomes.\n",
        );
        let omitted = self.omitted + skip;
        if omitted > 0 {
            let _ = writeln!(s, "({omitted} older exchange(s) not shown)");
        }
        let full_from = kept.len().saturating_sub(FULL_EXCHANGES);
        for (index, exchange) in kept.iter().enumerate() {
            let full = index >= full_from;
            let most = if full { 1_200 } else { 160 };
            let _ = writeln!(s, "- Person: {}", clip(&exchange.request, most));
            let shown = if full { exchange.actions.len() } else { 3 };
            for action in exchange.actions.iter().take(shown) {
                let action = if full {
                    action.clone()
                } else {
                    clip(action, 140)
                };
                let _ = writeln!(s, "  You did: {action}");
            }
            let more = exchange.more_actions + exchange.actions.len().saturating_sub(shown);
            if more > 0 {
                let _ = writeln!(s, "  (and {more} more operation(s))");
            }
            let _ = writeln!(s, "  You replied: {}", clip(&exchange.reply, most));
        }
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_bounded_and_drops_the_oldest_first() {
        let mut memory = Memory::default();
        for index in 0..20 {
            memory.remember(
                &format!("request {index} {}", "x".repeat(500)),
                vec!["Type text {\"text\":\"…\"} → done".into(); 3],
                &format!("reply {index}"),
            );
        }
        let rendered = memory.render();
        assert!(rendered.len() <= MEMORY_CHARS, "{}", rendered.len());
        assert!(memory.exchanges.len() <= MEMORY_EXCHANGES);
        assert!(rendered.contains("reply 19"));
        assert!(!rendered.contains("reply 0\n"));
        assert!(rendered.contains("older exchange(s) not shown"));
    }
}
