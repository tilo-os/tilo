//! "Send to {agent}: …" in search: what the person typed, handed to the agent they keep in the
//! workspace — a new one where they keep none — through the session's one send
//! (`session:send-to#send`), the send a window's box, text typed on the canvas and an agent run
//! (docs/designs/app-language/shell.md §8.1). It lands in their chat with it, and Chat opens that
//! chat on the shell.
//!
//! The row answers every query and stands after everything the query names, so it crowds nothing
//! out; it takes text nothing matched, as the canvas does, so text that names nothing is a
//! request the person can hand over rather than a dead end.

use tilo_desktop_search::providers::ask_agent;
use tilo_desktop_search::{DESKTOP_APP, Hit, InvokeTarget, Source, Target};
use ui::floating_surface::Recipient;

use crate::send_to;

/// The request the person types, handed to their agent.
pub(crate) struct AskAgent {
    /// The workspace the request is sent in: the one the search was opened in.
    workspace: String,
    /// Who takes it: the agent the person keeps there, or a new one.
    to: Recipient,
}

impl AskAgent {
    pub(crate) fn new(workspace: &str, name: Option<&str>) -> Self {
        Self {
            workspace: workspace.to_owned(),
            to: crate::agents::agent_recipient(name),
        }
    }
}

impl Source for AskAgent {
    fn id(&self) -> &'static str {
        ask_agent::PROVIDER
    }

    fn name(&self) -> String {
        tilo_desktop_search::i18n::t("search-kind-agent")
    }

    /// The request is the person's words as they typed them.
    fn verbatim(&self) -> bool {
        true
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let typed = needle.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        vec![Hit {
            // The row says what was typed back: nothing in it is a match.
            row: Some(ui::search::SearchRow::echoing_query()),
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ask_agent::kind(),
            provider: self.id().into(),
            id: format!("send:{typed}"),
            title: tilo_desktop_search::i18n::t_args(
                "search-send-to-agent",
                &[
                    ("agent", self.to.name.as_str().into()),
                    ("query", typed.into()),
                ],
            ),
            subtitle: None,
            facts: Vec::new(),
            score: 1,
            icon: Some(ui::icons::Icon::Bot),
            target: Target::Invoke(Box::new(
                InvokeTarget::action(
                    DESKTOP_APP,
                    &self.workspace,
                    send_to::RESOURCE,
                    send_to::SEND,
                )
                .with_argument("to", serde_json::json!([self.to]))
                .with_argument("text", typed)
                .with_argument("workspace", self.workspace.as_str()),
            )),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: Option<&str>, typed: &str) -> Hit {
        let source = AskAgent::new("workspace", name);
        let mut rows = source.query(typed);
        assert_eq!(rows.len(), 1, "{typed:?} is one row");
        rows.remove(0)
    }

    tilo_test_env::scenario! {
        fn the_agent_row_sends_what_was_typed_and_an_empty_query_offers_none;

        phase the_row_sends_what_was_typed_to_the_agent_it_names;
        // The row says who takes what was typed, and sends it through the session's one send, in the
        // workspace the search was opened in: the agent the person keeps there, or the new one the
        // send makes where they keep none.
        {
            let hit = row(Some("Tide"), "book a table for two");
            assert_eq!(hit.title, "Send to Tide: book a table for two");
            assert_eq!(hit.kind, ask_agent::kind());
            assert!(ask_agent::is_row(&hit), "the session's own row");
            // The row says what was typed back, so nothing in it is drawn as a match.
            assert!(
                matches!(&hit.row, Some(ui::search::SearchRow::Command(row)) if row.echoes_query),
                "the row declares it echoes the query: {:?}",
                hit.row
            );
            let Target::Invoke(target) = &hit.target else {
                panic!("the row runs the send");
            };
            assert_eq!(target.app, DESKTOP_APP);
            assert_eq!(target.address.resource.as_str(), send_to::RESOURCE);
            assert_eq!(target.address.action.as_str(), send_to::SEND);
            assert_eq!(target.arguments["text"], "book a table for two");
            assert_eq!(target.arguments["workspace"], "workspace");
            assert_eq!(
                target.arguments["to"],
                serde_json::json!([{"kind": "assistant", "name": "Tide"}])
            );

            let fresh = row(None, "what is left?");
            assert_eq!(fresh.title, "Send to Assistant: what is left?");
            let Target::Invoke(target) = &fresh.target else {
                panic!("the row runs the send");
            };
            assert_eq!(
                target.arguments["to"],
                serde_json::json!([{"kind": "assistant", "name": "Assistant"}])
            );
        }

        phase an_empty_query_offers_nothing;
        // Nothing typed is no request to hand over.
        {
            let source = AskAgent::new("workspace", None);
            for typed in ["", "   "] {
                assert!(source.query(typed).is_empty(), "{typed:?} sends nothing");
            }
        }
    }
}
