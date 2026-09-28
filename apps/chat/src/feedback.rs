//! Feedback channels: a channel whose top-level messages are reports.
//!
//! Nothing here is a second kind of message. A report is an ordinary
//! top-level message in a channel marked [`Channel::feedback`]; its votes are
//! the reactions of one canonical emoji ([`VOTE_EMOJI`]), so voting is the
//! reaction a Commenter can already add; its status is a typed fact on the
//! message ([`ReportState`]) set by [`crate::actions::ChatAction::SetReportStatus`],
//! which only an Editor may invoke. Other reactions stay ordinary reactions.
//!
//! How the list is ordered and filtered is this peer's view, never shared
//! state: two people triaging the same channel may sort it differently.
//!
//! [`Channel::feedback`]: crate::model::Channel::feedback

use serde::{Deserialize, Serialize};

use crate::model::Message;

/// The one reaction that counts as a vote on a report.
pub const VOTE_EMOJI: &str = "👍";

/// Where a report stands. Unset means [`ReportStatus::Open`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReportStatus {
    Open,
    Planned,
    InProgress,
    /// Fixed in image generation `generation`; a reporter whose build is at
    /// or past it has the fix.
    Fixed {
        generation: u64,
    },
    WontFix,
    /// The same report as the top-level message `of` in the same channel.
    Duplicate {
        of: String,
    },
}

impl ReportStatus {
    /// Stable machine key, shared by the palette, filters and tests.
    pub fn key(&self) -> &'static str {
        match self {
            ReportStatus::Open => "open",
            ReportStatus::Planned => "planned",
            ReportStatus::InProgress => "in_progress",
            ReportStatus::Fixed { .. } => "fixed",
            ReportStatus::WontFix => "wont_fix",
            ReportStatus::Duplicate { .. } => "duplicate",
        }
    }

    /// Whether a report in this state still asks for work.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            ReportStatus::Open | ReportStatus::Planned | ReportStatus::InProgress
        )
    }

    /// Whether a build at `generation` contains the fix.
    pub fn fixed_for(&self, generation: u64) -> bool {
        matches!(self, ReportStatus::Fixed { generation: fixed } if generation >= *fixed)
    }
}

/// A report's status, who set it and when — one authored claim, stored as one
/// atomic value so a merge can never pair one Editor's status with another's
/// name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportState {
    pub status: ReportStatus,
    pub by: String,
    pub since: i64,
}

/// Bounds on a drafted issue, checked by [`IssueDraft::is_valid`] before a
/// row carrying one is written.
const MAX_ISSUE_TITLE_BYTES: usize = 256;
const MAX_ISSUE_BODY_BYTES: usize = 16 * 1024;

/// A forge issue drafted for a report, attached to a reply in the report's
/// thread (`Attachment::Issue`). Nothing is filed until an Editor approves
/// the reply that carries it ([`crate::actions::ChatAction::ApproveIssue`]);
/// what is filed is exactly this draft, so what the Editor read is what the
/// forge receives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDraft {
    /// The report the issue is for: a top-level message of the channel.
    pub report: String,
    pub title: String,
    /// The issue's text, already free of anything personal: it is made only
    /// of what the report showed the room.
    pub body: String,
}

impl IssueDraft {
    pub fn is_valid(&self) -> bool {
        tilo_reference::is_chat_message_id(&self.report)
            && !self.title.trim().is_empty()
            && self.title.len() <= MAX_ISSUE_TITLE_BYTES
            && !self.title.contains(['\n', '\r'])
            && self.body.len() <= MAX_ISSUE_BODY_BYTES
    }
}

/// An Editor's approval of a drafted issue: who approved it and when, one
/// authored value like [`ReportState`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueApproval {
    pub by: String,
    pub since: i64,
}

/// How a feedback channel lists its reports on this peer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportSort {
    /// Most votes first; ties newest first.
    #[default]
    Votes,
    /// Newest first.
    New,
}

/// Which reports a feedback channel shows on this peer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFilter {
    /// Open, planned or in progress.
    #[default]
    Active,
    /// Fixed, won't fix or duplicate.
    Closed,
    All,
}

impl ReportFilter {
    pub fn admits(self, status: &ReportStatus) -> bool {
        match self {
            ReportFilter::Active => status.is_active(),
            ReportFilter::Closed => !status.is_active(),
            ReportFilter::All => true,
        }
    }
}

/// This peer's view of one feedback channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportView {
    pub sort: ReportSort,
    pub filter: ReportFilter,
}

/// The status a report reads as: what was set, or open.
pub fn status_of(message: &Message) -> ReportStatus {
    message
        .report
        .as_ref()
        .map_or(ReportStatus::Open, |state| state.status.clone())
}

/// Votes on a report: the people who reacted with [`VOTE_EMOJI`].
pub fn vote_count(message: &Message) -> usize {
    message
        .reactions
        .iter()
        .find(|reaction| reaction.emoji == VOTE_EMOJI)
        .map_or(0, crate::model::Reaction::count)
}

pub fn voted_by(message: &Message, user: &str) -> bool {
    message
        .reactions
        .iter()
        .any(|reaction| reaction.emoji == VOTE_EMOJI && reaction.reacted_by(user))
}

/// A status in the reader's words.
pub fn status_label(status: &ReportStatus) -> String {
    use crate::i18n::{t, t_args};
    match status {
        ReportStatus::Open => t("chat-report-status-open"),
        ReportStatus::Planned => t("chat-report-status-planned"),
        ReportStatus::InProgress => t("chat-report-status-in-progress"),
        ReportStatus::Fixed { generation } => t_args(
            "chat-report-status-fixed",
            &[("generation", ui::i18n::ArgValue::Number(*generation as f64))],
        ),
        ReportStatus::WontFix => t("chat-report-status-wont-fix"),
        ReportStatus::Duplicate { .. } => t("chat-report-status-duplicate"),
    }
}

/// `count` of something, through a plural-aware catalogue entry.
pub fn counted(key: &str, count: usize) -> String {
    crate::i18n::t_args(key, &[("count", ui::i18n::ArgValue::Number(count as f64))])
}

/// A short label for an app id (`so.tilo.mail` → `Mail`), for a card that
/// names the apps a recording covers.
pub fn app_label(app_id: &str) -> String {
    let last = app_id.rsplit('.').next().unwrap_or(app_id);
    let words: Vec<String> = last
        .split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(ui::format::capitalize_first)
        .collect();
    if words.is_empty() {
        app_id.to_string()
    } else {
        words.join(" ")
    }
}

/// `m:ss`, or `h:mm:ss` past the hour.
pub fn duration_label(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The feedback surface's local state on this peer. None of it is shared.
#[derive(Debug, Default)]
pub struct FeedbackUi {
    /// How each feedback channel is listed here.
    pub views: std::collections::HashMap<String, ReportView>,
    /// Controls clicked on screen that the host must run through the action
    /// door — the workspace requests (open a reference, replay, message,
    /// invite) and this peer's own record sharing.
    pub controls: std::collections::VecDeque<tilo_actions::ControlInvocation>,
    /// The last thing that could not be done, in words for the reader.
    pub notice: Option<String>,
    /// Records this session has already asked the room to reference (`true`)
    /// or to drop (`false`), by message id, so each is asked once.
    pub shared: std::collections::HashSet<(String, bool)>,
    /// When to ask again for shares the workspace could not take yet.
    pub reshare_at: Option<std::time::Instant>,
    /// Comment operations invoked through the resource surface, performed on
    /// the next sync.
    pub pending: std::collections::VecDeque<crate::actions::ChatAction>,
    /// A report waiting for the Editor to pick the report it duplicates.
    pub duplicate_pick: Option<String>,
    /// Where this frame's report strip left room for the fixed-generation
    /// field; the row paints the field there once the strip is done.
    pub fixed_field_at: std::cell::Cell<Option<vello::kurbo::Point>>,
}

impl FeedbackUi {
    pub fn view(&self, channel: &str) -> ReportView {
        self.views.get(channel).copied().unwrap_or_default()
    }

    /// Queue one of Chat's own resource actions, as a click on it would.
    pub fn invoke(&mut self, resource: String, action: &str) {
        self.controls.push_back(tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new(resource, action),
            arguments: serde_json::Map::new(),
        });
    }
}

/// Indices of the reports `view` shows, in the order it shows them.
/// Replies are never reports and are skipped.
pub fn arrange(messages: &[Message], view: ReportView) -> Vec<usize> {
    let mut out: Vec<(usize, &Message)> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.is_top_level())
        .filter(|(_, message)| view.filter.admits(&status_of(message)))
        .collect();
    out.sort_by(|(_, ma), (_, mb)| {
        let newest = mb.ts.cmp(&ma.ts).then_with(|| mb.id.cmp(&ma.id));
        match view.sort {
            ReportSort::Votes => vote_count(mb).cmp(&vote_count(ma)).then(newest),
            ReportSort::New => newest,
        }
    });
    out.into_iter().map(|(index, _)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Reaction;

    fn report(id: &str, ts: i64, votes: &[&str], status: Option<ReportStatus>) -> Message {
        Message {
            id: id.into(),
            channel: "c_feedback".into(),
            author: "u_a".into(),
            ts,
            text: id.into(),
            body: Vec::new(),
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![
                Reaction {
                    emoji: VOTE_EMOJI.into(),
                    users: votes.iter().map(|u| (*u).to_string()).collect(),
                },
                // Another emoji is an ordinary reaction and never a vote.
                Reaction {
                    emoji: "🎉".into(),
                    users: vec!["u_x".into(), "u_y".into(), "u_z".into()],
                },
            ],
            mentions: vec![],
            reference: None,
            also: Vec::new(),
            attachment: None,
            attachment_withdrawn: false,
            pinned: false,
            work: None,
            report: status.map(|status| ReportState {
                status,
                by: "u_team".into(),
                since: 0,
            }),
            approved: None,
        }
    }

    #[test]
    fn only_the_canonical_reaction_counts_as_a_vote() {
        let m = report("r", 0, &["u_1", "u_2"], None);
        assert_eq!(vote_count(&m), 2);
        assert!(voted_by(&m, "u_1"));
        assert!(!voted_by(&m, "u_x"), "a party popper is not a vote");
    }

    #[test]
    fn reports_sort_by_votes_or_recency_and_filter_by_status() {
        let mut reply = report("reply", 50, &["u_1", "u_2", "u_3", "u_4"], None);
        reply.parent = Some("a".into());
        let messages = vec![
            report("a", 10, &["u_1"], None),
            report("b", 20, &["u_1", "u_2", "u_3"], Some(ReportStatus::Planned)),
            report(
                "c",
                30,
                &["u_1", "u_2", "u_3", "u_4", "u_5"],
                Some(ReportStatus::Fixed { generation: 9 }),
            ),
            report("d", 40, &["u_1"], None),
            reply,
        ];
        let ids = |view| {
            arrange(&messages, view)
                .into_iter()
                .map(|i| messages[i].id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(ReportView::default()), ["b", "d", "a"]);
        assert_eq!(
            ids(ReportView {
                sort: ReportSort::New,
                filter: ReportFilter::All
            }),
            ["d", "c", "b", "a"]
        );
        assert_eq!(
            ids(ReportView {
                sort: ReportSort::Votes,
                filter: ReportFilter::Closed
            }),
            ["c"]
        );
    }

    #[test]
    fn a_fix_reaches_builds_at_or_past_its_generation() {
        let fixed = ReportStatus::Fixed { generation: 12 };
        assert!(!fixed.fixed_for(11));
        assert!(fixed.fixed_for(12));
        assert!(!ReportStatus::Planned.fixed_for(99));
    }

    #[test]
    fn a_status_serialises_with_a_stable_tag() {
        let json = serde_json::to_string(&ReportStatus::Duplicate { of: "m_1".into() }).unwrap();
        assert_eq!(json, r#"{"status":"duplicate","of":"m_1"}"#);
    }
}
