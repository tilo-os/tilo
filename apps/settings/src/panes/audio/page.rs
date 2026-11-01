//! The Sound page as view code over the person's `audio` section and the
//! session's `sound` reading: the outputs and inputs, the levels, the
//! cards' profiles, each app's stream, the way into Speakers and the
//! system sounds.

use std::rc::Rc;

use provider_settings_ui::bound::SettingRows as _;
use settings_store::system_sounds::SystemSoundEvent;
use tilo_audio_service::sound::{Device, Stream};
use ui::LocalState;
use ui::framework::{Actions, Dispatcher, ViewCx};
use ui::widget::{Button, Dropdown, Element, Row, Slider, Widget};

use super::{
    AudioArea, AudioEffect, AudioModel, OUTPUT_SLIDER_WIDTH, STREAM_SLIDER_MIN_WIDTH,
    STREAM_SLIDER_WIDTH, area, bound, device_is_headphones, echo_cancellation, event_shown,
    event_sound, follow_shown, level_meter, mono, stream_live_row, system_sounds,
};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::prefs::AudioPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, error_caption, error_row, note, row, rows, section, setting_row,
    setting_row_described,
};

/// The Sound page. The page is built again when what it shows of the
/// section changes shape (system sounds turned on or off, mono output
/// changed), and when the device reports something new or a change of it
/// says how it went; the device is followed while the page is open.
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let following = held.model().borrow_mut().follow();
    let shown = held.shown.clone();
    let (actions, dispatcher) = (cx.actions(), cx.dispatcher());
    let app = Rc::clone(app);
    let prefs = cx.settings::<AudioPrefs>();
    cx.record(&prefs, move |cx| {
        let (app, actions, dispatcher, shown, following) = (
            Rc::clone(&app),
            actions.clone(),
            dispatcher.clone(),
            shown.clone(),
            Rc::clone(&following),
        );
        let shape = cx.value(|prefs: &AudioPrefs| (prefs.system_sounds.enabled, prefs.mono_output));
        cx.switch(&shape, move |cx, &(sounds_on, mono_on)| {
            let (app, actions, dispatcher, following) = (
                Rc::clone(&app),
                actions.clone(),
                dispatcher.clone(),
                Rc::clone(&following),
            );
            if let Some(area) = area(&app) {
                area.saw_mono(mono_on);
            }
            // The part keeps the device followed while it is shown.
            cx.switch(&shown, move |cx, _| {
                let _followed = &following;
                match area(&app) {
                    Some(area) => page(cx, &actions, &dispatcher, area, sounds_on),
                    None => provider_settings_ui::widgets::page(Vec::new()),
                }
            })
        })
    })
}

/// The page over what `area` read, system sounds on or off (`sounds_on`).
fn page(
    cx: &ViewCx<AudioPrefs>,
    actions: &Actions<Settings>,
    dispatcher: &Dispatcher<Settings>,
    area: &AudioArea,
    sounds_on: bool,
) -> Element {
    let model = area.model().borrow();
    // With no sound server to ask, every reading fails the same way: say it
    // once, instead of a failure per row and empty lists of apps that only
    // look like silence.
    if let (Some(error), Some(_), Some(_)) = (
        &model.devices_error,
        &model.volume_error,
        &model.input_error,
    ) {
        return provider_settings_ui::widgets::page(vec![
            section(
                &t("sound-unavailable-title"),
                rows(vec![row(error_caption(&t_args(
                    "sound-devices-error",
                    &[("error", error.as_str().into())],
                )))]),
            ),
            speakers_card(actions),
            system_sounds_card(cx, &model, sounds_on),
        ]);
    }
    let mut cards = vec![
        output_card(cx, actions, &model),
        input_card(cx, actions, &model),
    ];
    cards.extend(hardware_card(cx, actions, &model));
    // One line per playback stream, routing just that app to a chosen
    // output, with its own volume and mute: the music app to the headphones
    // while the browser stays on the speakers.
    cards.push(streams_card(
        dispatcher,
        &model,
        &t("sound-apps"),
        &model.streams(true),
        model.outputs(),
        &t("sound-apps-empty"),
    ));
    // The capture streams (which app is using the microphone), routed to a
    // chosen input, with their own level and mute.
    cards.push(streams_card(
        dispatcher,
        &model,
        &t("sound-recording"),
        &model.streams(false),
        model.inputs(),
        &t("sound-recording-empty"),
    ));
    cards.push(speakers_card(actions));
    cards.push(system_sounds_card(cx, &model, sounds_on));
    provider_settings_ui::widgets::page(cards)
}

/// `control` (a button), bound to `effect`.
fn invoking<C: ui::framework::ActionControl<()>>(
    actions: &Actions<Settings>,
    control: C,
    effect: impl Into<SettingsEffect>,
) -> C {
    actions.bind_effect_invocation(control, &effect.into())
}

/// The way into Speakers: several of your devices as one set.
fn speakers_card(actions: &Actions<Settings>) -> SettingsSection {
    let open = invoking(
        actions,
        Button::new("sound.speakers", &t("sound-speakers-open")),
        crate::effects::NavigationEffect::OpenPane {
            pane: crate::categories::Category::Speakers.slug().to_owned(),
        },
    );
    section(
        &t("sound-speakers-title"),
        rows(vec![setting_row_described(
            &t("sound-speakers-row"),
            &t("sound-speakers-desc"),
            Box::new(open),
        )]),
    )
}

/// The "Output" card: the outputs to choose from (only when there is more
/// than one), then the volume and mute of the output in use, plus the
/// balance, mono and speaker-test controls the runtime reports support for.
fn output_card(
    cx: &ViewCx<AudioPrefs>,
    actions: &Actions<Settings>,
    model: &AudioModel,
) -> SettingsSection {
    let mut output_rows = Vec::new();
    if let Some(error) = &model.devices_error {
        output_rows.push(row(error_caption(&t_args(
            "sound-devices-error",
            &[("error", error.as_str().into())],
        ))));
    }
    if model.outputs().len() + model.sets.shown.len() >= 2 {
        // The outputs as a list to pick from, the person's speaker sets
        // among them: the one in use checked.
        output_rows.push(row(device_list(actions, model, model.outputs(), true)));
    }
    if let Some(error) = &model.volume_error {
        output_rows.push(error_row(
            &t("sound-volume"),
            &t_args("sound-volume-error", &[("error", error.as_str().into())]),
        ));
    } else {
        output_rows.push(setting_row(&t("sound-volume"), volume_line(actions, model)));
    }
    // A stereo output is the one a channel test and a balance apply to.
    let stereo = model
        .output_device()
        .is_some_and(|device| device.channels == 2);
    if model.runtime_caps.balance && stereo {
        output_rows.push(balance(cx));
    }
    if model.offers_mono() {
        output_rows.push(mono(cx));
        output_rows.push(note(&t("sound-mono-caption")));
    }
    if model.runtime_caps.channel_test && stereo {
        // A tone on one side, then the other: one row, its two buttons the
        // row's control.
        let side = |id: &str, label: &str, channel: &str| {
            invoking(
                actions,
                Button::new(id, label).control_size(ui::widget::ControlSize::Small),
                AudioEffect::TestSpeaker {
                    channel: channel.into(),
                },
            )
        };
        output_rows.push(setting_row_described(
            &t("sound-test-row"),
            &t("sound-test-caption"),
            Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(side(
                        "settings.audio.test.left",
                        &t("sound-test-left"),
                        "left",
                    )))
                    .push(Box::new(side(
                        "settings.audio.test.right",
                        &t("sound-test-right"),
                        "right",
                    ))),
            ),
        ));
    }
    if let Some(notice) = &model.runtime_notice {
        output_rows.push(note(notice));
    }
    section(&t("sound-output"), rows(output_rows))
}

/// The output's volume and, beside it, the button that mutes it, as each
/// app's line has.
fn volume_line(actions: &Actions<Settings>, model: &AudioModel) -> Box<dyn Widget> {
    let volume = actions.bind_effect(
        Slider::from_state(&LocalState::new(f64::from(model.volume)))
            .with_id("audio.volume")
            .with_accessible_name(&t("sound-volume"))
            .shows_value()
            .with_enabled(!model.muted)
            // What the output in use plays, while the page is open: inside
            // the volume's own track, never a second bar.
            .with_level({
                let meter = model.output_meter.clone();
                move || meter.is_live().then(|| meter.get())
            }),
        effect(AudioEffect::SET_OUTPUT_VOLUME),
    );
    let mute = invoking(
        actions,
        ui::widget::primitives::IconButton::regular(
            "audio.mute",
            if model.muted {
                ui::icons::Icon::VolumeX
            } else {
                ui::icons::Icon::Volume2
            },
            &t(if model.muted {
                "sound-unmute"
            } else {
                "sound-mute"
            }),
        ),
        AudioEffect::SetOutputMuted {
            muted: !model.muted,
        },
    );
    Box::new(
        Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                ui::widget::Container::new()
                    .with_no_padding()
                    .with_width(OUTPUT_SLIDER_WIDTH)
                    .with_child(Box::new(volume)),
            ))
            .push(Box::new(mute)),
    )
}

/// The left/right balance, the section's field.
fn balance(cx: &ViewCx<AudioPrefs>) -> Item {
    cx.setting(t("sound-balance")).custom(
        |prefs: &AudioPrefs| f64::from(prefs.balance.clamp(0.0, 1.0)),
        |shown| {
            let slider = Slider::from_state(shown).with_id("audio.balance");
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a balance between 0 and 1 is stored as f32"
            )]
            let binding = cx
                .actions()
                .set_binding(AudioPrefs::paths().balance, |balance: f64| {
                    Ok(balance as f32)
                });
            provider_settings_ui::widgets::trailing_slider(Box::new(bound(slider, binding)))
        },
    )
}

/// The "Input" card: the inputs to choose from, then the microphone level
/// and (when the runtime supports it) a bounded microphone meter test.
fn input_card(
    cx: &ViewCx<AudioPrefs>,
    actions: &Actions<Settings>,
    model: &AudioModel,
) -> SettingsSection {
    let mut input_rows = Vec::new();
    if model.inputs().len() >= 2 {
        input_rows.push(row(device_list(actions, model, model.inputs(), false)));
    }
    if let Some(error) = &model.input_error {
        input_rows.push(error_row(
            &t("sound-input-level"),
            &t_args("sound-input-error", &[("error", error.as_str().into())]),
        ));
    } else {
        input_rows.push(
            cx.setting(t("sound-input-level"))
                .id("audio.input_volume")
                .slider_effect(
                    &LocalState::new(f64::from(model.input_volume)),
                    actions,
                    effect(AudioEffect::SET_INPUT_VOLUME),
                ),
        );
    }
    if model.runtime_caps.microphone_meter && model.input_device().is_some() {
        let running = model.mic_session.is_some();
        let activity = if running {
            t("sound-mic-listening")
        } else {
            t("sound-mic-idle")
        };
        if running {
            input_rows.push(block(Box::new(level_meter::LevelMeter::new(
                &model.mic_meter,
                t("sound-input-level"),
            ))));
        }
        input_rows.push(setting_row(
            &activity,
            Box::new(invoking(
                actions,
                Button::new(
                    "settings.audio.mic_test",
                    &if running {
                        t("sound-mic-stop")
                    } else {
                        t("sound-mic-test")
                    },
                ),
                AudioEffect::ToggleMicrophoneTest,
            )),
        ));
        input_rows.push(note(&t("sound-mic-caption")));
    }
    if model.offers_echo_cancellation() {
        input_rows.push(echo_cancellation(cx));
        input_rows.push(note(&t("sound-echo-caption")));
    }
    section(&t("sound-input"), rows(input_rows))
}

/// The "Device capabilities" card: a profile chooser for each device
/// offering more than one profile, and each output's codec. `None` when
/// there is nothing to show: no empty card.
fn hardware_card(
    cx: &ViewCx<AudioPrefs>,
    actions: &Actions<Settings>,
    model: &AudioModel,
) -> Option<SettingsSection> {
    let mut hardware_rows = Vec::new();
    for card in &model.sound.hardware {
        let selectable: Vec<_> = card.profiles.iter().filter(|p| p.available).collect();
        if selectable.len() < 2 {
            continue;
        }
        let options: Vec<String> = selectable.iter().map(|p| p.label.clone()).collect();
        let device = card.node;
        let active = card
            .profile
            .and_then(|i| card.profiles.iter().find(|p| p.index == i))
            .map_or_else(|| t("sound-choose-profile"), |p| p.label.clone());
        hardware_rows.push(
            cx.setting(&card.label)
                .described(t("sound-profile"))
                .id(format!("audio.profile.{}", card.node))
                .choice_effect(
                    options,
                    &LocalState::new(active),
                    actions,
                    effect(AudioEffect::SET_DEVICE_PROFILE)
                        .map(move |profile: String| (device, profile)),
                ),
        );
    }
    for output in model.outputs() {
        if let Some(codec) = &output.codec {
            hardware_rows.push(provider_settings_ui::widgets::value_row(
                &t_args("sound-codec", &[("device", output.label.as_str().into())]),
                codec,
            ));
        }
    }
    (!hardware_rows.is_empty()).then(|| section(&t("sound-hardware"), rows(hardware_rows)))
}

/// One streams card (Applications or Recording): each stream a line with
/// its volume, mute and (with more than one device) where it plays, or
/// `empty_note` rather than a made-up line.
fn streams_card(
    dispatcher: &Dispatcher<Settings>,
    model: &AudioModel,
    title: &str,
    streams: &[&Stream],
    devices: &[Device],
    empty_note: &str,
) -> SettingsSection {
    if streams.is_empty() {
        return section(title, rows(vec![note(empty_note)]));
    }
    // The routes: every device, after "follow the default".
    let mut options = vec![follow_shown()];
    options.extend(devices.iter().map(|d| d.label.clone()));
    let items: Vec<Item> = streams
        .iter()
        .map(|stream| {
            row(stream_row(
                dispatcher,
                model,
                stream,
                &options,
                devices.len() >= 2,
            ))
        })
        .collect();
    section(title, rows(items))
}

/// Outputs or inputs as rows to choose from, the one in use checked;
/// choosing another plays (or records) through it.
fn device_list(
    actions: &Actions<Settings>,
    model: &AudioModel,
    devices: &[Device],
    outputs: bool,
) -> Box<dyn Widget> {
    let choosing = |effect: AudioEffect| actions.effect(&SettingsEffect::from(effect));
    let mut list = ui::widget::Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0);
    for device in devices {
        let icon = if !outputs {
            ui::icons::Icon::Mic
        } else if device_is_headphones(&device.label) {
            ui::icons::Icon::Headphones
        } else {
            ui::icons::Icon::Speaker
        };
        let kind = if outputs { "output" } else { "input" };
        let mut entry = ui::widget::ListRow::new(
            format!("audio.{kind}.{}", device.node),
            device.label.clone(),
        )
        .icon(icon)
        .checked(device.chosen)
        .selected(device.chosen);
        if !device.chosen {
            let device = device.label.clone();
            let choose = if outputs {
                AudioEffect::SelectOutput { device }
            } else {
                AudioEffect::SelectInput { device }
            };
            if let Some(binding) = choosing(choose) {
                entry = entry.on_action(binding);
            }
        }
        list = list.push(Box::new(entry));
    }
    if outputs {
        for set in &model.sets.shown {
            let mut entry =
                ui::widget::ListRow::new(format!("audio.output.set.{}", set.id), set.name.clone())
                    .icon(ui::icons::Icon::Speaker)
                    .secondary(t_args(
                        "sound-speaker-set-detail",
                        &[(
                            "count",
                            ui::i18n::ArgValue::from(
                                i64::try_from(set.speakers).unwrap_or(i64::MAX),
                            ),
                        )],
                    ))
                    .checked(set.output_here)
                    .selected(set.output_here);
            if !set.output_here
                && let Some(binding) = choosing(AudioEffect::SelectSpeakerSet {
                    group: set.id.clone(),
                })
            {
                entry = entry.on_action(binding);
            }
            list = list.push(Box::new(entry));
        }
    }
    Box::new(list)
}

/// One app's stream on one line: the app and what it plays, its volume, a
/// mute button and, with more than one device to choose from (`routable`),
/// where it plays. Its controls act on its row of the published surface,
/// where its volume, mute and route are operations.
fn stream_row(
    dispatcher: &Dispatcher<Settings>,
    model: &AudioModel,
    s: &Stream,
    options: &[String],
    routable: bool,
) -> Box<dyn Widget> {
    let id = s.node;
    let muted = s.muted;
    let resource = stream_live_row(s).into_resource();
    let volume = bound(
        Slider::from_state(&LocalState::new(f64::from(s.volume.unwrap_or(1.0))))
            .with_id(&format!("audio.stream.{id}.volume.sl"))
            .with_accessible_name(&t_args(
                "sound-app-volume",
                &[("app", s.app.as_str().into())],
            ))
            .with_enabled(!muted),
        crate::on_row::binding(
            dispatcher,
            &resource,
            effect(AudioEffect::SET_STREAM_VOLUME).map(move |volume: f64| (id, volume)),
        ),
    );
    let mute = bound(
        ui::widget::primitives::IconButton::regular(
            &format!("audio.stream.{id}.muted.tg"),
            if muted {
                ui::icons::Icon::VolumeX
            } else {
                ui::icons::Icon::Volume2
            },
            &t(if muted { "sound-unmute" } else { "sound-mute" }),
        ),
        crate::on_row::binding(
            dispatcher,
            &resource,
            effect(AudioEffect::SET_STREAM_MUTED).map(move |()| (id, !muted)),
        ),
    );
    // The volume and its mute button stay together, then the route; in a
    // narrow window they move under the name.
    let mut controls = Row::new()
        .with_align_items(taffy::AlignItems::Center)
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(Box::new(
            // One width for every app's volume, so they line up, down to a
            // short slider in a narrow window.
            ui::widget::Container::new()
                .with_no_padding()
                .with_width(STREAM_SLIDER_WIDTH)
                .with_min_width(STREAM_SLIDER_MIN_WIDTH)
                .with_child(Box::new(volume)),
        ))
        .push(Box::new(mute));
    if routable {
        let current = model.stream_target_label(s);
        let route = Dropdown::from_state(options.to_vec(), &LocalState::new(current.clone()))
            .with_id(&format!("audio.stream.{id}.sink.dd"))
            .with_placeholder(&current);
        let route = bound(
            route,
            crate::on_row::binding(
                dispatcher,
                &resource,
                effect(AudioEffect::ROUTE_STREAM).map(move |device: String| (id, device)),
            ),
        );
        controls = controls.push(Box::new(route));
    }
    let mut line = ui::widget::ListRow::new(format!("audio.stream.{id}"), s.app.clone())
        .trailing(Box::new(controls));
    if let Some(media) = s.media.as_deref().filter(|media| !media.is_empty()) {
        line = line.secondary(media);
    }
    Box::new(line)
}

/// The "System sounds" card: the master switch, then volume, theme and
/// one switch per event while it is on (`sounds_on`).
fn system_sounds_card(
    cx: &ViewCx<AudioPrefs>,
    model: &AudioModel,
    sounds_on: bool,
) -> SettingsSection {
    if !model.event_sounds_available {
        let reason = match crate::panes::installed_programs_unreadable() {
            Some(error) => row(error_caption(&t_args(
                "settings-programs-error",
                &[("error", error.into())],
            ))),
            None => note(&t("sound-system-missing")),
        };
        return section(&t("sound-system"), rows(vec![reason]));
    }
    let mut sound_rows = vec![system_sounds(cx)];
    if !sounds_on {
        return section(&t("sound-system"), rows(sound_rows));
    }
    sound_rows.push(
        cx.setting(t("sound-volume"))
            .id("audio.system_sound_volume")
            .slider(AudioPrefs::paths().system_sounds.volume),
    );
    if model.sound_themes.len() >= 2 {
        let themes = model
            .sound_themes
            .iter()
            .map(|theme| (theme.id.clone(), theme.name.clone()))
            .collect();
        sound_rows.push(cx.setting(t("sound-theme")).id("audio.sound_theme").keyed(
            AudioPrefs::paths().system_sounds.theme,
            themes,
            |prefs: &AudioPrefs| prefs.system_sounds.theme_name().to_owned(),
            // The default theme is stored as no choice.
            |id: String| Ok((id != settings_store::system_sounds::DEFAULT_THEME).then_some(id)),
        ));
    }
    for event in SystemSoundEvent::ALL {
        sound_rows.push(
            cx.setting(event_shown(event))
                .id(format!("audio.event_sound.{}", event.key()))
                .toggle(event_sound(event)),
        );
    }
    sound_rows.push(note(&t("sound-system-caption")));
    section(&t("sound-system"), rows(sound_rows))
}
