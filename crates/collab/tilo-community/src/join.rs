//! The rules that keep a person in the Tilo Community.

/// Everyone is in the community by default. How long a host waits before
/// asking again when a join has not gone through yet (offline, or the host
/// not reached), doubling up to [`AUTO_JOIN_MAX_BACKOFF`].
pub const AUTO_JOIN_FIRST_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
/// The longest wait between two join attempts.
pub const AUTO_JOIN_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(300);

/// What the host knows about the community when it decides what to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// The host has a certified identity to join with.
    pub identity: bool,
    /// The id (hex) of the configured standing invitation, if any.
    pub invite: Option<String>,
    /// The person's remembered choice.
    pub membership: settings_store::community::Membership,
    /// The invitation the community's host refused, as remembered.
    pub refused_invite: Option<String>,
    /// The host holds the community room.
    pub held: bool,
    /// It holds a role in it: the community's host admitted it.
    pub admitted: bool,
}

/// The next thing a host does about the community.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing, for now.
    Wait,
    /// Redeem the standing invitation: the session is not in the room.
    Join,
    /// Ask the room's hosts again: the session holds the room, but the host
    /// has not admitted it yet.
    Redeem,
    /// Open the private room with the Tilo team, once.
    OpenTeamRoom,
    /// Leave the room: the person left, or its host refused this device.
    Leave,
    /// Remember that the host refused the invitation, then leave.
    RecordRefusal,
}

/// Keeps a session in the Tilo Community without asking: it joins once the
/// session has an identity and the network is up, retries with back-off
/// while the host cannot be reached, never re-joins a person who left, and
/// takes a refusal by the host as final for that invitation. Pure: the
/// caller observes, performs the step and reports back.
#[derive(Clone, Debug)]
pub struct AutoJoin {
    invite: Option<String>,
    due: Option<std::time::Instant>,
    backoff: std::time::Duration,
    team_room_opened: bool,
    refused: Option<String>,
}

impl Default for AutoJoin {
    fn default() -> Self {
        Self {
            invite: None,
            due: None,
            backoff: AUTO_JOIN_FIRST_BACKOFF,
            team_room_opened: false,
            refused: None,
        }
    }
}

impl AutoJoin {
    /// Decide the next step. A different invitation (a new release) starts
    /// over.
    pub fn next(&mut self, now: std::time::Instant, observed: &Observed) -> Step {
        use settings_store::community::Membership;
        if self.invite != observed.invite {
            *self = Self {
                invite: observed.invite.clone(),
                ..Self::default()
            };
        }
        let Some(invite) = observed.invite.as_deref() else {
            return Step::Wait;
        };
        if !observed.identity {
            return Step::Wait;
        }
        if observed.membership == Membership::Left {
            return if observed.held {
                Step::Leave
            } else {
                Step::Wait
            };
        }
        if self.refused.as_deref() == Some(invite)
            && observed.refused_invite.as_deref() != Some(invite)
        {
            return Step::RecordRefusal;
        }
        if observed.refused_invite.as_deref() == Some(invite) {
            return if observed.held && !observed.admitted {
                Step::Leave
            } else {
                Step::Wait
            };
        }
        if observed.admitted {
            self.due = None;
            self.backoff = AUTO_JOIN_FIRST_BACKOFF;
            return if self.team_room_opened {
                Step::Wait
            } else {
                Step::OpenTeamRoom
            };
        }
        if self.due.is_some_and(|due| now < due) {
            return Step::Wait;
        }
        if observed.held {
            Step::Redeem
        } else {
            Step::Join
        }
    }

    /// A join or redemption was attempted at `now`: the next one waits,
    /// longer each time until the host admits the session.
    pub fn attempted(&mut self, now: std::time::Instant) {
        self.due = now.checked_add(self.backoff);
        self.backoff = self.backoff.saturating_mul(2).min(AUTO_JOIN_MAX_BACKOFF);
    }

    /// The team room was opened, or cannot be (no team configured, or this
    /// session is the team); either way it is not tried again.
    pub fn team_room_opened(&mut self) {
        self.team_room_opened = true;
    }

    /// The community's host refused the invitation `invite` (hex).
    pub fn refused(&mut self, invite: &str) {
        self.refused = Some(invite.to_string());
    }

    /// The person chose to join again, or to leave: whatever was pending
    /// starts over.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(held: bool, admitted: bool) -> Observed {
        Observed {
            identity: true,
            invite: Some("aa".into()),
            held,
            admitted,
            ..Observed::default()
        }
    }

    #[test]
    fn a_fresh_session_joins_retries_with_back_off_then_opens_the_team_room_once() {
        let start = std::time::Instant::now();
        let mut auto = AutoJoin::default();
        assert_eq!(auto.next(start, &observed(false, false)), Step::Join);
        auto.attempted(start);
        assert_eq!(auto.next(start, &observed(true, false)), Step::Wait);
        let first = start + AUTO_JOIN_FIRST_BACKOFF;
        assert_eq!(auto.next(first, &observed(true, false)), Step::Redeem);
        auto.attempted(first);
        assert_eq!(
            auto.next(first + AUTO_JOIN_FIRST_BACKOFF, &observed(true, false)),
            Step::Wait,
            "the wait doubles while the host is out of reach"
        );
        let mut later = first;
        for _ in 0..20 {
            later += AUTO_JOIN_MAX_BACKOFF;
            assert_eq!(auto.next(later, &observed(true, false)), Step::Redeem);
            auto.attempted(later);
        }
        assert_eq!(
            auto.next(later + AUTO_JOIN_MAX_BACKOFF / 2, &observed(true, false)),
            Step::Wait,
            "and stops growing at the cap"
        );
        assert_eq!(auto.next(later, &observed(true, true)), Step::OpenTeamRoom);
        auto.team_room_opened();
        assert_eq!(auto.next(later, &observed(true, true)), Step::Wait);
    }

    #[test]
    fn nothing_happens_without_an_identity_or_an_invitation() {
        let now = std::time::Instant::now();
        let mut auto = AutoJoin::default();
        let mut no_identity = observed(false, false);
        no_identity.identity = false;
        assert_eq!(auto.next(now, &no_identity), Step::Wait);
        let mut no_invite = observed(false, false);
        no_invite.invite = None;
        assert_eq!(auto.next(now, &no_invite), Step::Wait);
    }

    #[test]
    fn a_person_who_left_is_never_joined_again() {
        let mut auto = AutoJoin::default();
        let mut left = observed(false, false);
        left.membership = settings_store::community::Membership::Left;
        let mut now = std::time::Instant::now();
        for _ in 0..100 {
            assert_eq!(auto.next(now, &left), Step::Wait);
            now += AUTO_JOIN_MAX_BACKOFF;
        }
        left.held = true;
        left.admitted = true;
        assert_eq!(
            auto.next(now, &left),
            Step::Leave,
            "a device that learns the choice leaves the room"
        );
    }

    #[test]
    fn a_refusal_is_final_until_the_invitation_changes() {
        let mut now = std::time::Instant::now();
        let mut auto = AutoJoin::default();
        assert_eq!(auto.next(now, &observed(false, false)), Step::Join);
        auto.attempted(now);
        auto.refused("aa");
        assert_eq!(auto.next(now, &observed(true, false)), Step::RecordRefusal);
        let mut refused = observed(true, false);
        refused.refused_invite = Some("aa".into());
        assert_eq!(auto.next(now, &refused), Step::Leave);
        refused.held = false;
        for _ in 0..100 {
            now += AUTO_JOIN_MAX_BACKOFF;
            assert_eq!(auto.next(now, &refused), Step::Wait, "never asked again");
        }
        refused.invite = Some("bb".into());
        assert_eq!(
            auto.next(now, &refused),
            Step::Join,
            "a different invitation is asked once more"
        );
    }
}
