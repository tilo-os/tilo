//! Transport boundary primitives usable by either messaging product.
//!
//! No app policy, account login or external credentials live here. Codecs
//! do not authorize a send. The broker must durably reserve an operation and
//! recheck account ownership/revocation before writing the encoded request.

pub mod journal;
#[cfg(feature = "signal-broker")]
pub mod native_broker;
pub mod routing;
pub mod signal;
#[cfg(all(unix, feature = "signal-broker"))]
pub mod signal_linking;
#[cfg(all(unix, feature = "signal-broker"))]
pub mod signal_profile;
#[cfg(unix)]
pub mod signal_runtime;
pub mod signal_session;
pub mod signal_setup;
pub mod sms;
#[cfg(all(target_os = "linux", feature = "sms-dbus"))]
pub mod sms_runtime;
pub mod whatsapp;
#[cfg(feature = "whatsapp-sdk")]
pub mod whatsapp_runtime;
#[cfg(feature = "whatsapp-sdk")]
pub mod whatsapp_store;

/// The service is part of an address, never a fallback preference.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    Tilo,
    Whatsapp,
    Signal,
    Sms,
}

/// Immutable routing identity. Labels and contact names are not addresses.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Route {
    service: Service,
    account: String,
    recipient: String,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl Route {
    /// Keep the protocol's canonical account and recipient unchanged. This
    /// validates bounds only; callers must resolve membership and identity.
    ///
    /// # Errors
    /// Refuses empty, oversized or whitespace/control-containing identities.
    pub fn new(service: Service, account: String, recipient: String) -> Result<Self, Error> {
        if !identifier(&account) || !identifier(&recipient) {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            service,
            account,
            recipient,
        })
    }

    pub fn service(&self) -> Service {
        self.service
    }
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn recipient(&self) -> &str {
        &self.recipient
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control)
}

/// Diagnostics deliberately contain no raw payload, address or remote error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidIdentity,
    InvalidRequest,
    InvalidFrame,
    FrameTooLarge,
    WrongAccount,
    WrongRequest,
    Unsupported,
    RemoteRejected,
    ConnectionUnavailable,
}

/// Protocol acknowledgment identities, kept with the immutable route/owner.
/// Carrier object paths are meaningful only within their pinned daemon epoch.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProtocolReference {
    Signal {
        timestamp: u64,
    },
    Whatsapp {
        message_id: String,
    },
    Sms {
        modem_object: String,
        message_object: String,
        daemon_epoch: u64,
    },
    Tilo {
        message_id: String,
    },
}

impl std::fmt::Debug for ProtocolReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Tilo { .. } => "Tilo(<reference>)",
            Self::Signal { .. } => "Signal(<reference>)",
            Self::Whatsapp { .. } => "Whatsapp(<reference>)",
            Self::Sms { .. } => "Sms(<reference>)",
        })
    }
}

impl ProtocolReference {
    pub(crate) fn valid_for(&self, service: Service) -> bool {
        match (self, service) {
            (Self::Tilo { message_id }, Service::Tilo) => uuid::Uuid::parse_str(message_id)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == *message_id),
            (Self::Signal { timestamp }, Service::Signal) => *timestamp > 0,
            (Self::Whatsapp { message_id }, Service::Whatsapp) => identifier(message_id),
            (
                Self::Sms {
                    modem_object,
                    message_object,
                    daemon_epoch,
                },
                Service::Sms,
            ) => {
                *daemon_epoch > 0
                    && modem_object.starts_with("/org/freedesktop/ModemManager1/Modem/")
                    && message_object.starts_with("/org/freedesktop/ModemManager1/SMS/")
                    && modem_object.len() < 512
                    && message_object.len() < 512
                    && !modem_object.chars().any(char::is_whitespace)
                    && !message_object.chars().any(char::is_whitespace)
            }
            _ => false,
        }
    }
}

/// Evidence at the send boundary. No variant implies recipient delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    /// Durable reservation exists; no adapter write has begun.
    Reserved,
    /// The broker persisted this boundary before any external write.
    Started,
    /// The adapter returned a matching acknowledgment, not a delivery receipt.
    Acknowledged,
    /// Submission may have occurred; automatic retry is unsafe.
    Uncertain,
}

impl Submission {
    /// Only an untouched reservation permits the initial write. Started and
    /// uncertain operations require protocol reconciliation, never a retry.
    pub fn may_submit(self) -> bool {
        self == Self::Reserved
    }

    #[must_use]
    pub fn interrupted(self) -> Self {
        match self {
            Self::Started => Self::Uncertain,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_do_not_merge_routes_and_drafts_remain_account_specific() {
        let a = Route::new(Service::Signal, "account-a".into(), "person".into()).unwrap();
        let b = Route::new(Service::Signal, "account-b".into(), "person".into()).unwrap();
        let c = Route::new(Service::Sms, "account-a".into(), "person".into()).unwrap();
        let drafts = std::collections::BTreeMap::from([(a.clone(), "private draft")]);
        assert_eq!(drafts.get(&a), Some(&"private draft"));
        assert!(!drafts.contains_key(&b));
        assert!(!drafts.contains_key(&c));
        assert!(!format!("{a:?}").contains("account-a"));
    }

    #[test]
    fn interruption_after_the_write_boundary_never_permits_retry() {
        assert!(Submission::Reserved.interrupted().may_submit());
        assert_eq!(Submission::Started.interrupted(), Submission::Uncertain);
        assert!(!Submission::Uncertain.may_submit());
        assert!(!Submission::Acknowledged.may_submit());
    }
}

#[cfg(all(unix, feature = "signal-broker"))]
pub mod signal_broker;

#[cfg(all(
    unix,
    feature = "signal-broker",
    any(feature = "whatsapp-sdk", feature = "sms-dbus")
))]
pub mod external_broker;

#[cfg(all(
    unix,
    feature = "signal-broker",
    any(feature = "whatsapp-sdk", feature = "sms-dbus")
))]
pub mod supervisor;
