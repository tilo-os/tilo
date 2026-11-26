//! A host's own calls of its people's providers ([`PersonProviders`]): the
//! provider and the key, from the person's own settings and vault, are
//! handed in for each turn.
//!
//! This is not an RPC and must never be wired to sandbox-controlled endpoints
//! or keys. Sandboxed apps use `Complete` over their launcher-issued
//! capability. Provider encoding/decoding is the one wire codec
//! ([`agent_provider_config::wire`]), not a second HTTP implementation.
//!
//! This module is a child of `service` so both entry points share its private
//! transport policy. Only this deliberately host-only facade is re-exported.
use super::*;
pub use agent_provider_config::protocol;
use protocol as neutral;
pub use provider_call::ModelPermission;

/// One turn at a person's own provider, asked by the host that keeps that
/// person's settings and their key.
///
/// The host read the provider from the person's own settings and the key
/// from their own vault. Neither is the operator's and neither is adopted
/// into any other store: the key goes into this one request and is wiped
/// when it ends.
pub struct PersonTurn<'a> {
    /// The provider, as the person's own settings keep it.
    pub profile: &'a ProviderProfile,
    /// The model asked.
    pub model: &'a str,
    /// The provider's key, from the person's own vault.
    pub key: Option<&'a str>,
    /// What is asked.
    pub request: &'a neutral::ModelRequest,
    /// How long the answer may take.
    pub timeout: Duration,
    /// Whether the provider may be at an address off the public internet:
    /// the host operator's choice, never the person's.
    pub private_addresses: bool,
    /// How long the provider may show nothing before the call is let go
    /// and ends as timed out: no response headers after the request, then
    /// no byte of any kind. None holds the call to `timeout` alone.
    pub quiet: Option<Duration>,
}

impl std::fmt::Debug for PersonTurn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonTurn")
            .field("provider", &self.profile.id)
            .field("model", &self.model)
            .field("keyed", &self.key.is_some())
            .finish_non_exhaustive()
    }
}

/// Where a turn's stream goes as it arrives. Presentation only: the answer
/// the turn returns is what counts.
pub type PersonEvents<'a> = &'a mut (dyn FnMut(neutral::StreamEvent) + Send);

/// Whether `endpoint` is a provider on the public internet, reached over
/// TLS, as far as its address says: a public literal address, or a name
/// that is not this machine's or its network's (and is checked again when
/// it resolves, [`PersonProviders`]).
#[must_use]
pub fn public_endpoint(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint.trim()) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    if url.domain().is_some() {
        return EndpointLocation::of_endpoint(endpoint) == Some(EndpointLocation::Hosted);
    }
    url.host_str()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(tilo_net::proxy::host::is_public)
}

/// Resolves a provider's name, and refuses it when any address it resolves
/// to is off the public internet: a person's settings never point the host
/// into its own network, also not through a name that resolves there.
struct PublicNames;

impl reqwest::dns::Resolve for PublicNames {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().to_owned();
        Box::pin(async move {
            let addresses: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|address| !tilo_net::proxy::host::is_public(address.ip()))
            {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "the provider's name resolves to no public address",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// What a host asks its people's own providers through: the gateway's own
/// provider transport, wire encoding, stream reading, retry rule and price
/// table, with the provider and the key handed in per turn.
pub struct PersonProviders {
    /// Dials addresses on the public internet only.
    public: reqwest::Client,
    /// Dials any address, where the host's operator allows it.
    any: reqwest::Client,
}

impl PersonProviders {
    /// The transport, its connections kept between turns.
    ///
    /// # Errors
    /// A bounded diagnostic when the HTTP clients cannot start.
    pub fn new() -> Result<Self, neutral::ModelError> {
        let unavailable = |_| neutral::ModelError::new(neutral::ErrorKind::Unavailable);
        Ok(Self {
            public: provider_builder()
                .dns_resolver(PublicNames)
                // A proxy taken from the environment would look the name up
                // itself, past the check made here.
                .no_proxy()
                .build()
                .map_err(unavailable)?,
            any: provider_builder().build().map_err(unavailable)?,
        })
    }

    /// Ask `turn`'s provider, handing what streams to `events` as it
    /// arrives, and answer the whole turn with what it cost. Dropping the
    /// answer on its way ends the request.
    ///
    /// # Errors
    /// The provider's failure as a diagnostic safe to show, the deadline,
    /// or a provider at an address this host does not call.
    pub async fn complete(
        &self,
        turn: PersonTurn<'_>,
        events: PersonEvents<'_>,
    ) -> Result<neutral::ModelResponse, neutral::ModelError> {
        self.complete_while_permitted(turn, events, None).await
    }

    /// Ask a provider while the host still authorizes this turn. The transport
    /// checks immediately before every request, including retries.
    ///
    /// # Errors
    /// A cancelled turn when its permission ends, or the failures of [`Self::complete`].
    pub async fn complete_while_permitted(
        &self,
        turn: PersonTurn<'_>,
        events: PersonEvents<'_>,
        permission: Option<&ModelPermission>,
    ) -> Result<neutral::ModelResponse, neutral::ModelError> {
        if !turn.private_addresses && !public_endpoint(&turn.profile.endpoint) {
            return Err(neutral::ModelError::new(
                neutral::ErrorKind::PermissionDenied,
            ));
        }
        let http = if turn.private_addresses {
            &self.any
        } else {
            &self.public
        };
        let call = PlanCall {
            profile_id: &turn.profile.id,
            model_id: turn.model,
            timeout: turn.timeout,
            stream: true,
            trace: None,
        };
        let mut emit = |frame: &Response| {
            if let (Some(kind), Some(payload)) = (frame.event.as_deref(), frame.output.as_deref())
                && let Ok(Some(event)) = crate::transport::decode_event(kind, payload)
            {
                events(event);
            }
            true
        };
        let secret = turn.key.map(|key| Zeroizing::new(key.to_owned()));
        let timing = PlanTiming::start();
        if let Some(quiet) = turn.quiet {
            timing.signs.let_go_after(quiet);
        }
        provider_call::complete_with(
            http,
            &call,
            (turn.profile, secret),
            turn.request,
            &mut emit,
            (
                timing,
                provider_call::Carry {
                    resend: true,
                    callers_deadline: true,
                    permission,
                },
            ),
        )
        .await
    }
}

#[cfg(test)]
#[path = "host_person_tests.rs"]
mod person_tests;
