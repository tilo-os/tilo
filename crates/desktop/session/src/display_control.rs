//! Display intent an app's Settings pane asks of the session that owns the displays.
//! The app supplies intent and the output set it observed; this session owns prior state,
//! transaction identity and persistence, and the compositor keeps KMS. Who may change them is
//! decided by the device daemon's broker before the request reaches this seat: only this
//! computer's own Settings changes its displays.

use std::sync::Arc;

use settings_store::SettingsSource;
use settings_store::display_control::{DisplayCommand, MAX_DISPLAY_CONTROL_BYTES};
use settings_store::hardware::DisplayHardwareSnapshot;
use tilo_workspace_service::apps::services::DisplayControlServices;

/// The displays' state as the compositor published it.
type Observed = dyn Fn() -> Option<DisplayHardwareSnapshot> + Send + Sync;

/// The session's display service over its own settings document and the shell it shows.
pub(crate) struct DisplayControl {
    source: Arc<dyn SettingsSource>,
    observed: Arc<Observed>,
}

impl DisplayControl {
    /// The session's own answer, over the displays the compositor publishes.
    pub(crate) fn native(source: Arc<dyn SettingsSource>) -> Self {
        Self {
            source,
            observed: Arc::new(DisplayHardwareSnapshot::load),
        }
    }

    /// The answer to display intent the daemon's broker admitted.
    fn answer(&self, command: &[u8]) -> Result<Vec<u8>, String> {
        if command.len() > MAX_DISPLAY_CONTROL_BYTES {
            return Err("The display request exceeds its wire bound.".into());
        }
        let command: DisplayCommand =
            serde_json::from_slice(command).map_err(|_| "The display request is invalid.")?;
        self.execute(command)
    }

    /// The command against the compositor's display state as it is now, with the reply kept to what
    /// the wire carries.
    fn execute(&self, command: DisplayCommand) -> Result<Vec<u8>, String> {
        let observed =
            (self.observed)().ok_or("The current compositor display state is unavailable.")?;
        let reply =
            settings_store::display_control::execute(self.source.as_ref(), observed, command)?;
        let reply =
            serde_json::to_vec(&reply).map_err(|_| "The display reply could not be encoded.")?;
        if reply.len() > MAX_DISPLAY_CONTROL_BYTES {
            return Err("The display reply exceeds its wire bound.".into());
        }
        Ok(reply)
    }
}

impl DisplayControlServices for DisplayControl {
    fn command(
        &self,
        _context: &tilo_workspace_service::apps::services::DisplayControlContext,
        command: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.answer(command)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Map, Value, json};
    use settings_store::HostDocument;
    use settings_store::hardware::{
        DisplayConfig, DisplayOutputSnapshot, DisplaySet, LiveOutputPlacement, OutputConfig,
        OutputDescriptor, OutputMode,
    };

    use super::*;

    const MODE: OutputMode = OutputMode {
        width: 1280,
        height: 720,
        refresh_mhz: 60_000,
    };

    /// A settings document this process holds: where a display change lands.
    fn document() -> Arc<dyn SettingsSource> {
        Arc::new(HostDocument::new(Map::new(), |_| Ok(())))
    }

    /// One display, and where the compositor puts it.
    fn snapshot() -> DisplayHardwareSnapshot {
        DisplayHardwareSnapshot {
            outputs: vec![DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: "DP-1".into(),
                    connector_label: "DP-1".into(),
                    modes: vec![
                        MODE,
                        OutputMode {
                            width: 1920,
                            height: 1080,
                            refresh_mhz: 60_000,
                        },
                    ],
                    hdr: false,
                    vrr: false,
                    color_profiles: Vec::new(),
                    monitor: None,
                },
                current_mode: MODE,
                scale: 1.0,
                builtin: false,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                unavailable_reason: None,
                color_profile_labels: BTreeMap::new(),
            }],
            layout: vec![LiveOutputPlacement {
                connector: "DP-1".into(),
                width: 1280,
                height: 720,
                ..LiveOutputPlacement::default()
            }],
            ..DisplayHardwareSnapshot::default()
        }
    }

    /// The session's display service over a fresh settings document and one display.
    fn control() -> DisplayControl {
        DisplayControl {
            source: document(),
            observed: Arc::new(|| Some(snapshot())),
        }
    }

    /// The change from the display it is on to a larger mode.
    fn larger_mode() -> DisplayCommand {
        let mut candidate = DisplayConfig::default();
        candidate.outputs.insert(
            "DP-1".into(),
            serde_json::from_value::<OutputConfig>(json!({
                "enabled": true,
                "primary": true,
                "x": 0,
                "y": 0,
                "scale": 1.0,
                "mode": {"width": 1920, "height": 1080, "refresh_mhz": 60000},
            }))
            .expect("an output configuration"),
        );
        DisplayCommand::Apply {
            observed_set: DisplaySet::of_descriptors(&[snapshot().outputs[0].descriptor.clone()]),
            candidate,
        }
    }

    fn asked(control: &DisplayControl, command: &DisplayCommand) -> Result<Vec<u8>, String> {
        control.answer(&serde_json::to_vec(command).expect("a display request"))
    }

    tilo_test_env::scenario! {
        fn a_settings_workers_display_request_is_read_bounded_and_kept;

        phase a_settings_worker_reads_the_displays_it_stands_on;
        // Reading the displays needs nothing beyond the connection the broker admitted: the reply is
        // the compositor's state and the committed configuration, and the app changes nothing.
        {
            let reply: Value = serde_json::from_slice(
                &asked(
                    &control(),
                    &DisplayCommand::Inspect,
                )
                .expect("the reply"),
            )
            .expect("a display reply");
            assert_eq!(
                reply["observed"]["outputs"][0]["descriptor"]["connector"],
                "DP-1"
            );
            assert_eq!(reply["committed"]["outputs"], json!({}));
            assert!(reply["transaction"].is_null(), "nothing was changed");
        }

        phase a_display_request_is_bounded_and_typed;
        // Intent that cannot be a display transaction is refused as such, never run as something else.
        {
            let displays = control();
            assert_eq!(
                displays
                    .answer(b"not json")
                    .expect_err("invalid intent"),
                "The display request is invalid."
            );
            assert_eq!(
                displays
                    .answer(
                        &vec![b' '; MAX_DISPLAY_CONTROL_BYTES + 1],
                    )
                    .expect_err("oversized intent"),
                "The display request exceeds its wire bound."
            );
            let unavailable = DisplayControl {
                observed: Arc::new(|| None),
                ..control()
            };
            assert_eq!(
                asked(&unavailable, &DisplayCommand::Inspect)
                    .expect_err("no compositor state to answer from"),
                "The current compositor display state is unavailable."
            );
        }

        phase an_admitted_change_lands_in_the_sessions_settings_document;
        // An admitted change becomes the transaction the app must keep or revert, kept in this
        // session's settings document; reverting it is written there too.
        {
            let source = document();
            let displays = DisplayControl {
                source: Arc::clone(&source),
                ..control()
            };
            let reply: Value = serde_json::from_slice(
                &asked(&displays, &larger_mode()).expect("the change is applied"),
            )
            .expect("a display reply");
            let id = reply["transaction"]["id"]
                .as_u64()
                .expect("a preview transaction");
            assert_eq!(
                source
                    .load()
                    .get("display_transaction")
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_u64),
                Some(id),
                "the session keeps the transaction it started"
            );

            asked(
                &displays,
                &DisplayCommand::Revert { transaction: id },
            )
            .expect("the change is reverted");
            assert_eq!(
                source
                    .load()
                    .get("display_transaction")
                    .and_then(|value| value.get("state"))
                    .and_then(Value::as_str),
                Some("revert_requested"),
                "and the person's answer is the transaction's next state"
            );
        }
    }
}
