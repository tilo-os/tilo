//! What an app says of its person on the connection its launcher handed it, and what the device
//! hands it of others: the two shapes the line wire carries as JSON ([`Said`], [`Heard`]).
//!
//! An app holds no live session: its words leave as one frame under the key they are said under,
//! and the device's one live presence says them for the app's member in the app's workspace, on
//! to the other devices exactly as the shell's words go. What those devices say comes back the
//! same way, one frame each, as the workspace's live session hears it.
//!
//! An app that carries people in from another service (a bridge) says their words as a persona
//! of its own ([`Persona`]): someone on that service, shown by their name there and the service's,
//! attributed to the app and never to a member here, whoever their account is linked to.

use serde::{Deserialize, Serialize};

use crate::MemberId;
use crate::presence::{Bridged, Payload, PresenceKey};

/// One word of an app's person: what they say under `key`, as the workspace's live session
/// carries it. The app, the workspace and the member are the connection's, never the frame's.
/// With a `persona`, it is that persona's word, of the app's own, and of no member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Said {
    pub key: PresenceKey,
    pub payload: Payload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<Persona>,
}

/// One word an app's views are shown: `member` said `payload` under `key` in the app's
/// workspace, and this device's live session heard it from another device. A persona's word
/// names the persona, and `member` is the app's member for it ([`Persona::member`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heard {
    pub member: MemberId,
    pub key: PresenceKey,
    pub payload: Payload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<Persona>,
}

/// Someone an app carries in from another service and speaks for in presence: their account
/// and name there. It is the app's, so nobody here is credited with what it says.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persona {
    /// The service, lowercase (`discord`).
    pub service: String,
    /// Their account on that service.
    pub account: String,
    /// The name they are shown by there.
    pub name: String,
}

/// What the live session's stand-in for a persona is said under: its session begins so, and the
/// rest is the persona's id ([`Persona::id`]). A reader takes such a stand-in as the app's persona,
/// never as the speaking device's person.
pub const PERSONA_SESSION: &str = "persona:";

impl Persona {
    /// This, as a receiver keeps it: bounded and lowercased as a bridged participant is.
    #[must_use]
    pub fn cleaned(self) -> Self {
        let bridged = Bridged {
            service: self.service,
            account: self.account,
            name: self.name,
            avatar: None,
            linked: None,
        }
        .cleaned();
        Self {
            service: bridged.service,
            account: bridged.account,
            name: bridged.name,
        }
    }

    /// Whether this names someone at all: a service, an account and a name. An account carries
    /// no `/`, so an id splits back into its parts one way only.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.service.is_empty()
            && !self.account.is_empty()
            && !self.account.contains(['/', ':'])
            && !self.name.is_empty()
    }

    /// The persona's id in `app`: the app, the service and the account. One per account per app.
    #[must_use]
    pub fn id(&self, app: &str) -> String {
        format!("{app}/{}:{}", self.service, self.account)
    }

    /// The member presence names the persona of `app` by: the app's own, never a person's.
    #[must_use]
    pub fn member(&self, app: &str) -> MemberId {
        MemberId::bot(self.id(app))
    }

    /// The session of the live session's stand-in for the persona of `app` ([`PERSONA_SESSION`]).
    #[must_use]
    pub fn session(&self, app: &str) -> String {
        format!("{PERSONA_SESSION}{}", self.id(app))
    }

    /// What a surface calls them: their name, and the service they are on ("Dana (Discord)").
    #[must_use]
    pub fn shown(&self) -> String {
        let mut service = self.service.chars();
        let service: String = service
            .next()
            .map(|first| first.to_uppercase().chain(service).collect())
            .unwrap_or_default();
        format!("{} ({service})", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::Persona;
    use crate::MemberId;

    /// A persona is its app's, named by its service, and kept to what a view can show.
    #[test]
    fn a_persona_is_its_apps_and_shows_its_service() {
        let dana = Persona {
            service: " Discord ".into(),
            account: "3001".into(),
            name: "  Dana ".into(),
        }
        .cleaned();
        assert!(dana.valid());
        assert_eq!(dana.shown(), "Dana (Discord)");
        assert_eq!(
            dana.member("so.tilo.discord"),
            MemberId::bot("so.tilo.discord/discord:3001")
        );
        let slashed = Persona {
            account: "30/01".into(),
            ..dana
        };
        assert!(!slashed.valid(), "an account names one persona only");
    }
}
