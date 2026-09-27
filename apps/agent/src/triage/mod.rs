//! The feedback triage agent: an ordinary agent member of the Tilo Community
//! room and of the team's direct rooms with reporters
//! (`docs/architecture/feedback.md`, "Triage").
//!
//! It is not a backend. It runs Chat as its own participant
//! ([`participant::Participant`]), admitted to a room like any other agent:
//! it shows with the `AGENT` label, holds only the grants the room gave it,
//! and removing it revokes it. Everything it does is one of Chat's own
//! published actions, which the broker checks like anyone's.
//!
//! What it does with a report, oldest first, one thing at a time:
//!
//! 1. **Groups duplicates.** A new report that matches an earlier one —
//!    by the address it was written about, its app, its recording's summary
//!    and its words ([`matching`]), confirmed by a model when there is one
//!    ([`think`]) — gets a reply in its thread that links the earlier report
//!    and suggests "duplicate of". The agent never sets the status: that is
//!    an Editor's.
//! 2. **Drafts a forge issue.** Any other report gets a reply carrying a
//!    drafted issue ([`draft`]): a title, a body made only of what the
//!    report showed the room, the report's address and its recording's
//!    facts.
//! 3. **Files it once approved.** When an Editor approves the draft (Chat's
//!    `approve-issue`, which only an Editor can take), the agent files
//!    exactly that draft through account custody ([`forge`]) and replies
//!    with where it was filed.
//!
//! Every reply the agent writes carries an address, and those addresses are
//! how it knows, after a restart or next to another copy of itself, what it
//! already did: a reply linking another report is a duplicate suggestion, a
//! reply carrying an issue is a draft, a reply about a `forge.issue` is a
//! filing. Nothing is remembered in the process that the room does not show.

pub mod draft;
pub mod forge;
pub mod lease;
pub mod matching;
pub mod participant;
pub mod think;

use std::collections::HashSet;

use agent_provider_config::tasks::PrivacyClass;
use chat::feedback::IssueDraft;
use chat::model::{Attachment, Message};
use tilo_reference::ResourceRef;

/// The resource kind of a Chat message, which a duplicate suggestion links.
pub const MESSAGE_KIND: &str = "chat.message";

/// The resource kind of a filed forge issue, which a filing reply links.
pub const ISSUE_KIND: &str = "forge.issue";

/// Where the agent triages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// The public `#feedback` channel of the Tilo Community room. Every
    /// top-level message there is a report.
    Community,
    /// A private direct room between a reporter and the team. A report there
    /// is a remark: a top-level message about something.
    Direct,
}

/// One channel the agent triages, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub place: Place,
    pub channel: String,
}

impl Scope {
    /// The community room's `#feedback`.
    #[must_use]
    pub fn community() -> Self {
        Self {
            place: Place::Community,
            channel: tilo_reference::FEEDBACK_CHANNEL.into(),
        }
    }

    /// A direct room's conversation.
    #[must_use]
    pub fn direct() -> Self {
        Self {
            place: Place::Direct,
            channel: tilo_reference::ROOM_CONVERSATION.into(),
        }
    }

    /// How private what the agent reads here is: public reports are open
    /// content, a direct room's are sensitive.
    #[must_use]
    pub fn privacy(&self) -> PrivacyClass {
        match self.place {
            Place::Community => PrivacyClass::Open,
            Place::Direct => PrivacyClass::Sensitive,
        }
    }

    fn is_report(&self, message: &Message, me: &str) -> bool {
        message.is_top_level()
            && message.author != me
            && message.channel == self.channel
            && match self.place {
                Place::Community => true,
                Place::Direct => message.reference.is_some(),
            }
            && chat::feedback::status_of(message).is_active()
    }
}

/// One thing the agent will do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Move {
    /// Reply in `report`'s thread linking the earlier report `of`.
    SuggestDuplicate {
        report: String,
        of: ResourceRef,
        text: String,
    },
    /// Reply in `report`'s thread with a drafted issue.
    Draft {
        report: String,
        about: ResourceRef,
        draft: IssueDraft,
        text: String,
    },
    /// File the draft on message `draft_message`, which an Editor approved.
    File {
        report: String,
        draft_message: String,
        draft: IssueDraft,
    },
}

impl Move {
    /// The key a runner holds a move under while it is in flight, so it is
    /// not asked for twice before the room shows it.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Move::SuggestDuplicate { report, .. } | Move::Draft { report, .. } => {
                format!("report:{report}")
            }
            Move::File { draft_message, .. } => format!("file:{draft_message}"),
        }
    }
}

/// What the agent has already said in `report`'s thread.
#[derive(Default)]
struct Said {
    suggested: bool,
    drafted: bool,
}

fn said(messages: &[Message], me: &str, report: &str) -> Said {
    let mut said = Said::default();
    for reply in messages
        .iter()
        .filter(|m| m.author == me && m.parent.as_deref() == Some(report))
    {
        if reply
            .attachment
            .as_ref()
            .is_some_and(|attachment| attachment.issue().is_some())
        {
            said.drafted = true;
        } else if reply.reference.as_ref().is_some_and(|reference| {
            reference.kind == MESSAGE_KIND && reference.id != message_address(report)
        }) {
            said.suggested = true;
        }
    }
    said
}

/// Whether the agent already replied under `report` with a filing.
pub(crate) fn filed(messages: &[Message], me: &str, report: &str) -> bool {
    messages.iter().any(|m| {
        m.author == me
            && m.parent.as_deref() == Some(report)
            && m.reference
                .as_ref()
                .is_some_and(|reference| reference.kind == ISSUE_KIND)
    })
}

/// A Chat message's resource address.
#[must_use]
pub fn message_address(id: &str) -> String {
    format!("{MESSAGE_KIND}:{id}")
}

/// A short, redacted label for a report, for the chip that links it.
fn label_of(report: &Message) -> String {
    let text = draft::redacted_text(report);
    let first = text.lines().next().unwrap_or_default().trim();
    let mut label: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        label.push('…');
    }
    label
}

/// The triage decision: given the room's messages, what to do next.
pub struct Triage {
    /// The agent's participant id in Chat.
    pub me: String,
    /// The room, for the address an issue links its report by.
    pub room: String,
    pub scope: Scope,
}

impl Triage {
    /// The next move, or `None` when everything in the channel is handled.
    /// `busy` holds the keys of moves already in flight ([`Move::key`]).
    pub fn next(
        &self,
        messages: &[Message],
        think: &dyn think::Think,
        busy: &HashSet<String>,
    ) -> Option<Move> {
        let messages: Vec<Message> = messages
            .iter()
            .filter(|m| m.channel == self.scope.channel)
            .cloned()
            .collect();
        // Approved drafts first: a person is waiting on them.
        for reply in &messages {
            let Some(issue) = reply.attachment.as_ref().and_then(Attachment::issue) else {
                continue;
            };
            let Some(report) = reply.parent.as_deref() else {
                continue;
            };
            if reply.author != self.me
                || reply.approved.is_none()
                || filed(&messages, &self.me, report)
            {
                continue;
            }
            let candidate = Move::File {
                report: report.to_string(),
                draft_message: reply.id.clone(),
                draft: issue.clone(),
            };
            if !busy.contains(&candidate.key()) {
                return Some(candidate);
            }
        }
        let mut reports: Vec<&Message> = messages
            .iter()
            .filter(|m| self.scope.is_report(m, &self.me))
            .collect();
        reports.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        let earlier: Vec<Message> = messages
            .iter()
            .filter(|m| m.is_top_level() && m.author != self.me)
            .cloned()
            .collect();
        for report in reports {
            if busy.contains(&format!("report:{}", report.id)) {
                continue;
            }
            let said = said(&messages, &self.me, &report.id);
            if said.suggested || said.drafted {
                continue;
            }
            return Some(self.decide(report, &earlier, think));
        }
        None
    }

    fn decide(&self, report: &Message, earlier: &[Message], think: &dyn think::Think) -> Move {
        let candidates = matching::candidates(report, earlier);
        let judged = think.judge(
            self.scope.privacy(),
            &think::Ask {
                report: draft::redacted_text(report),
                candidates: candidates
                    .iter()
                    .map(|(m, _)| think::Candidate {
                        id: m.id.clone(),
                        text: draft::redacted_text(m),
                    })
                    .collect(),
            },
        );
        let original = match &judged {
            // A model that answered decides among the candidates it saw.
            Some(judged) => judged
                .duplicate_of
                .as_deref()
                .and_then(|id| candidates.iter().find(|(m, _)| m.id == id)),
            None => candidates
                .first()
                .filter(|(_, score)| *score >= matching::DUPLICATE_AT),
        };
        if let Some((original, _)) = original {
            return Move::SuggestDuplicate {
                report: report.id.clone(),
                of: ResourceRef::whole(
                    MESSAGE_KIND,
                    message_address(&original.id),
                    label_of(original),
                ),
                text: "This looks like the same problem as an earlier report. An Editor can \
                       mark it as a duplicate of that one."
                    .into(),
            };
        }
        let judged = judged.unwrap_or_default();
        let draft = draft::draft(
            &self.room,
            report,
            judged.title.as_deref(),
            judged.summary.as_deref(),
        );
        Move::Draft {
            report: report.id.clone(),
            about: ResourceRef::whole(MESSAGE_KIND, message_address(&report.id), label_of(report)),
            text: "I drafted a forge issue for this report. It is filed once an Editor \
                   approves it."
                .into(),
            draft,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::think::{Ask, Judgement, NoModel, Think};

    const ME: &str = "agent:triage";

    fn message(id: &str, author: &str, ts: i64, text: &str) -> Message {
        Message {
            id: id.into(),
            channel: tilo_reference::FEEDBACK_CHANNEL.into(),
            author: author.into(),
            ts,
            text: text.into(),
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![],
            mentions: vec![],
            reference: Some(ResourceRef::whole("mail.field", "mail.field:to", "To")),
            also: Vec::new(),
            attachment: None,
            attachment_withdrawn: false,
            pinned: false,
            work: None,
            report: None,
            approved: None,
        }
    }

    fn triage() -> Triage {
        Triage {
            me: ME.into(),
            room: "room-1".into(),
            scope: Scope::community(),
        }
    }

    fn reply_to(parent: &str, id: &str, mv: &Move) -> Message {
        let mut reply = message(id, ME, 100, "");
        reply.parent = Some(parent.into());
        match mv {
            Move::SuggestDuplicate { of, .. } => reply.reference = Some(of.clone()),
            Move::Draft { about, draft, .. } => {
                reply.reference = Some(about.clone());
                reply.attachment = Some(Attachment::Issue(Box::new(draft.clone())));
            }
            Move::File { .. } => {
                reply.reference = Some(ResourceRef::whole(ISSUE_KIND, "https://f/1", "#1"));
            }
        }
        reply
    }

    #[test]
    fn a_first_report_is_drafted_a_repeat_is_linked_and_an_approval_is_filed() {
        let t = triage();
        let first = message(
            "m_1",
            "u_a",
            10,
            "Send does nothing when To ends with a comma",
        );
        let mut messages = vec![first];
        let mv = t.next(&messages, &NoModel, &HashSet::new()).unwrap();
        assert!(matches!(&mv, Move::Draft { report, .. } if report == "m_1"));
        messages.push(reply_to("m_1", "m_r1", &mv));
        assert_eq!(t.next(&messages, &NoModel, &HashSet::new()), None);

        messages.push(message(
            "m_2",
            "u_b",
            20,
            "Pressing send does nothing if To has a trailing comma",
        ));
        let mv = t.next(&messages, &NoModel, &HashSet::new()).unwrap();
        match &mv {
            Move::SuggestDuplicate { report, of, .. } => {
                assert_eq!(report, "m_2");
                assert_eq!(of.id, "chat.message:m_1");
            }
            other => panic!("expected a duplicate suggestion, got {other:?}"),
        }
        messages.push(reply_to("m_2", "m_r2", &mv));
        assert_eq!(t.next(&messages, &NoModel, &HashSet::new()), None);

        // Only an approval files, and only once.
        messages[1].approved = Some(chat::feedback::IssueApproval {
            by: "u_team".into(),
            since: 200,
        });
        let mv = t.next(&messages, &NoModel, &HashSet::new()).unwrap();
        assert!(matches!(&mv, Move::File { draft_message, .. } if draft_message == "m_r1"));
        let busy: HashSet<String> = [mv.key()].into_iter().collect();
        assert_eq!(t.next(&messages, &NoModel, &busy), None, "in flight");
        messages.push(reply_to("m_1", "m_r3", &mv));
        assert_eq!(t.next(&messages, &NoModel, &HashSet::new()), None);
    }

    struct Says(Option<&'static str>);
    impl Think for Says {
        fn judge(&self, _privacy: PrivacyClass, ask: &Ask) -> Option<Judgement> {
            Some(Judgement {
                duplicate_of: self
                    .0
                    .filter(|id| ask.candidates.iter().any(|c| c.id == *id))
                    .map(str::to_string),
                title: Some("A model's title".into()),
                summary: None,
            })
        }
    }

    #[test]
    fn a_model_decides_among_the_candidates_and_titles_the_draft() {
        let t = triage();
        let first = message(
            "m_1",
            "u_a",
            10,
            "Send does nothing when To ends with a comma",
        );
        let drafted = reply_to(
            "m_1",
            "m_r1",
            &t.next(std::slice::from_ref(&first), &NoModel, &HashSet::new())
                .unwrap(),
        );
        // The same app and alike words, another control: a candidate, not
        // enough to decide without a model.
        let mut second = message("m_2", "u_b", 20, "Send button does nothing after a comma");
        second.reference = Some(ResourceRef::whole(
            "mail.button",
            "mail.button:send",
            "Send",
        ));
        let messages = vec![first, drafted, second];
        let without = t.next(&messages, &NoModel, &HashSet::new());
        assert!(matches!(without, Some(Move::Draft { .. })), "{without:?}");
        match t.next(&messages, &Says(Some("m_1")), &HashSet::new()) {
            Some(Move::SuggestDuplicate { of, .. }) => assert_eq!(of.id, "chat.message:m_1"),
            other => panic!("expected the model's duplicate, got {other:?}"),
        }
        match t.next(&messages, &Says(Some("m_999")), &HashSet::new()) {
            Some(Move::Draft { draft, .. }) => assert_eq!(draft.title, "A model's title"),
            other => panic!("a report the model was not shown is no duplicate: {other:?}"),
        }
    }

    #[test]
    fn a_direct_room_reads_as_sensitive_and_only_remarks_are_reports() {
        let t = Triage {
            me: ME.into(),
            room: "direct".into(),
            scope: Scope::direct(),
        };
        assert_eq!(t.scope.privacy(), PrivacyClass::Sensitive);
        assert_eq!(Scope::community().privacy(), PrivacyClass::Open);
        let mut chatter = message("m_1", "u_a", 10, "thanks!");
        chatter.channel = tilo_reference::ROOM_CONVERSATION.into();
        chatter.reference = None;
        assert_eq!(t.next(&[chatter.clone()], &NoModel, &HashSet::new()), None);
        chatter.reference = Some(ResourceRef::whole("mail.field", "mail.field:to", "To"));
        assert!(t.next(&[chatter], &NoModel, &HashSet::new()).is_some());
    }
}
