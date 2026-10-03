//! What an agent remembers across messages.
//!
//! Two sources, one shape. The host hands every task the agent's own durable
//! history (`EnrolledTaskEnvelope::history`: its earlier requests, the exact
//! operations each step ran with their outcomes, and its replies), composed
//! from the Store, so memory survives a crash or a host restart. Within one
//! process the agent also records each finished task itself, so a host that
//! predates the history still gets a conversation.
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
    /// When it was asked (Unix ms), when known.
    pub at_ms: Option<u64>,
}

/// The agent's rolling conversation memory.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    pub(crate) exchanges: VecDeque<Exchange>,
    /// Exchanges older than those kept, known only to exist.
    omitted: usize,
    /// Workspace members as the host's roster names them, one line each.
    people: Vec<String>,
}

fn clip(text: &str, most: usize) -> String {
    crate::live::head(text.trim(), most).to_owned()
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
            at_ms: None,
        });
        while self.exchanges.len() > MEMORY_EXCHANGES {
            self.exchanges.pop_front();
            self.omitted += 1;
        }
    }

    /// Replace memory with the host's durable history of this agent (JSON
    /// `{"v":1,"tasks":[…],"omitted":n}`). A malformed or unknown history
    /// leaves memory unchanged and returns false.
    pub fn restore(&mut self, history: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(history) else {
            return false;
        };
        if value.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
            return false;
        }
        let Some(tasks) = value.get("tasks").and_then(serde_json::Value::as_array) else {
            return false;
        };
        let text = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let exchanges = tasks
            .iter()
            .map(|task| {
                let actions = task
                    .get("steps")
                    .and_then(serde_json::Value::as_array)
                    .map(|steps| {
                        steps
                            .iter()
                            .map(|step| {
                                let mut line = format!("{} ", text(step, "app"));
                                let record = text(step, "record");
                                if !record.is_empty() {
                                    let _ = write!(line, "({record}) ");
                                }
                                line.push_str(&text(step, "action"));
                                let resource = text(step, "resource");
                                if !resource.is_empty() {
                                    let _ = write!(line, " on {resource}");
                                }
                                let args = text(step, "args");
                                if !args.is_empty() && args != "{}" {
                                    let _ = write!(line, " {args}");
                                }
                                let _ = write!(line, " → {}", text(step, "outcome"));
                                clip(&line, 600)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let outcome = text(task, "outcome");
                let reply = text(task, "reply");
                Exchange {
                    request: clip(&text(task, "request"), 1_200),
                    actions,
                    more_actions: task
                        .get("more_steps")
                        .and_then(serde_json::Value::as_u64)
                        .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX)),
                    reply: if outcome == "succeeded" || outcome.is_empty() {
                        clip(&reply, 1_200)
                    } else {
                        clip(&format!("({outcome}) {reply}"), 1_200)
                    },
                    at_ms: task.get("at_ms").and_then(serde_json::Value::as_u64),
                }
            })
            .collect();
        self.exchanges = exchanges;
        self.people = value
            .get("people")
            .and_then(serde_json::Value::as_array)
            .map(|people| {
                people
                    .iter()
                    .take(24)
                    .filter_map(|person| {
                        let name = clip(person.get("name")?.as_str()?, 80);
                        let role = text(person, "role");
                        let requester = person
                            .get("requester")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        Some(match (role.is_empty(), requester) {
                            (_, true) => format!("{name} ({role}; the person you work for)"),
                            (false, false) => format!("{name} ({role})"),
                            (true, false) => name,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.omitted = value
            .get("omitted")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
        true
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
        if !self.people.is_empty() {
            let _ = writeln!(
                s,
                "People in this workspace (write @Name to mention someone): {}\n",
                self.people.join(", ")
            );
        }
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

    pub fn len(&self) -> usize {
        self.exchanges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.exchanges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(tasks: usize) -> String {
        serde_json::json!({
            "v": 1,
            "omitted": 2,
            "tasks": (0..tasks).map(|index| serde_json::json!({
                "at_ms": 1000 + index,
                "request": format!("request {index} {}", "r".repeat(700)),
                "reply": format!("reply {index} {}", "y".repeat(700)),
                "outcome": "succeeded",
                "steps": (0..6).map(|step| serde_json::json!({
                    "app": "so.tilo.word-processor",
                    "record": "tab:1",
                    "resource": "word-processor.document",
                    "action": format!("op-{step}"),
                    "args": "{\"text\":\"Thunderstorms roll in.\"}",
                    "outcome": "succeeded"
                })).collect::<Vec<_>>(),
                "more_steps": 1
            })).collect::<Vec<_>>()
        })
        .to_string()
    }

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
        assert!(memory.len() <= MEMORY_EXCHANGES);
        assert!(rendered.contains("reply 19"));
        assert!(!rendered.contains("reply 0\n"));
        assert!(rendered.contains("older exchange(s) not shown"));
    }

    /// After a restart the process has nothing in memory; the host's durable
    /// history restores it, action log included, tiered and capped.
    #[test]
    fn host_history_restores_memory_after_a_restart() {
        let mut memory = Memory::default();
        assert!(memory.render().is_empty());
        assert!(memory.restore(&history(12)));
        let rendered = memory.render();
        assert!(rendered.len() <= MEMORY_CHARS, "{}", rendered.len());
        assert!(rendered.contains("request 11"), "the newest is kept");
        assert!(
            rendered.contains(
                "You did: so.tilo.word-processor (tab:1) op-0 on word-processor.document \
                 {\"text\":\"Thunderstorms roll in.\"} → succeeded"
            ),
            "{rendered}"
        );
        assert!(rendered.contains("older exchange(s) not shown"));
        // Recent in full, older summarised.
        let newest = rendered.find("request 11").unwrap();
        assert!(rendered[newest..].contains(&"r".repeat(600)));
        let mut people = Memory::default();
        assert!(people.restore(
            r#"{"v":1,"tasks":[],"people":[{"name":"Ada","role":"owner","requester":true},{"name":"Ben","role":"editor"}]}"#
        ));
        assert_eq!(
            people.render(),
            "People in this workspace (write @Name to mention someone): Ada (owner; the person \
             you work for), Ben (editor)\n\n"
        );
        assert!(!memory.restore("{\"v\":9,\"tasks\":[]}"));
        assert!(!memory.restore("not json"));
        assert_eq!(memory.len(), 12, "a bad history leaves memory as it was");
    }
}
