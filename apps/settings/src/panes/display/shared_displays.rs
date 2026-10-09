//! Shared display facts and arrangement controls through the Settings resource door.

use super::{
    DisplayEffect, DisplayModel, Dropdown, El, LiveRow, LocalState, SettingsEffect, Toggle,
    caption, card, effect, rows, setting_row, setting_row_described, t, t_args,
};
use settings_store::person_displays::{Arrangement, Layout, ResolvedDisplay};
use ui::tilo_resource::{self, Effect};

#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct SharedDisplayPrefs {
    pub arrangement: Arrangement,
}

impl ui::storage::SettingsSchema for SharedDisplayPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("display");
}

#[ui::tilo_resource::resource(kind = "settings.display-arrangement", label = "Display arrangement")]
impl SharedDisplayPrefs {}

#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct AutomaticDisplayPrefs {
    pub automatic_display_input: bool,
}

impl ui::storage::SettingsSchema for AutomaticDisplayPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("continuity");
}

#[ui::tilo_resource::resource(
    kind = "settings.automatic-display",
    label = "Automatic display input"
)]
impl AutomaticDisplayPrefs {}

impl DisplayModel {
    pub(super) fn set_automatic_display_input(&mut self, on: bool) -> Result<String, String> {
        self.automatic_display_prefs
            .settings()
            .update(|prefs| prefs.automatic_display_input = on)
            .map_err(|error| {
                t_args(
                    "display-shared-save-failed",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.automatic_display_prefs.refresh();
        self.changed = true;
        Ok(t("display-shared-saved"))
    }

    fn bind_shared_row<I: 'static, C: ui::framework::ActionControl<I>>(
        &self,
        row: &ui::tilo_actions::Resource,
        control: C,
        effect: impl ui::tilo_resource::Binds<SettingsEffect, I>,
    ) -> C {
        match self.cx.row_binding(row, effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    fn shared_layout(&self) -> Layout {
        let origins = self
            .person_devices
            .iter()
            .filter_map(|device| {
                let local_x = device
                    .outputs
                    .iter()
                    .filter(|output| output.active())
                    .map(|output| output.x)
                    .min()?;
                let local_y = device
                    .outputs
                    .iter()
                    .filter(|output| output.active())
                    .map(|output| output.y)
                    .min()?;
                let screens = self
                    .device_displays
                    .iter()
                    .filter(|screen| screen.device == device.device)
                    .collect::<Vec<_>>();
                let x = screens
                    .iter()
                    .map(|screen| screen.x)
                    .min()?
                    .saturating_sub(local_x);
                let y = screens
                    .iter()
                    .map(|screen| screen.y)
                    .min()?
                    .saturating_sub(local_y);
                Some((device.device.clone(), (x, y)))
            })
            .collect();
        Layout::resolve_at_device_origins(
            &self.person_devices,
            &self.shared_display_prefs.get().arrangement,
            &origins,
        )
    }

    fn shared_revision(&self, device: &str, output: &str) -> u64 {
        self.shared_display_prefs
            .get()
            .arrangement
            .entry(device, output)
            .map_or(0, |entry| entry.rev)
    }

    fn shared_row(&self, display: &ResolvedDisplay) -> LiveRow {
        let revision = self.shared_revision(&display.device, &display.output);
        LiveRow::new(
            format!(
                "settings.shared-display:{}:{}",
                display.device, display.output
            ),
            "settings.shared-display",
            display.name.clone(),
        )
        .operation(
            SettingsEffect::from(DisplayEffect::IncludeSharedDisplay {
                device: display.device.clone(),
                output: display.output.clone(),
                revision,
                on: display.enabled,
            })
            .bound_asking(&["on"]),
        )
        .operation(
            SettingsEffect::from(DisplayEffect::OrderSharedDisplay {
                device: display.device.clone(),
                output: display.output.clone(),
                revision,
                order: u32::try_from(display.order + 1).unwrap_or(1),
            })
            .bound_asking(&["order"]),
        )
    }

    pub(super) fn capture_shared_move_revisions(
        &mut self,
        device: &str,
    ) -> Option<Vec<(String, u64)>> {
        self.shared_display_prefs.refresh();
        let outputs = &self
            .person_devices
            .iter()
            .find(|item| item.device == device)?
            .outputs;
        Some(
            outputs
                .iter()
                .map(|output| (output.id.clone(), self.shared_revision(device, &output.id)))
                .collect(),
        )
    }

    /// Only the target's newer confirmed observation can pin a shared move.
    /// A concurrent arrangement edit is preserved rather than overwritten.
    pub(super) fn confirm_shared_device_move(&mut self, device: &str) -> Result<(), String> {
        let Some(expected) = self.shared_move_revisions.as_ref() else {
            return Ok(());
        };
        self.shared_display_prefs.refresh();
        if expected
            .iter()
            .any(|(output, revision)| self.shared_revision(device, output) != *revision)
        {
            return Err(t("display-shared-move-stale"));
        }
        let layout = self.shared_layout();
        let group = layout
            .displays
            .iter()
            .filter(|display| display.device == device)
            .collect::<Vec<_>>();
        let screens = self
            .device_displays
            .iter()
            .filter(|screen| screen.device == device)
            .collect::<Vec<_>>();
        let Some(dx) = screens
            .iter()
            .map(|screen| screen.x)
            .min()
            .zip(group.iter().map(|display| display.rect.x).min())
            .map(|(target, current)| target.saturating_sub(current))
        else {
            return Err(t("display-shared-unavailable"));
        };
        let Some(dy) = screens
            .iter()
            .map(|screen| screen.y)
            .min()
            .zip(group.iter().map(|display| display.rect.y).min())
            .map(|(target, current)| target.saturating_sub(current))
        else {
            return Err(t("display-shared-unavailable"));
        };
        let next = layout.move_device(&self.shared_display_prefs.get().arrangement, device, dx, dy);
        self.shared_display_prefs
            .settings()
            .update(|prefs| prefs.arrangement = next)
            .map_err(|error| {
                t_args(
                    "display-shared-save-failed",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.shared_display_prefs.refresh();
        Ok(())
    }

    pub(super) fn shared_display_rows(&self) -> Vec<LiveRow> {
        let layout = self.shared_layout();
        layout
            .displays
            .iter()
            .chain(&layout.inactive)
            .map(|display| self.shared_row(display))
            .collect()
    }

    pub(super) fn shared_display_inspection(&self) -> serde_json::Value {
        serde_json::json!({"devices": self.person_devices, "layout": self.shared_layout(), "arrangement": self.shared_display_prefs.get().arrangement, "automatic_input": self.automatic_display_prefs.get().automatic_display_input})
    }

    pub(super) fn edit_shared_display(&mut self, effect: &DisplayEffect) -> Result<String, String> {
        self.shared_display_prefs.refresh();
        let (device, output, revision) = match effect {
            DisplayEffect::IncludeSharedDisplay {
                device,
                output,
                revision,
                ..
            }
            | DisplayEffect::OrderSharedDisplay {
                device,
                output,
                revision,
                ..
            } => (device, output, *revision),
            _ => return Err(t("display-shared-unavailable")),
        };
        if self.shared_revision(device, output) != revision {
            return Err(t("display-shared-stale"));
        }
        let layout = self.shared_layout();
        if !layout
            .displays
            .iter()
            .chain(&layout.inactive)
            .any(|display| display.is(device, output))
        {
            return Err(t("display-shared-unavailable"));
        }
        let previous = &self.shared_display_prefs.get().arrangement;
        let next = match effect {
            DisplayEffect::IncludeSharedDisplay { on, .. } => {
                layout.set_enabled(previous, device, output, *on)
            }
            DisplayEffect::OrderSharedDisplay { order, .. } => {
                let index = order
                    .checked_sub(1)
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < layout.displays.len())
                    .ok_or_else(|| t("display-shared-order-invalid"))?;
                if !layout
                    .displays
                    .iter()
                    .any(|display| display.is(device, output))
                {
                    return Err(t("display-shared-unavailable"));
                }
                layout.move_to_order(previous, device, output, index)
            }
            _ => return Err(t("display-shared-unavailable")),
        };
        self.shared_display_prefs
            .settings()
            .update(|prefs| prefs.arrangement = next)
            .map_err(|error| {
                t_args(
                    "display-shared-save-failed",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.shared_display_prefs.refresh();
        self.changed = true;
        Ok(t("display-shared-saved"))
    }

    pub(super) fn build_shared_displays_card(&self) -> Option<El> {
        let layout = self.shared_layout();
        if layout.displays.is_empty() && layout.inactive.is_empty() {
            return None;
        }
        let mut content = vec![caption(&t("display-shared-description"))];
        content.push(setting_row_described(
            &t("display-shared-auto-input"),
            &t("display-shared-auto-description"),
            Box::new(self.cx.bind(
                Toggle::from_state(&LocalState::new(
                    self.automatic_display_prefs.get().automatic_display_input,
                )),
                effect(DisplayEffect::SET_AUTOMATIC_DISPLAY_INPUT),
            )),
        ));
        for display in layout.displays.iter().chain(&layout.inactive) {
            let row = self.shared_row(display).into_resource();
            let device = display.device.clone();
            let output = display.output.clone();
            let revision = self.shared_revision(&device, &output);
            let toggle = self.bind_shared_row(
                &row,
                Toggle::from_state(&LocalState::new(display.enabled)),
                effect(DisplayEffect::INCLUDE_SHARED_DISPLAY)
                    .map(move |on: bool| (device.clone(), output.clone(), revision, on)),
            );
            let label = t_args(
                "display-device-screen",
                &[
                    ("device", display.device_label.clone().into()),
                    ("screen", display.name.clone().into()),
                    (
                        "status",
                        t(if display.online {
                            "display-device-online"
                        } else {
                            "display-device-offline"
                        })
                        .into(),
                    ),
                ],
            );
            content.push(setting_row_described(
                &label,
                &t("display-shared-include"),
                Box::new(toggle),
            ));
            if layout
                .displays
                .iter()
                .any(|active| active.is(&display.device, &display.output))
            {
                let options = (1..=layout.displays.len())
                    .map(|index| index.to_string())
                    .collect::<Vec<_>>();
                let device = display.device.clone();
                let output = display.output.clone();
                let choice = self.bind_shared_row(
                    &row,
                    Dropdown::from_state(
                        options,
                        &LocalState::new((display.order + 1).to_string()),
                    ),
                    effect(DisplayEffect::ORDER_SHARED_DISPLAY).map(move |choice: String| {
                        (
                            device.clone(),
                            output.clone(),
                            revision,
                            choice.parse::<u32>().unwrap_or(0),
                        )
                    }),
                );
                content.push(setting_row(&t("display-shared-order"), Box::new(choice)));
            }
        }
        Some(card(&t("display-shared-title"), rows(content)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::PaneCx;
    use crate::settings_app::SettingsApp;

    fn devices() -> Vec<settings_store::person_displays::PersonDevice> {
        vec![settings_store::person_displays::PersonDevice {
            device: "offline-peer".into(),
            label: "Desk computer".into(),
            local: false,
            online: false,
            outputs: vec![settings_store::person_displays::DeviceOutput {
                id: "edid:panel-1".into(),
                name: "Desk panel".into(),
                builtin: false,
                x: -960,
                y: 120,
                width: 960,
                height: 540,
                mode_width: 1920,
                mode_height: 1080,
                refresh_mhz: 60_000,
                scale_milli: 2000,
                connected: true,
                enabled: true,
            }],
        }]
    }

    #[test]
    fn shared_display_offline_edits_preserve_hardware_and_revision_boundaries() {
        let profile = tempfile::tempdir().unwrap();
        let mut model =
            DisplayModel::detached(PaneCx::in_file(profile.path().join("settings.json")));
        model.person_devices = devices();
        let before = model.person_devices.clone();
        model
            .apply(&DisplayEffect::IncludeSharedDisplay {
                device: "offline-peer".into(),
                output: "edid:panel-1".into(),
                revision: 0,
                on: false,
            })
            .unwrap();
        let revision = model.shared_revision("offline-peer", "edid:panel-1");
        assert!(revision > 0);
        assert!(!model.shared_layout().displays[0].enabled);
        assert_eq!(model.person_devices, before);
        assert!(model.pending_candidate.is_none() && model.pending_device_move.is_none());
        assert!(
            model
                .apply(&DisplayEffect::IncludeSharedDisplay {
                    device: "offline-peer".into(),
                    output: "edid:panel-1".into(),
                    revision: 0,
                    on: true
                })
                .is_err()
        );
        assert!(!model.shared_layout().displays[0].enabled);
        assert!(
            model
                .apply(&DisplayEffect::OrderSharedDisplay {
                    device: "offline-peer".into(),
                    output: "edid:panel-1".into(),
                    revision,
                    order: 0
                })
                .is_err()
        );
        model.save(|prefs| prefs.night_light = true).unwrap();
        model.shared_display_prefs.refresh();
        assert!(
            !model.shared_layout().displays[0].enabled,
            "comfort edits preserve the shared arrangement leaf"
        );
        assert!(model.prefs.get().night_light);
    }

    #[test]
    fn shared_display_operations_use_the_mounted_exact_resource_door() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let open = crate::categories::Category::Displays;
        app.effect_on(
            &crate::effects::pane_row_id(open),
            &crate::effects::open_pane(open),
        )
        .unwrap();
        app.surface::<SettingsApp, _>(|settings| {
            settings.displays().model().borrow_mut().person_devices = devices();
        });
        app.settle();
        let address = "settings.shared-display:offline-peer:edid:panel-1";
        let surface = app.agent_surface();
        let row = surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["resource"] == address && row["action"] == "include-shared-display")
            .expect("observed output exports the actual operation");
        assert_eq!(row["params"].as_array().unwrap().len(), 1);
        assert!(
            app.call_on(
                address,
                "include-shared-display",
                serde_json::json!({"on":false,"device":"another-peer"})
            )
            .is_err()
        );
        app.call_on(
            address,
            "include-shared-display",
            serde_json::json!({"on":false}),
        )
        .unwrap();
        app.settle();
        let inspection = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(inspection["devices"][0]["online"], false);
        assert_eq!(inspection["devices"][0]["outputs"][0]["mode_width"], 1920);
        assert_eq!(inspection["layout"]["displays"][0]["enabled"], false);
        assert!(app.harness().component_contract_violations().is_empty());
    }

    #[test]
    fn shared_display_move_pins_only_confirmed_positions_and_preserves_concurrent_edits() {
        for concurrent in [false, true] {
            let profile = tempfile::tempdir().unwrap();
            let mut model =
                DisplayModel::detached(PaneCx::in_file(profile.path().join("settings.json")));
            model.person_devices = devices();
            model.person_devices[0].online = true;
            model.device_displays = vec![settings_store::device_displays::DeviceDisplay {
                device: "offline-peer".into(),
                device_label: "Desk computer".into(),
                connector: "DP-1".into(),
                label: "Desk panel".into(),
                online: true,
                local: false,
                x: 1920,
                y: 120,
                width: 960,
                height: 540,
                scale_milli: 2000,
                revision: 1,
            }];
            model
                .apply(&DisplayEffect::IncludeSharedDisplay {
                    device: "offline-peer".into(),
                    output: "edid:panel-1".into(),
                    revision: 0,
                    on: false,
                })
                .unwrap();
            model
                .apply(&DisplayEffect::MoveDeviceDisplay {
                    display: model.device_displays[0].key(),
                    revision: 1,
                    x: 2000,
                    y: 200,
                })
                .unwrap();
            assert_eq!(
                model.shared_layout().displays[0].rect.x,
                1920,
                "a request never commits a placement"
            );
            let ui::widget::AppHostIntent::MoveDeviceDisplay { reply, .. } =
                model.take_host_intent().unwrap()
            else {
                panic!("typed device placement");
            };
            reply.upgrade().unwrap().borrow_mut().result = Some(Ok(()));
            model.poll_device_move();
            assert_eq!(
                model.shared_layout().displays[0].rect.x,
                1920,
                "delivery is not target confirmation"
            );
            if concurrent {
                let revision = model.shared_revision("offline-peer", "edid:panel-1");
                model
                    .apply(&DisplayEffect::IncludeSharedDisplay {
                        device: "offline-peer".into(),
                        output: "edid:panel-1".into(),
                        revision,
                        on: true,
                    })
                    .unwrap();
            }
            let screen = &mut model.device_displays[0];
            screen.x = 2000;
            screen.y = 200;
            screen.revision = 2;
            model.poll_device_move();
            let display = model.shared_layout().displays[0].clone();
            if concurrent {
                assert!(display.enabled);
                assert_eq!(display.rect.x, 1920);
                assert_eq!(model.notice, Some(t("display-shared-move-stale")));
            } else {
                assert_eq!((display.rect.x, display.rect.y), (2000, 200));
                assert!(!display.enabled);
                assert_eq!(model.notice, Some(t("display-device-move-confirmed")));
            }
            assert!(model.awaiting_device_move.is_none());
        }
    }

    #[test]
    fn shared_display_viewer_observes_without_edit_authority() {
        let mut app =
            ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(ui::Role::Viewer)
                .unwrap();
        let open = crate::categories::Category::Displays;
        app.effect_on(
            &crate::effects::pane_row_id(open),
            &crate::effects::open_pane(open),
        )
        .unwrap();
        app.surface::<SettingsApp, _>(|settings| {
            settings.displays().model().borrow_mut().person_devices = devices();
        });
        app.settle();
        let before = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(before["devices"][0]["outputs"][0]["refresh_mhz"], 60_000);
        assert!(
            app.call_on(
                "settings.shared-display:offline-peer:edid:panel-1",
                "include-shared-display",
                serde_json::json!({"on":false})
            )
            .is_err()
        );
        let after = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(before, after);
    }
    #[test]
    fn shared_display_automatic_input_is_a_separate_local_admin_choice() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let before = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(before["automatic_input"], false);
        app.call(
            "set-automatic-display-input",
            serde_json::json!({"on":true}),
        )
        .unwrap();
        app.settle();
        let after = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(after["automatic_input"], true);
        app.surface::<SettingsApp, _>(|settings| {
            let privacy = settings
                .displays()
                .model()
                .borrow()
                .cx
                .settings::<crate::continuity::ContinuityPrivacy>()
                .get();
            assert!(
                !privacy.enabled && !privacy.display_input,
                "automatic opt-in never enables the existing privacy switches"
            );
        });
        assert_eq!(
            settings_store::continuity::classify("tilo.continuity.automatic_display_input"),
            Some(settings_store::continuity::SettingScope::DeviceLocal)
        );
    }

    #[test]
    fn shared_display_automatic_input_cannot_be_enabled_by_a_viewer() {
        let mut app =
            ui::framework::testing::TestApp::<crate::SettingsShell>::with_role(ui::Role::Viewer)
                .unwrap();
        assert!(
            app.call(
                "set-automatic-display-input",
                serde_json::json!({"on":true})
            )
            .is_err()
        );
        let view = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(view["automatic_input"], false);
    }
}
