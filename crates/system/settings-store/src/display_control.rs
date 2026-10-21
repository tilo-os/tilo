//! Typed requests to the session that owns display configuration. An app
//! supplies intent and the output set it observed; the host owns prior state,
//! transaction identity and persistence. Admission belongs to the broker.
use crate::SettingsSource;
use crate::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplaySet, DisplayTransaction, DisplayTransactionState,
};
use serde::{Deserialize, Serialize};

pub const MAX_DISPLAY_CONTROL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayCommand {
    Inspect,
    Identify,
    Apply {
        observed_set: DisplaySet,
        candidate: DisplayConfig,
    },
    Keep {
        transaction: u64,
    },
    Revert {
        transaction: u64,
    },
}
impl DisplayCommand {
    pub fn mutates(&self) -> bool {
        !matches!(self, Self::Inspect)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct DisplayReply {
    pub observed: DisplayHardwareSnapshot,
    pub transaction: Option<DisplayTransaction>,
    pub committed: DisplayConfig,
    pub identify: Option<crate::display_identify::IdentifyRequest>,
}

crate::read_through_json_value!(DisplayReply);

fn transaction(
    document: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<DisplayTransaction>, String> {
    let Some(value) = document.get("display_transaction") else {
        return Ok(None);
    };
    if value.get("id").is_none() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| "The display transaction is invalid.".into())
}

fn descriptors(observed: &DisplayHardwareSnapshot) -> Vec<crate::hardware::OutputDescriptor> {
    observed
        .outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect()
}

fn previous(observed: &DisplayHardwareSnapshot, saved: &DisplayConfig) -> DisplayConfig {
    DisplayConfig {
        outputs: observed
            .outputs
            .iter()
            .map(|output| {
                let mut config = saved
                    .outputs
                    .get(&output.descriptor.connector)
                    .cloned()
                    .unwrap_or_else(|| output.unconfigured());
                config.mode = Some(output.current_mode);
                config.scale = output.scale;
                let placement = observed
                    .layout
                    .iter()
                    .find(|placement| placement.connector == output.descriptor.connector);
                config.enabled = placement.is_some();
                if let Some(placement) = placement {
                    config.x = placement.x;
                    config.y = placement.y;
                }
                (output.descriptor.connector.clone(), config)
            })
            .collect(),
    }
}

fn apply(
    observed: &DisplayHardwareSnapshot,
    current: Option<&DisplayTransaction>,
    committed: &DisplayConfig,
    observed_set: DisplaySet,
    candidate: DisplayConfig,
) -> Result<DisplayTransaction, String> {
    let descriptors = descriptors(observed);
    if descriptors.is_empty() || observed_set != DisplaySet::of_descriptors(&descriptors) {
        return Err("The connected displays have changed. Review the configuration again.".into());
    }
    if candidate.outputs.len() != descriptors.len()
        || descriptors
            .iter()
            .any(|output| !candidate.outputs.contains_key(&output.connector))
    {
        return Err("The configuration must name exactly the connected displays.".into());
    }
    candidate
        .validate_for(&descriptors)
        .map_err(|error| format!("The display configuration is unsupported: {error:?}"))?;
    if current.is_some_and(|value| pending(value.state)) {
        return Err("A display preview is already pending.".into());
    }
    Ok(DisplayTransaction {
        id: tilo_clock::unix_now_ms().max(current.map_or(1, |value| value.id.saturating_add(1))),
        state: DisplayTransactionState::Requested,
        set: observed_set,
        previous: previous(observed, committed),
        candidate,
        deadline_unix_ms: None,
        message: None,
    })
}

fn pending(state: DisplayTransactionState) -> bool {
    matches!(
        state,
        DisplayTransactionState::Requested
            | DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::ConfirmRequested
            | DisplayTransactionState::RevertRequested
    )
}

fn decide(
    observed: &DisplayHardwareSnapshot,
    current: &mut Option<DisplayTransaction>,
    id: u64,
    keep: bool,
) -> Result<(), String> {
    let value = current
        .as_mut()
        .filter(|value| value.id == id)
        .ok_or("This display preview is no longer pending.")?;
    if value.set != DisplaySet::of_descriptors(&descriptors(observed)) {
        return Err("The connected displays have changed. Review the configuration again.".into());
    }
    if keep {
        if value.state != DisplayTransactionState::AwaitingConfirmation
            || value.expired(tilo_clock::unix_now_ms())
        {
            return Err("This display preview is no longer waiting for confirmation.".into());
        }
        value.state = DisplayTransactionState::ConfirmRequested;
    } else {
        if !matches!(
            value.state,
            DisplayTransactionState::Requested
                | DisplayTransactionState::AwaitingConfirmation
                | DisplayTransactionState::ConfirmRequested
        ) {
            return Err("This display preview is no longer pending.".into());
        }
        value.state = DisplayTransactionState::RevertRequested;
    }
    Ok(())
}

/// Execute admitted intent against the host's fresh runtime snapshot and
/// settings source. The host derives prior state and transaction identity.
///
/// # Errors
/// Refuses stale outputs, unsupported intent, expired decisions, oversized
/// replies and persistence errors without changing other settings sections.
pub fn execute(
    source: &dyn SettingsSource,
    observed: DisplayHardwareSnapshot,
    command: DisplayCommand,
) -> Result<DisplayReply, String> {
    let document = source.load();
    let profiles: crate::hardware::DisplayProfiles = serde_json::from_value(
        document
            .get("display_profiles")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .map_err(|_| "The saved display profiles are invalid.")?;
    let committed = profiles
        .layout_for(&DisplaySet::of_descriptors(&descriptors(&observed)))
        .unwrap_or_default();
    let mut current = transaction(&document)?;
    let mutates = command.mutates();
    let identify = matches!(command, DisplayCommand::Identify).then(|| {
        crate::display_identify::IdentifyRequest {
            requested_unix_ms: tilo_clock::unix_now_ms(),
        }
    });
    match command {
        DisplayCommand::Inspect | DisplayCommand::Identify => {}
        DisplayCommand::Apply {
            observed_set,
            candidate,
        } => {
            current = Some(apply(
                &observed,
                current.as_ref(),
                &committed,
                observed_set,
                candidate,
            )?);
        }
        DisplayCommand::Keep { transaction: id } => decide(&observed, &mut current, id, true)?,
        DisplayCommand::Revert { transaction: id } => decide(&observed, &mut current, id, false)?,
    }
    let reply = DisplayReply {
        observed,
        transaction: current,
        committed,
        identify,
    };
    let bytes =
        serde_json::to_vec(&reply).map_err(|_| "The display reply could not be encoded.")?;
    if bytes.len() > MAX_DISPLAY_CONTROL_BYTES {
        return Err("The display reply exceeds its wire bound.".into());
    }
    if let Some(request) = identify {
        if reply.observed.outputs.is_empty() {
            return Err("No connected display can be identified.".into());
        }
        crate::display_identify::IdentifyRequest::request_in(source, request)
            .map_err(|error| format!("The display identify request could not be saved: {error}"))?;
    } else if mutates {
        let value = serde_json::to_value(
            reply
                .transaction
                .as_ref()
                .ok_or("The display transaction is missing.")?,
        )
        .map_err(|_| "The display transaction could not be encoded.")?;
        source
            .compare_display_transaction(
                document.get("display_transaction"),
                value
                    .as_object()
                    .cloned()
                    .ok_or("The display transaction is not an object.")?,
            )
            .map_err(|error| format!("The display transaction could not be saved: {error}"))?;
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HostDocument,
        hardware::{DisplayOutputSnapshot, LiveOutputPlacement, OutputDescriptor, OutputMode},
    };
    use std::collections::BTreeMap;

    fn observed() -> DisplayHardwareSnapshot {
        let mode = OutputMode {
            width: 2944,
            height: 1840,
            refresh_mhz: 59_999,
        };
        DisplayHardwareSnapshot {
            outputs: vec![DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: "eDP-1".into(),
                    connector_label: "eDP-1".into(),
                    modes: vec![
                        mode,
                        OutputMode {
                            refresh_mhz: 89_999,
                            ..mode
                        },
                    ],
                    hdr: false,
                    vrr: false,
                    color_profiles: vec![],
                    monitor: None,
                },
                current_mode: mode,
                scale: 2.0,
                builtin: true,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                unavailable_reason: None,
                color_profile_labels: BTreeMap::new(),
            }],
            layout: vec![LiveOutputPlacement {
                connector: "eDP-1".into(),
                width: 1472,
                height: 920,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn request(snapshot: &DisplayHardwareSnapshot) -> DisplayCommand {
        let mut candidate = previous(snapshot, &DisplayConfig::default());
        candidate
            .outputs
            .get_mut("eDP-1")
            .unwrap()
            .mode
            .as_mut()
            .unwrap()
            .refresh_mhz = 89_999;
        DisplayCommand::Apply {
            observed_set: DisplaySet::of_descriptors(&descriptors(snapshot)),
            candidate,
        }
    }

    #[test]
    fn host_identify_uses_the_existing_request_section_without_creating_a_preview() {
        // Freshness is judged in wall time; it stands still here.
        tilo_clock::freeze_wall(tilo_clock::wall());
        let source = HostDocument::new(
            serde_json::json!({"unrelated":{"keep":true}})
                .as_object()
                .unwrap()
                .clone(),
            |_| Ok(()),
        );
        let reply = execute(&source, observed(), DisplayCommand::Identify).unwrap();
        let request = reply.identify.expect("host stamped the identify request");
        assert!(request.is_fresh(tilo_clock::unix_now_ms()));
        assert_eq!(
            source.load()["display_identify"]["requested_unix_ms"],
            request.requested_unix_ms
        );
        assert_eq!(source.load()["unrelated"]["keep"], true);
        assert!(source.load().get("display_transaction").is_none());
        assert!(reply.transaction.is_none());
    }

    #[test]
    fn host_transaction_owns_previous_state_and_persists_only_its_section() {
        let initial = serde_json::json!({"app_personal":{"so.tilo.settings":{"theme":"dark"}}})
            .as_object()
            .unwrap()
            .clone();
        let source = HostDocument::new(initial.clone(), |_| Ok(()));
        let reply = execute(&source, observed(), request(&observed())).unwrap();
        let transaction = reply.transaction.unwrap();
        assert_eq!(transaction.state, DisplayTransactionState::Requested);
        assert_eq!(
            transaction.previous.outputs["eDP-1"]
                .mode
                .unwrap()
                .refresh_mhz,
            59_999
        );
        assert_eq!(
            transaction.previous.outputs["eDP-1"].scale.to_bits(),
            2.0_f64.to_bits()
        );
        assert_eq!(
            transaction.candidate.outputs["eDP-1"]
                .mode
                .unwrap()
                .refresh_mhz,
            89_999
        );
        assert_eq!(source.load()["app_personal"], initial["app_personal"]);
        assert!(execute(&source, observed(), request(&observed())).is_err());
        assert_eq!(
            execute(&source, observed(), DisplayCommand::Inspect)
                .unwrap()
                .transaction
                .unwrap()
                .id,
            transaction.id
        );
    }

    #[test]
    fn sandbox_denied_direct_preview_uses_explicit_host_source_without_global_write_grant() {
        struct Restore(Option<std::sync::Arc<dyn SettingsSource>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::install_source(self.0.take());
            }
        }
        let _restore = Restore(crate::installed_source());
        let sandbox = std::sync::Arc::new(HostDocument::new(serde_json::Map::default(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }));
        crate::install_source(Some(sandbox.clone()));
        let snapshot = observed();
        let DisplayCommand::Apply {
            observed_set,
            candidate,
        } = request(&snapshot)
        else {
            unreachable!()
        };
        assert_eq!(
            DisplayTransaction::request(observed_set.clone(), candidate.clone())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            crate::display_identify::IdentifyRequest::request()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let host = HostDocument::new(serde_json::Map::default(), |_| Ok(()));
        let identify = execute(&host, snapshot.clone(), DisplayCommand::Identify).unwrap();
        assert!(identify.identify.is_some());
        assert!(host.load().get("display_identify").is_some());
        assert!(sandbox.load().get("display_identify").is_none());
        assert!(crate::display_identify::IdentifyRequest::request().is_err());
        let reply = execute(
            &host,
            snapshot,
            DisplayCommand::Apply {
                observed_set,
                candidate,
            },
        )
        .unwrap();
        assert_eq!(
            reply.transaction.unwrap().state,
            DisplayTransactionState::Requested
        );
        assert!(sandbox.load().get("display_transaction").is_none());
        assert!(host.load().get("display_transaction").is_some());
        assert_eq!(
            DisplayTransaction::request(DisplaySet::default(), DisplayConfig::default())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied,
            "host mediation must not change the sandbox's generic settings rights"
        );
    }

    #[test]
    fn readonly_inspect_does_not_write_and_failed_persistence_does_not_claim_apply() {
        let source = HostDocument::new(serde_json::Map::default(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert!(execute(&source, observed(), DisplayCommand::Inspect).is_ok());
        assert!(
            execute(&source, observed(), request(&observed()))
                .unwrap_err()
                .contains("could not be saved")
        );
        assert!(source.load().get("display_transaction").is_none());
    }

    #[test]
    fn decisions_require_the_current_host_transaction_and_connected_outputs() {
        // The confirmation deadline is wall time; it stands still here.
        tilo_clock::freeze_wall(tilo_clock::wall());
        let source = HostDocument::new(serde_json::Map::default(), |_| Ok(()));
        let mut transaction = execute(&source, observed(), request(&observed()))
            .unwrap()
            .transaction
            .unwrap();
        transaction.state = DisplayTransactionState::AwaitingConfirmation;
        transaction.deadline_unix_ms = Some(tilo_clock::unix_now_ms() + 15_000);
        source
            .update_section(
                "display_transaction",
                serde_json::to_value(&transaction)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert!(
            execute(
                &source,
                observed(),
                DisplayCommand::Keep {
                    transaction: transaction.id + 1
                }
            )
            .is_err()
        );
        assert!(
            execute(
                &source,
                DisplayHardwareSnapshot::default(),
                DisplayCommand::Keep {
                    transaction: transaction.id
                }
            )
            .is_err()
        );
        assert!(
            execute(
                &source,
                DisplayHardwareSnapshot::default(),
                DisplayCommand::Revert {
                    transaction: transaction.id
                }
            )
            .is_err()
        );
        let reply = execute(
            &source,
            observed(),
            DisplayCommand::Keep {
                transaction: transaction.id,
            },
        )
        .unwrap();
        assert_eq!(
            reply.transaction.unwrap().state,
            DisplayTransactionState::ConfirmRequested
        );
        let reply = execute(
            &source,
            observed(),
            DisplayCommand::Revert {
                transaction: transaction.id,
            },
        )
        .unwrap();
        assert_eq!(
            reply.transaction.unwrap().state,
            DisplayTransactionState::RevertRequested
        );
    }

    #[test]
    fn stale_set_unsupported_mode_and_oversized_reply_are_refused_before_persistence() {
        let source = HostDocument::new(serde_json::Map::default(), |_| Ok(()));
        let mut snapshot = observed();
        let mut candidate = previous(&snapshot, &DisplayConfig::default());
        candidate
            .outputs
            .get_mut("eDP-1")
            .unwrap()
            .mode
            .as_mut()
            .unwrap()
            .refresh_mhz = 120_000;
        let set = DisplaySet::of_descriptors(&descriptors(&snapshot));
        assert!(
            execute(
                &source,
                snapshot.clone(),
                DisplayCommand::Apply {
                    observed_set: set,
                    candidate
                }
            )
            .is_err()
        );
        let command = request(&snapshot);
        snapshot.outputs[0].descriptor.connector = "HDMI-A-1".into();
        assert!(execute(&source, snapshot, command).is_err());
        let mut snapshot = observed();
        let command = request(&snapshot);
        snapshot.outputs[0]
            .color_profile_labels
            .insert("profile".into(), "x".repeat(MAX_DISPLAY_CONTROL_BYTES));
        assert!(execute(&source, snapshot, command).is_err());
        assert!(source.load().get("display_transaction").is_none());
    }
}

#[cfg(test)]
mod compare_tests {
    use super::*;
    use crate::{HostDocument, SessionPaths, SessionSettings};
    use serde_json::json;

    fn refuses_stale_confirmation(source: &dyn SettingsSource) {
        source
            .update_section(
                "display_transaction",
                json!({"id":1,"state":"awaiting_confirmation"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let before = source.load();
        source
            .update_section(
                "display_transaction",
                json!({"id":1,"state":"reverted"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            source
                .compare_display_transaction(
                    before.get("display_transaction"),
                    json!({"state":"confirm_requested"})
                        .as_object()
                        .unwrap()
                        .clone()
                )
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(source.load()["display_transaction"]["state"], "reverted");
        let current = source.load();
        source
            .compare_display_transaction(
                current.get("display_transaction"),
                json!({"message":"observed"}).as_object().unwrap().clone(),
            )
            .unwrap();
        assert_eq!(source.load()["display_transaction"]["state"], "reverted");
        assert_eq!(source.load()["display_transaction"]["message"], "observed");
    }

    #[test]
    fn host_document_does_not_overwrite_a_newer_compositor_decision() {
        refuses_stale_confirmation(&HostDocument::new(serde_json::Map::default(), |_| Ok(())));
    }

    #[test]
    fn session_records_do_not_overwrite_a_newer_compositor_decision() {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::beside(directory.path().join("settings.json"));
        let session = SessionSettings::open(paths).unwrap();
        refuses_stale_confirmation(&session);
        let reopened = SessionSettings::open(session.paths().clone()).unwrap();
        assert_eq!(reopened.load()["display_transaction"]["state"], "reverted");
    }

    #[test]
    fn host_transaction_accepts_durable_state_when_the_copy_needs_repair() {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::beside(directory.path().join("settings.json"));
        let session = SessionSettings::open(paths.clone()).unwrap();
        let before = session.load();
        let copy = std::fs::read(&paths.copy).unwrap();
        // Save the device record, then fail publication of the readable copy.
        tilo_files::native::atomic::test_support::with_failure_on_nth(
            tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
            2,
            || {
                session.compare_display_transaction(
                    before.get("display_transaction"),
                    json!({"id":7,"state":"requested"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&paths.copy).unwrap(), copy);
        let durable = crate::load_at(&paths.device);
        assert_eq!(durable["display_transaction"]["id"], 7);
        assert_eq!(durable["display_transaction"]["state"], "requested");
        assert_eq!(session.load(), durable);
        assert_eq!(crate::load_at(&paths.copy), durable);
    }
}
