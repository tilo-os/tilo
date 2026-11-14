//! How the workspace's members are here now: presence, which every frame says
//! again and the workspace's document never holds.
//!
//! The membership itself is [`crate::Member`]: who is in the workspace and the
//! colour each wears there. This is the other stream, keyed by the same
//! [`MemberId`]: where each of them is, what they say they are doing, how
//! available they are and the call they are in.

use collab_types::presence::{Availability, CallPresence};
use serde::{Deserialize, Serialize};
use tilo_action_policy::MemberId;

use crate::ids::TabId;

/// What someone here says they are doing, and who said it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The words as their presence carries them, in their own language.
    pub text: String,
    /// Whether software said this for them, from what they did, rather than
    /// them typing it: software's work is said for it, never by it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub automatic: bool,
}

/// One member's presence in this session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Present {
    pub member: MemberId,
    /// The live publisher the member is here through, hex: the device or the
    /// agent's own key. What a wave, a follow or a call is addressed to.
    pub device: String,
    /// The window of the record they focus, when this session shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<TabId>,
    /// The app of the record they focus, as their presence names it: where
    /// following them goes while no window here shows that record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    pub availability: Availability,
    /// The voice channel they are in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallPresence>,
    /// The live session their device is here through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<u64>,
    /// This session follows their view: the bar keeps that face when it is too
    /// narrow to show everyone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub followed: bool,
}

impl Present {
    /// `member` here through their own key, with nothing said of them yet.
    #[must_use]
    pub fn of(member: MemberId) -> Self {
        Self {
            device: member.key(),
            member,
            tab: None,
            app: None,
            status: None,
            availability: Availability::Available,
            call: None,
            session: None,
            followed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_is_written_as_it_is_read() {
        let row = Present {
            tab: Some(7),
            call: Some(CallPresence {
                channel: "voice".into(),
                name: "Standup".into(),
            }),
            status: Some(Status {
                text: "Writing in Launch notes".into(),
                automatic: true,
            }),
            session: Some(9),
            ..Present::of(MemberId::agent("helper"))
        };
        let json = serde_json::to_string(&row).unwrap();
        assert_eq!(serde_json::from_str::<Present>(&json).unwrap(), row);
    }
}
