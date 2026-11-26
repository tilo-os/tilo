//! An issuer described as data, for confirming who a person is there.
//!
//! An app that bridges to an outside service describes that service's sign-in
//! with endpoints, scopes and the fields its identity endpoint answers. The
//! system signs the person in with it (a public PKCE client on one fixed
//! loopback redirect the client's owner registered) and reads back only the
//! account it confirmed: its id and name. Nothing about any one service is in
//! code.

use serde::{Deserialize, Serialize};

use crate::{OAuthError, Provider, Redirect, RedirectPorts, Refresh, Registration, Revocation};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribedIssuer {
    /// Stable identifier, unique among the issuers one app describes.
    pub id: String,
    pub label: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// Answers the signed-in account as JSON to a bearer access token.
    pub identity_endpoint: String,
    /// RFC 7009 revocation, when the issuer has one.
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    /// The field of the identity answer holding the account's stable id.
    pub id_field: String,
    /// The field of the identity answer holding the account's display name.
    pub name_field: String,
    pub scopes: Vec<String>,
    /// The one loopback port the redirect comes back to. Issuers that match
    /// redirects exactly need `http://127.0.0.1:<port>/oauth` registered with
    /// the client, which [`DescribedIssuer::redirect_uri`] answers.
    pub redirect_port: u16,
}

/// The account a sign-in confirmed at an issuer: all that is kept besides the
/// grant itself, and all an app may read of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedAccount {
    pub id: String,
    pub name: String,
}

impl DescribedIssuer {
    /// Refuses a description that could send a code or token anywhere but
    /// the issuer over TLS, or that names nothing to confirm.
    pub fn check(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.label.trim().is_empty() {
            return Err("an issuer needs an id and a label".into());
        }
        if self.id_field.is_empty() || self.name_field.is_empty() {
            return Err("an issuer names the identity fields for the account's id and name".into());
        }
        if self.redirect_port < 1024 {
            return Err("the redirect port is an unprivileged one, 1024 or above".into());
        }
        let endpoints = [
            ("authorization", Some(&self.authorization_endpoint)),
            ("token", Some(&self.token_endpoint)),
            ("identity", Some(&self.identity_endpoint)),
            ("revocation", self.revocation_endpoint.as_ref()),
        ];
        for (name, endpoint) in endpoints {
            if let Some(endpoint) = endpoint
                && !secure(endpoint)
            {
                return Err(format!(
                    "the {name} endpoint is https, or http on this computer"
                ));
            }
        }
        Ok(())
    }

    pub fn provider(&self) -> Provider {
        Provider {
            id: self.id.clone(),
            label: self.label.clone(),
            authorization_endpoint: self.authorization_endpoint.clone(),
            token_endpoint: self.token_endpoint.clone(),
            revocation: self
                .revocation_endpoint
                .clone()
                .map_or(Revocation::Unsupported, |endpoint| Revocation::Rfc7009 {
                    endpoint,
                }),
            scopes: self.scopes.clone(),
            authorization_params: Vec::new(),
            refresh: Refresh::RefreshToken,
            device_authorization_endpoint: None,
        }
    }

    /// A public client: PKCE protects the code, so no secret is involved.
    pub fn registration(&self, client_id: &str) -> Registration {
        Registration {
            redirect_ports: RedirectPorts::Registered(vec![self.redirect_port]),
            ..Registration::public(client_id)
        }
    }

    /// The exact address the client's owner registers as a redirect.
    pub fn redirect_uri(&self) -> String {
        let redirect = Redirect::default();
        format!(
            "http://{}:{}{}",
            redirect.host, self.redirect_port, redirect.path
        )
    }

    /// The account the identity endpoint answered, read from the described
    /// fields. An id may be a JSON string or number; anything else is not an
    /// account.
    pub fn account_of(&self, identity: &serde_json::Value) -> Result<ConfirmedAccount, OAuthError> {
        let field = |name: &str| match identity.get(name) {
            Some(serde_json::Value::String(value)) if !value.is_empty() => Some(value.clone()),
            Some(serde_json::Value::Number(value)) => Some(value.to_string()),
            _ => None,
        };
        let id = field(&self.id_field).ok_or_else(|| {
            OAuthError::Protocol(format!("the identity answer has no `{}`", self.id_field))
        })?;
        let name = field(&self.name_field).unwrap_or_else(|| id.clone());
        Ok(ConfirmedAccount { id, name })
    }

    /// Confirms who the tokens belong to at the issuer's identity endpoint.
    #[cfg(feature = "client")]
    pub fn confirm(
        &self,
        client: &crate::client::OAuthClient,
        tokens: &crate::TokenSet,
    ) -> Result<ConfirmedAccount, OAuthError> {
        self.account_of(&client.userinfo(&self.identity_endpoint, &tokens.access_token)?)
    }
}

fn secure(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name == "localhost",
            None => false,
        },
        _ => false,
    }
}
