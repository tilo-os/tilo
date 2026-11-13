//! Handing the person's agent what they typed: the row "Send to {agent}: …", and the group it is
//! listed under (docs/designs/app-language/shell.md §8.1).
//!
//! The row answers every query and stands after everything the query names, so it crowds nothing
//! out. It takes text nothing matched, as the canvas does: a query the search found nothing for
//! still stands its row, so text that names nothing is a request the person can hand over rather
//! than a dead end.
//!
//! The row itself is the session's ([`PROVIDER`]): a request goes through the session's one send
//! (`session:send-to#send`, the send a window's box, the canvas and an agent run), and only the
//! session knows which agent the person keeps in a workspace.

use crate::model::Hit;

/// The id the session's source answers as: what search asks for the agent's rows, and how it
/// knows one.
pub const PROVIDER: &str = "ask-agent";

/// The kind the row is grouped under, in the desktop's language.
#[must_use]
pub fn kind() -> crate::ResourceKind {
    crate::ResourceKind::Custom(crate::i18n::t("search-kind-agent"))
}

/// Whether `hit` is the row that hands what was typed to the person's agent.
#[must_use]
pub fn is_row(hit: &Hit) -> bool {
    hit.provider == PROVIDER
}
