//! A turn: the person's message an agent acts for.
//!
//! An agent holds no authority of its own. It acts inside a turn, and a turn
//! is caused by a person's message in a conversation the agent takes part in.
//! Its authority is that person's grants, capped at
//! [`roles::agent_policy::ROLE_CAP`], narrowed by the bound the agent's record
//! keeps ([`roles::agent_turn`]).
//!
//! Every one of those four facts is checked here, in one place, so every door
//! and every receiver asks the same question:
//!
//! 1. the turn names a cause, and the cause was authored by the person;
//! 2. the agent takes part in the cause's conversation at the op's heads;
//! 3. the turn has not ended ([`Looked::ended`]: its work is done or stopped);
//! 4. the action's kind is within the person's grants at its heads ∩
//!    [`roles::agent_policy::ROLE_CAP`] ∩ the agent's recorded bound — which
//!    [`roles::agent_turn`] turns into grants the door decides with.
//!
//! A cause that has not reached the receiver yet is not a refusal: the op waits
//! for it ([`Held::waits`]), the same way an op from a late joiner does.

use crate::{Subject, TreePath, roles};

/// The person's message an agent acts for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    /// The conversation the message was posted in, by its address in the
    /// workspace.
    pub conversation: String,
    /// The message, by its id in that conversation.
    pub message: String,
}

impl Turn {
    /// The turn `person`'s message `message` in `conversation` makes.
    #[must_use]
    pub fn new(conversation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            conversation: conversation.into(),
            message: message.into(),
        }
    }
}

/// What a receiver looked up about a turn's cause, as it holds it now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Looked {
    /// Who authored the message.
    pub author: String,
    /// Whether the agent takes part in the conversation.
    pub participant: bool,
    /// Whether the turn ended: its work is done, or a person stopped it.
    pub ended: bool,
    /// What the agent's record bounds it to; empty follows the record's
    /// default.
    pub bound: Vec<crate::Rule>,
}

impl Looked {
    /// The cause as the person `person`'s message, taken part in, still going.
    #[must_use]
    pub fn by(person: impl Into<String>) -> Self {
        Self {
            author: person.into(),
            participant: true,
            ended: false,
            bound: Vec::new(),
        }
    }

    /// The same cause, narrowed to `bound`.
    #[must_use]
    pub fn bounded_by(mut self, bound: Vec<crate::Rule>) -> Self {
        self.bound = bound;
        self
    }

    /// The same cause, ended.
    #[must_use]
    pub fn ended(mut self) -> Self {
        self.ended = true;
        self
    }

    /// The same cause, in a conversation the agent does not take part in.
    #[must_use]
    pub fn apart(mut self) -> Self {
        self.participant = false;
        self
    }
}

/// Why an agent's op is not admitted on its turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    /// The op names no turn.
    NoCause,
    /// The turn's cause has not reached this receiver yet: hold the op and
    /// decide it again once it lands.
    Waiting,
    /// The message the op names was written by someone else: an agent acts
    /// only for the person whose message caused the turn.
    AnotherAuthor,
    /// The agent is not in the cause's conversation.
    NotAParticipant,
    /// The turn ended: its work is done, or a person stopped it.
    Ended,
}

impl Held {
    /// Whether the op is held for its cause rather than refused: its cause has
    /// not arrived yet.
    #[must_use]
    pub fn waits(&self) -> bool {
        matches!(self, Self::Waiting)
    }

    /// Why, in a sentence an agent and a person can read.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NoCause => {
                "an agent acts only inside a turn a person's message caused".to_owned()
            }
            Self::Waiting => "the message this turn answers has not arrived here yet".to_owned(),
            Self::AnotherAuthor => {
                "an agent acts for the person whose message caused its turn, and this one is not theirs"
                    .to_owned()
            }
            Self::NotAParticipant => {
                "the agent does not take part in the conversation this turn answers".to_owned()
            }
            Self::Ended => "the turn ended: its agent takes no further action".to_owned(),
        }
    }
}

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

/// The authority agent `agent` acts with for `person` on `turn` in `workspace`:
/// the person's grants, capped and narrowed by the agent's record, once the
/// turn is admitted.
///
/// `looked` is what the receiver found for the turn's own cause, asked with
/// that turn: nothing found yet holds the op ([`Held::Waiting`]) rather than
/// refusing it.
///
/// # Errors
/// The op has no turn, or its turn does not admit it ([`Held`]).
pub fn authority(
    workspace: &str,
    agent: &str,
    person: &str,
    turn: Option<&Turn>,
    looked: impl FnOnce(&Turn) -> Option<Looked>,
) -> Result<crate::Bound, Held> {
    let Some(turn) = turn else {
        return Err(Held::NoCause);
    };
    let Some(looked) = looked(turn) else {
        return Err(Held::Waiting);
    };
    if !same_person(&looked.author, person) {
        return Err(Held::AnotherAuthor);
    }
    if !looked.participant {
        return Err(Held::NotAParticipant);
    }
    if looked.ended {
        return Err(Held::Ended);
    }
    Ok(roles::agent_turn(workspace, agent, person, &looked.bound))
}

/// Whether two ids name the same person: a member's root, with or without the
/// `person:` prefix grants carry.
fn same_person(a: &str, b: &str) -> bool {
    root_of(a) == root_of(b)
}

fn root_of(id: &str) -> &str {
    id.strip_prefix("person:").unwrap_or(id)
}

/// The node an agent's turn decides an action at.
#[must_use]
pub fn node(workspace: &str, app: &str, kind: &str) -> TreePath {
    TreePath::workspace(workspace).app(app).kind(kind)
}

/// The subject an agent's turn carries in a workspace: the agent, acting for
/// its person.
#[must_use]
pub fn subject(agent: &str) -> Subject {
    Subject::agent(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, Answer, Context, Party};

    const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const ADA: &str = "ada";
    const BOB: &str = "bob";

    /// The turn's authority is the person's grants, capped at Editor and
    /// narrowed by the record: Bob's message gives what Bob may do, Ada's
    /// what Ada may.
    #[test]
    fn a_turn_gives_the_grants_of_the_person_whose_message_caused_it() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        let bob = authority(WORKSPACE, "scribe", BOB, Some(&turn), |_| {
            Some(Looked::by(BOB))
        })
        .unwrap();
        let ada = authority(WORKSPACE, "scribe", ADA, Some(&turn), |_| {
            Some(Looked::by(ADA))
        })
        .unwrap();
        let granted =
            |bound: &crate::Bound| bound.grants.first().map(|grant| grant.granted_by.clone());
        assert_ne!(
            granted(&bob),
            granted(&ada),
            "each turn carries its own person"
        );
        assert_eq!(
            bob.subject,
            Subject::agent("scribe"),
            "the turn acts as the agent"
        );

        // The person's grants reach the turn: Ada may read, Bob was refused
        // reading this app, and the turn follows them.
        let node = node(WORKSPACE, "so.tilo.notes", "document");
        let bob_refused = crate::Grant::new(
            "bob-no-notes",
            Subject::person(BOB),
            node.clone(),
            crate::KindSet::ALL,
            Answer::Deny,
            Subject::person("governor"),
        );
        let policy = {
            let mut policy = crate::WorkspacePolicy::new(WORKSPACE, "governor");
            policy.grants.push(bob_refused);
            policy
        };
        let cx = Context::at_time(tilo_clock::wall());
        let decision = |bound: &crate::Bound| {
            let party = bound.party(&policy.grants);
            let person = policy.party(BOB);
            crate::Authority::person(person)
                .bounded_by(party)
                .decide(&node, ActionKind::Read, &cx)
                .answer
        };
        assert_eq!(
            decision(&bob),
            Answer::Deny,
            "Bob's message gives Bob's grants"
        );
    }

    /// Without a cause, or with a cause the agent does not take part in, an
    /// op is refused; a cause that has not arrived is held, not refused.
    #[test]
    fn only_a_cause_the_agent_takes_part_in_admits_it() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        let looked = Looked::by(BOB);
        assert_eq!(
            authority(WORKSPACE, "scribe", BOB, None, |_| Some(looked.clone())),
            Err(Held::NoCause)
        );
        assert_eq!(
            authority(WORKSPACE, "scribe", BOB, Some(&turn), |_| None),
            Err(Held::Waiting)
        );
        assert!(Held::Waiting.waits(), "a cause on its way is waited for");
        assert_eq!(
            authority(WORKSPACE, "scribe", BOB, Some(&turn), |_| {
                Some(looked.clone().apart())
            }),
            Err(Held::NotAParticipant)
        );
        assert_eq!(
            authority(WORKSPACE, "scribe", ADA, Some(&turn), |_| Some(
                looked.clone()
            )),
            Err(Held::AnotherAuthor)
        );
    }

    /// A turn that ended takes no further action.
    #[test]
    fn a_turn_after_it_is_done_is_refused() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        assert_eq!(
            authority(WORKSPACE, "scribe", BOB, Some(&turn), |_| Some(
                Looked::by(BOB).ended()
            )),
            Err(Held::Ended)
        );
    }

    /// An outside effect always asks: no bound and no person's grant makes it
    /// run unasked.
    #[test]
    fn an_outside_action_asks_first() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        let bound = authority(WORKSPACE, "scribe", BOB, Some(&turn), |_| {
            Some(Looked::by(BOB))
        })
        .unwrap();
        let node = TreePath::workspace(WORKSPACE);
        let cx = Context::at_time(tilo_clock::wall());
        let party: Party<'_> = bound.party(&[]);
        assert_eq!(
            party.decide(&node, ActionKind::Outside, &cx).answer,
            Answer::Ask
        );
        assert_eq!(
            party.decide(&node, ActionKind::Undoable, &cx).answer,
            Answer::Allow
        );
        // The workspace's own settings and moderation are never an agent's.
        assert_eq!(
            party
                .decide(
                    &roles::governance_action(WORKSPACE, "grant"),
                    ActionKind::Undoable,
                    &cx
                )
                .answer,
            Answer::Deny
        );
    }

    /// A bound the record keeps narrows the turn; it never widens it.
    #[test]
    fn the_records_bound_narrows_the_turn() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        let notes = node(WORKSPACE, "so.tilo.notes", "document");
        let looked = Looked::by(BOB).bounded_by(vec![crate::Rule::new(
            notes.clone(),
            crate::KindSet::UNDOABLE,
            Answer::Deny,
        )]);
        let bound = authority(WORKSPACE, "scribe", BOB, Some(&turn), |_| {
            Some(looked.clone())
        })
        .unwrap();
        let cx = Context::at_time(tilo_clock::wall());
        let party = bound.party(&[]);
        assert_eq!(
            party.decide(&notes, ActionKind::Undoable, &cx).answer,
            Answer::Deny
        );
        assert_eq!(
            party
                .decide(
                    &node(WORKSPACE, "so.tilo.tasks", "issue"),
                    ActionKind::Undoable,
                    &cx
                )
                .answer,
            Answer::Allow,
            "what the bound does not name stays as the turn allows"
        );
    }

    #[test]
    fn a_refusal_says_why() {
        for held in [
            Held::NoCause,
            Held::Waiting,
            Held::AnotherAuthor,
            Held::NotAParticipant,
            Held::Ended,
        ] {
            assert!(!held.reason().is_empty(), "{held:?} has a reason");
        }
    }

    #[test]
    fn a_turn_reads_and_writes_as_its_parts() {
        let turn = Turn::new("chat/workspace/channels/1", "m1");
        let text = serde_json::to_string(&turn).unwrap();
        assert_eq!(serde_json::from_str::<Turn>(&text).unwrap(), turn);
    }

    #[test]
    fn the_subject_of_a_turn_is_its_agent() {
        assert_eq!(subject("scribe"), Subject::agent("scribe"));
        assert_eq!(
            super::node(WORKSPACE, "so.tilo.notes", "document"),
            TreePath::workspace(WORKSPACE)
                .app("so.tilo.notes")
                .kind("document")
        );
    }
}
