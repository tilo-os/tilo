//! What a person is told when the host refuses one of their changes.
//!
//! [`crate::transport::Envelope::CommitRefused`] carries the host's reason
//! as text meant for logs: the receipt check's wording, the member budget's,
//! the workspace's. The author's device keeps that text for its log and shows
//! the person [`plain_reason`] of it instead.

use std::borrow::Cow;
use std::time::Duration;

/// Why a host turns away everyone in a workspace its owner deleted, live or
/// arriving later ([`crate::transport::Envelope::NotAuthorised`]): the
/// workspace is gone for good, so a device forgets it rather than tries
/// again.
pub const WORKSPACE_DELETED: &str = "workspace deleted";

/// Why a host refuses a device whose person it keeps nothing of
/// ([`crate::transport::Envelope::NotAuthorised`]): its state was reset, or it
/// pruned them. The person joins again through the host's entry.
pub const NOT_ADMITTED: &str = "not a member here";

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

/// What a host says when the content budget refuses a piece: why, and when
/// the budget holds that piece again.
#[must_use]
pub fn too_much_content(wait: Duration) -> String {
    let ms = wait.as_millis().max(1);
    format!("{TOO_MUCH_CONTENT}{RETRY_IN}{ms} ms")
}

/// The wait a content budget's refusal `raw` says, when it says one.
#[must_use]
pub fn content_wait(raw: &str) -> Option<Duration> {
    raw.trim()
        .strip_prefix(TOO_MUCH_CONTENT)?
        .strip_prefix(RETRY_IN)?
        .strip_suffix(" ms")?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

/// Sending too fast: the change budget refused it.
pub const PLAIN_TOO_FAST: &str = "You're sending too fast; slow down a moment.";
/// The hourly content budget refused it, from a host that did not say when
/// it holds the piece again.
pub const PLAIN_TOO_MUCH: &str = "You've shared too much this hour. Try again in a little while.";

/// The hourly content budget refused it, and holds it again after `wait`:
/// said in whole minutes, rounded up, so the person never tries too early.
#[must_use]
pub fn plain_too_much(wait: Duration) -> String {
    let minutes = wait.as_secs().div_ceil(60).max(1);
    if minutes == 1 {
        "You've shared too much this hour. Try again in a minute.".to_owned()
    } else {
        format!("You've shared too much this hour. Try again in {minutes} minutes.")
    }
}
/// A person's own workspace keeps as much as the host keeps for them, so the
/// change was not kept: make room, then send it again.
pub const PLAIN_FULL: &str = "Your workspace is full, so this change wasn't kept. Remove something to make room, then try again.";
/// What a full workspace's refusal (the host account's typed refusal, as
/// JSON) carries.
const FULL: &str = "\"storage_full\"";
/// The workspace's grants or its app's surface do not allow the change.
pub const PLAIN_NOT_ALLOWED: &str = "This workspace doesn't allow that.";
/// The same, where the reason is a grant the person does not hold: a
/// workspace owner can give it.
pub const PLAIN_NOT_GRANTED: &str =
    "This workspace doesn't allow that. Ask a workspace owner for access.";
/// The person is no longer a member, or this device no longer counts as one.
pub const PLAIN_NOT_A_MEMBER: &str = "You're no longer a member of this workspace.";
/// The session was not ready for the change, and was still not after the
/// device sent it again a few times ([`RETRY_NOT_READY_MS`]).
pub const PLAIN_NOT_READY: &str = "The workspace wasn't ready for that change. Try again.";
/// Anything else.
pub const PLAIN_REFUSED: &str = "The workspace didn't accept this change.";

// Wordings of the receipt check (`collab_types::ReceiptRefusal`), the
// workspace's shared-desktop rules and its access decisions.
/// The wording of a grant the person does not hold: the one refusal a
/// workspace owner can lift for them.
const NOT_GRANTED: &[&str] = &["is not granted"];
const NOT_ALLOWED: &[&str] = &[
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
    "not delegated this workspace",
];
const NOT_READY: &[&str] = &[
    "before subscribing",
    "not bound on this relay",
    "already committed with different content",
];

/// Refusals the host makes of a change it was not ready for yet: the session
/// had not subscribed, or the workspace was not bound here yet. The same
/// change goes through once it is, so a device sends it again before it tells
/// anyone ([`RETRY_NOT_READY_MS`]).
const RETRIED: &[&str] = &["before subscribing", "not bound on this relay"];

/// The waits, in ms, before a device sends again a change its host was not
/// ready for ([`retry_helps`]); past the last, the person is told.
pub const RETRY_NOT_READY_MS: [u64; 5] = [500, 1_000, 2_000, 4_000, 8_000];

/// Whether `raw`, a host's refusal of a change, is one sending the same
/// change again a moment later gets through.
#[must_use]
pub fn retry_helps(raw: &str) -> bool {
    RETRIED.iter().any(|needle| raw.contains(needle))
}

/// The person-readable form of `raw`, a host's refusal of a change.
#[must_use]
pub fn plain_reason(raw: &str) -> Cow<'static, str> {
    let raw = raw.trim();
    if raw == TOO_MANY_CHANGES {
        return PLAIN_TOO_FAST.into();
    }
    if let Some(wait) = content_wait(raw) {
        return plain_too_much(wait).into();
    }
    if raw == TOO_MUCH_CONTENT {
        return PLAIN_TOO_MUCH.into();
    }
    if raw.contains(FULL) {
        return PLAIN_FULL.into();
    }
    let has = |needles: &[&str]| needles.iter().any(|needle| raw.contains(needle));
    if has(NOT_A_MEMBER) {
        PLAIN_NOT_A_MEMBER
    } else if has(NOT_READY) {
        PLAIN_NOT_READY
    } else if has(NOT_GRANTED) {
        PLAIN_NOT_GRANTED
    } else if has(NOT_ALLOWED) {
        PLAIN_NOT_ALLOWED
    } else {
        PLAIN_REFUSED
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::ReceiptRefusal;

    tilo_test_env::scenario! {
        fn refusals_read_as_what_the_reader_should_do;

        phase a_busy_refusal_says_its_wait_through_any_wrapping;
        {
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

        phase only_a_host_that_was_not_ready_is_asked_again;
        {
            assert!(retry_helps("commit sent before subscribing to workspace"));
            assert!(retry_helps("workspace not bound on this relay"));
            assert!(!retry_helps("already committed with different content"));
            assert!(!retry_helps(TOO_MANY_CHANGES));
        }

        phase budgets_read_as_what_to_do;
        {
            assert_eq!(plain_reason(TOO_MANY_CHANGES), PLAIN_TOO_FAST);
            assert_eq!(plain_reason(TOO_MUCH_CONTENT), PLAIN_TOO_MUCH);
            // A host that says its window: the person is told when.
            let said = too_much_content(Duration::from_secs(19 * 60 + 1));
            assert_eq!(content_wait(&said), Some(Duration::from_secs(19 * 60 + 1)));
            assert_eq!(
                plain_reason(&said),
                "You've shared too much this hour. Try again in 20 minutes."
            );
            assert_eq!(
                plain_reason(&too_much_content(Duration::from_millis(1))),
                "You've shared too much this hour. Try again in a minute."
            );
            assert_eq!(
                plain_reason(
                    r#"{"reason":{"storage_full":{"limit_bytes":1,"used_bytes":2}},"message":"m","next":[]}"#
                ),
                PLAIN_FULL
            );
        }

        phase receipt_refusals_read_as_not_allowed;
        {
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

        phase a_verb_not_granted_reads_as_access_to_ask_for;
        {
            // `ReceiptRefusal::NotGranted`'s wording: a workspace owner can
            // give the grant, so the person is told to ask one.
            assert_eq!(
                plain_reason("the author is not granted Undoable on /workspaces/r/apps/a/post"),
                PLAIN_NOT_GRANTED
            );
        }

        phase access_and_session_refusals;
        {
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
}

#[cfg(test)]
mod older_wording_tests {
    use super::*;

    #[test]
    fn a_reason_the_needles_do_not_name_is_plainly_refused() {
        assert_eq!(plain_reason("something new"), PLAIN_REFUSED);
    }
}
