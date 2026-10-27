//! Workspace-level collaboration experience shared by every application.
//!
//! Pointer/caret presence answers *where*. This module answers the two human
//! questions that should not be inferred independently by every app: what a
//! participant is doing, and what they are trying to achieve. Inference is
//! deliberately local and transient; only the resulting short status is fit
//! for publication, and a manual status always wins.

use crate::time::{Duration, SystemTime};

pub use collab_types::presence::{Availability, IDLE_AFTER};

/// Automatic summaries go stale quickly. A stale intention is worse than no
/// status because it makes collaborators coordinate around work that stopped.
pub const INFERRED_STATUS_TTL: Duration = Duration::from_secs(5 * 60);
/// Manual statuses are explicit communication and therefore remain until the
/// author clears them.
pub const MAX_STATUS_CHARS: usize = 96;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusSharing {
    /// No inferred status is produced or published. Manual status remains
    /// available so privacy never removes the useful explicit path.
    Off,
    /// Only a status typed by the participant is visible.
    ManualOnly,
    /// A local model may summarize recent semantic actions.
    #[default]
    Automatic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusOrigin {
    Manual,
    /// Name of the local generator, surfaced in details rather than disguised
    /// as something the participant literally wrote.
    Inferred {
        generator: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveStatus {
    pub text: String,
    pub origin: StatusOrigin,
    pub updated_at: SystemTime,
}

/// What the people in a workspace see of this person, chosen per workspace.
///
/// A public workspace (one listed in its host's directory, open to anyone)
/// starts at [`Self::ThatImHere`]: strangers see a face, never where it is.
/// A private one starts at [`Self::WhereIAm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresenceVisibility {
    /// Your face, which window you are in, your pointer, your selection and
    /// what you say beside your pointer.
    WhereIAm,
    /// Your face only: not which window, not your pointer.
    ThatImHere,
    /// Nothing: you do not appear, and you do not see others' pointers
    /// either.
    Nothing,
}

impl PresenceVisibility {
    /// What a workspace starts at before the person chooses.
    #[must_use]
    pub const fn default_for(public: bool) -> Self {
        if public {
            Self::ThatImHere
        } else {
            Self::WhereIAm
        }
    }

    /// Whether others may see where this person is: which window, their
    /// pointer, caret and selection, what they say beside the pointer.
    #[must_use]
    pub const fn shows_where(self) -> bool {
        matches!(self, Self::WhereIAm)
    }

    /// Whether others may see this person at all.
    #[must_use]
    pub const fn shows_presence(self) -> bool {
        !matches!(self, Self::Nothing)
    }

    /// Whether this person sees others' pointers: someone who shows nothing
    /// sees nothing of where others are, which is fair.
    #[must_use]
    pub const fn sees_pointers(self) -> bool {
        !matches!(self, Self::Nothing)
    }

    /// The stored word.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::WhereIAm => "where",
            Self::ThatImHere => "here",
            Self::Nothing => "nothing",
        }
    }

    /// The choice a stored word names; `None` for one this build does not
    /// know.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "where" => Some(Self::WhereIAm),
            "here" => Some(Self::ThatImHere),
            "nothing" => Some(Self::Nothing),
            _ => None,
        }
    }
}

/// The voice channel a person is in, as their presence carries it beside
/// how available they are: the channel's id (what joining it names) and its
/// name (what the people here read, "In voice"). It rides in presence's one
/// `call` text as the id, a line break, then the name; a text without the
/// break is an id alone, its name not said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallPresence {
    pub channel: String,
    pub name: String,
}

impl CallPresence {
    /// Longest channel name carried; a longer one is cut.
    pub const MAX_NAME_CHARS: usize = 64;

    /// The `call` text that carries this.
    #[must_use]
    pub fn encode(&self) -> String {
        let name = clean_name(&self.name);
        if name.is_empty() {
            self.channel.clone()
        } else {
            format!("{}\n{name}", self.channel)
        }
    }

    /// What a `call` text carries; `None` for an empty one.
    #[must_use]
    pub fn decode(call: &str) -> Option<Self> {
        let (channel, name) = call.split_once('\n').unwrap_or((call, ""));
        let channel = channel.trim();
        if channel.is_empty() || channel.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            channel: channel.to_owned(),
            name: clean_name(name),
        })
    }
}

/// A channel name as a call carries it: no control characters, at most
/// [`CallPresence::MAX_NAME_CHARS`], trimmed.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .take(CallPresence::MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod presence_choice_tests {
    use super::CallPresence;

    /// A call carries its channel's id and name in one text, and reads back
    /// the same; an id alone reads with no name; an empty text is no call.
    #[test]
    fn a_call_carries_its_channel_id_and_name() {
        let voice = CallPresence {
            channel: "c_voice".into(),
            name: "voice".into(),
        };
        assert_eq!(CallPresence::decode(&voice.encode()), Some(voice));
        assert_eq!(
            CallPresence::decode("c_voice"),
            Some(CallPresence {
                channel: "c_voice".into(),
                name: String::new(),
            })
        );
        assert_eq!(CallPresence::decode(""), None);
        let long = CallPresence {
            channel: "c".into(),
            name: "x".repeat(200),
        };
        assert_eq!(
            CallPresence::decode(&long.encode()).map(|call| call.name.chars().count()),
            Some(CallPresence::MAX_NAME_CHARS)
        );
    }

    use super::*;

    /// A public workspace shows strangers a face and nothing of where it is;
    /// a private one shows where; Nothing shows nothing and sees no pointers.
    #[test]
    fn what_people_see_defaults_by_workspace_and_round_trips() {
        assert_eq!(
            PresenceVisibility::default_for(true),
            PresenceVisibility::ThatImHere
        );
        assert_eq!(
            PresenceVisibility::default_for(false),
            PresenceVisibility::WhereIAm
        );
        assert!(!PresenceVisibility::ThatImHere.shows_where());
        assert!(PresenceVisibility::ThatImHere.shows_presence());
        assert!(!PresenceVisibility::Nothing.shows_presence());
        assert!(!PresenceVisibility::Nothing.sees_pointers());
        for choice in [
            PresenceVisibility::WhereIAm,
            PresenceVisibility::ThatImHere,
            PresenceVisibility::Nothing,
        ] {
            assert_eq!(PresenceVisibility::from_id(choice.id()), Some(choice));
        }
    }

    /// Locked is away; five quiet minutes is idle unless another status
    /// was chosen.
    #[test]
    fn idle_and_away_come_on_their_own() {
        let quiet = IDLE_AFTER;
        assert_eq!(
            Availability::shown(Availability::Available, false, quiet),
            Availability::Idle
        );
        assert_eq!(
            Availability::shown(Availability::Available, false, Duration::from_secs(10)),
            Availability::Available
        );
        assert_eq!(
            Availability::shown(Availability::Focusing, false, quiet),
            Availability::Focusing
        );
        assert_eq!(
            Availability::shown(Availability::Focusing, true, Duration::ZERO),
            Availability::Away
        );
        assert!(Availability::Idle.dims() && Availability::Away.dims());
        assert!(!Availability::Focusing.dims());
    }
}
