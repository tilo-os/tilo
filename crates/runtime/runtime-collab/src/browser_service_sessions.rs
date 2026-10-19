//! Request-scoped Browser credentials; never a collaboration event or document.
use browser_continuity_core::sessions::{Service, SessionCookies};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES: usize = 4096;
pub const MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BROWSER_SERVICE_SESSIONS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrowserServiceSessionCommand {
    List,
    Resolve { id: String, setup_binding: String },
    Forget { id: String, setup_binding: String },
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
            Resolve { id: String, setup_binding: String },
            Forget { id: String, setup_binding: String },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::List {} => Self::List,
            Wire::Resolve { id, setup_binding } => Self::Resolve { id, setup_binding },
            Wire::Forget { id, setup_binding } => Self::Forget { id, setup_binding },
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
        }
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
        let failed =
            |message: &str| crate::NotAvailable::Capnp(capnp::Error::failed(message.into()));
        let bytes = command.encode().map_err(failed)?;
        let mut request = self.inner.browser_service_session_command_request();
        request.get().set_command(&bytes);
        let response = request.send().promise.await?;
        let reader = response.get()?.get_reply()?;
        if reader.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err(failed("service-session reply too large"));
        }
        let bytes = Zeroizing::new(reader.to_vec());
        let reply = BrowserServiceSessionReply::decode(&bytes).map_err(failed)?;
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
