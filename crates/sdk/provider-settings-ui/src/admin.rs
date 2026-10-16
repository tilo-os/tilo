//! Portable, mount-owned provider administration. No secret enters a settings record.
use std::{future::Future, pin::Pin};

use super::{
    CredentialStatus, EndpointLocation, ErrorKind, HashMap, LimitFieldBuffers, ModelError,
    PanelBindings, ProviderAdmin, ProviderConfig, ProviderEffect, ProviderPanel, ProviderSnapshot,
    t,
};
use ui::lock::MutexRecover as _;
use zeroize::Zeroize;

pub type PendingAdmin = Pin<Box<dyn Future<Output = AdminRead> + Send>>;

pub struct AdminRead {
    pub snapshot: Option<ProviderSnapshot>,
    pub keys: HashMap<String, Result<CredentialStatus, String>>,
    pub outcome: Result<(), ModelError>,
    pub acknowledged: bool,
}

impl AdminRead {
    async fn of_snapshot(
        admin: &dyn ProviderAdmin,
        snapshot: ProviderSnapshot,
        outcome: Result<(), ModelError>,
        acknowledged: bool,
    ) -> Self {
        let mut keys = HashMap::new();
        for profile in &snapshot.config.profiles {
            if profile.location == EndpointLocation::Hosted {
                keys.insert(
                    profile.id.clone(),
                    admin
                        .credential_status(profile.id.clone())
                        .await
                        .map_err(|error| error.to_string()),
                );
            }
        }
        Self {
            snapshot: Some(snapshot),
            keys,
            outcome,
            acknowledged,
        }
    }

    async fn after(
        admin: &dyn ProviderAdmin,
        result: Result<Option<ProviderSnapshot>, ModelError>,
    ) -> Self {
        match result {
            Ok(Some(snapshot)) => Self::of_snapshot(admin, snapshot, Ok(()), true).await,
            Ok(None) => Self::load(admin, Ok(())).await,
            Err(error) => Self::load(admin, Err(error)).await,
        }
    }

    pub async fn load(admin: &dyn ProviderAdmin, outcome: Result<(), ModelError>) -> Self {
        let acknowledged = outcome.is_ok();
        match admin.get_config().await {
            Ok(snapshot) => Self::of_snapshot(admin, snapshot, outcome, acknowledged).await,
            Err(error) => Self {
                snapshot: None,
                keys: HashMap::new(),
                outcome: outcome.and(Err(error)),
                acknowledged,
            },
        }
    }
}

pub struct LimitAck {
    id: (String, String),
    submitted: [String; 3],
}

fn limit_draft(fields: &LimitFieldBuffers) -> [String; 3] {
    [&fields.output, &fields.runtime, &fields.deadline].map(|field| field.lock_recover().clone())
}

impl<H: PanelBindings> ProviderPanel<H> {
    pub fn remember_limits(&self, effect: &ProviderEffect) -> Option<LimitAck> {
        let ProviderEffect::SetModelRequestLimits {
            provider, model, ..
        } = effect
        else {
            return None;
        };
        let id = (provider.clone(), model.clone());
        self.limit_field_buffers
            .lock_recover()
            .get(&id)
            .map(|fields| LimitAck {
                id,
                submitted: limit_draft(fields),
            })
    }

    pub fn acknowledge_limits(&self, acknowledgement: &LimitAck) {
        let mut buffers = self.limit_field_buffers.lock_recover();
        if buffers
            .get(&acknowledgement.id)
            .is_some_and(|fields| limit_draft(fields) == acknowledgement.submitted)
        {
            // Reset from acknowledged configuration, unless typing has moved on.
            buffers.remove(&acknowledgement.id);
        }
    }

    /// Submit host-specific metadata through the same serialized, revision-bound controller.
    ///
    /// # Errors
    /// Returns an error while another operation is pending, administration is unavailable,
    /// or the authoritative configuration has not been loaded.
    pub fn submit_config(
        &mut self,
        config: ProviderConfig,
        reply: ui::framework::EffectReply,
    ) -> Result<(), String> {
        if self.admin_pending.is_some() {
            return Err(t("agent-provider-busy"));
        }
        let admin = self
            .provider_admin
            .get()
            .map_err(|error| error.to_string())?;
        let revision = self
            .admin_snapshot
            .as_ref()
            .ok_or_else(|| t("agent-provider-refresh-required"))?
            .revision
            .clone();
        self.admin_pending = Some(Box::pin(async move {
            let outcome = admin.set_config(revision, config).await.map(Some);
            AdminRead::after(admin.as_ref(), outcome).await
        }));
        self.admin_reply = Some(reply);
        self.changed = true;
        Ok(())
    }

    /// Queue an administration effect against the current authoritative snapshot.
    ///
    /// # Errors
    /// Returns an error when administration is busy or unavailable, the snapshot is
    /// missing, the proposed change is invalid, or account custody forbids a key edit.
    pub fn submit_provider(
        &mut self,
        effect: ProviderEffect,
        reply: ui::framework::EffectReply,
    ) -> Result<(), String> {
        if self.admin_pending.is_some() {
            return Err(t("agent-provider-busy"));
        }
        let admin = self
            .provider_admin
            .get()
            .map_err(|error| error.to_string())?;
        if matches!(effect, ProviderEffect::RefreshProviders) {
            self.admin_pending = Some(Box::pin(async move {
                AdminRead::load(admin.as_ref(), Ok(())).await
            }));
        } else {
            let snapshot = self
                .admin_snapshot
                .as_ref()
                .ok_or_else(|| t("agent-provider-refresh-required"))?;
            let revision = snapshot.revision.clone();
            validate_key_custody(snapshot, &effect)?;
            self.limits_ack = self.remember_limits(&effect);
            let work: PendingMutation =
                match effect {
                    ProviderEffect::SaveProviderKey { provider, key } => {
                        let buffer = self.key_buffer(&provider);
                        if let Some(key) = key {
                            let mut draft = buffer.lock_recover();
                            draft.zeroize();
                            *draft = key;
                        }
                        let value = zeroize::Zeroizing::new(buffer.lock_recover().clone());
                        if value.trim().is_empty() {
                            return Err(t("agent-key-empty"));
                        }
                        self.saved_key = Some((buffer, value.clone()));
                        Box::pin(async move {
                            admin
                                .set_api_key(provider, value.to_string())
                                .await
                                .map(|()| None)
                        })
                    }
                    ProviderEffect::RemoveProviderKey { provider } => {
                        Box::pin(async move { admin.remove_api_key(provider).await.map(|()| None) })
                    }
                    ProviderEffect::RemoveLegacyProviderKey { provider } => Box::pin(async move {
                        admin.remove_legacy_api_key(provider).await.map(|()| None)
                    }),
                    ProviderEffect::MigrateProviderKey {
                        provider,
                        original_endpoint,
                    } => Box::pin(async move {
                        admin
                            .migrate_api_key(provider, original_endpoint)
                            .await
                            .map(|()| None)
                    }),
                    ProviderEffect::RemoveProvider { provider } => {
                        let config = self.proposed_config(ProviderEffect::RemoveProvider {
                            provider: provider.clone(),
                        })?;
                        let uses_api_key = snapshot
                            .config
                            .profiles
                            .iter()
                            .any(|profile| profile.id == provider && profile.sign_in.is_none());
                        Box::pin(async move {
                            // Revalidate before destructive cleanup. The backend forbids
                            // deletion while bound or legacy credentials remain.
                            if admin.get_config().await?.revision != revision {
                                return Err(ModelError::new(ErrorKind::Conflict));
                            }
                            if uses_api_key {
                                admin.remove_api_key(provider.clone()).await?;
                                admin.remove_legacy_api_key(provider).await?;
                            }
                            admin.set_config(revision, config).await.map(Some)
                        })
                    }
                    metadata => {
                        let config = self.proposed_config(metadata)?;
                        Box::pin(async move { admin.set_config(revision, config).await.map(Some) })
                    }
                };
            let reader = self
                .provider_admin
                .get()
                .map_err(|error| error.to_string())?;
            self.admin_pending = Some(Box::pin(async move {
                let outcome = work.await;
                // Reload even on conflict/failure: never silently replay a stale edit.
                AdminRead::after(reader.as_ref(), outcome).await
            }));
        }
        self.admin_reply = Some(reply);
        self.changed = true;
        Ok(())
    }
}

/// Sign-in credentials remain exclusively under account custody.
fn validate_key_custody(
    snapshot: &ProviderSnapshot,
    effect: &ProviderEffect,
) -> Result<(), String> {
    let provider = match effect {
        ProviderEffect::SaveProviderKey { provider, .. }
        | ProviderEffect::RemoveProviderKey { provider }
        | ProviderEffect::RemoveLegacyProviderKey { provider }
        | ProviderEffect::MigrateProviderKey { provider, .. } => provider,
        _ => return Ok(()),
    };
    if snapshot
        .config
        .profiles
        .iter()
        .any(|profile| &profile.id == provider && profile.sign_in.is_some())
    {
        return Err(t("agent-sign-in-custody"));
    }
    Ok(())
}

type PendingMutation =
    Pin<Box<dyn Future<Output = Result<Option<ProviderSnapshot>, ModelError>> + Send>>;
