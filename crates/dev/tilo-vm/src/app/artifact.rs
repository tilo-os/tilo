//! Verified downloaded layers use the ordinary authenticated import and swap
//! RPCs. In particular apps+runtime never skips the daemon's signed SDK/app,
//! publisher, runtime or ISA checks.

use tilo_vm_protocol::{ApplyResult, LayerImport, LayerSource, LayerVerify, SlotShow, Swap};

use super::{App, Outcome, describe_apply, wait_for_session};
use crate::artifact::{PreparedLayer, prepare};
use crate::cli::ArtifactSwap;
use crate::context::Worktree;
use crate::error::CliError;
use crate::remote::Remote;

fn import(remote: &mut Remote, layer: &mut PreparedLayer) -> Result<String, CliError> {
    super::builder::validate_header(
        &layer.packed,
        &layer.receipt.component,
        &layer.receipt.source_digest,
        &remote.hello.arch,
        &[],
    )?;
    let upload = remote.upload(layer.archive.as_file_mut())?;
    let imported = remote.call(LayerImport {
        source: LayerSource::Upload(upload),
        provenance: Box::new(layer.provenance.clone()),
    })?;
    if imported.hash != layer.receipt.layer_hash {
        return Err(CliError::InvalidInput(
            "imported artifact differs from canonical layer identity".into(),
        ));
    }
    let verified = remote.call(LayerVerify {
        hash: imported.hash.clone(),
    })?;
    if verified.hash != imported.hash || verified.provenance != layer.provenance {
        return Err(CliError::InvalidInput(
            "installed artifact content or provenance does not match the upstream receipt".into(),
        ));
    }
    Ok(imported.hash)
}

fn activate(
    remote: &mut Remote,
    slot: &str,
    layer: &PreparedLayer,
    info: Option<tilo_vm_protocol::manifest::ComponentInfo>,
) -> Result<ApplyResult, CliError> {
    let result = remote.call(Swap {
        slot: slot.into(),
        component: layer.receipt.component.clone(),
        layer: layer.receipt.layer_hash.clone(),
        info,
    })?;
    let installed = remote.call(SlotShow { name: slot.into() })?;
    if result.slot != slot
        || installed.slot.name != slot
        || installed.slot.components.get(&layer.receipt.component)
            != Some(&layer.receipt.layer_hash)
    {
        return Err(CliError::InvalidInput(
            "artifact activation did not install the addressed layer into the requested slot"
                .into(),
        ));
    }
    Ok(result)
}

/// A cohort installs only from a checkout at exactly its producing commit: the
/// manifest, scopes and slot come from this checkout. The refusal names the
/// checkout it read and what it saw there, so a check that could not observe
/// its subject says why instead of reporting a mismatch.
fn consumer_checkout(worktree: &Worktree, source: &str) -> Result<(), CliError> {
    match worktree.checkout_commit() {
        Ok(head) if head == source => Ok(()),
        Ok(head) => Err(CliError::InvalidInput(format!(
            "artifact consumer checkout differs from requested exact source: {} is at {head}, the cohort was produced from {source}",
            worktree.root.display()
        ))),
        Err(said) => Err(CliError::InvalidInput(format!(
            "artifact consumer checkout {} has no readable HEAD to compare with the requested exact source {source}: {said}",
            worktree.root.display()
        ))),
    }
}

impl App {
    pub(super) fn swap_artifact(&mut self, request: &ArtifactSwap) -> Result<Outcome, CliError> {
        // Complete local verification precedes even the first daemon connection.
        // No fallback to a builder, Cargo metadata, a CI image or an old layer.
        consumer_checkout(
            self.worktree.as_ref().ok_or(CliError::NotInWorktree)?,
            &request.source,
        )?;
        let mut prepared = self.receipt.phase("artifact-verify", || prepare(request))?;
        let manifest = self.manifest()?.ok_or_else(|| CliError::NoManifest {
            path: self
                .base_dir()
                .join("os/components.toml")
                .display()
                .to_string(),
        })?;
        let slot = self.slot_name(request.slot.clone())?;
        let order: Vec<&str> = if request.component == "apps+runtime" {
            vec!["launcher", "install", "shell", "apps+runtime"]
        } else {
            vec![request.component.as_str()]
        };
        for name in &order {
            let component = manifest
                .get(name)
                .ok_or_else(|| CliError::UnknownComponent {
                    name: (*name).into(),
                    known: manifest.components.keys().cloned().collect(),
                })?;
            if component.boot || component.info.scope == tilo_vm_protocol::manifest::Scope::Vm {
                return Err(CliError::InvalidInput(
                    "artifact route only accepts normal session components".into(),
                ));
            }
        }
        let vm = self.resolve_vm(None)?;
        let mut remote = self.connect_for_swap(&vm)?;
        if remote.hello.arch != prepared.architecture {
            return Err(CliError::InvalidInput(
                "artifact architecture differs from target daemon".into(),
            ));
        }
        self.check_vm_space(&mut remote, &vm)?;
        self.receipt.set("slot", &slot);
        self.receipt.set("component", &request.component);
        self.receipt.set("artifact_source", &request.source);
        self.receipt.set("artifact_builder", &request.builder_image);
        self.receipt.set("artifact_run_id", request.run_id);
        self.receipt
            .set("artifact_run_attempt", request.run_attempt);
        self.receipt
            .set("artifact_receipt_sha256", &request.receipt_sha256);
        // Import and verify every addressed layer before activating prerequisites.
        // The daemon rejects a malformed/unsigned cohort on its normal Swap path.
        for layer in &mut prepared.layers {
            if !order.contains(&layer.receipt.component.as_str()) {
                continue;
            }
            self.receipt
                .phase("artifact-import", || import(&mut remote, layer))?;
        }
        let mut outcomes = Vec::new();
        let mut last = None;
        for name in order {
            let layer = prepared
                .layers
                .iter()
                .find(|l| l.receipt.component == name)
                .ok_or_else(|| {
                    CliError::InvalidInput("required verified artifact missing".into())
                })?;
            let result = self.receipt.phase("apply", || {
                activate(
                    &mut remote,
                    &slot,
                    layer,
                    manifest.get(name).map(|c| c.info.clone()),
                )
            })?;
            if result.changed && result.policy == tilo_vm_protocol::Policy::SessionRestart {
                self.receipt
                    .phase("session-ready", || wait_for_session(&mut remote, &slot));
            }
            self.warnings.extend(result.warnings.iter().cloned());
            outcomes.push(serde_json::json!({"component": name, "layer": layer.receipt.layer_hash, "result": result}));
            self.receipt.set("artifact_activations", &outcomes);
            self.receipt.set("layer", &layer.receipt.layer_hash);
            last = Some(result);
        }
        let result = last.ok_or_else(|| CliError::InvalidInput("no artifact activated".into()))?;
        Ok(Outcome::new(
            describe_apply(&result),
            serde_json::json!(result),
        ))
    }
}

#[cfg(test)]
mod tests;
