//! A participant a bridge carries into a call from another service.
//!
//! A bridge (a workspace's host joined to a Discord voice channel) holds one
//! participant per person on the other side, each saying itself on presence as
//! a member of the call: so each is heard, shown and turned down on their own,
//! never as the bridge's own person. This is who such a participant is there.

/// Who a bridged participant is on the service its bridge reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Bridged {
    /// The service, lowercase (`discord`).
    pub service: String,
    /// The account on that service whose voice this carries.
    pub account: String,
    /// The name they are shown by: theirs as an appearance may carry it, else
    /// their account's.
    pub name: String,
    /// BLAKE3 hex of the avatar in the workspace's content store, when it is
    /// held.
    pub avatar: Option<String>,
    /// The workspace member the account is linked to, their root as hex: the
    /// participant is shown as that member, reached through `service`.
    pub linked: Option<String>,
}

/// The longest name a bridged participant is shown by, as an appearance's.
pub const MAX_BRIDGED_NAME_CHARS: usize = 80;
/// The longest account a bridge names.
pub const MAX_ACCOUNT_CHARS: usize = 64;

impl Bridged {
    /// This, as a receiver keeps it: the service in lowercase and bounded, the
    /// name trimmed and bounded, and an avatar or link only when it is a hash
    /// or a root key.
    #[must_use]
    pub fn cleaned(self) -> Self {
        Self {
            service: self
                .service
                .trim()
                .to_ascii_lowercase()
                .chars()
                .take(32)
                .filter(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || *character == b'-' as char
                })
                .collect(),
            account: self
                .account
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_ACCOUNT_CHARS)
                .collect(),
            name: self
                .name
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_BRIDGED_NAME_CHARS)
                .collect(),
            avatar: self.avatar.filter(|hash| hex32(hash)),
            linked: self.linked.filter(|root| hex32(root)),
        }
    }

    /// Whether this names a participant at all: a service and an account.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.service.is_empty() && !self.account.is_empty()
    }
}

/// 32 bytes in lowercase hex: a hash or a key.
fn hex32(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::Bridged;

    /// What a receiver keeps is bounded and lowercased, and an avatar or a link
    /// that is not a hash is not one.
    #[test]
    fn a_bridged_participant_is_cleaned_to_what_a_view_can_show() {
        let cleaned = Bridged {
            service: " Discord ".into(),
            account: " 7001 ".into(),
            name: "  Dana\u{7}  ".into(),
            avatar: Some("not a hash".into()),
            linked: Some("ab".repeat(32)),
        }
        .cleaned();
        assert_eq!(cleaned.service, "discord");
        assert_eq!(cleaned.account, "7001");
        assert_eq!(cleaned.name, "Dana");
        assert_eq!(cleaned.avatar, None);
        assert_eq!(cleaned.linked, Some("ab".repeat(32)));
        assert!(cleaned.valid());
        assert!(!Bridged::default().valid());
    }
}
