//! Durable, compare-and-swap provider descriptions under the administration lock.
use super::{
    ErrorKind, ModelError, ProviderConfig, Request, Response, VAULT_WRITES, blocking_model,
    management_write, succeeded,
};
use agent_provider_config::management::ProviderSnapshot;
use sha2::{Digest, Sha256};
use tilo_lock::MutexRecover as _;

pub(super) async fn configuration_request(request: Request) -> Response {
    let result = blocking_model(move || match request {
        Request::GetProviderConfig => {
            let _serial = VAULT_WRITES.lock_recover();
            let config = ProviderConfig::load();
            validate_descriptions(&config)?;
            provider_snapshot(config)
        }
        Request::SetProviderConfig {
            expected_revision,
            config,
        } => set_provider_config(&expected_revision, config),
        _ => Err(ModelError::new(ErrorKind::InvalidRequest)),
    })
    .await;
    result
        .and_then(|snapshot| admin_output(&snapshot))
        .unwrap_or_else(Response::failure)
}

pub(super) fn provider_snapshot(config: ProviderConfig) -> Result<ProviderSnapshot, ModelError> {
    let bytes =
        serde_json::to_vec(&config).map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
    if bytes.len() > 256 * 1024 {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    }
    Ok(ProviderSnapshot {
        config,
        revision: hex::encode(Sha256::digest(bytes)),
    })
}

fn admin_output(snapshot: &ProviderSnapshot) -> Result<Response, ModelError> {
    serde_json::to_string(snapshot)
        .map(|output| succeeded(Some(output), None))
        .map_err(|_| ModelError::new(ErrorKind::InvalidResponse))
}

fn bounded(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

fn validate_descriptions(config: &ProviderConfig) -> Result<(), ModelError> {
    let invalid = || ModelError::new(ErrorKind::InvalidRequest);
    let bytes = serde_json::to_vec(config).map_err(|_| invalid())?;
    if bytes.len() > 256 * 1024
        || config.profiles.len() > 64
        || !config.validation_errors().is_empty()
    {
        return Err(invalid());
    }
    for profile in &config.profiles {
        if !bounded(&profile.id, 128)
            || !bounded(&profile.label, 256)
            || !bounded(&profile.endpoint, 2048)
            || profile.models.len() > 256
        {
            return Err(invalid());
        }
        // Endpoint userinfo, queries and fragments can contain credentials.
        let url = url::Url::parse(&profile.endpoint).map_err(|_| invalid())?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        for model in &profile.models {
            if !bounded(&model.id, 256) || !bounded(&model.label, 256) {
                return Err(invalid());
            }
        }
    }
    if config.agent_models.len() > 256
        || config.agent_models.keys().any(|agent| !bounded(agent, 256))
    {
        return Err(invalid());
    }
    Ok(())
}

fn set_provider_config(
    expected_revision: &str,
    config: ProviderConfig,
) -> Result<ProviderSnapshot, ModelError> {
    validate_descriptions(&config)?;
    let _serial = management_write().map_err(|_| ModelError::new(ErrorKind::Unavailable))?;
    let previous = ProviderConfig::load();
    if expected_revision.len() != 64
        || provider_snapshot(previous.clone())?.revision != expected_revision
    {
        return Err(ModelError::new(ErrorKind::Conflict));
    }
    for profile in &config.profiles {
        for model in &profile.models {
            if model.revision.is_some() {
                // Signed installation metadata is owned by the provisioner,
                // never minted or retargeted by a remote configuration edit.
                let old = previous.profiles.iter().find(|old| old.id == profile.id);
                if !old.is_some_and(|old| {
                    old.endpoint == profile.endpoint
                        && old.protocol == profile.protocol
                        && old.location == profile.location
                        && old.models.iter().any(|known| known == model)
                }) {
                    return Err(ModelError::new(ErrorKind::PermissionDenied));
                }
            }
        }
        if let Some(old) = previous.profiles.iter().find(|old| old.id == profile.id)
            && old.models.iter().any(|model| model.revision.is_some())
            && (old.endpoint != profile.endpoint
                || old.protocol != profile.protocol
                || old.location != profile.location
                || old.models != profile.models)
        {
            return Err(ModelError::new(ErrorKind::PermissionDenied));
        }
    }
    if !config
        .compare_save(&previous)
        .map_err(|_| ModelError::new(ErrorKind::Unavailable))?
    {
        return Err(ModelError::new(ErrorKind::Conflict));
    }
    provider_snapshot(config)
}
