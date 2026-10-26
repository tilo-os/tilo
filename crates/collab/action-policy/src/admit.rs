//! The one door: every place that invokes an action asks here first.
//!
//! A door says who is calling ([`Caller`]: the person the call acts for,
//! every delegation bounding them, whether the caller's own gesture can
//! answer for them, and whether anyone can be asked at all), holds it to the
//! room ([`Caller::in_room`]) and gets one of three outcomes from
//! [`Asked::admit`], never a boolean it interprets itself: the invocation is
//! [admitted](DoorAnswer::Admitted) (with the `Once` grants running it
//! spends), it [asks](DoorAnswer::Ask) because the person has to be, or it is
//! [refused](DoorAnswer::Refused). Nothing a request carries can stand in for
//! the answer to an ask: only the host on the person's device witnesses one,
//! and it hands the invocation on as admitted.
//!
//! Discovery is the same function ([`admit_catalogue`]): a catalogue lists
//! exactly what its caller may invoke, with the actions that ask first marked
//! as asking, so nobody learns they cannot invoke something only by trying.
//!
//! What it decides is the grants' answer weighed with how the call is asked
//! ([`crate::admission`]): someone acting for the person is asked before an
//! action its person attends, and an ask nobody can answer refuses.

use crate::admission::{self, Asker, Invoked};
use crate::{ActionKind, Answer, Bound, Context, GrantId, RoomPolicy, Standing, Subject, TreePath};

/// Who is calling a door: the person the call acts for, the delegations
/// bounding them, and whether the caller is that person's own gesture.
///
/// A person acting in their own person, from a device of theirs, may answer
/// their own ask by acting: their gesture is the answer. Anyone else — an
/// agent, a bot, an app, a Shortcut — and a person working from a device that
/// is not theirs cannot, and their ask is put to the person by their host
/// ([`Caller::hosted`]), or held until one can ([`DoorAnswer::Ask`]). A door
/// with no way to reach them says so ([`Caller::unreachable`]) and an ask
/// refuses rather than waiting forever.
#[derive(Clone, Debug)]
pub struct Caller<'a> {
    /// The person whose authority the call works with.
    pub person: &'a str,
    /// The delegations bounding that authority, each with the grants it
    /// carries itself.
    pub bounds: Vec<Bound>,
    /// Whether the call is the person's own gesture on their own device.
    pub own_gesture: bool,
    /// Whether the person's host, on their device, answers their asks here.
    /// Only a host that reaches them has one.
    pub hosted: bool,
    /// Whether anyone can put an ask to the person at all. A door that has
    /// no way to reach them — a relay, a program running where they are not
    /// — refuses an ask instead of holding it.
    pub reachable: bool,
}

impl<'a> Caller<'a> {
    /// A person acting as themselves, from a device of theirs.
    #[must_use]
    pub fn person(person: &'a str) -> Self {
        Self {
            person,
            bounds: Vec::new(),
            own_gesture: true,
            hosted: false,
            reachable: true,
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

    /// Nobody can put an ask to the person from here: an ask refuses.
    #[must_use]
    pub const fn unreachable(mut self) -> Self {
        self.reachable = false;
        self
    }

    /// The person's host answers their asks here: it is the device they are
    /// on, and it puts each ask to them. A door that only relays, with no
    /// way to ask them, leaves this unset and an ask refuses.
    #[must_use]
    pub const fn hosted(mut self) -> Self {
        self.hosted = true;
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
            hosted: self.hosted,
            reachable: self.reachable,
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
    hosted: bool,
    reachable: bool,
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

    /// What the door answers for `kind` at `node`.
    ///
    /// `privileged`: the action acts on the room or on others in it, so it
    /// needs the person's own grant and their own gesture to answer an ask.
    /// `attended`: the action waits for its person however its grants read,
    /// so someone acting for them is asked first.
    #[must_use]
    pub fn admit(
        &self,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        attended: bool,
        cx: &Context<'_>,
    ) -> DoorAnswer<'_> {
        let mut verdict = self.verdict(node, kind, cx);
        if privileged {
            verdict = verdict.privileged();
        }
        let reached = admission::admit(
            Invoked {
                answer: verdict.answer,
                attended,
                record: None,
            },
            Asker {
                reachable: self.reachable,
                delegated: !self.own_gesture,
            },
        );
        match reached {
            Err(_) => DoorAnswer::Refused(verdict),
            // A person on their own device answers by the gesture itself,
            // and a host-witnessed door has their answer in hand; anywhere
            // else the ask waits for them.
            Ok(admission::Admission::AsksFirst) if !self.own_gesture && !self.hosted => {
                DoorAnswer::Ask(verdict)
            }
            Ok(admission::Admission::AsksFirst | admission::Admission::Runs) => {
                DoorAnswer::Admitted(verdict)
            }
        }
    }
}

/// What a door answered.
#[derive(Clone, Debug)]
pub enum DoorAnswer<'v> {
    /// Run it: the verdict it relied on, whose `Once` grants it spends.
    Admitted(crate::Verdict<'v>),
    /// Pause it until the person answers on their own device.
    Ask(crate::Verdict<'v>),
    /// Refuse it.
    Refused(crate::Verdict<'v>),
}

impl<'v> DoorAnswer<'v> {
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
        .map(|entry| {
            asked
                .admit(entry.node, entry.kind, false, false, cx)
                .answer()
        })
        .collect()
}
