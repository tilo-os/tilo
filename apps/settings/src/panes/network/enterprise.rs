//! The work or school (enterprise, 802.1X) network: the setup form, the
//! configuration it builds from what the person or an agent gives, and the
//! action on the session's `network` resource that joins it, so Settings and
//! a script join such a network the same way.

use os_actions::EnterpriseWifiConfiguration;
use ui::LocalState;

use super::non_blank;

/// The session's `network` resource, which the connectivity service
/// publishes.
const NETWORK_RESOURCE: &str = "network";

/// Its action that joins an enterprise (802.1X) network.
pub(super) const ENTERPRISE_JOIN: &str = "connect-enterprise-wifi";

/// What an enterprise connection was given; the setup form fills the rest.
#[derive(Clone)]
pub(super) struct EnterpriseGiven {
    pub(super) profile: Option<String>,
    pub(super) ssid: Option<String>,
    pub(super) identity: Option<String>,
    pub(super) anonymous_identity: Option<String>,
    pub(super) ca_certificate: Option<String>,
    pub(super) method: Option<String>,
    pub(super) inner_auth: Option<String>,
    pub(super) client_certificate: Option<String>,
    pub(super) private_key: Option<String>,
}

/// The enterprise network setup form.
pub(super) struct EnterpriseForm {
    pub(super) profile: LocalState<String>,
    pub(super) ssid: LocalState<String>,
    pub(super) identity: LocalState<String>,
    pub(super) anonymous: LocalState<String>,
    pub(super) ca: LocalState<String>,
    pub(super) client_certificate: LocalState<String>,
    pub(super) private_key: LocalState<String>,
    pub(super) method: LocalState<String>,
    pub(super) inner_auth: LocalState<String>,
}

impl EnterpriseForm {
    pub(super) fn new() -> Self {
        Self {
            profile: LocalState::new(String::new()),
            ssid: LocalState::new(String::new()),
            identity: LocalState::new(String::new()),
            anonymous: LocalState::new(String::new()),
            ca: LocalState::new(String::new()),
            client_certificate: LocalState::new(String::new()),
            private_key: LocalState::new(String::new()),
            method: LocalState::new("PEAP".into()),
            inner_auth: LocalState::new("MSCHAPv2".into()),
        }
    }

    /// The enterprise network `given` asks for, completed from the setup
    /// form.
    pub(super) fn config(&self, given: EnterpriseGiven) -> EnterpriseWifiConfiguration {
        let EnterpriseGiven {
            profile,
            ssid,
            identity,
            anonymous_identity,
            ca_certificate,
            method,
            inner_auth,
            client_certificate,
            private_key,
        } = given;
        EnterpriseWifiConfiguration {
            profile_name: profile.unwrap_or_else(|| self.profile.get()),
            ssid: ssid.unwrap_or_else(|| self.ssid.get()),
            identity: identity.unwrap_or_else(|| self.identity.get()),
            anonymous_identity: anonymous_identity
                .or_else(|| Some(self.anonymous.get()))
                .and_then(non_blank),
            ca_certificate: ca_certificate.unwrap_or_else(|| self.ca.get()),
            method: match method.unwrap_or_else(|| self.method.get()).as_str() {
                "TTLS" => os_actions::EnterpriseEapMethod::Ttls,
                "TLS" => os_actions::EnterpriseEapMethod::Tls,
                _ => os_actions::EnterpriseEapMethod::Peap,
            },
            phase2_auth: Some(
                inner_auth
                    .unwrap_or_else(|| self.inner_auth.get())
                    .to_ascii_lowercase(),
            ),
            client_certificate: client_certificate
                .or_else(|| Some(self.client_certificate.get()))
                .and_then(non_blank),
            private_key: private_key
                .or_else(|| Some(self.private_key.get()))
                .and_then(non_blank),
        }
    }
}

/// Join the enterprise network `config` describes with `password`, through
/// the `network` resource's action: the connectivity service saves the
/// profile and connects it, so Settings and a script join it the same way.
pub(super) fn join_enterprise(
    config: &EnterpriseWifiConfiguration,
    password: &str,
) -> Result<(), String> {
    let network = ui::collaboration::SessionResource::open(NETWORK_RESOURCE)?;
    network.invoke(ENTERPRISE_JOIN, &enterprise_arguments(config, password))
}

/// The arguments `connect-enterprise-wifi` takes, from what the setup form
/// holds: the profile, the network, the identity and the certificates, the
/// method and inner authentication, and the password, which the action keeps
/// out of readings and history.
pub(super) fn enterprise_arguments(
    config: &EnterpriseWifiConfiguration,
    password: &str,
) -> serde_json::Map<String, serde_json::Value> {
    fn put(
        arguments: &mut serde_json::Map<String, serde_json::Value>,
        name: &str,
        value: Option<&str>,
    ) {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            arguments.insert(name.to_owned(), value.into());
        }
    }
    let method = match config.method {
        os_actions::EnterpriseEapMethod::Peap => "PEAP",
        os_actions::EnterpriseEapMethod::Ttls => "TTLS",
        os_actions::EnterpriseEapMethod::Tls => "TLS",
    };
    let mut arguments = serde_json::Map::new();
    put(
        &mut arguments,
        "profile",
        Some(config.profile_name.as_str()),
    );
    put(&mut arguments, "ssid", Some(config.ssid.as_str()));
    put(&mut arguments, "identity", Some(config.identity.as_str()));
    put(
        &mut arguments,
        "anonymous_identity",
        config.anonymous_identity.as_deref(),
    );
    put(
        &mut arguments,
        "ca_certificate",
        Some(config.ca_certificate.as_str()),
    );
    put(&mut arguments, "method", Some(method));
    put(&mut arguments, "inner_auth", config.phase2_auth.as_deref());
    put(
        &mut arguments,
        "client_certificate",
        config.client_certificate.as_deref(),
    );
    put(&mut arguments, "private_key", config.private_key.as_deref());
    put(&mut arguments, "password", Some(password));
    arguments
}
