//! Display intent an app's Settings pane asks of the session that owns the displays.
//! The app supplies intent and the output set it observed; this session owns prior state,
//! transaction identity and persistence, and the compositor keeps KMS. A request that changes
//! anything is admitted only from the device this workspace's identity enrolled and only for a
//! person who may govern the workspace: a Settings worker on someone else's device changes nothing
//! here, however well it was admitted to the app's channel.

use std::sync::Arc;

use settings_store::SettingsSource;
use settings_store::display_control::{DisplayCommand, MAX_DISPLAY_CONTROL_BYTES};
use settings_store::hardware::DisplayHardwareSnapshot;
use tilo_action_broker::services::DisplayControlServices;

/// This device's certificate in a workspace, for the person it belongs to.
type Certificate =
    dyn Fn(&str, &str) -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync;

/// Whether a person may govern a workspace: its owner, or a role the workspace admits with
/// governance.
type Administrator = dyn Fn(&str, &str) -> Result<bool, String> + Send + Sync;

/// The displays' state as the compositor published it.
type Observed = dyn Fn() -> Option<DisplayHardwareSnapshot> + Send + Sync;

/// The session's display service over its own settings document, its identity store and the desk it
/// shows.
pub(crate) struct DisplayControl {
    source: Arc<dyn SettingsSource>,
    certificate: Arc<Certificate>,
    may_administer: Arc<Administrator>,
    observed: Arc<Observed>,
}

impl DisplayControl {
    /// The session's own answers: what the identity store says this device is, and what the desk
    /// says the person may do in the workspace.
    pub(crate) fn native(
        source: Arc<dyn SettingsSource>,
        desk: &Arc<crate::plane::DeskAuthority>,
    ) -> Self {
        let governing = Arc::clone(desk);
        Self {
            source,
            certificate: Arc::new(tilo_identity_authority::device_certificate),
            may_administer: Arc::new(move |workspace: &str, person: &str| {
                use tilo_broker_core::services::WorkspaceAuthority as _;
                let mut may = false;
                governing.with_workspace(workspace, &mut |held| {
                    use tilo_desktop::authority::WorkspaceAuthority as _;
                    may = held.is_some_and(|held| held.may_administer(&person.to_owned()));
                })?;
                Ok(may)
            }),
            observed: Arc::new(DisplayHardwareSnapshot::load),
        }
    }

    /// The answer to display intent from the private Settings worker `root` /`device` is on, in
    /// `workspace` .
    fn answer(
        &self,
        workspace: &str,
        root: [u8; 32],
        device: [u8; 32],
        command: &[u8],
    ) -> Result<Vec<u8>, String> {
        if command.len() > MAX_DISPLAY_CONTROL_BYTES {
            return Err("The display request exceeds its wire bound.".into());
        }
        let command: DisplayCommand =
            serde_json::from_slice(command).map_err(|_| "The display request is invalid.")?;
        if !command.mutates() {
            return self.execute(command);
        }
        let person = tilo_broker_core::principal_id_for_root(root);
        let certificate = (self.certificate)(workspace, &person)?;
        if certificate.user_public_key != root || certificate.device_public_key != device {
            return Err("The Settings worker is not on this accepted owner device.".into());
        }
        if !(self.may_administer)(workspace, &person)? {
            return Err("The current workspace policy does not permit display changes.".into());
        }
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
        context: &tilo_action_broker::services::DisplayControlContext,
        command: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.answer(
            context.workspace(),
            context.root(),
            context.device(),
            command,
        )
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

    const ROOT: [u8; 32] = [3; 32];
    const DEVICE: [u8; 32] = [7; 32];
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

    /// This device and the workspace's policy, as the host answers them.
    #[derive(Clone, Copy)]
    struct Device {
        /// Whether this device's certificate names the worker's keys.
        enrolled: bool,
        /// Whether the person may govern the workspace.
        governs: bool,
    }

    fn control(device: Device) -> DisplayControl {
        DisplayControl {
            source: document(),
            certificate: Arc::new(move |_, _| {
                Ok(tilo_user_identity::DeviceCert {
                    user_public_key: ROOT,
                    device_public_key: if device.enrolled { DEVICE } else { [9; 32] },
                    device_label: "This laptop".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::member(),
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                })
            }),
            may_administer: Arc::new(move |_, _| Ok(device.governs)),
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

    fn asked(
        control: &DisplayControl,
        root: [u8; 32],
        device: [u8; 32],
        command: &DisplayCommand,
    ) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(command).expect("a display request");
        control.answer("the-workspace", root, device, &bytes)
    }

    /// Reading the displays needs nothing beyond the connection the broker admitted: the reply is
    /// the compositor's state and the committed configuration, and the app changes nothing.
    #[test]
    fn a_settings_worker_reads_the_displays_it_stands_on() {
        let reply: Value = serde_json::from_slice(
            &asked(
                &control(Device {
                    enrolled: true,
                    governs: true,
                }),
                ROOT,
                DEVICE,
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

    /// A change is taken only from the device this workspace's identity enrolled, and only for a
    /// person who may govern the workspace.
    #[test]
    fn a_display_change_requires_this_devices_certificate_and_governing_policy() {
        let allowed = control(Device {
            enrolled: true,
            governs: true,
        });
        assert_eq!(
            asked(&allowed, ROOT, [9; 32], &larger_mode())
                .expect_err("another device changes nothing"),
            "The Settings worker is not on this accepted owner device."
        );
        assert_eq!(
            asked(&allowed, [4; 32], DEVICE, &larger_mode())
                .expect_err("another person changes nothing"),
            "The Settings worker is not on this accepted owner device."
        );

        let editor = control(Device {
            enrolled: true,
            governs: false,
        });
        assert_eq!(
            asked(&editor, ROOT, DEVICE, &larger_mode()).expect_err("an editor changes nothing"),
            "The current workspace policy does not permit display changes."
        );
        assert!(
            asked(&editor, ROOT, DEVICE, &DisplayCommand::Inspect).is_ok(),
            "reading the displays is open to every admitted Settings worker"
        );
    }

    /// Intent that cannot be a display transaction is refused as such, never run as something else.
    #[test]
    fn a_display_request_is_bounded_and_typed() {
        let displays = control(Device {
            enrolled: true,
            governs: true,
        });
        assert_eq!(
            displays
                .answer("the-workspace", ROOT, DEVICE, b"not json")
                .expect_err("invalid intent"),
            "The display request is invalid."
        );
        assert_eq!(
            displays
                .answer(
                    "the-workspace",
                    ROOT,
                    DEVICE,
                    &vec![b' '; MAX_DISPLAY_CONTROL_BYTES + 1],
                )
                .expect_err("oversized intent"),
            "The display request exceeds its wire bound."
        );
        let unavailable = DisplayControl {
            observed: Arc::new(|| None),
            ..control(Device {
                enrolled: true,
                governs: true,
            })
        };
        assert_eq!(
            asked(&unavailable, ROOT, DEVICE, &DisplayCommand::Inspect)
                .expect_err("no compositor state to answer from"),
            "The current compositor display state is unavailable."
        );
    }

    /// An admitted change becomes the transaction the app must keep or revert, kept in this
    /// session's settings document; reverting it is written there too.
    #[test]
    fn an_admitted_change_lands_in_the_sessions_settings_document() {
        let source = document();
        let displays = DisplayControl {
            source: Arc::clone(&source),
            ..control(Device {
                enrolled: true,
                governs: true,
            })
        };
        let reply: Value = serde_json::from_slice(
            &asked(&displays, ROOT, DEVICE, &larger_mode()).expect("the change is applied"),
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
            ROOT,
            DEVICE,
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
