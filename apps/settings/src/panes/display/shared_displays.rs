//! The person's other devices' screens, drawn on the arrangement canvas beside this device's own
//! where the person's layout places them, through the Settings resource door.

use super::{
    DisplayEffect, DisplayModel, LiveRow, SettingsEffect, SettingsSection, effect, t, t_args,
};
use crate::panes::display_arrangement::{
    ArrangedDisplay, SharedDisplayState, placement::Placement,
};
use provider_settings_ui::bound::SettingRows as _;
use provider_settings_ui::widgets::{rows, section, value_row};
use settings_store::person_displays::{Arrangement, Layout, Rect, ResolvedDisplay};
use ui::framework::{Dispatcher, ViewCx};
use ui::tilo_resource::Effect as _;

pub use crate::prefs::{AutomaticDisplayPrefs, SharedDisplayPrefs};
use crate::resource::Settings;

/// A tile's move on the canvas, by the tile it moves.
type Move = (String, ui::tilo_actions::ActionBinding<(i32, i32)>);

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

    /// The person's layout, once a device of theirs other than this one has a screen in it.
    pub(super) fn with_other_devices(&self) -> Option<Layout> {
        let layout = self.shared_layout();
        layout
            .displays
            .iter()
            .any(|display| !display.local)
            .then_some(layout)
    }

    /// A screen of the person's layout: dragging it moves its device's screens.
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

    fn device_move(
        &self,
        dispatcher: &Dispatcher<Settings>,
        display: &ResolvedDisplay,
    ) -> Option<ui::tilo_actions::ActionBinding<(i32, i32)>> {
        let target = screen_key(display);
        let revision = self.shared_revision(&display.device, &display.output);
        crate::on_row::binding(
            dispatcher,
            &self.device_screen_row(display).into_resource(),
            effect(DisplayEffect::MOVE_DEVICE_DISPLAY)
                .map(move |(x, y): (i32, i32)| (target.clone(), revision, x, y)),
        )
    }

    /// Move the device whose screen `display` names so that screen's corner stands at `at` in the
    /// person's layout; the device's other screens keep their places beside it. A device put
    /// apart from the others is out of the pointer's reach.
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

    /// Put the person's other devices' screens on the canvas beside this device's `displays`,
    /// where their layout places them, and answer where this device stands in that layout and
    /// how the shared tiles move. This device's own displays keep their native arrangement
    /// against each other; with only one of them, it moves as its device does.
    pub(super) fn join_other_screens(
        &self,
        dispatcher: &Dispatcher<Settings>,
        displays: &mut Vec<ArrangedDisplay>,
    ) -> ((i32, i32), Vec<Move>) {
        let Some(layout) = self.with_other_devices() else {
            return ((0, 0), Vec::new());
        };
        let own: Vec<&ResolvedDisplay> = layout
            .displays
            .iter()
            .filter(|display| display.local)
            .collect();
        let offset = own.first().map_or((0, 0), |display| {
            (
                display.rect.x.saturating_sub(display.local_rect.x),
                display.rect.y.saturating_sub(display.local_rect.y),
            )
        });
        let label = |display: &ResolvedDisplay, screen: &str| {
            t_args(
                "display-device-screen",
                &[
                    ("device", display.device_label.clone().into()),
                    ("screen", screen.to_owned().into()),
                ],
            )
        };
        let shared = |display: &ResolvedDisplay| SharedDisplayState {
            device: display.device.clone(),
            output: display.output.clone(),
            online: display.online,
            reachable: reachable(&layout, display),
        };
        let mut moves = Vec::new();
        let alone = displays.iter().filter(|display| display.movable).count() == 1;
        for tile in displays.iter_mut() {
            tile.placement = tile.placement.at(
                tile.placement.x.saturating_add(offset.0),
                tile.placement.y.saturating_add(offset.1),
            );
            let Some(here) = own.first() else {
                continue;
            };
            tile.label = label(here, &tile.label);
            if alone && own.len() == 1 && tile.movable {
                tile.movement_group = Some(here.device.clone());
                tile.shared = Some(shared(here));
                moves.extend(
                    self.device_move(dispatcher, here)
                        .map(|binding| (tile.connector.clone(), binding)),
                );
            }
        }
        let mut number = displays.len();
        for display in layout.displays.iter().filter(|display| !display.local) {
            let (Ok(width), Ok(height)) = (
                i32::try_from(display.rect.width),
                i32::try_from(display.rect.height),
            ) else {
                continue;
            };
            number += 1;
            let key = screen_key(display);
            moves.extend(
                self.device_move(dispatcher, display)
                    .map(|binding| (key.clone(), binding)),
            );
            displays.push(ArrangedDisplay {
                connector: key,
                movement_group: Some(display.device.clone()),
                shared: Some(shared(display)),
                label: label(display, &display.name),
                number,
                placement: Placement::new(display.rect.x, display.rect.y, width, height),
                primary: false,
                movable: true,
            });
        }
        (offset, moves)
    }

    /// The other device's screen the canvas has chosen, if one is.
    pub(super) fn selected_screen(&self) -> Option<ResolvedDisplay> {
        let chosen = self.selected.with(Clone::clone);
        self.with_other_devices()?
            .displays
            .into_iter()
            .find(|display| !display.local && screen_key(display) == chosen)
    }

    /// What there is to know of another device's screen: whose it is and whether the pointer
    /// reaches it.
    pub(super) fn screen_card(&self, screen: &ResolvedDisplay) -> SettingsSection {
        let reaches = self
            .with_other_devices()
            .is_some_and(|layout| reachable(&layout, screen));
        let status = t(match (screen.online, reaches) {
            (false, _) => "display-device-offline",
            (true, false) => "display-device-apart",
            (true, true) => "display-device-online",
        });
        section(
            &screen.name,
            rows(vec![
                value_row(&t("display-screen-device"), &screen.device_label),
                value_row(&t("display-screen-status"), &status),
            ]),
        )
    }

    pub(super) fn shared_display_rows(&self) -> Vec<LiveRow> {
        self.with_other_devices()
            .map(|layout| {
                layout
                    .displays
                    .iter()
                    .map(|display| self.device_screen_row(display))
                    .collect()
            })
            .unwrap_or_default()
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
}

/// Whether this device's other devices may use its screen, as the canvas of their screens shows.
pub(super) fn automatic_input(
    cx: &ViewCx<AutomaticDisplayPrefs>,
) -> provider_settings_ui::widgets::Item {
    cx.setting(t("display-shared-auto-input"))
        .described(t("display-shared-auto-description"))
        .toggle_verb(
            |prefs: &AutomaticDisplayPrefs| prefs.automatic_display_input,
            AutomaticDisplayPrefs::SET_AUTOMATIC_DISPLAY_INPUT,
        )
}

/// One screen of the person's layout, as the canvas names its tile.
fn screen_key(display: &ResolvedDisplay) -> String {
    format!("{}/{}", display.device, display.output)
}

/// Whether `display` meets a screen of another device along an edge: the pointer crosses only
/// there, so a screen placed apart is out of its reach.
fn reachable(layout: &Layout, display: &ResolvedDisplay) -> bool {
    layout
        .displays
        .iter()
        .filter(|other| other.device != display.device)
        .any(|other| meet(display.rect, other.rect))
}

fn meet(a: Rect, b: Rect) -> bool {
    let along = |a0: i32, a1: i32, b0: i32, b1: i32| a0 < b1 && b0 < a1;
    ((a.right() == b.x || b.right() == a.x) && along(a.y, a.bottom(), b.y, b.bottom()))
        || ((a.bottom() == b.y || b.bottom() == a.y) && along(a.x, a.right(), b.x, b.right()))
}

#[cfg(test)]
mod tests {
    use crate::panes::display::tests::{
        App, change_model, display_with_modes, frame, on_displays, read_model,
    };
    use settings_store::hardware::{OutputConfig, OutputMode};
    use settings_store::person_displays::{DeviceOutput, PersonDevice, PersonDisplays};

    const DESK: &str = "settings.device-display:desk/edid:desk";

    fn screen(id: &str, name: &str, builtin: bool) -> DeviceOutput {
        DeviceOutput {
            id: id.into(),
            name: name.into(),
            builtin,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            mode_width: 1920,
            mode_height: 1080,
            refresh_mhz: 60_000,
            scale_milli: 1000,
            connected: true,
            enabled: true,
        }
    }

    /// This laptop and the person's desk computer, which is offline when `online` is not.
    fn devices(online: bool) -> Vec<PersonDevice> {
        vec![
            PersonDevice {
                device: "laptop".into(),
                label: "Laptop".into(),
                local: true,
                online: true,
                outputs: vec![screen("edid:panel", "Built-in display", true)],
            },
            PersonDevice {
                device: "desk".into(),
                label: "Desk computer".into(),
                local: false,
                online,
                outputs: vec![screen("edid:desk", "Desk monitor", false)],
            },
        ]
    }

    /// Displays open on the laptop with its one panel, beside the desk computer's screen.
    fn beside_the_desk(online: bool) -> App {
        let mut app = on_displays();
        let people = devices(online);
        change_model(&mut app, move |model| {
            model.outputs = vec![display_with_modes(
                "eDP-1",
                "Built-in display",
                true,
                vec![OutputMode {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                }],
            )];
            model.output_config.outputs.insert(
                "eDP-1".into(),
                OutputConfig {
                    primary: true,
                    ..Default::default()
                },
            );
            model.person_devices = people;
            model.normalize_selection();
        });
        frame(&mut app);
        app
    }

    /// The canvas's tiles as a screen reader meets them, with where they are drawn.
    fn tiles(app: &mut App) -> Vec<ui::a11y::AccessNode> {
        fn visit(widget: &dyn ui::widget::Widget, found: &mut Vec<ui::a11y::AccessNode>) {
            if widget.name() == "DisplayArrangement" {
                found.extend(widget.accessibility().unwrap().children);
            }
            for child in widget.children() {
                visit(child, found);
            }
        }
        frame(app);
        let mut found = Vec::new();
        visit(app.harness().root_widget(), &mut found);
        found
    }

    fn tile(app: &mut App, name: &str) -> ui::a11y::AccessNode {
        tiles(app)
            .into_iter()
            .find(|tile| tile.name.as_ref().is_some_and(|label| label.contains(name)))
            .unwrap_or_else(|| panic!("no tile named {name}"))
    }

    fn centre(tile: &ui::a11y::AccessNode) -> (f64, f64) {
        let bounds = tile.bounds.unwrap();
        (
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        )
    }

    /// Where the session's pointer goes from the middle of the laptop's panel edge `toward` the
    /// screen there: the device it lands on.
    fn crossing(app: &mut App, toward: (f64, f64)) -> String {
        let (people, arrangement) = read_model(app, |model| {
            (model.person_devices.clone(), model.arrangement())
        });
        let layout = PersonDisplays {
            devices: people,
            ..Default::default()
        }
        .layout(&arrangement);
        let here = layout
            .displays
            .iter()
            .position(|display| display.local)
            .unwrap();
        let rect = layout.displays[here].rect;
        let middle = (
            f64::from(rect.x) + f64::from(rect.width) / 2.0,
            f64::from(rect.y) + f64::from(rect.height) / 2.0,
        );
        let edge = (
            middle.0 + toward.0 * (f64::from(rect.width) / 2.0 - 0.5),
            middle.1 + toward.1 * (f64::from(rect.height) / 2.0 - 0.5),
        );
        let step = layout
            .step_pointer(here, edge, (toward.0 * 4.0, toward.1 * 4.0), |_| true)
            .unwrap();
        layout.displays[step.display].device.clone()
    }

    #[test]
    fn one_canvas_draws_this_device_s_displays_and_the_other_devices_screens() {
        let mut app = beside_the_desk(false);
        let shown = tiles(&mut app);
        assert_eq!(shown.len(), 2, "{shown:?}");
        let here = tile(&mut app, "Built-in display on Laptop");
        let desk = tile(&mut app, "Desk monitor on Desk computer");
        assert!(desk.description.as_ref().unwrap().contains("Offline"));
        assert!(
            !here.description.as_ref().unwrap().contains("Offline"),
            "{here:?}"
        );
        let page = crate::panes::display::tests::page_ids(&mut app);
        assert!(
            !page.iter().any(|id| id.contains("shared-display")),
            "no list of displays to include or number: {page:?}"
        );
    }

    #[test]
    fn dragging_a_screen_on_the_canvas_moves_where_the_pointer_crosses() {
        let mut app = beside_the_desk(true);
        // Not placed yet, the desk computer stands left of the laptop: its id sorts first.
        assert_eq!(crossing(&mut app, (-1.0, 0.0)), "desk");
        assert_eq!(crossing(&mut app, (0.0, 1.0)), "laptop");
        let here = tile(&mut app, "on Laptop").bounds.unwrap();
        let desk = tile(&mut app, "on Desk computer");
        let (x, y) = centre(&desk);
        app.harness().send(&ui::synthetic_input::cursor_moved(x, y));
        app.harness().send(&ui::synthetic_input::mouse_press());
        let below = (
            f64::midpoint(here.x0, here.x1),
            here.y1 + (here.y1 - here.y0) / 2.0,
        );
        for step in 1..=10 {
            let at = f64::from(step) / 10.0;
            app.harness().send(&ui::synthetic_input::cursor_moved(
                x + (below.0 - x) * at,
                y + (below.1 - y) * at,
            ));
        }
        app.harness().send(&ui::synthetic_input::mouse_release());
        frame(&mut app);
        assert_eq!(
            crossing(&mut app, (0.0, 1.0)),
            "desk",
            "the pointer goes down onto the desk computer's screen"
        );
        assert_eq!(
            crossing(&mut app, (-1.0, 0.0)),
            "laptop",
            "and no longer leaves the panel's left edge"
        );
        assert!(
            !tile(&mut app, "on Desk computer")
                .description
                .unwrap()
                .contains("Apart")
        );
    }

    #[test]
    fn a_screen_placed_apart_is_out_of_the_pointer_s_reach() {
        let mut app = beside_the_desk(true);
        app.call_on(
            DESK,
            "move-device-display",
            serde_json::json!({"x": 9000, "y": 4000}),
        )
        .unwrap();
        frame(&mut app);
        for toward in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            assert_eq!(crossing(&mut app, toward), "laptop");
        }
        let desk = tile(&mut app, "on Desk computer");
        assert!(desk.description.unwrap().contains("Apart"));
    }

    #[test]
    fn choosing_another_device_s_screen_shows_whose_it_is() {
        let mut app = beside_the_desk(true);
        let desk = tile(&mut app, "on Desk computer");
        let (x, y) = centre(&desk);
        app.harness().send(&ui::synthetic_input::cursor_moved(x, y));
        app.harness().send(&ui::synthetic_input::mouse_press());
        app.harness().send(&ui::synthetic_input::mouse_release());
        frame(&mut app);
        let chosen = read_model(&mut app, |model| {
            (
                model.selected_screen().map(|screen| screen.device),
                model.selected_output().is_none(),
            )
        });
        assert_eq!(chosen, (Some("desk".to_owned()), true));
        let mut words = Vec::new();
        crate::panes::display::tests::page_words(app.harness().root_widget(), &mut words);
        assert!(
            words.iter().any(|word| word.contains("Desk computer")),
            "{words:?}"
        );
        assert!(
            !words.iter().any(|word| word.contains("Resolution")),
            "this device's display settings make way: {words:?}"
        );
    }

    #[test]
    fn settings_lays_out_unplaced_devices_as_the_crossing_pointer_does() {
        let mut app = beside_the_desk(true);
        let (shown, arrangement) = read_model(&mut app, |model| {
            (model.shared_layout(), model.arrangement())
        });
        let crossed = PersonDisplays {
            devices: devices(true),
            ..Default::default()
        }
        .layout(&arrangement);
        assert_eq!(
            shown, crossed,
            "Settings shows the layout the session's pointer crosses by"
        );
    }

    #[test]
    fn shared_display_operations_use_the_mounted_exact_resource_door() {
        let mut app = beside_the_desk(false);
        let surface = app.agent_surface();
        let row = surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["resource"] == DESK && row["action"] == "move-device-display")
            .expect("another device's screen exports its move");
        assert_eq!(row["params"].as_array().unwrap().len(), 2);
        assert!(
            app.call_on(
                DESK,
                "move-device-display",
                serde_json::json!({"x": 0, "y": 1080, "display": "laptop/edid:panel"})
            )
            .is_err()
        );
        app.call_on(
            DESK,
            "move-device-display",
            serde_json::json!({"x": 0, "y": 1080}),
        )
        .unwrap();
        app.settle();
        let inspection = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(inspection["devices"][1]["online"], false);
        let desk = inspection["layout"]["displays"]
            .as_array()
            .unwrap()
            .iter()
            .find(|display| display["device"] == "desk")
            .unwrap();
        assert_eq!(desk["rect"]["y"], 1080);
        assert!(app.harness().component_contract_violations().is_empty());
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
        change_model(&mut app, |model| model.person_devices = devices(false));
        app.settle();
        let before = app
            .call("inspect-shared-displays", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(before["devices"][1]["outputs"][0]["refresh_mhz"], 60_000);
        assert!(
            app.call_on(
                DESK,
                "move-device-display",
                serde_json::json!({"x": 0, "y": 1080})
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
