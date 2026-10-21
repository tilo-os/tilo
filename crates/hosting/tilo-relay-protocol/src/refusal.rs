//! What a person is told when the host refuses one of their changes.
//!
//! [`crate::transport::Envelope::CommitRefused`] carries the host's reason
//! as text meant for logs: the receipt check's wording, the member budget's,
//! the room's. The author's device keeps that text for its log and shows the
//! person [`plain_reason`] of it instead.

/// The member change budget's refusal ([`crate::config::MemberLimitsConfig`]).
pub const TOO_MANY_CHANGES: &str = "too many changes; slow down";
/// The member content budget's refusal.
pub const TOO_MUCH_CONTENT: &str = "too much content this hour";
/// The refusal of a member's windows or layout by a room whose layout is
/// personal, as a public room's is: it shares none, so a page keeps its
/// person's layout to itself once it hears this.
pub const PERSONAL_LAYOUT: &str =
    "the room's layout is personal, as a public room's is: it takes no member's windows or layout";

/// Sending too fast: the change budget refused it.
pub const PLAIN_TOO_FAST: &str = "You're sending too fast; slow down a moment.";
/// The hourly content budget refused it.
pub const PLAIN_TOO_MUCH: &str = "You've shared too much this hour; try again later.";
/// The room's grants or its app's surface do not allow the change.
pub const PLAIN_NOT_ALLOWED: &str = "This room doesn't allow that.";
/// The person only watches the room.
pub const PLAIN_WATCHING: &str = "You can only watch this room.";
/// The person is no longer a member, or this device no longer counts as one.
pub const PLAIN_NOT_A_MEMBER: &str = "You're no longer a member of this room.";
/// The session was not ready for the change; sending it again may work.
pub const PLAIN_NOT_READY: &str = "The room wasn't ready for this change; try again.";
/// Anything else.
pub const PLAIN_REFUSED: &str = "The room didn't accept this change.";

// Wordings of the receipt check (`collab_types::ReceiptRefusal`), the
// room's shared-desktop rules and its access decisions.
const NOT_ALLOWED: &[&str] = &[
    "is not granted",
    "publishes no",
    "may not write the unit",
    "which its claim does not name",
    "the claims do not name",
    "names no action",
    "another app surface",
    "may not show the appearance",
    "may not",
];
const WATCHING: &[&str] = &[
    "only watches",
    "watching this room",
    "from those who only watch",
];
const NOT_A_MEMBER: &[&str] = &[
    "membership changed",
    "not in workspace membership",
    "removed from signed workspace authority",
    "was revoked",
    "certificate expired",
    "withdrawn",
    "delegation expired",
    "not delegated this room",
];
const NOT_READY: &[&str] = &[
    "before subscribing",
    "not bound on this relay",
    "already committed with different content",
];

/// The person-readable form of `raw`, a host's refusal of a change.
#[must_use]
pub fn plain_reason(raw: &str) -> &'static str {
    let raw = raw.trim();
    if raw == TOO_MANY_CHANGES {
        return PLAIN_TOO_FAST;
    }
    if raw == TOO_MUCH_CONTENT {
        return PLAIN_TOO_MUCH;
    }
    let has = |needles: &[&str]| needles.iter().any(|needle| raw.contains(needle));
    if has(WATCHING) {
        PLAIN_WATCHING
    } else if has(NOT_A_MEMBER) {
        PLAIN_NOT_A_MEMBER
    } else if has(NOT_READY) {
        PLAIN_NOT_READY
    } else if has(NOT_ALLOWED) {
        PLAIN_NOT_ALLOWED
    } else {
        PLAIN_REFUSED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::ReceiptRefusal;

    #[test]
    fn budgets_read_as_what_to_do() {
        assert_eq!(plain_reason(TOO_MANY_CHANGES), PLAIN_TOO_FAST);
        assert_eq!(plain_reason(TOO_MUCH_CONTENT), PLAIN_TOO_MUCH);
    }

    #[test]
    fn receipt_refusals_read_as_not_allowed() {
        for refusal in [
            ReceiptRefusal::NotPublished("org.tilo.Chat/chat.workspace/post".into()),
            ReceiptRefusal::OutsideFootprint {
                action: "post".into(),
                unit: "messages".into(),
            },
            ReceiptRefusal::Unclaimed,
            ReceiptRefusal::OtherSurface,
            ReceiptRefusal::Unnamed("change".into()),
            ReceiptRefusal::UnclaimedUnit("comments.c1".into()),
            ReceiptRefusal::Appearance {
                actor: "ada".into(),
                reason: "not a bot".into(),
            },
        ] {
            assert_eq!(
                plain_reason(&refusal.to_string()),
                PLAIN_NOT_ALLOWED,
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_verb_not_granted_reads_as_not_allowed() {
        // `ReceiptRefusal::NotGranted`'s wording.
        assert_eq!(
            plain_reason("the author is not granted Undoable on /rooms/r/apps/a/post"),
            PLAIN_NOT_ALLOWED
        );
    }

    #[test]
    fn access_and_session_refusals() {
        assert_eq!(
            plain_reason("a spectator only watches this room"),
            PLAIN_WATCHING
        );
        assert_eq!(
            plain_reason("workspace membership changed"),
            PLAIN_NOT_A_MEMBER
        );
        assert_eq!(
            plain_reason("device not in workspace membership"),
            PLAIN_NOT_A_MEMBER
        );
        assert_eq!(
            plain_reason("commit sent before subscribing to workspace"),
            PLAIN_NOT_READY
        );
        assert_eq!(
            plain_reason("workspace not bound on this relay"),
            PLAIN_NOT_READY
        );
        assert_eq!(plain_reason("something new"), PLAIN_REFUSED);
    }
}
