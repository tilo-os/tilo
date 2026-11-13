//! Shared display facts and arrangement controls through the Settings resource door.

use super::{
    DisplayEffect, DisplayModel, Dropdown, LiveRow, LocalState, SettingsEffect, SettingsSection,
    Toggle, block, effect, note, rows, section, setting_row, setting_row_described, t, t_args,
};
use provider_settings_ui::bound::{SettingRows as _, record_item};
use settings_store::person_displays::{Arrangement, Layout, ResolvedDisplay};
use ui::framework::{Dispatcher, ViewCx};
use ui::tilo_resource::Effect;

pub use crate::prefs::{AutomaticDisplayPrefs, NightLightPrefs, SharedDisplayPrefs};
use crate::resource::Settings;

impl DisplayModel {
    fn arrangement(&self) -> Arrangement {
        self.storage
            .settings::<SharedDisplayPrefs>()
            .get()
            .arrangement
    }

    /// Store `next` as the person's arrangement, through the section's
    /// verb: only the displays whose entries it changes are written.
    fn arrange(&self, next: &Arrangement) -> Result<(), String> {
        self.sections
            .invoke::<SharedDisplayPrefs>(
                SharedDisplayPrefs::ARRANGE_DISPLAYS.with(next.to_value()),
            )
            .map_err(|error| t_args("display-shared-save-failed", &[("error", error.into())]))
    }

    /// The person's screens as their arrangement lays them out: the layout the pointer crosses
    /// between their devices by, so the edge shown here is the edge that crosses.
    pub(super) fn shared_layout(&self) -> Layout {
        Layout::resolve(&self.person_devices, &self.arrangement())
    }

    fn shared_revision(&self, device: &str, output: &str) -> u64 {
        self.arrangement()
            .entry(device, output)
            .map_or(0, |entry| entry.rev)
    }

    /// A shared display's row: it can always be kept in or out, and moved in
    /// the display order only while its device has it `active` (switched on).
    fn shared_row(&self, display: &ResolvedDisplay, active: bool) -> LiveRow {
        let revision = self.shared_revision(&display.device, &display.output);
        let row = LiveRow::new(
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
        );
        if !active {
            return row;
        }
        row.operation(
            SettingsEffect::from(DisplayEffect::OrderSharedDisplay {
                device: display.device.clone(),
                output: display.output.clone(),
                revision,
                order: u32::try_from(display.order + 1).unwrap_or(1),
            })
            .bound_asking(&["order"]),
        )
    }

    /// A screen's tile on the device arrangement: dragging it moves its device's screens.
    fn device_screen_row(&self, display: &ResolvedDisplay) -> LiveRow {
        LiveRow::new(
            format!("settings.device-display:{}", screen_key(display)),
            "settings.device-display",
            display.name.clone(),
        )
        .operation(
            SettingsEffect::from(DisplayEffect::MoveDeviceDisplay {
                display: screen_key(display),
                revision: self.shared_revision(&display.device, &display.output),
                x: display.rect.x,
                y: display.rect.y,
            })
            .bound_asking(&["x", "y"]),
        )
    }

    /// Move the device whose screen `display` names so that screen's corner stands at `at` in the
    /// person's layout; the device's other screens keep their places beside it.
    pub(super) fn move_shared_device(
        &mut self,
        display: &str,
        revision: u64,
        (x, y): (i32, i32),
    ) -> Result<String, String> {
        let layout = self.shared_layout();
        let moved = layout
            .displays
            .iter()
            .find(|screen| screen_key(screen) == display)
            .ok_or_else(|| t("display-shared-unavailable"))?;
        if self.shared_revision(&moved.device, &moved.output) != revision {
            return Err(t("display-shared-stale"));
        }
        let next = layout.move_device(
            &self.arrangement(),
            &moved.device,
            x.saturating_sub(moved.rect.x),
            y.saturating_sub(moved.rect.y),
        );
        self.arrange(&next)?;
        self.changed = true;
        Ok(t("display-shared-saved"))
    }

    /// The person's devices' screens as their layout places them, once a device of theirs other
    /// than this one has a screen in it: where two devices' screens meet is where the pointer
    /// crosses between them.
    pub(super) fn build_device_arrangement_card(
        &self,
        dispatcher: &Dispatcher<Settings>,
    ) -> Option<SettingsSection> {
        use crate::panes::display_arrangement::{
            ArrangedDisplay, DisplayArrangement, SharedDisplayState, placement::Placement,
        };
        let layout = self.shared_layout();
        if layout.displays.iter().all(|display| display.local) {
            return None;
        }
        let first_local = layout.displays.iter().position(|display| display.local);
        let displays = layout
            .displays
            .iter()
            .enumerate()
            .filter_map(|(index, display)| {
                Some(ArrangedDisplay {
                    connector: screen_key(display),
                    movement_group: Some(display.device.clone()),
                    shared: Some(SharedDisplayState {
                        device: display.device.clone(),
                        output: display.output.clone(),
                        online: display.online,
                        included: display.enabled,
                    }),
                    label: t_args(
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
                    ),
                    number: display.order.saturating_add(1),
                    placement: Placement::new(
                        display.rect.x,
                        display.rect.y,
                        i32::try_from(display.rect.width).ok()?,
                        i32::try_from(display.rect.height).ok()?,
                    ),
                    primary: first_local == Some(index),
                    movable: true,
                })
            })
            .collect();
        let moves = layout
            .displays
            .iter()
            .filter_map(|display| {
                let key = screen_key(display);
                let target = key.clone();
                let revision = self.shared_revision(&display.device, &display.output);
                crate::on_row::binding(
                    dispatcher,
                    &self.device_screen_row(display).into_resource(),
                    effect(DisplayEffect::MOVE_DEVICE_DISPLAY)
                        .map(move |(x, y): (i32, i32)| (target.clone(), revision, x, y)),
                )
                .map(|binding| (key, binding))
            })
            .collect();
        Some(section(
            &t("display-devices-title"),
            rows(vec![block(Box::new(DisplayArrangement::new(
                displays,
                moves,
                self.device_arrangement_drag.clone(),
            )))]),
        ))
    }

    pub(super) fn shared_display_rows(&self) -> Vec<LiveRow> {
        let layout = self.shared_layout();
        let mut rows: Vec<LiveRow> = shared_displays(&layout)
            .map(|(display, active)| self.shared_row(display, active))
            .collect();
        if layout.displays.iter().any(|display| !display.local) {
            rows.extend(
                layout
                    .displays
                    .iter()
                    .map(|display| self.device_screen_row(display)),
            );
        }
        rows
    }

    pub(super) fn shared_display_inspection(&self) -> serde_json::Value {
        serde_json::json!({
            "devices": self.person_devices,
            "layout": self.shared_layout(),
            "arrangement": self.arrangement(),
            "automatic_input": self
                .storage
                .settings::<AutomaticDisplayPrefs>()
                .get()
                .automatic_display_input,
        })
    }

    pub(super) fn edit_shared_display(&mut self, effect: &DisplayEffect) -> Result<String, String> {
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
        let previous = &self.arrangement();
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
        self.arrange(&next)?;
        self.changed = true;
        Ok(t("display-shared-saved"))
    }

    pub(super) fn build_shared_displays_card(
        &self,
        cx: &ViewCx<NightLightPrefs>,
        dispatcher: &Dispatcher<Settings>,
    ) -> Option<SettingsSection> {
        let layout = self.shared_layout();
        if layout.displays.is_empty() && layout.inactive.is_empty() {
            return None;
        }
        let mut content = vec![note(&t("display-shared-description"))];
        content.extend(record_item(
            cx,
            &cx.settings::<AutomaticDisplayPrefs>(),
            automatic_input,
        ));
        for (display, active) in shared_displays(&layout) {
            let row = self.shared_row(display, active).into_resource();
            let device = display.device.clone();
            let output = display.output.clone();
            let revision = self.shared_revision(&device, &output);
            let toggle = crate::on_row::bind(
                dispatcher,
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
                        t(if !display.online {
                            "display-device-offline"
                        } else if active {
                            "display-device-online"
                        } else {
                            "display-device-switched-off"
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
            if active {
                let options = (1..=layout.displays.len())
                    .map(|index| index.to_string())
                    .collect::<Vec<_>>();
                let device = display.device.clone();
                let output = display.output.clone();
                let choice = crate::on_row::bind(
                    dispatcher,
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
        Some(section(&t("display-shared-title"), rows(content)))
    }
}

fn automatic_input(cx: &ViewCx<AutomaticDisplayPrefs>) -> provider_settings_ui::widgets::Item {
    cx.setting(t("display-shared-auto-input"))
        .described(t("display-shared-auto-description"))
        .toggle_verb(
            |prefs: &AutomaticDisplayPrefs| prefs.automatic_display_input,
            AutomaticDisplayPrefs::SET_AUTOMATIC_DISPLAY_INPUT,
        )
}

/// One screen of the person's layout, as the device arrangement names its tile.
fn screen_key(display: &ResolvedDisplay) -> String {
    format!("{}/{}", display.device, display.output)
}

fn shared_displays(layout: &Layout) -> impl Iterator<Item = (&ResolvedDisplay, bool)> {
    let active = layout.displays.iter().map(|display| (display, true));
    active.chain(layout.inactive.iter().map(|display| (display, false)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::display::tests::{
        App, change_model, dispatcher, frame, on_displays, read_model,
    };

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

    fn offline_and_local() -> (
        settings_store::person_displays::PersonDevice,
        settings_store::person_displays::PersonDevice,
    ) {
        let offline = devices().remove(0);
        let mut local = offline.clone();
        local.device = "local-peer".into();
        local.label = "Local computer".into();
        local.local = true;
        local.online = true;
        (offline, local)
    }

    fn shared(device: &str) -> String {
        format!("settings.shared-display:{device}:edid:panel-1")
    }

    fn names(widget: &dyn ui::widget::Widget, found: &mut Vec<String>) {
        fn visit(node: &ui::a11y::AccessNode, found: &mut Vec<String>) {
            if let Some(name) = &node.name {
                found.push(name.clone());
            }
            for child in &node.children {
                visit(child, found);
            }
        }
        if let Some(node) = widget.accessibility() {
            visit(&node, found);
        }
        for child in widget.children() {
            names(child, found);
        }
    }

    fn tiles(app: &mut App) -> Vec<ui::a11y::AccessNode> {
        fn visit(widget: &dyn ui::widget::Widget, found: &mut Vec<ui::a11y::AccessNode>) {
            if widget.name() == "DisplayArrangement" {
                found.extend(widget.accessibility().unwrap().children);
            }
            for child in widget.children() {
                visit(child, found);
            }
        }
        read_model(app, |model| {
            let mut found = Vec::new();
            visit(
                provider_settings_ui::widgets::page(vec![
                    model.build_device_arrangement_card(&dispatcher()).unwrap(),
                ])
                .as_ref(),
                &mut found,
            );
            found
        })
    }

    fn canvas_names(app: &mut App) -> Vec<String> {
        read_model(app, |model| {
            let mut found = Vec::new();
            names(
                provider_settings_ui::widgets::page(vec![
                    model.build_device_arrangement_card(&dispatcher()).unwrap(),
                ])
                .as_ref(),
                &mut found,
            );
            found
        })
    }

    fn automatic_input(app: &App) -> bool {
        app.settings::<AutomaticDisplayPrefs>()
            .get()
            .automatic_display_input
    }

    #[test]
    fn shared_canvas_keeps_offline_tiles_and_marks_exclusion_after_the_resource_edit() {
        let mut app = on_displays();
        let (offline, local) = offline_and_local();
        let people = vec![offline.clone(), local];
        change_model(&mut app, move |model| model.person_devices = people);
        frame(&mut app);
        let before = tiles(&mut app);
        assert_eq!(before.len(), 2);
        let peer = before
            .iter()
            .find(|tile| tile.name.as_ref().unwrap().contains("Desk computer"))
            .unwrap();
        assert!(peer.description.as_ref().unwrap().contains("Offline"));
        assert!(
            !peer
                .description
                .as_ref()
                .unwrap()
                .contains("not in the layout")
        );
        let identity = peer.semantic_key.clone();
        let position = peer
            .description
            .as_ref()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        app.call_on(
            &shared(&offline.device),
            "include-shared-display",
            serde_json::json!({"on": false}),
        )
        .unwrap();
        frame(&mut app);
        let after = tiles(&mut app);
        assert_eq!(
            after.len(),
            2,
            "exclusion keeps the remembered offline tile"
        );
        let peer = after
            .iter()
            .find(|tile| tile.semantic_key == identity)
            .unwrap();
        let description = peer.description.as_ref().unwrap();
        assert!(description.contains("Offline") && description.contains("not in the layout"));
        assert_eq!(
            description.split(';').next().unwrap(),
            position,
            "inclusion does not rewrite native placement"
        );
        assert!(
            automatic_input(&app),
            "inclusion leaves automatic input as it was"
        );
    }

    #[test]
    fn settings_lays_out_unplaced_devices_as_the_crossing_pointer_does() {
        let mut app = on_displays();
        let (offline, local) = offline_and_local();
        // Neither device was placed: both stand where the person's arrangement puts devices not
        // dragged into place yet.
        let people = vec![offline.clone(), local.clone()];
        change_model(&mut app, move |model| model.person_devices = people);
        let (shown, arrangement) = read_model(&mut app, |model| {
            (model.shared_layout(), model.arrangement())
        });
        let crossed = settings_store::person_displays::PersonDisplays {
            devices: vec![offline, local],
            ..Default::default()
        }
        .layout(&arrangement);
        assert_eq!(
            shown, crossed,
            "Settings shows the layout the session's pointer crosses by"
        );
        let rect = |device: &str| {
            shown
                .displays
                .iter()
                .find(|display| display.device == device)
                .map(|display| display.rect)
                .unwrap()
        };
        assert_eq!(
            rect("offline-peer").x,
            rect("local-peer").right(),
            "the second device stands at the first one's right edge"
        );
    }

    #[test]
    fn shared_canvas_numbers_follow_resolved_device_output_identity_and_order() {
        let mut app = on_displays();
        let (offline, local) = offline_and_local();
        let people = vec![offline.clone(), local.clone()];
        change_model(&mut app, move |model| model.person_devices = people);
        frame(&mut app);
        app.call_on(
            &shared(&local.device),
            "order-shared-display",
            serde_json::json!({"order": 1}),
        )
        .unwrap();
        frame(&mut app);
        let before = canvas_names(&mut app);
        assert!(
            before.iter().any(|name| name.starts_with("Display 2:")
                && name.contains("Desk computer")
                && name.contains("Offline")),
            "Actual accessible names: {before:?}"
        );
        assert!(
            before
                .iter()
                .any(|name| name.starts_with("Display 1:") && name.contains("Local computer"))
        );
        app.call_on(
            &shared(&offline.device),
            "order-shared-display",
            serde_json::json!({"order": 1}),
        )
        .unwrap();
        frame(&mut app);
        let after = canvas_names(&mut app);
        assert!(
            after
                .iter()
                .any(|name| name.starts_with("Display 1:") && name.contains("Desk computer"))
        );
        assert!(
            after
                .iter()
                .any(|name| name.starts_with("Display 2:") && name.contains("Local computer"))
        );
        assert!(
            automatic_input(&app),
            "numbering leaves automatic input as it was"
        );
    }

    #[test]
    fn shared_display_offline_edits_preserve_hardware_and_revision_boundaries() {
        let mut app = on_displays();
        change_model(&mut app, |model| model.person_devices = devices());
        frame(&mut app);
        app.call_on(
            &shared("offline-peer"),
            "include-shared-display",
            serde_json::json!({"on": false}),
        )
        .unwrap();
        frame(&mut app);
        let (revision, enabled, observed, untouched) = read_model(&mut app, |model| {
            (
                model.shared_revision("offline-peer", "edid:panel-1"),
                model.shared_layout().displays[0].enabled,
                model.person_devices.clone(),
                model.pending_candidate.is_none(),
            )
        });
        assert!(revision > 0);
        assert!(!enabled);
        assert_eq!(observed, devices());
        assert!(untouched);
        let refused = app
            .with_widget::<crate::window::Inspected, _>(move |settings| {
                let mut model = crate::panes::display::area(settings)
                    .unwrap()
                    .model()
                    .borrow_mut();
                let stale = model.apply(&DisplayEffect::IncludeSharedDisplay {
                    device: "offline-peer".into(),
                    output: "edid:panel-1".into(),
                    revision: 0,
                    on: true,
                });
                let out_of_order = model.apply(&DisplayEffect::OrderSharedDisplay {
                    device: "offline-peer".into(),
                    output: "edid:panel-1".into(),
                    revision,
                    order: 0,
                });
                stale.is_err() && out_of_order.is_err()
            })
            .unwrap();
        assert!(refused, "a stale revision and order 0 are refused");
        frame(&mut app);
        assert!(!read_model(&mut app, |model| model
            .shared_layout()
            .displays[0]
            .enabled));
        crate::testing::edit_path(&mut app, "settings.night-light", &["night_light"], true)
            .unwrap();
        frame(&mut app);
        assert!(
            !read_model(&mut app, |model| model.shared_layout().displays[0].enabled),
            "comfort edits preserve the shared arrangement leaf"
        );
        assert!(app.settings::<NightLightPrefs>().get().night_light);
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
        app.with_widget::<crate::window::Inspected, _>(|settings| {
            crate::panes::display::area(settings)
                .unwrap()
                .model()
                .borrow_mut()
                .person_devices = devices();
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
    fn a_switched_off_shared_display_says_so_and_offers_no_ordering() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let open = crate::categories::Category::Displays;
        app.effect_on(
            &crate::effects::pane_row_id(open),
            &crate::effects::open_pane(open),
        )
        .unwrap();
        let mut observed = devices();
        observed[0].online = true;
        observed[0].outputs[0].enabled = false;
        change_model(&mut app, move |model| model.person_devices = observed);
        frame(&mut app);
        let mut captions = Vec::new();
        names(app.harness().root_widget(), &mut captions);
        assert!(
            captions.iter().any(|name| name.contains("Desk computer")
                && name.contains("Switched off on that device")),
            "{captions:?}"
        );
        let address = "settings.shared-display:offline-peer:edid:panel-1";
        let offered = |app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
                       action: &str| {
            app.agent_surface()
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == address && row["action"] == action)
        };
        assert!(offered(&mut app, "include-shared-display"));
        assert!(!offered(&mut app, "order-shared-display"));

        app.with_widget::<crate::window::Inspected, _>(|settings| {
            settings
                .areas()
                .find::<crate::panes::display::DisplayArea>()
                .unwrap()
                .model()
                .borrow_mut()
                .person_devices[0]
                .outputs[0]
                .enabled = true;
        });
        app.settle();
        assert!(offered(&mut app, "order-shared-display"));
        assert!(app.harness().component_contract_violations().is_empty());
    }

    #[test]
    fn dragging_a_screen_moves_its_device_in_the_person_layout() {
        let mut app = on_displays();
        let (offline, local) = offline_and_local();
        let people = vec![offline, local];
        change_model(&mut app, move |model| model.person_devices = people);
        frame(&mut app);
        // A choice made beside the drag is kept: the screen stays out of the layout.
        app.call_on(
            &shared("offline-peer"),
            "include-shared-display",
            serde_json::json!({"on": false}),
        )
        .unwrap();
        frame(&mut app);
        app.call_on(
            "settings.device-display:offline-peer/edid:panel-1",
            "move-device-display",
            serde_json::json!({"x": 2000, "y": 200}),
        )
        .unwrap();
        frame(&mut app);
        let layout = read_model(&mut app, DisplayModel::shared_layout);
        let screen = |device: &str| {
            layout
                .displays
                .iter()
                .find(|display| display.device == device)
                .cloned()
                .unwrap()
        };
        let moved = screen("offline-peer");
        assert_eq!((moved.rect.x, moved.rect.y), (2000, 200));
        assert!(
            !moved.enabled,
            "the drag keeps the screen out of the layout"
        );
        let here = screen("local-peer");
        assert_eq!(
            (here.rect.x, here.rect.y),
            (0, 0),
            "this device's screens stay where they were"
        );
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
        app.with_widget::<crate::window::Inspected, _>(|settings| {
            crate::panes::display::area(settings)
                .unwrap()
                .model()
                .borrow_mut()
                .person_devices = devices();
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
        let mut app = App::new().unwrap();
        let before = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(before["automatic_input"], true, "on until turned off");
        crate::testing::invoke_section(
            &mut app,
            "settings.automatic-display",
            "set-automatic-display-input",
            serde_json::json!({"on": false}),
        )
        .unwrap();
        app.settle();
        let after = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(after["automatic_input"], false);
        let privacy = app.settings::<crate::continuity::ContinuityPrivacy>().get();
        assert!(
            privacy.enabled && privacy.display_input,
            "automatic input is its own switch: turning it off leaves display and input on"
        );
        assert_eq!(
            settings_store::placement::holder("continuity"),
            settings_store::placement::Holder::Device
        );
    }
}
