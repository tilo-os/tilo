//! The host's own way to a provider: the model gateway's transport, asked
//! with a person's provider, model and key.
//!
//! The gateway owns the providers' wires (OpenAI-compatible chat
//! completions and Responses, Anthropic's Messages, Google's Gemini through
//! its OpenAI-compatible endpoint), reads their streams, prices the answer
//! and refuses an address off the public internet when it is dialled. This
//! hands it one call and nothing of the host's own.

use std::sync::OnceLock;

use agent_provider_config::protocol::{ErrorKind, ModelError};
use tilo_model_gateway::host_backend::{PersonProviders, PersonTurn, public_endpoint};

use super::{Events, ModelProvider, ProviderCall, ProviderFuture};

/// Asks providers through the model gateway.
#[derive(Default)]
pub(super) struct GatewayProvider {
    /// The gateway's transport, its connections kept between calls; made
    /// by the first call.
    providers: OnceLock<Option<PersonProviders>>,
}

impl GatewayProvider {
    fn providers(&self) -> Result<&PersonProviders, ModelError> {
        self.providers
            .get_or_init(|| {
                PersonProviders::new()
                    .inspect_err(|error| {
                        tracing::warn!(%error, "the host cannot ask people's model providers");
                    })
                    .ok()
            })
            .as_ref()
            .ok_or_else(|| ModelError::new(ErrorKind::Unavailable))
    }
}

impl ModelProvider for GatewayProvider {
    fn answer<'a>(&'a self, call: ProviderCall<'a>, events: Events<'a>) -> ProviderFuture<'a> {
        Box::pin(async move {
            let providers = self.providers()?;
            let timeout = call.deadline.saturating_duration_since(tilo_clock::now());
            if timeout.is_zero() {
                return Err(ModelError::new(ErrorKind::TimedOut));
            }
            providers
                .complete(
                    PersonTurn {
                        profile: call.profile,
                        model: &call.model.id,
                        key: call.key(),
                        request: call.request,
                        timeout,
                        private_addresses: call.private_addresses,
                        quiet: call.quiet,
                    },
                    events,
                )
                .await
        })
    }

    fn reaches(&self, endpoint: &str) -> bool {
        public_endpoint(endpoint)
    }
}
