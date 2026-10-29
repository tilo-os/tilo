//! The Displays page as view code over the person's night light, this
//! device's display preferences and the displays' arrangement, and what the
//! area reads of the displays: the change being tried, the arrangement
//! canvas, the selected display's settings, the person's other devices'
//! displays and the trusted layout tools.

use std::rc::Rc;

use provider_settings_ui::bound::{SettingRows as _, record_item};
use ui::LocalState;
use ui::framework::{ActionControl, Actions, Dispatcher, ViewCx};
use ui::widget::{Button, Element, Toggle};

use super::{
    DISPLAY_SCALES, DisplayEffect, DisplayModel, DisplayOutput, DisplayTransactionState, Dropdown,
    Item, NIGHT_LIGHT_TEMPERATURES, OutputConfig, OutputMode, SettingsSection,
    adaptive_sync_unavailable_key, area, automatic_brightness, hdr_unavailable_key,
    mode_for_refresh, mode_for_resolution, note, output_title, output_unavailable_key,
    refresh_choices, refresh_label, resolution_choices, resolution_label, rows, scale_label,
    sdr_brightness_fraction, section, setting_row, setting_row_described, temperature_label,
    transform_label, transform_options, unix_now_ms,
};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::prefs::{DisplayPrefs, NightLightPrefs, PendingDisplayChange, SharedDisplayPrefs};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{block, error_row, value_row};

/// The Displays page. The page is built again when the arrangement of the
/// person's displays changes (the shared cards list and number them by
/// it), and when the displays report something new, a change of them is
/// staged or settles, or another display is chosen on the canvas; the
/// displays are followed while the page is open.
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let following = held.model().borrow_mut().follow();
    let shown = held.shown.clone();
    let (actions, dispatcher) = (cx.actions(), cx.dispatcher());
    let app = Rc::clone(app);
    let shared = cx.settings::<SharedDisplayPrefs>();
    cx.record(&shared, move |cx| {
        let arrangement = cx.value(|prefs: &SharedDisplayPrefs| prefs.arrangement.clone());
        cx.switch(&arrangement, move |cx, _| {
            let (app, actions, dispatcher, shown, following) = (
                Rc::clone(&app),
                actions.clone(),
                dispatcher.clone(),
                shown.clone(),
                following.clone(),
            );
            // The change being tried, as the session wrote it: the model
            // takes it in before the page shows it.
            let pending = cx.settings::<PendingDisplayChange>();
            cx.record(&pending, move |cx| {
                let change = cx.value(PendingDisplayChange::transaction);
                cx.switch(&change, move |cx, change| {
                    let (app, actions, dispatcher, shown, following) = (
                        Rc::clone(&app),
                        actions.clone(),
                        dispatcher.clone(),
                        shown.clone(),
                        following.clone(),
                    );
                    if let Some(area) = area(&app) {
                        area.model().borrow_mut().saw_pending_change(change.clone());
                    }
                    let night_light = cx.settings::<NightLightPrefs>();
                    cx.record(&night_light, move |cx| {
                        // The part keeps the displays followed while it is
                        // shown.
                        cx.switch(&shown, move |cx, _| {
                            let _followed = &following;
                            match area(&app) {
                                Some(area) => {
                                    page(cx, &actions, &dispatcher, &area.model().borrow())
                                }
                                None => provider_settings_ui::widgets::page(Vec::new()),
                            }
                        })
                    })
                    .unwrap_or_else(|| provider_settings_ui::widgets::page(Vec::new()))
                })
            })
            .unwrap_or_else(|| provider_settings_ui::widgets::page(Vec::new()))
        })
    })
}

/// The page over what `model` read of the displays.
fn page(
    cx: &ViewCx<NightLightPrefs>,
    actions: &Actions<Settings>,
    dispatcher: &Dispatcher<Settings>,
    model: &DisplayModel,
) -> Element {
    let connected = model.connected_outputs();
    // What is being tried, or waits to be, leads: its countdown must be
    // seen. Then the arrangement, which is the selection, and below it the
    // one selected display's settings: never a list of every display, and
    // no brightness outside the selected display's. Then the comforts that
    // are not one display's, and the displays of other devices.
    let mut cards: Vec<SettingsSection> = model.safety_card(cx, actions).into_iter().collect();
    cards.push(model.arrangement_card(actions, dispatcher, &connected));
    if let Some(notice) = &model.notice {
        cards.push(section(&t("display-card-title"), rows(vec![note(notice)])));
    }
    if let Some(output) = model.selected_output() {
        cards.push(model.output_card(cx, actions, dispatcher, output, &connected));
    }
    cards.push(night_light_card(cx));
    if let Some(shared) = model.build_shared_displays_card(cx, dispatcher) {
        cards.push(shared);
    }
    if let Some(devices) = model.build_device_arrangement_card(dispatcher) {
        cards.push(devices);
    }
    if let Some(tools) = model.trusted_tools_card(cx, dispatcher) {
        cards.push(tools);
    }
    provider_settings_ui::widgets::page(cards)
}

/// `control` (a button), bound to `effect`.
fn invoking<C: ActionControl<()>>(
    actions: &Actions<Settings>,
    control: C,
    effect: impl Into<SettingsEffect>,
) -> C {
    actions.bind_effect_invocation(control, &effect.into())
}

/// Night light: a comfort of every display, so it is not one display's
/// setting and carries no brightness. Its switch heads the group; when it
/// comes on and how warm it makes the screen, as pictures of the screen at
/// each warmth.
fn night_light_card(cx: &ViewCx<NightLightPrefs>) -> SettingsSection {
    let paths = NightLightPrefs::paths();
    let on = cx.value_at(&paths.night_light);
    let switch = Toggle::from_state(&on)
        .with_id("display.night_light")
        .with_accessible_name(&t("display-night-light"));
    let switch = match cx.actions().field_binding(paths.night_light) {
        Some(binding) => switch.bind_action(binding),
        None => switch.unavailable(),
    };
    provider_settings_ui::widgets::section_with(
        &t("display-night-light"),
        Box::new(switch),
        rows(vec![
            cx.setting(t("display-night-light-schedule"))
                .described(t("display-night-light-schedule-desc"))
                .id("display.night_light_schedule")
                .toggle(paths.night_light_schedule),
            cx.setting(t("display-night-light-warmth"))
                .id("display.night_light_temperature")
                .pictures(
                    paths.night_light_temperature,
                    NIGHT_LIGHT_TEMPERATURES.iter().map(|(_, kelvin)| *kelvin),
                    |kelvin: &u32| temperature_label(*kelvin),
                    |kelvin: &u32| ui::widget::Picture::Warmth(*kelvin),
                ),
            note(&t("display-night-light-caption")),
        ]),
    )
}

impl DisplayModel {
    /// The arrangement card: the canvas of the connected displays, which is
    /// also where a display is chosen (by pointer, touch, keyboard or screen
    /// reader). Identify sits in the card's header. See
    /// [`Self::connected_outputs`] for why disconnected ports aren't
    /// enumerated.
    fn arrangement_card(
        &self,
        actions: &Actions<Settings>,
        dispatcher: &Dispatcher<Settings>,
        connected: &[&DisplayOutput],
    ) -> SettingsSection {
        if connected.is_empty() {
            return section(
                &t("display-card-title"),
                rows(vec![note(&t(if self.outputs_read {
                    "display-none"
                } else {
                    "display-unreadable"
                }))]),
            );
        }
        let disconnected = self.outputs.len() - connected.len();
        let canvas = self.arrangement_canvas(dispatcher, connected);
        let identify = invoking(
            actions,
            Button::new("settings.display.identify", &t("display-identify"))
                .role(ui::widget::ButtonRole::Secondary),
            DisplayEffect::IdentifyDisplays,
        );
        // One honest line about the idle ports rather than a row each, under
        // the heading, saying what the canvas is for.
        let caption = if connected.len() > 1 {
            t("display-caption-switch")
        } else if disconnected == 0 {
            t("display-caption-second")
        } else {
            t("display-caption-port")
        };
        provider_settings_ui::widgets::section_with(
            &t("display-card-title"),
            Box::new(identify),
            rows(vec![note(&caption), block(Box::new(canvas))]),
        )
    }

    /// The selected display's brightness, where it can be controlled: the
    /// built-in panel's backlight (with automatic brightness beside it), or
    /// a monitor's over DDC/CI or dimmed by Tilo. Nothing for a display
    /// whose brightness cannot be set.
    fn output_brightness_rows(
        &self,
        cx: &ViewCx<NightLightPrefs>,
        actions: &Actions<Settings>,
        dispatcher: &Dispatcher<Settings>,
        output: &DisplayOutput,
    ) -> Vec<Item> {
        if output.builtin {
            let mut brightness_rows = vec![match &self.brightness_error {
                Some(error) => error_row(
                    &t("display-brightness"),
                    &t_args(
                        "display-brightness-error",
                        &[("error", error.as_str().into())],
                    ),
                ),
                None => cx
                    .setting(t("display-brightness"))
                    .stacked()
                    .id("display.brightness")
                    .slider_effect(
                        &LocalState::new(f64::from(self.brightness)),
                        actions,
                        effect(DisplayEffect::SET_BRIGHTNESS),
                    ),
            }];
            if self.ambient_sensor_available {
                brightness_rows.extend(record_item(
                    cx,
                    &cx.settings::<DisplayPrefs>(),
                    automatic_brightness,
                ));
            } else {
                brightness_rows.push(value_row(
                    &t("display-auto-brightness"),
                    &t("display-no-ambient"),
                ));
            }
            return brightness_rows;
        }
        let Some(level) = self.output_brightness.get(&output.connector) else {
            return Vec::new();
        };
        let mut out_rows = vec![provider_settings_ui::widgets::choice_row_stacked(
            &t("display-brightness"),
            Box::new(self.slider(
                dispatcher,
                &output.connector,
                "brightness",
                f64::from(*level),
                effect(DisplayEffect::SET_MONITOR_BRIGHTNESS),
            )),
        )];
        if self.dimmed_by_tilo.contains(&output.connector) {
            out_rows.push(note(&t("display-brightness-dimmed-by-tilo")));
        }
        out_rows
    }

    /// The selected display's settings: brightness where it can be set,
    /// resolution, refresh rate, scale, rotation, the main display,
    /// mirroring and its place, then HDR, variable refresh and colour where
    /// the display offers them, and turning it off.
    fn output_card(
        &self,
        cx: &ViewCx<NightLightPrefs>,
        actions: &Actions<Settings>,
        dispatcher: &Dispatcher<Settings>,
        output: &DisplayOutput,
        connected: &[&DisplayOutput],
    ) -> SettingsSection {
        let index = connected
            .iter()
            .position(|candidate| candidate.connector == output.connector)
            .unwrap_or_default();
        let config = self
            .staged()
            .outputs
            .get(&output.connector)
            .cloned()
            .unwrap_or_else(|| OutputConfig {
                primary: index == 0,
                x: i32::try_from(index)
                    .unwrap_or(i32::MAX)
                    .saturating_mul(1920),
                ..output.unconfigured()
            });
        let mut output_rows = self.output_brightness_rows(cx, actions, dispatcher, output);
        output_rows.extend(self.output_mode_rows(dispatcher, output, &config));
        output_rows.extend(self.output_primary_rows(dispatcher, output, connected, &config));
        output_rows.extend(self.output_capability_rows(dispatcher, output, &config));
        output_rows.push(setting_row(
            &t("display-use"),
            Box::new(self.toggle(
                dispatcher,
                &output.connector,
                "enabled",
                config.enabled,
                effect(DisplayEffect::USE_DISPLAY),
            )),
        ));
        section(&output_title(output), rows(output_rows))
    }

    /// Scale, rotation, the main display and mirroring. Where the display
    /// sits is the arrangement canvas's alone (by hand or arrow keys).
    fn output_primary_rows(
        &self,
        dispatcher: &Dispatcher<Settings>,
        output: &DisplayOutput,
        connected: &[&DisplayOutput],
        config: &OutputConfig,
    ) -> Vec<Item> {
        let scale_choices: Vec<String> = DISPLAY_SCALES
            .iter()
            .map(|(label, _)| (*label).into())
            .collect();
        let mirror_choices = self.relative_choices(output, true);
        let mirror_placeholder = config
            .mirror_of
            .as_ref()
            .and_then(|target| {
                connected
                    .iter()
                    .find(|candidate| candidate.connector == *target)
            })
            .map_or_else(
                || "Extend workspace".into(),
                |target| format!("Mirror {}", target.label),
            );
        vec![
            setting_row(
                &t("display-scale"),
                Box::new(self.choice(
                    dispatcher,
                    &output.connector,
                    "scale",
                    scale_choices,
                    scale_label(config.scale),
                    effect(DisplayEffect::SET_OUTPUT_SCALE),
                )),
            ),
            setting_row(
                &t("display-rotation"),
                Box::new(self.choice(
                    dispatcher,
                    &output.connector,
                    "transform",
                    transform_options(),
                    &transform_label(config.transform),
                    effect(DisplayEffect::ROTATE_DISPLAY),
                )),
            ),
            setting_row(
                &t("display-primary"),
                Box::new({
                    let display = output.connector.clone();
                    self.bind_output(
                        dispatcher,
                        &output.connector,
                        Toggle::from_state(&LocalState::new(config.primary))
                            .with_id(&Self::id(&output.connector, "primary")),
                        effect(DisplayEffect::SET_PRIMARY_DISPLAY)
                            .map(move |_: bool| display.clone()),
                    )
                }),
            ),
            setting_row(
                &t("display-mirroring"),
                Box::new(self.choice(
                    dispatcher,
                    &output.connector,
                    "mirror",
                    mirror_choices,
                    &mirror_placeholder,
                    effect(DisplayEffect::MIRROR_DISPLAY),
                )),
            ),
        ]
    }

    /// Why the display is dark, if it is; then its resolution and, for that
    /// resolution, its refresh rate. Both choose one of the display's modes.
    fn output_mode_rows(
        &self,
        dispatcher: &Dispatcher<Settings>,
        output: &DisplayOutput,
        config: &OutputConfig,
    ) -> Vec<Item> {
        let mut out_rows = Vec::new();
        if let Some(reason) = output.unavailable_reason {
            out_rows.push(value_row(
                &t("display-status"),
                &t(output_unavailable_key(reason)),
            ));
        }
        let Some(current) = config
            .mode
            .or(output.current_mode)
            .or_else(|| output.modes.first().copied())
        else {
            out_rows.push(value_row(
                &t("display-resolution"),
                &t("display-modes-unavailable"),
            ));
            return out_rows;
        };
        if output.modes.is_empty() {
            out_rows.push(value_row(
                &t("display-resolution"),
                &resolution_label(current),
            ));
            out_rows.push(value_row(
                &t("display-refresh-rate"),
                &refresh_label(current),
            ));
            return out_rows;
        }
        let modes = output.modes.clone();
        let resolutions = resolution_choices(&modes);
        let rates = refresh_choices(&modes, current);
        let target = output.connector.clone();
        let for_resolution = modes.clone();
        out_rows.push(setting_row(
            &t("display-resolution"),
            Box::new(
                self.bind_output(
                    dispatcher,
                    &output.connector,
                    Dropdown::from_state(resolutions, &LocalState::new(resolution_label(current)))
                        .with_id(&Self::id(&output.connector, "mode"))
                        .with_placeholder(&resolution_label(current)),
                    effect(DisplayEffect::SET_DISPLAY_MODE).map(move |resolution: String| {
                        (
                            target.clone(),
                            mode_for_resolution(&for_resolution, current, &resolution)
                                .map_or(resolution, OutputMode::label),
                        )
                    }),
                ),
            ),
        ));
        let target = output.connector.clone();
        out_rows.push(setting_row(
            &t("display-refresh-rate"),
            Box::new(
                self.bind_output(
                    dispatcher,
                    &output.connector,
                    Dropdown::from_state(rates, &LocalState::new(refresh_label(current)))
                        .with_id(&Self::id(&output.connector, "refresh"))
                        .with_placeholder(&refresh_label(current)),
                    effect(DisplayEffect::SET_DISPLAY_MODE).map(move |rate: String| {
                        (
                            target.clone(),
                            mode_for_refresh(&modes, current, &rate)
                                .map_or(rate, OutputMode::label),
                        )
                    }),
                ),
            ),
        ));
        out_rows
    }

    /// HDR / variable-refresh-rate / colour-profile rows.
    fn output_capability_rows(
        &self,
        dispatcher: &Dispatcher<Settings>,
        output: &DisplayOutput,
        config: &OutputConfig,
    ) -> Vec<Item> {
        let mut out_rows = Vec::new();

        if output.hdr {
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-hdr"),
                &crate::i18n::t("display-hdr-description"),
                Box::new(self.toggle(
                    dispatcher,
                    &output.connector,
                    "hdr",
                    config.hdr,
                    effect(DisplayEffect::SET_HDR),
                )),
            ));
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-hdr-sdr-brightness"),
                &crate::i18n::t("display-hdr-sdr-brightness-description"),
                provider_settings_ui::widgets::trailing_slider(Box::new(self.slider(
                    dispatcher,
                    &output.connector,
                    "sdr_brightness",
                    sdr_brightness_fraction(config.sdr_brightness_nits),
                    effect(DisplayEffect::SET_SDR_BRIGHTNESS),
                ))),
            ));
        } else {
            out_rows.push(value_row(
                &crate::i18n::t("display-hdr"),
                &crate::i18n::t(hdr_unavailable_key(
                    output.sink_hdr,
                    output.hdr_unavailable_reason,
                )),
            ));
        }
        if output.vrr {
            out_rows.push(setting_row_described(
                &crate::i18n::t("display-vrr"),
                &crate::i18n::t("display-vrr-description"),
                Box::new(self.toggle(
                    dispatcher,
                    &output.connector,
                    "vrr",
                    config.vrr,
                    effect(DisplayEffect::SET_VRR),
                )),
            ));
        } else {
            out_rows.push(value_row(
                &crate::i18n::t("display-vrr"),
                &crate::i18n::t(adaptive_sync_unavailable_key(
                    output.sink_vrr,
                    output.vrr_unavailable_reason,
                )),
            ));
        }
        if output.color_profiles.is_empty() {
            out_rows.push(value_row(
                &t("display-colour-profile"),
                &t("display-colour-profile-empty"),
            ));
        } else {
            let mut choices = vec!["System default".into()];
            choices.extend(output.color_profiles.iter().filter_map(|id| {
                output
                    .color_profile_labels
                    .get(id)
                    .map(|label| format!("{label} ({id})"))
            }));
            let placeholder = config
                .color_profile
                .as_ref()
                .and_then(|id| {
                    output
                        .color_profile_labels
                        .get(id)
                        .map(|label| format!("{label} ({id})"))
                })
                .unwrap_or_else(|| "System default".into());
            out_rows.push(setting_row(
                &t("display-colour-profile"),
                Box::new(self.choice(
                    dispatcher,
                    &output.connector,
                    "color_profile",
                    choices,
                    &placeholder,
                    effect(DisplayEffect::SET_COLOR_PROFILE),
                )),
            ));
        }
        out_rows
    }

    /// The safety/apply-transaction card, driven by the on-disk
    /// [`super::DisplayTransaction`] state machine.
    fn safety_card(
        &self,
        cx: &ViewCx<NightLightPrefs>,
        actions: &Actions<Settings>,
    ) -> Option<SettingsSection> {
        let transaction = self.transaction();
        match transaction.as_ref().map(|transaction| transaction.state) {
            Some(DisplayTransactionState::AwaitingConfirmation) => {
                let seconds = transaction
                    .as_ref()
                    .and_then(|transaction| transaction.deadline_unix_ms)
                    .map(|deadline| deadline.saturating_sub(unix_now_ms()).div_ceil(1_000))
                    .unwrap_or_default();
                let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                // One question, one pair of answers.
                let revert = invoking(
                    actions,
                    Button::new("settings.display.revert", &t("display-revert-now")),
                    DisplayEffect::RevertDisplayChanges,
                );
                let keep = invoking(
                    actions,
                    Button::new("settings.display.keep", &t("display-keep"))
                        .role(ui::widget::ButtonRole::Primary),
                    DisplayEffect::KeepDisplayChanges,
                );
                Some(section(
                    &t("display-keep-title"),
                    rows(vec![provider_settings_ui::widgets::item_row(
                        &t_args("display-keep-caption", &[("seconds", seconds.into())]),
                        Vec::new(),
                        &[],
                        vec![revert, keep],
                    )]),
                ))
            }
            Some(
                DisplayTransactionState::Requested
                | DisplayTransactionState::ConfirmRequested
                | DisplayTransactionState::RevertRequested,
            ) => Some(section(
                &t("display-applying-title"),
                rows(vec![note(&t("display-applying-caption"))]),
            )),
            Some(DisplayTransactionState::Rejected) => {
                let mut content = vec![note(&t("display-rejected-caption"))];
                if self.pending_candidate.is_some() {
                    content.push(
                        cx.setting(t("display-retry"))
                            .id("settings.display.apply")
                            .button(&t("display-try-again"), |button| {
                                invoking(actions, button, DisplayEffect::ApplyDisplayChanges)
                            }),
                    );
                }
                Some(section(&t("display-rejected-title"), rows(content)))
            }
            _ if self.pending_candidate.is_some() => Some(section(
                &t("display-apply-title"),
                rows(vec![
                    cx.setting(t("display-apply-row"))
                        .described(t("display-apply-caption"))
                        .id("settings.display.apply")
                        .button(&t("display-apply"), |button| {
                            invoking(actions, button, DisplayEffect::ApplyDisplayChanges)
                        }),
                ]),
            )),
            // Nothing staged and nothing being tried: no card.
            _ => None,
        }
    }

    /// Layout tools whose display changes are kept without asking, each
    /// with a way to be asked again. Absent while no tool is trusted.
    fn trusted_tools_card(
        &self,
        cx: &ViewCx<NightLightPrefs>,
        dispatcher: &Dispatcher<Settings>,
    ) -> Option<SettingsSection> {
        let tools = self.trusted_tools();
        if tools.is_empty() {
            return None;
        }
        let mut content = vec![note(&t("display-tools-caption"))];
        for tool in tools {
            content.push(
                cx.setting(tool.name())
                    .id("settings.display.tool.forget")
                    .button(&t("display-tools-forget"), |button| {
                        crate::on_row::bind_invocation(
                            dispatcher,
                            &DisplayModel::tool_row(&tool).into_resource(),
                            button,
                            DisplayEffect::ForgetDisplayTool {
                                tool: tool.executable.clone(),
                            },
                        )
                    }),
            );
        }
        Some(section(&t("display-tools-title"), rows(content)))
    }
}
