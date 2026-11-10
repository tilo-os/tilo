//! What a person is told when the host refuses one of their changes.
//!
//! [`crate::transport::Envelope::CommitRefused`] carries the host's reason
//! as text meant for logs: the receipt check's wording, the member budget's,
//! the workspace's. The author's device keeps that text for its log and shows
//! the person [`plain_reason`] of it instead.

use std::time::Duration;

/// Why a host turns away everyone in a workspace its owner deleted, live or
/// arriving later ([`crate::transport::Envelope::NotAuthorised`]): the
/// workspace is gone for good, so a device forgets it rather than tries
/// again.
pub const WORKSPACE_DELETED: &str = "workspace deleted";

/// The member change budget's refusal ([`crate::config::MemberLimitsConfig`]).
pub const TOO_MANY_CHANGES: &str = "too many changes; slow down";
/// The token that leads the reason of an action a relay is too busy to take
/// ([`crate::transport::Envelope::RemoteActionUndeliverable`]): nothing ran,
/// and the same call works once the wait it says is over.
pub const BACKPRESSURE: &str = "backpressure";
/// The wait a busy refusal stands for when it says none (a relay from before
/// they said one).
pub const BUSY_WAIT_UNSAID: Duration = Duration::from_secs(1);
const RETRY_IN: &str = "; retry in ";

/// What a relay says after [`BACKPRESSURE`] when it refuses an action because
/// the change budget is spent: why, and when the budget admits the next
/// change.
#[must_use]
pub fn too_many_changes(wait: Duration) -> String {
    let ms = wait.as_millis().max(1);
    format!("{TOO_MANY_CHANGES}{RETRY_IN}{ms} ms")
}

/// How long to wait before asking again when `message` carries a relay's busy
/// refusal of an action anywhere in it (an error's text wraps the reason): the
/// wait it says, else [`BUSY_WAIT_UNSAID`]. `None` for anything else, which
/// says nothing about whether the call ran.
#[must_use]
pub fn busy_wait(message: &str) -> Option<Duration> {
    let (_, refused) = message.split_once(&format!("{BACKPRESSURE}: "))?;
    let said = refused
        .split_once(RETRY_IN)
        .and_then(|(_, rest)| rest.split_once(" ms"))
        .and_then(|(ms, _)| ms.trim().parse::<u64>().ok())
        .map(Duration::from_millis);
    Some(said.unwrap_or(BUSY_WAIT_UNSAID))
}
/// The member content budget's refusal.
pub const TOO_MUCH_CONTENT: &str = "too much content this hour";
/// Sending too fast: the change budget refused it.
pub const PLAIN_TOO_FAST: &str = "You're sending too fast; slow down a moment.";
/// The hourly content budget refused it.
pub const PLAIN_TOO_MUCH: &str = "You've shared too much this hour; try again later.";
/// A person's own workspace keeps as much as the host keeps for them.
pub const PLAIN_FULL: &str = "Your workspace is full, so this change wasn't kept.";
/// What a full workspace's refusal (the host account's typed refusal, as
/// JSON) carries.
const FULL: &str = "\"storage_full\"";
/// The workspace's grants or its app's surface do not allow the change.
pub const PLAIN_NOT_ALLOWED: &str = "This workspace doesn't allow that.";
/// The person is no longer a member, or this device no longer counts as one.
pub const PLAIN_NOT_A_MEMBER: &str = "You're no longer a member of this workspace.";
/// The session was not ready for the change; sending it again may work.
pub const PLAIN_NOT_READY: &str = "The workspace wasn't ready for this change; try again.";
/// Anything else.
pub const PLAIN_REFUSED: &str = "The workspace didn't accept this change.";

// Wordings of the receipt check (`collab_types::ReceiptRefusal`), the
// workspace's shared-desktop rules and its access decisions. Each needle
// matches both the wording hosts wrote when they called a workspace a room
// and the wording they write now, so a device classifies a refusal from a
// host that has not been updated the same way.
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
const NOT_A_MEMBER: &[&str] = &[
    "membership changed",
    "not in workspace membership",
    "removed from signed workspace authority",
    "was revoked",
    "certificate expired",
    "withdrawn",
    "delegation expired",
    "not delegated this room",
    "not delegated this workspace",
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
    if raw.contains(FULL) {
        return PLAIN_FULL;
    }
    let has = |needles: &[&str]| needles.iter().any(|needle| raw.contains(needle));
    if has(NOT_A_MEMBER) {
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
    fn a_busy_refusal_says_its_wait_through_any_wrapping() {
        let reason = format!(
            "{BACKPRESSURE}: {}",
            too_many_changes(Duration::from_millis(480))
        );
        assert_eq!(busy_wait(&reason), Some(Duration::from_millis(480)));
        assert_eq!(
            busy_wait(&format!("remote actions are unavailable: {reason}")),
            Some(Duration::from_millis(480))
        );
        // A relay that says no wait: a second.
        assert_eq!(
            busy_wait("remote actions are unavailable: backpressure: too many changes; slow down"),
            Some(BUSY_WAIT_UNSAID)
        );
        assert_eq!(busy_wait(TOO_MANY_CHANGES), None);
        assert_eq!(busy_wait("offline: the device left"), None);
    }

    #[test]
    fn budgets_read_as_what_to_do() {
        assert_eq!(plain_reason(TOO_MANY_CHANGES), PLAIN_TOO_FAST);
        assert_eq!(plain_reason(TOO_MUCH_CONTENT), PLAIN_TOO_MUCH);
        assert_eq!(
            plain_reason(
                r#"{"reason":{"storage_full":{"limit_bytes":1,"used_bytes":2}},"message":"m","next":[]}"#
            ),
            PLAIN_FULL
        );
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

#[cfg(test)]
mod older_wording_tests {
    use super::*;

    // A host deployed before a workspace was called one everywhere still
    // sends reasons that name the workspace. The device reading them classifies
    // them exactly as it classifies the wording it writes now.
    #[test]
    fn a_reason_written_before_the_wording_changed_reads_the_same() {
        for (old, new, plain) in [
            (
                "the author is not delegated this room",
                "the author is not delegated this workspace",
                PLAIN_NOT_A_MEMBER,
            ),
            ("something new", "something new", PLAIN_REFUSED),
        ] {
            assert_eq!(plain_reason(old), plain, "{old}");
            assert_eq!(plain_reason(old), plain_reason(new), "{old} vs {new}");
        }
    }
}
