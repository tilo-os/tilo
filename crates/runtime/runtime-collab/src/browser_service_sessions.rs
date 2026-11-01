//! Request-scoped account credentials from the session's custody; never a
//! collaboration event or document.
//!
//! One request shape on an app's own admitted route reaches the one
//! custodian, the session, which holds the person's account vault:
//!
//! - Browser lists, resolves and forgets its website sessions;
//! - Settings, the keeper of the person's accounts, seals an account's
//!   secret ([`BrowserServiceSessionCommand::StoreAccountSecret`]), which is
//!   never readable back by Settings;
//! - the app that keeps an account's type (Mail for email) is released that
//!   one account's secret
//!   ([`BrowserServiceSessionCommand::ReleaseAccountSecret`]).
//!
//! A sandboxed app never opens the vault itself: its sandbox reaches neither
//! the vault's files nor the key custody that seals them.
use browser_continuity_core::sessions::{Service, SessionCookies};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES: usize = 4096;
pub const MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BROWSER_SERVICE_SESSIONS: usize = 128;

/// The longest account secret custody seals or releases.
pub const MAX_ACCOUNT_SECRET_BYTES: usize = 2048;

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrowserServiceSessionCommand {
    List,
    Resolve {
        id: String,
        setup_binding: String,
    },
    Forget {
        id: String,
        setup_binding: String,
    },
    /// Seal `password` as the secret of the person's account `account`.
    /// Only the accounts' keeper (Settings) may; nothing reads it back to it.
    StoreAccountSecret {
        account: String,
        password: Zeroizing<String>,
    },
    /// The secret of the person's account `account`, for the app that keeps
    /// that account's type and nobody else.
    ReleaseAccountSecret {
        account: String,
    },
}
impl std::fmt::Debug for BrowserServiceSessionCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::List => f.write_str("List"),
            Self::Resolve { id, .. } => f.debug_struct("Resolve").field("id", id).finish(),
            Self::Forget { id, .. } => f.debug_struct("Forget").field("id", id).finish(),
            Self::StoreAccountSecret { account, .. } => f
                .debug_struct("StoreAccountSecret")
                .field("account", account)
                .finish_non_exhaustive(),
            Self::ReleaseAccountSecret { account } => f
                .debug_struct("ReleaseAccountSecret")
                .field("account", account)
                .finish(),
        }
    }
}
impl<'de> Deserialize<'de> for BrowserServiceSessionCommand {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // An internally tagged unit variant ignores extra fields in Serde.
        // Use an empty struct variant so List has the same strict wire contract
        // as Resolve, including refusal of caller-supplied identity claims.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
        enum Wire {
            List {},
            Resolve {
                id: String,
                setup_binding: String,
            },
            Forget {
                id: String,
                setup_binding: String,
            },
            StoreAccountSecret {
                account: String,
                password: Zeroizing<String>,
            },
            ReleaseAccountSecret {
                account: String,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::List {} => Self::List,
            Wire::Resolve { id, setup_binding } => Self::Resolve { id, setup_binding },
            Wire::Forget { id, setup_binding } => Self::Forget { id, setup_binding },
            Wire::StoreAccountSecret { account, password } => {
                Self::StoreAccountSecret { account, password }
            }
            Wire::ReleaseAccountSecret { account } => Self::ReleaseAccountSecret { account },
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserServiceSessionDescriptor {
    pub id: String,
    pub service: Service,
    pub setup_binding: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrowserServiceSessionReply {
    List {
        sessions: Vec<BrowserServiceSessionDescriptor>,
    },
    Resolved {
        session: SessionCookies,
    },
    Forgotten {
        id: String,
        setup_binding: String,
    },
    /// The account's secret is sealed in the person's vault.
    AccountSecretStored {
        account: String,
    },
    /// The account's secret, released to the app that keeps its type.
    AccountSecret {
        account: String,
        password: Zeroizing<String>,
    },
}
impl std::fmt::Debug for BrowserServiceSessionReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserServiceSessionReply { private request-scoped reply }")
    }
}
fn binding(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn id(value: &str) -> bool {
    browser_continuity_core::sessions::valid_id(value)
}
/// An account id as the person's accounts record keys it (an address, a
/// provider id): bounded, printable.
fn account(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ACCOUNT_SECRET_BYTES
}

impl BrowserServiceSessionCommand {
    pub fn valid(&self) -> bool {
        match self {
            Self::List => true,
            Self::Resolve {
                id: value,
                setup_binding,
            }
            | Self::Forget {
                id: value,
                setup_binding,
            } => id(value) && binding(setup_binding),
            Self::StoreAccountSecret {
                account: value,
                password,
            } => account(value) && secret(password),
            Self::ReleaseAccountSecret { account: value } => account(value),
        }
    }
    /// Whether only Browser may send this: its website sessions. The
    /// account-secret requests are each authorized by the custodian for the
    /// app sending them.
    pub fn browser_only(&self) -> bool {
        matches!(
            self,
            Self::List | Self::Resolve { .. } | Self::Forget { .. }
        )
    }
    /// # Errors
    /// Refuses invalid commands, serialization failures or oversized wire bytes.
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        if !self.valid() {
            return Err("invalid service-session command");
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(self).map_err(|_| "service-session encoding failed")?,
        );
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES {
            return Err("service-session command too large");
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses oversized, malformed or invalid commands, including unknown fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES {
            return Err("service-session command too large");
        }
        let command: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid service-session command")?;
        if !command.valid() {
            return Err("invalid service-session command");
        }
        Ok(command)
    }
}
impl BrowserServiceSessionReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::List { sessions } => {
                let mut ids = std::collections::BTreeSet::new();
                sessions.len() <= MAX_BROWSER_SERVICE_SESSIONS
                    && sessions
                        .iter()
                        .all(|s| id(&s.id) && binding(&s.setup_binding) && ids.insert(&s.id))
            }
            Self::Resolved { session } => session.validate().is_ok(),
            Self::Forgotten {
                id: value,
                setup_binding,
            } => id(value) && binding(setup_binding),
            Self::AccountSecretStored { account: value } => account(value),
            Self::AccountSecret {
                account: value,
                password,
            } => account(value) && secret(password),
        }
    }
    pub fn matches(&self, command: &BrowserServiceSessionCommand) -> bool {
        match (self, command) {
            (Self::List { .. }, BrowserServiceSessionCommand::List) => self.valid(),
            (
                Self::Resolved { session },
                BrowserServiceSessionCommand::Resolve { id, setup_binding },
            ) => {
                self.valid() && &session.session_id == id && &session.setup_binding == setup_binding
            }
            (
                Self::Forgotten {
                    id: forgotten,
                    setup_binding: binding,
                },
                BrowserServiceSessionCommand::Forget { id, setup_binding },
            ) => self.valid() && forgotten == id && binding == setup_binding,
            (
                Self::AccountSecretStored { account: stored },
                BrowserServiceSessionCommand::StoreAccountSecret { account, .. },
            )
            | (
                Self::AccountSecret {
                    account: stored, ..
                },
                BrowserServiceSessionCommand::ReleaseAccountSecret { account },
            ) => self.valid() && stored == account,
            _ => false,
        }
    }
    /// # Errors
    /// Refuses invalid replies, serialization failures or oversized wire bytes.
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        if !self.valid() {
            return Err("invalid service-session reply");
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(self).map_err(|_| "service-session encoding failed")?,
        );
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err("service-session reply too large");
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses oversized, malformed or invalid replies, including unknown fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err("service-session reply too large");
        }
        let reply: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid service-session reply")?;
        if !reply.valid() {
            return Err("invalid service-session reply");
        }
        Ok(reply)
    }
}
#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Resolve only this admitted Browser channel's own selected credentials.
    ///
    /// # Errors
    /// Refuses invalid commands, transport failures and invalid or mismatched replies.
    pub async fn browser_service_session_command(
        &self,
        command: &BrowserServiceSessionCommand,
    ) -> Result<BrowserServiceSessionReply, crate::NotAvailable> {
        let failed = |message: &str| crate::NotAvailable::Refused(message.into());
        let bytes = command.encode().map_err(failed)?;
        let reader = Zeroizing::new(self.browser_service_session(bytes.to_vec()).await?);
        if reader.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err(failed("service-session reply too large"));
        }
        let reply = BrowserServiceSessionReply::decode(&reader).map_err(failed)?;
        if !reply.matches(command) {
            return Err(failed("service-session reply does not match request"));
        }
        Ok(reply)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn forget_wire_requires_exact_selection_and_reply_binding() {
        let command = super::BrowserServiceSessionCommand::Forget {
            id: "selected-slack".into(),
            setup_binding: "operation".into(),
        };
        assert_eq!(
            super::BrowserServiceSessionCommand::decode(&command.encode().unwrap()).unwrap(),
            command
        );
        let reply = super::BrowserServiceSessionReply::Forgotten {
            id: "selected-slack".into(),
            setup_binding: "operation".into(),
        };
        assert!(
            super::BrowserServiceSessionReply::decode(&reply.encode().unwrap())
                .unwrap()
                .matches(&command)
        );
        assert!(
            !reply.matches(&super::BrowserServiceSessionCommand::Resolve {
                id: "selected-slack".into(),
                setup_binding: "operation".into(),
            })
        );
        assert!(
            !reply.matches(&super::BrowserServiceSessionCommand::Forget {
                id: "selected-slack".into(),
                setup_binding: "foreign".into(),
            })
        );
        for bytes in [
            br#"{"kind":"forget","id":"selected-slack","setup_binding":"operation","person":"foreign"}"#.as_slice(),
            br#"{"kind":"forget","id":"selected-slack","setup_binding":""}"#.as_slice(),
            br#"{"kind":"forget","id":"../foreign","setup_binding":"operation"}"#.as_slice(),
        ] {
            assert!(super::BrowserServiceSessionCommand::decode(bytes).is_err());
        }
    }
    use super::*;
    #[test]
    fn rejects_identity_injection_and_redacts_malformed_credentials() {
        assert!(
            BrowserServiceSessionCommand::decode(br#"{"kind":"list","person":"someone"}"#).is_err()
        );
        let error = BrowserServiceSessionReply::decode(
            br#"{"kind":"resolved","session":"private-synthetic-value"}"#,
        )
        .unwrap_err();
        assert!(!error.contains("private-synthetic-value"));
        assert!(
            BrowserServiceSessionReply::decode(&vec![
                0;
                MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES + 1
            ])
            .is_err()
        );
    }
    #[test]
    fn selected_private_reply_roundtrips_and_binds_exact_request() {
        use browser_continuity_core::sessions::{Cookie, SameSite};
        let session = SessionCookies {
            schema: 1,
            session_id: "selected-session".into(),
            service: Service::Slack,
            identity_public_key: [21; 32],
            setup_binding: "prepared-binding".into(),
            source_snapshot_digest: [22; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "app.slack.com".into(),
                host_only: true,
                name: "synthetic-session".into(),
                value: "synthetic-private-value".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::Lax,
            }],
        };
        let reply = BrowserServiceSessionReply::Resolved { session };
        let encoded = reply.encode().unwrap();
        let decoded = BrowserServiceSessionReply::decode(&encoded).unwrap();
        assert!(decoded.matches(&BrowserServiceSessionCommand::Resolve {
            id: "selected-session".into(),
            setup_binding: "prepared-binding".into()
        }));
        assert!(!decoded.matches(&BrowserServiceSessionCommand::Resolve {
            id: "other-session".into(),
            setup_binding: "prepared-binding".into()
        }));
        assert!(!decoded.matches(&BrowserServiceSessionCommand::Resolve {
            id: "selected-session".into(),
            setup_binding: "other-binding".into()
        }));
        assert!(!format!("{decoded:?}").contains("synthetic-private-value"));
        assert!(!decoded.matches(&BrowserServiceSessionCommand::List));
    }
    #[test]
    fn bounds_and_deduplicates_public_descriptors() {
        let descriptor = BrowserServiceSessionDescriptor {
            id: "opaque-session".into(),
            service: Service::Slack,
            setup_binding: "prepared-binding".into(),
        };
        assert!(
            BrowserServiceSessionReply::List {
                sessions: vec![descriptor.clone()]
            }
            .valid()
        );
        assert!(
            !BrowserServiceSessionReply::List {
                sessions: vec![descriptor.clone(), descriptor]
            }
            .valid()
        );
        assert!(
            !BrowserServiceSessionCommand::Resolve {
                id: "../escape".into(),
                setup_binding: "binding".into()
            }
            .valid()
        );
    }
}
