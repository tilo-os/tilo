//! The one door: every place that invokes an action asks here first.
//!
//! A door hands [`Caller::admit`] the person the call acts for, every
//! delegation bounding them and whether the caller's own gesture can answer
//! for them, and gets one of three outcomes, never a boolean it interprets
//! itself: the invocation is
//! [admitted](Admission::Admitted) (with the `Once` grants running it
//! spends), it [asks](Admission::Ask) because the person has to be, or it is
//! [refused](Admission::Refused). Nothing a request carries can stand in for
//! the answer to an ask: only the host on the person's device witnesses one,
//! and it hands the invocation on as admitted.
//!
//! Discovery is the same function ([`admit_catalogue`]): a catalogue lists
//! exactly what its caller may invoke, with the actions that ask first marked
//! as asking, so nobody learns they cannot invoke something only by trying.

use crate::{ActionKind, Answer, Bound, Context, GrantId, RoomPolicy, Standing, Subject, TreePath};

/// Who is calling a door: the person the call acts for, the delegations
/// bounding them, and whether the caller is that person's own gesture.
///
/// A person acting in their own person, from a device of theirs, may answer
/// their own ask by acting: their gesture is the answer. Anyone else — an
/// agent, a bot, an app, a Shortcut — and a person working from a device that
/// is not theirs cannot, and their ask is put to the person by their host.
#[derive(Clone, Debug)]
pub struct Caller<'a> {
    /// The person whose authority the call works with.
    pub person: &'a str,
    /// The delegations bounding that authority, each with the grants it
    /// carries itself.
    pub bounds: Vec<Bound>,
    /// Whether the call is the person's own gesture on their own device.
    pub own_gesture: bool,
}

impl<'a> Caller<'a> {
    /// A person acting as themselves, from a device of theirs.
    #[must_use]
    pub fn person(person: &'a str) -> Self {
        Self {
            person,
            bounds: Vec::new(),
            own_gesture: true,
        }
    }

    /// A call acting through a delegation: an agent, a task, a Shortcut, an
    /// app, or any other program, whatever lock it holds.
    #[must_use]
    pub fn bounded_by(mut self, bound: Bound) -> Self {
        self.bounds.push(bound);
        self.own_gesture = false;
        self
    }

    /// The person working from a device that is not theirs: their ask is put
    /// to them by their host like anyone else's.
    #[must_use]
    pub fn on_foreign_device(mut self) -> Self {
        self.bounds.push(Bound {
            subject: Subject::ForeignDevice,
            grants: crate::roles::foreign_device_bound(),
        });
        self.own_gesture = false;
        self
    }

    /// The asked caller, held to `policy`.
    #[must_use]
    pub fn in_room(&self, policy: &'a RoomPolicy) -> Asked<'a> {
        Asked {
            standing: Standing {
                person: self.person.to_owned(),
                policy: policy.clone(),
                bounds: self.bounds.clone(),
            },
            policy,
            own_gesture: self.own_gesture,
        }
    }
}

/// A caller held to one room's policy, deciding one action.
///
/// It borrows the policy so the verdicts it returns point into it: build one
/// per door call, not per request process.
#[derive(Clone, Debug)]
pub struct Asked<'a> {
    standing: Standing,
    policy: &'a RoomPolicy,
    own_gesture: bool,
}

impl<'a> Asked<'a> {
    /// The room the caller acts in.
    #[must_use]
    pub fn room(&self) -> &'a str {
        &self.policy.room
    }

    /// The room's policy, as held.
    #[must_use]
    pub const fn policy(&self) -> &'a RoomPolicy {
        self.policy
    }

    /// The caller's standing, as a host hands it to an app.
    #[must_use]
    pub const fn standing(&self) -> &Standing {
        &self.standing
    }

    /// The caller's verdict for `kind` at the room-relative node `node`.
    #[must_use]
    pub fn verdict(
        &self,
        node: &TreePath,
        kind: ActionKind,
        cx: &Context<'_>,
    ) -> crate::Verdict<'_> {
        self.standing.verdict(&self.standing.node(node), kind, cx)
    }

    /// What the door answers for `kind` at `node`. `privileged`: the action
    /// acts on the room or on others in it, so it needs the person's own
    /// grant and their own gesture to answer an ask.
    #[must_use]
    pub fn admit(
        &self,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        cx: &Context<'_>,
    ) -> Admission<'_> {
        let mut verdict = self.verdict(node, kind, cx);
        if privileged {
            verdict = verdict.privileged();
        }
        match verdict.answer {
            Answer::Deny => Admission::Refused(verdict),
            Answer::Ask if !self.own_gesture => Admission::Ask(verdict),
            Answer::Allow | Answer::Ask => Admission::Admitted(verdict),
        }
    }
}

/// What a door answered.
#[derive(Clone, Debug)]
pub enum Admission<'v> {
    /// Run it: the verdict it relied on, whose `Once` grants it spends.
    Admitted(crate::Verdict<'v>),
    /// Pause it until the person answers on their own device.
    Ask(crate::Verdict<'v>),
    /// Refuse it.
    Refused(crate::Verdict<'v>),
}

impl<'v> Admission<'v> {
    /// Whether the invocation may run without anyone being asked.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted(_))
    }

    /// Whether the invocation waits for the person's answer.
    #[must_use]
    pub fn is_ask(&self) -> bool {
        matches!(self, Self::Ask(_))
    }

    /// The verdict behind the answer, whichever it is.
    #[must_use]
    pub fn verdict(&self) -> &crate::Verdict<'v> {
        match self {
            Self::Admitted(verdict) | Self::Ask(verdict) | Self::Refused(verdict) => verdict,
        }
    }

    /// The `Once` granting this run spends: none, unless the door runs it.
    #[must_use]
    pub fn spent(&self) -> Vec<GrantId> {
        match self {
            Self::Admitted(verdict) => verdict.spent(),
            Self::Ask(_) | Self::Refused(_) => Vec::new(),
        }
    }

    /// The answer discovery marks this action with: what invoking it will
    /// answer, an ask included.
    #[must_use]
    pub const fn answer(&self) -> Answer {
        match self {
            Self::Admitted(verdict) => verdict.answer,
            Self::Ask(_) => Answer::Ask,
            Self::Refused(_) => Answer::Deny,
        }
    }

    /// Why, in a sentence people and agents can read.
    #[must_use]
    pub fn explain(&self) -> String {
        self.verdict().explain()
    }
}

/// One action a catalogue offers its caller: where it sits in the room and
/// the kind of what it does.
#[derive(Clone, Copy, Debug)]
pub struct CatalogueEntry<'n> {
    pub node: &'n TreePath,
    pub kind: ActionKind,
}

/// The [`Answer`] a catalogue marks each `entry` with: exactly what this
/// caller's door answers for it.
#[must_use]
pub fn admit_catalogue<'n>(
    asked: &Asked<'_>,
    entries: impl IntoIterator<Item = CatalogueEntry<'n>>,
    cx: &Context<'_>,
) -> Vec<Answer> {
    entries
        .into_iter()
        .map(|entry| asked.admit(entry.node, entry.kind, false, cx).answer())
        .collect()
}
