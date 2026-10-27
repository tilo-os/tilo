//! The settings the palette changes to a typed value without opening
//! Settings: when the screen locks and blanks, when the machine suspends,
//! the scale and refresh rate of a display, the keyboard layout and whether
//! the desktop is dark or light.
//!
//! Each is its row in the index ([`super`]): the row takes the value,
//! offered as the Settings pane offers it, and changes it through the
//! section's own field edit or verb, which the session keeps whether or not
//! Settings is open ([`crate::shell::settings_records`]); a change that
//! weakens the person's security asks them first (`confirm`). A row of its
//! own, hidden from people, reads the value and the values allowed.
//!
//! Display changes are staged through the keep-or-restore handshake every
//! display change goes through, and answer when staged;
//! `display.change.keep` and `display.change.revert` settle them.

use std::rc::Rc;

use serde_json::{Value, json};
use settings_store::display_tuning;
use settings_store::hardware::{DisplayHardwareSnapshot, RichInputConfig};
use settings_store::{keyboard_sources, power};
use tilo_actions::{
    ActionAddress, ActionContract, ConfirmationPolicy, OperationKind, RetryPolicy, RiskLevel,
};

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{ActionReceipt, InvocationDisposition, ResourceKind, SearchResult};

use super::PROVIDER_ID;

/// What a setting is, which decides how it is read, validated and stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    LockAfter,
    BlankAfter,
    SuspendAfter,
    DisplayScale,
    DisplayRefreshRate,
    KeyboardLayout,
    AppearanceMode,
}

impl Kind {
    /// The kind of the setting the index calls `setting`, where the palette
    /// changes it to a typed value.
    pub(super) fn of(setting: &str) -> Option<Self> {
        Some(match setting {
            "power.lock_after" => Self::LockAfter,
            "power.blank" => Self::BlankAfter,
            "power.suspend_after" => Self::SuspendAfter,
            "display.scale" => Self::DisplayScale,
            "display.refresh_rate" => Self::DisplayRefreshRate,
            "keyboard.layout" => Self::KeyboardLayout,
            "appearance.style" => Self::AppearanceMode,
            _ => return None,
        })
    }

    /// Whether it changes a display, which is staged for the person to keep
    /// rather than stepped.
    pub(super) fn is_display(self) -> bool {
        matches!(self, Self::DisplayScale | Self::DisplayRefreshRate)
    }

    const ALL: [Self; 7] = [
        Self::LockAfter,
        Self::BlankAfter,
        Self::SuspendAfter,
        Self::DisplayScale,
        Self::DisplayRefreshRate,
        Self::KeyboardLayout,
        Self::AppearanceMode,
    ];
}

/// The contract of a typed setting's row: reading is a view, changing it a
/// write that is undone by setting it back (a display change restores
/// itself unless kept).
pub(super) fn contract(reads: bool, setting: &str) -> ActionContract {
    let tags = vec!["desktop".into(), "settings".into(), setting.into()];
    if reads {
        return ActionContract {
            tags,
            ..ActionContract::view()
        };
    }
    ActionContract {
        kind: tilo_actions::contract::ActionKind::Undoable,
        risk: RiskLevel::Low,
        confirmation: ConfirmationPolicy::None,
        retry: RetryPolicy::Idempotent,
        tags,
        ..ActionContract::default()
    }
}

/// A setting as it stands: the value, how a person reads it, the values that
/// are allowed and whatever else describes it.
pub(super) struct Reading {
    value: Value,
    label: String,
    choices: Vec<Value>,
    details: Value,
}

fn minutes_label(minutes: u32) -> String {
    if minutes == 0 {
        return t("setting-never");
    }
    t_args("setting-minutes", &[("minutes", i64::from(minutes).into())])
}

fn mode_label(mode: Option<&str>) -> String {
    match mode {
        Some("dark") => t("setting-mode-dark"),
        Some("light") => t("setting-mode-light"),
        Some(settings_store::day_schedule::AUTOMATIC_MODE) => t("setting-mode-automatic"),
        _ => t("setting-mode-unset"),
    }
}

fn minutes_reading(key: &settings_store::Key<u32>, choices: &[u32]) -> Reading {
    let minutes = key.get();
    Reading {
        value: json!(minutes),
        label: minutes_label(minutes),
        choices: choices.iter().map(|choice| json!(choice)).collect(),
        details: json!({"unit": "minutes", "never": 0}),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a display scale is between 1 and 4 and is rounded first"
)]
fn percent(scale: f64) -> i64 {
    (scale * 100.0).round() as i64
}

/// The compositor's published displays and the committed layout, when it has
/// published any.
/// The display hardware as published. The compositor publishes it itself,
/// so it reads its own copy from memory; only a process that reads another's
/// snapshot reads the file (and checks its publisher is alive).
fn displays() -> Option<DisplayHardwareSnapshot> {
    DisplayHardwareSnapshot::published()
        .map(|published| published.as_ref().clone())
        .or_else(DisplayHardwareSnapshot::load)
        .filter(|snapshot| !snapshot.outputs.is_empty())
}

fn committed_layout(snapshot: &DisplayHardwareSnapshot) -> settings_store::hardware::DisplayConfig {
    settings_store::hardware::DisplayConfig::load_for(&settings_store::display_mode::attached_set(
        &snapshot.outputs,
    ))
}

fn display_details(snapshot: &DisplayHardwareSnapshot) -> Value {
    let base = committed_layout(snapshot);
    let outputs: Vec<Value> = snapshot
        .outputs
        .iter()
        .map(|output| {
            json!({
                "display": output.descriptor.label(),
                "scale_percent": percent(output.scale),
                "refresh_hz": display_tuning::hz_label(output.current_mode.refresh_mhz),
                "refresh_rates_hz": display_tuning::refresh_choices(output)
                    .into_iter()
                    .map(display_tuning::hz_label)
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let default = display_tuning::target(&snapshot.outputs, &base, None)
        .map_or(Value::Null, |output| json!(output.descriptor.label()));
    json!({"default_display": default, "displays": outputs})
}

fn refresh_rates(snapshot: &DisplayHardwareSnapshot) -> Vec<String> {
    let mut rates: Vec<u32> = snapshot
        .outputs
        .iter()
        .flat_map(display_tuning::refresh_choices)
        .collect();
    rates.sort_unstable();
    rates.dedup();
    rates.into_iter().map(display_tuning::hz_label).collect()
}

fn input_config() -> RichInputConfig {
    keyboard_sources::effective(
        &RichInputConfig::from_section(&settings_store::load_section(
            settings_store::hardware::INPUT_SECTION,
        )),
        keyboard_sources::ALL,
    )
}

/// The setting as it stands now; `None` where this session has nothing to
/// read (no display has been published).
pub(super) fn read(kind: Kind) -> Option<Reading> {
    Some(match kind {
        Kind::LockAfter => minutes_reading(&power::LOCK_MINUTES, power::LOCK_CHOICES),
        Kind::BlankAfter => minutes_reading(&power::BLANK_MINUTES, power::BLANK_CHOICES),
        Kind::SuspendAfter => {
            let mut reading = minutes_reading(&power::SUSPEND_MINUTES, power::SUSPEND_CHOICES);
            // One timer per power source; a profile's own only where the
            // stored profile timers truly differ.
            let timers =
                power::SuspendTimers::from_section(&settings_store::load_section(power::SECTION));
            reading.details = json!({
                "unit": "minutes",
                "never": 0,
                "enabled": power::AUTO_SUSPEND.get(),
                "plugged_in": timers.plugged_in(),
                "on_battery": timers.on_battery(),
                "profiles": timers.profiles_differ().then(|| json!({
                    "saver": timers.of_profile("saver"),
                    "balanced": timers.of_profile("balanced"),
                    "performance": timers.of_profile("performance"),
                })),
            });
            reading
        }
        Kind::DisplayScale => {
            let snapshot = displays()?;
            let output =
                display_tuning::target(&snapshot.outputs, &committed_layout(&snapshot), None)
                    .ok()?;
            let current = percent(output.scale);
            Reading {
                value: json!(current),
                label: format!("{current}%"),
                choices: display_tuning::scale_choices()
                    .into_iter()
                    .map(|scale| json!(percent(scale)))
                    .collect(),
                details: display_details(&snapshot),
            }
        }
        Kind::DisplayRefreshRate => {
            let snapshot = displays()?;
            let output =
                display_tuning::target(&snapshot.outputs, &committed_layout(&snapshot), None)
                    .ok()?;
            let hertz = display_tuning::hz_label(output.current_mode.refresh_mhz);
            Reading {
                label: format!("{hertz} Hz"),
                value: json!(hertz),
                choices: refresh_rates(&snapshot)
                    .into_iter()
                    .map(Value::from)
                    .collect(),
                details: display_details(&snapshot),
            }
        }
        Kind::KeyboardLayout => {
            let input = input_config();
            Reading {
                value: json!(input.keyboard_layout),
                label: input.keyboard_layout.clone(),
                choices: input.keyboard_layouts.iter().map(|id| json!(id)).collect(),
                details: json!({"layouts": input.keyboard_layouts}),
            }
        }
        Kind::AppearanceMode => {
            let mode = settings_store::appearance::MODE.get();
            Reading {
                value: json!(mode),
                label: mode_label(mode.as_deref()),
                choices: [
                    "dark",
                    "light",
                    settings_store::day_schedule::AUTOMATIC_MODE,
                ]
                .into_iter()
                .map(Value::from)
                .collect(),
                details: Value::Null,
            }
        }
    })
}

/// The typed parameters setting it takes.
pub(super) fn set_params(kind: Kind) -> Vec<ActionParam> {
    let choices = |values: &[u32]| values.iter().map(u32::to_string).collect::<Vec<_>>();
    let minutes = |what: &'static str, values: &[u32]| {
        ActionParam::required_owned("minutes", ParamType::Integer, what)
            .with_choices(choices(values))
    };
    let display = || {
        ActionParam::optional_owned(
            "display",
            ParamType::Text,
            "The display to change, by its connector name (eDP-1). Default: the primary one.",
        )
    };
    match kind {
        Kind::LockAfter => vec![
            minutes(
                "Minutes of inactivity before the session locks; 0 means never.",
                power::LOCK_CHOICES,
            ),
            ActionParam::optional_owned(
                "confirm",
                ParamType::Boolean,
                "Confirm a later lock or none, which makes the device less safe.",
            )
            .with_labelled_choices([("true", t("setting-confirm-weaker"))]),
        ],
        Kind::BlankAfter => vec![minutes(
            "Minutes of inactivity before the screen turns off; 0 means never.",
            power::BLANK_CHOICES,
        )],
        Kind::SuspendAfter => vec![
            minutes(
                "Minutes of inactivity before the machine suspends; 0 means never.",
                power::SUSPEND_CHOICES,
            ),
            ActionParam::optional_owned(
                "power",
                ParamType::Text,
                "The timer of this power source. Default: the one the device runs on now.",
            )
            .with_labelled_choices([
                (PLUGGED_IN, t("setting-suspend-plugged-in")),
                (ON_BATTERY, t("setting-suspend-on-battery")),
            ]),
        ],
        Kind::DisplayScale => vec![
            ActionParam::required_owned("percent", ParamType::Integer, "Display scale in percent.")
                .with_choices(
                    display_tuning::scale_choices()
                        .into_iter()
                        .map(|scale| percent(scale).to_string()),
                ),
            display(),
        ],
        Kind::DisplayRefreshRate => {
            let rates = displays()
                .map(|snapshot| refresh_rates(&snapshot))
                .unwrap_or_default();
            vec![
                ActionParam::required_owned(
                    "hertz",
                    ParamType::Text,
                    "Refresh rate in Hz at the display's current resolution (60, 59.94).",
                )
                .with_choices(rates),
                display(),
            ]
        }
        Kind::KeyboardLayout => vec![ActionParam::required_owned(
            "layout",
            ParamType::Text,
            "An XKB layout or layout:variant (de, us:intl). One that is not yet in the list is added.",
        )],
        Kind::AppearanceMode => vec![
            ActionParam::required_owned(
                "mode",
                ParamType::Text,
                "Dark, light, or automatic by day.",
            )
            .with_choices([
                "dark",
                "light",
                settings_store::day_schedule::AUTOMATIC_MODE,
            ]),
        ],
    }
}

/// The arguments that move `kind` one value along its choices (the values
/// its first parameter allows, else the reading's own list), held at either
/// end.
pub(super) fn stepped_args(kind: Kind, delta: i8) -> Result<ActionArgs, ActionError> {
    let reading = read(kind).ok_or_else(|| refuse("this setting is not available here"))?;
    let params = set_params(kind);
    let param = params
        .first()
        .ok_or_else(|| refuse("this setting takes no value"))?;
    let as_text = |value: &Value| {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned)
    };
    let choices: Vec<String> = if param.choices.is_empty() {
        reading.choices.iter().map(as_text).collect()
    } else {
        param.choices.iter().map(ToString::to_string).collect()
    };
    let current = as_text(&reading.value);
    let at = choices.iter().position(|choice| *choice == current);
    let next = match (at, delta > 0) {
        (Some(at), true) => at.saturating_add(1).min(choices.len().saturating_sub(1)),
        (Some(at), false) => at.saturating_sub(1),
        (None, _) => 0,
    };
    let chosen = choices
        .get(next)
        .ok_or_else(|| refuse("this setting has no values to step through"))?;
    let value = match param.ty {
        ParamType::Integer => chosen
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| refuse("not a whole number"))?,
        _ => Value::String(chosen.clone()),
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert(param.name.to_string(), value);
    ActionArgs::bind_json(&params, &arguments).map_err(ActionError::Arguments)
}

fn refuse(error: impl std::fmt::Display) -> ActionError {
    ActionError::Refused(error.to_string())
}

fn minutes_arg(args: &ActionArgs) -> Result<u32, ActionError> {
    args.integer("minutes")
        .and_then(|minutes| u32::try_from(minutes).ok())
        .ok_or_else(|| refuse("minutes must be a whole number of minutes"))
}

/// What a store write answers: the setting, what it is now and what it was.
fn changed(setting: &str, previous: &Reading, value: &Value) -> Value {
    json!({"setting": setting, "value": value, "previous": previous.value})
}

/// The `power` choices of setting the suspend timer.
const PLUGGED_IN: &str = "plugged-in";
const ON_BATTERY: &str = "battery";

/// The change setting the suspend timer to `minutes` as Settings' Power
/// pane shows it: where the profiles' own timers truly differ, the selected
/// profile's; otherwise the timer of the power source `args` names, else of
/// the one the device runs on now (on battery only where it has one).
fn suspend_change(
    st: &crate::shell::chrome_state::ChromeState,
    minutes: u32,
    args: &ActionArgs,
) -> Result<(ActionAddress, serde_json::Map<String, Value>), ActionError> {
    use crate::shell::quick_settings::PowerProfile;
    use crate::shell::settings_records::{address, set_field};
    let timers = power::SuspendTimers::from_section(&settings_store::load_section(power::SECTION));
    if timers.profiles_differ() {
        let profile = match st.quick_settings.power_profile {
            PowerProfile::Saver => "saver",
            PowerProfile::Balanced => "balanced",
            PowerProfile::Performance => "performance",
        };
        return Ok((
            address("settings.power", "field-edit"),
            set_field(&format!("{profile}_suspend_minutes"), &json!(minutes)),
        ));
    }
    let on_battery = match args.text("power") {
        None => st.quick_settings.battery().runs_on_battery(),
        Some(PLUGGED_IN) => false,
        Some(ON_BATTERY) if st.quick_settings.battery().present => true,
        Some(ON_BATTERY) => return Err(refuse("this device has no battery")),
        Some(other) => return Err(refuse(format!("{other:?} is not a power source"))),
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert("minutes".into(), json!(minutes));
    let verb = if on_battery {
        "set-suspend-on-battery"
    } else {
        "set-suspend-plugged-in"
    };
    Ok((address("settings.power", verb), arguments))
}

/// Change a setting that is not a display's through the section's field
/// edit or verb, as the person at this seat.
pub(super) fn apply(
    chrome: &Chrome,
    kind: Kind,
    setting: &str,
    args: &ActionArgs,
) -> Result<Value, ActionError> {
    use crate::shell::settings_records::{address, change, set_field};
    let invoked_by = settings_store::InvokingAction::action(format!("{PROVIDER_ID}:{setting}"));
    let previous = read(kind).ok_or_else(|| refuse("this setting is not available here"))?;
    let of_power = |action: &str| address("settings.power", action);
    let (action, arguments) = match kind {
        Kind::LockAfter => (
            of_power("field-edit"),
            set_field("lock_minutes", &json!(minutes_arg(args)?)),
        ),
        Kind::BlankAfter => (
            of_power("field-edit"),
            set_field("blank_minutes", &json!(minutes_arg(args)?)),
        ),
        Kind::SuspendAfter => suspend_change(&chrome.borrow(), minutes_arg(args)?, args)?,
        Kind::KeyboardLayout => {
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                "layout".into(),
                json!(args.text("layout").unwrap_or_default()),
            );
            (address("settings.input", "choose-layout"), arguments)
        }
        Kind::AppearanceMode => {
            super::super::actions::set_mode(
                &mut chrome.borrow_mut(),
                args.text("mode").unwrap_or_default(),
                &invoked_by,
            )?;
            let value = read(kind).map_or(Value::Null, |reading| reading.value);
            return Ok(changed(setting, &previous, &value));
        }
        Kind::DisplayScale | Kind::DisplayRefreshRate => {
            return Err(refuse("display changes require lifecycle dispatch"));
        }
    };
    change(
        &action,
        &arguments,
        args.boolean("confirm") == Some(true),
        &invoked_by,
    )?;
    let value = read(kind).map_or(Value::Null, |reading| reading.value);
    Ok(changed(setting, &previous, &value))
}

/// Stage a display change; it is in effect, and waiting to be kept or
/// reverted, once the ticket completes.
fn stage_display(
    chrome: &Chrome,
    kind: Kind,
    setting: &'static str,
    args: &ActionArgs,
) -> Result<InvocationDisposition, ActionError> {
    let previous = read(kind).ok_or_else(|| refuse("no display has been published"))?;
    let connector = args.text("display").map(str::to_owned);
    let (value, pending) = if kind == Kind::DisplayScale {
        {
            let percent = args
                .integer("percent")
                .ok_or_else(|| refuse("percent must be a whole number"))?;
            #[allow(
                clippy::cast_precision_loss,
                reason = "a scale percentage is at most a few hundred"
            )]
            let scale = percent as f64 / 100.0;
            let pending = crate::shell::display_persistence::stage_plan(|snapshot, base| {
                display_tuning::plan_scale(&snapshot.outputs, base, connector.as_deref(), scale)
                    .map_err(std::io::Error::other)
            });
            (json!(percent), pending)
        }
    } else {
        {
            let text = args.text("hertz").unwrap_or_default();
            let hertz: f64 = text
                .parse()
                .map_err(|_| refuse(format!("{text:?} is not a refresh rate in Hz")))?;
            let pending = crate::shell::display_persistence::stage_plan(|snapshot, base| {
                display_tuning::plan_refresh(&snapshot.outputs, base, connector.as_deref(), hertz)
                    .map_err(std::io::Error::other)
            });
            (json!(text), pending)
        }
    };
    let pending = pending.map_err(refuse)?;
    let mut answer = changed(setting, &previous, &value);
    if let Some(object) = answer.as_object_mut() {
        object.insert("awaiting_confirmation".into(), json!(true));
        object.insert("keep".into(), json!("actions:display.change.keep"));
        object.insert("revert".into(), json!("actions:display.change.revert"));
    }
    let chrome = chrome.clone();
    Ok(InvocationDisposition::Pending(Rc::new(move || {
        pending
            .poll()
            .map(|done| {
                done.map(|_| {
                    chrome.borrow_mut().bump();
                    ActionReceipt::completed_with(answer.clone())
                })
            })
            .map_err(refuse)
    })))
}

/// What reading a setting answers.
fn answer_of(reading: &Reading, setting: &str) -> Value {
    json!({
        "setting": setting,
        "value": reading.value,
        "label": reading.label,
        "allowed": reading.choices,
        "details": reading.details,
    })
}

/// What the settings' readings were read from: the settings document, the
/// published display hardware, and the language they speak.
type ReadFrom = (
    Option<settings_store::SettingsStamp>,
    Option<std::time::SystemTime>,
    u64,
    String,
);

/// Every typed setting as it stands, read again only when what it is read
/// from moved: a keystroke checks the documents' stamps instead of reading
/// and parsing every setting again.
/// Every typed setting's reading, in [`Kind::ALL`] order, and what they
/// were read from.
type Read = (ReadFrom, Vec<Option<Rc<Reading>>>);

#[derive(Default)]
pub(super) struct Readings {
    read: std::cell::RefCell<Option<Read>>,
}

impl Readings {
    /// `kind` as it stands now; `None` where this session has nothing to
    /// read (no display has been published).
    pub(super) fn of(&self, kind: Kind) -> Option<Rc<Reading>> {
        let from = read_from();
        let mut read_now = self.read.borrow_mut();
        if read_now.as_ref().is_none_or(|(read, _)| *read != from) {
            let all = Kind::ALL
                .iter()
                .map(|&kind| read(kind).map(Rc::new))
                .collect();
            *read_now = Some((from, all));
        }
        let (_, all) = read_now.as_ref()?;
        let at = Kind::ALL.iter().position(|&each| each == kind)?;
        all.get(at)?.clone()
    }
}

/// Asked on every keystroke. A process that publishes the display snapshot
/// itself (the compositor) knows when it moved from the published
/// generation, so only a reader of another's snapshot stats the file.
fn read_from() -> ReadFrom {
    let displays = if DisplayHardwareSnapshot::published().is_some() {
        None
    } else {
        settings_store::hardware::display_snapshot_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
    };
    (
        settings_store::stamp(),
        displays,
        DisplayHardwareSnapshot::published_generation(),
        crate::shell::i18n::current_locale(),
    )
}

/// The row changing `setting` (index id) to a typed value: its name, the
/// value as it stands as its pill ([`live`]); choosing it asks for the new
/// value. A display's change is staged for the person to keep.
pub(super) fn changing(
    kind_of_row: &ResourceKind,
    kind: Kind,
    setting: &'static str,
    id: String,
    title: String,
    facts: Vec<String>,
) -> SearchResult {
    let run = Rc::new(move |chrome: &Chrome, args: &ActionArgs| apply(chrome, kind, setting, args));
    let open = Rc::clone(&run);
    let mut row = SearchResult {
        operation_kind: OperationKind::Effect,
        params: set_params(kind),
        contract: Some(contract(false, setting)),
        on_open: Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  chrome: &Chrome,
                  args: &ActionArgs| open(chrome, args).map(drop),
        ),
        on_invoke: Some(Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  chrome: &Chrome,
                  args: &ActionArgs| {
                run(chrome, args).map(ActionReceipt::completed_with)
            },
        )),
        ..SearchResult::nullary(
            kind_of_row.clone(),
            PROVIDER_ID,
            id,
            title,
            None,
            0,
            |_, _, _| {},
        )
    }
    .with_facts(facts);
    if kind.is_display() {
        row.on_open = Rc::new(
            |_doc: &mut ui::widget::Edits<'_>,
             _root: &automerge::ObjId,
             _chrome: &Chrome,
             _args: &ActionArgs| {
                Err(refuse("display changes require lifecycle dispatch"))
            },
        );
        row.on_invoke = None;
        row.on_begin_invoke = Some(Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  chrome: &Chrome,
                  args: &ActionArgs| stage_display(chrome, kind, setting, args),
        ));
    }
    row
}

/// The row reading `setting` (index id): its value, how a person reads it,
/// the values allowed and whatever else describes it. Hidden from people,
/// whose row already shows the value.
pub(super) fn reading(
    kind_of_row: &ResourceKind,
    kind: Kind,
    setting: &'static str,
    id: String,
    title: String,
) -> SearchResult {
    let run = Rc::new(move || {
        read(kind)
            .map(|reading| answer_of(&reading, setting))
            .ok_or_else(|| refuse("this setting is not available here"))
    });
    let open = Rc::clone(&run);
    SearchResult {
        operation_kind: OperationKind::View,
        subtitle: Some(t("setting-get-detail")),
        contract: Some(contract(true, setting)),
        on_open: Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  _chrome: &Chrome,
                  _args: &ActionArgs| open().map(drop),
        ),
        on_invoke: Some(Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  _chrome: &Chrome,
                  _args: &ActionArgs| run().map(ActionReceipt::completed_with),
        )),
        ..SearchResult::nullary(
            kind_of_row.clone(),
            PROVIDER_ID,
            id,
            title,
            None,
            0,
            |_, _, _| {},
        )
    }
}

/// Row `row` of `kind` as it reads now: the changing row's pill says the
/// value, its parameters offer what is allowed now (a display's refresh
/// rates); the reading row's title says the value.
pub(super) fn live(
    row: &SearchResult,
    kind: Kind,
    reading: &Reading,
    reads: bool,
    name: &str,
) -> SearchResult {
    if reads {
        return SearchResult {
            title: t_args(
                "setting-get",
                &[
                    ("name", name.to_owned().into()),
                    ("value", reading.label.clone().into()),
                ],
            ),
            ..row.clone()
        };
    }
    SearchResult {
        badge: Some(reading.label.clone()),
        params: set_params(kind),
        ..row.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::super::SettingsPagesProvider;
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::ArgValue;
    use crate::shell::search::{SearchContext, SearchProvider};
    use automerge::{AutoCommit, ROOT};
    use std::sync::Arc;

    fn with_context<R>(
        provider: &SettingsPagesProvider,
        chrome: &Chrome,
        needle: &str,
        read: impl FnOnce(SearchContext<'_>) -> R,
    ) -> R {
        let state = chrome.borrow();
        let doc = AutoCommit::new();
        let wanted = [provider.resource_kinds()[0].clone()];
        read(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &wanted,
            chrome: &state,
            doc: &doc,
            doc_root: &ROOT,
        })
    }

    /// What the palette lists for `needle`, best first.
    fn rows(provider: &SettingsPagesProvider, chrome: &Chrome, needle: &str) -> Vec<SearchResult> {
        let mut rows = with_context(provider, chrome, needle, |ctx| provider.query(ctx));
        rows.sort_by_key(|row| std::cmp::Reverse(row.score));
        rows
    }

    /// The row at the address `id`, as it reads now.
    fn row(provider: &SettingsPagesProvider, chrome: &Chrome, id: &str) -> Option<SearchResult> {
        with_context(provider, chrome, "", |ctx| provider.resolve(id, ctx))
    }

    /// Invoke `id` with `args`, bound against the row's declared parameters
    /// the way a control-socket caller's are.
    fn run(
        provider: &SettingsPagesProvider,
        chrome: &Chrome,
        id: &str,
        args: Vec<(String, ArgValue)>,
    ) -> Result<Value, ActionError> {
        let row =
            row(provider, chrome, id).ok_or_else(|| refuse(format!("{id} is not offered")))?;
        let args = ActionArgs::bind(&row.params, args).map_err(ActionError::Arguments)?;
        let mut doc = AutoCommit::new();
        let mut edits = ui::widget::Edits::lend(&mut doc);
        let invoke = row
            .on_invoke
            .as_ref()
            .ok_or_else(|| refuse("the row has no typed receipt"))?;
        match invoke(&mut edits, &ROOT, chrome, &args)?.update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => output
                .value
                .ok_or_else(|| refuse("the receipt carries no answer")),
            other => Err(refuse(format!("unexpected update {other:?}"))),
        }
    }

    fn arg(name: &str, value: ArgValue) -> Vec<(String, ArgValue)> {
        vec![(name.to_owned(), value)]
    }

    #[test]
    #[serial_test::serial]
    fn typed_settings_are_index_rows_reachable_by_name_and_stored_through_the_store() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths).unwrap());
        let settings = crate::shell::test_support::install_settings(session.host_view().unwrap());
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let provider = SettingsPagesProvider::new();
        let provider = &provider;

        {
            for id in [
                "setting:power.lock_after",
                "setting:power.blank",
                "setting:power.suspend_after",
                "setting:keyboard.layout",
                "setting:appearance.style",
            ] {
                assert!(row(provider, &chrome, id).is_some(), "{id} is offered");
                assert!(
                    row(provider, &chrome, &format!("{id}.get")).is_some(),
                    "{id} is read"
                );
            }
            for id in ["setting:display.scale", "setting:display.refresh_rate"] {
                assert!(
                    row(provider, &chrome, id).is_none(),
                    "{id}: no display has been published, so there is none to change"
                );
            }
            let blank = row(provider, &chrome, "setting:power.blank").unwrap();
            assert_eq!(blank.params[0].ty, ParamType::Integer);
            assert_eq!(
                blank.params[0]
                    .choices
                    .iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect::<Vec<_>>(),
                ["0", "1", "5", "10", "15", "30"]
            );
            assert_eq!(
                blank.title,
                t(&index_name("power.blank")),
                "the setting's name in the index"
            );
            assert_eq!(
                blank.badge.as_deref(),
                Some("10 minutes"),
                "its value is the pill"
            );
            let get = row(provider, &chrome, "setting:power.blank.get").unwrap();
            let state = chrome.borrow();
            assert!(
                provider.hidden_from_people(&get, "", &state),
                "reading is the row's pill"
            );
            assert!(!provider.hidden_from_people(&blank, "", &state));
            drop(state);

            // ←/→ step through the values Settings offers, held at the ends.
            let listed = rows(provider, &chrome, &blank.title)
                .into_iter()
                .find(|row| row.id == blank.id)
                .expect("listed by its name");
            assert!(provider.adjustable(&listed));
            let mut doc = AutoCommit::new();
            let mut step = |delta: i8| {
                provider
                    .adjust(
                        &listed,
                        delta,
                        &mut ui::widget::Edits::lend(&mut doc),
                        &ROOT,
                        &chrome,
                    )
                    .unwrap();
                power::BLANK_MINUTES.get()
            };
            assert_eq!(step(1), 15, "10 minutes, one notch up");
            assert_eq!(step(1), 30);
            assert_eq!(step(-1), 15);
            for _ in 0..8 {
                step(1);
            }
            assert_eq!(step(1), 30, "held at the longest");
            for _ in 0..8 {
                step(-1);
            }
            assert_eq!(power::BLANK_MINUTES.get(), 0, "down to never");
            step(1);
            step(1);
            assert_eq!(power::BLANK_MINUTES.get(), 5);
            assert_eq!(step(1), 10, "back where it started");
        }

        {
            for needle in ["blank screen", "turn screen off after", "screen timeout"] {
                let top = rows(provider, &chrome, needle)
                    .into_iter()
                    .next()
                    .map(|row| row.id);
                assert_eq!(top.as_deref(), Some("setting:power.blank"), "{needle}");
            }
            crate::shell::i18n::ensure();
            ui::i18n::I18nManager::set_locale("de-DE");
            let german = rows(provider, &chrome, "Bildschirm ausschalten nach")
                .into_iter()
                .next()
                .map(|row| row.id);
            ui::i18n::I18nManager::set_locale("en-US");
            assert_eq!(german.as_deref(), Some("setting:power.blank"));
        }

        {
            let answer = run(provider, &chrome, "setting:power.blank.get", Vec::new()).unwrap();
            assert_eq!(answer["value"], 10);
            assert_eq!(answer["allowed"], json!([0, 1, 5, 10, 15, 30]));
        }

        {
            assert!(matches!(
                run(
                    provider,
                    &chrome,
                    "setting:power.blank",
                    arg("minutes", ArgValue::Integer(7))
                ),
                Err(ActionError::Arguments(_))
            ));
            assert_eq!(
                power::BLANK_MINUTES.get(),
                10,
                "a refused value stores nothing"
            );
            let answer = run(
                provider,
                &chrome,
                "setting:power.blank",
                arg("minutes", ArgValue::Integer(0)),
            )
            .unwrap();
            assert_eq!(answer["value"], 0);
            assert_eq!(answer["previous"], 10);
            assert_eq!(power::BLANK_MINUTES.get(), 0);
            let stored = settings_store::load_section("power");
            assert_eq!(stored["blank_minutes"], 0, "the leaf the compositor reads");
            let origin = session.change_log();
            assert!(
                origin
                    .iter()
                    .any(|entry| entry["action"] == "settings.power/field-edit"),
                "the change is the section's field edit: {origin:?}"
            );
        }

        {
            let lock = |minutes: i64, confirm: Option<bool>| {
                let mut args = arg("minutes", ArgValue::Integer(minutes));
                if let Some(confirm) = confirm {
                    args.push(("confirm".into(), ArgValue::Boolean(confirm)));
                }
                run(provider, &chrome, "setting:power.lock_after", args)
            };
            assert_eq!(
                lock(1, None).unwrap()["value"],
                1,
                "locking sooner asks nothing"
            );
            assert_eq!(power::LOCK_MINUTES.get(), 1);
            for weaker in [10, 0] {
                assert!(
                    matches!(lock(weaker, None), Err(ActionError::ConfirmationRequired)),
                    "locking later or never asks first"
                );
            }
            assert_eq!(power::LOCK_MINUTES.get(), 1, "nothing changed unconfirmed");
            assert_eq!(lock(0, Some(true)).unwrap()["previous"], 1);
            assert_eq!(power::LOCK_MINUTES.get(), 0);
            let origin = session.change_log();
            assert!(
                origin
                    .last()
                    .is_some_and(|entry| entry["action"] == "settings.power/field-edit"),
                "Settings' own field edit made it: {origin:?}"
            );
        }

        {
            let suspend = |minutes: i64, source: Option<&str>| {
                let mut args = arg("minutes", ArgValue::Integer(minutes));
                if let Some(source) = source {
                    args.push(("power".into(), ArgValue::Text(source.into())));
                }
                run(provider, &chrome, "setting:power.suspend_after", args)
            };
            let timers = || {
                power::SuspendTimers::from_section(&settings_store::load_section(power::SECTION))
            };
            // No battery: the one timer, plugged in.
            suspend(30, None).unwrap();
            assert_eq!(timers().plugged_in(), 30);
            assert!(
                suspend(5, Some("battery")).is_err(),
                "no battery timer without a battery"
            );
            // On battery: its own timer, the other source still named.
            chrome.borrow_mut().quick_settings.status.battery =
                Some(tilo_power_mgmt::BatterySnapshot {
                    present: true,
                    percent: 50,
                    state: tilo_power_mgmt::BatteryState::Discharging,
                });
            suspend(5, None).unwrap();
            suspend(60, Some("plugged-in")).unwrap();
            assert_eq!((timers().on_battery(), timers().plugged_in()), (5, 60));
            // Profiles that keep their own timers: the selected profile's.
            settings_store::SectionWrite::new()
                .set(&power::SAVER_SUSPEND_MINUTES, &10)
                .commit_by(&settings_store::ChangeOrigin::action("test"))
                .unwrap();
            chrome.borrow_mut().quick_settings.power_profile =
                crate::shell::quick_settings::PowerProfile::Saver;
            suspend(15, None).unwrap();
            assert_eq!(timers().of_profile("saver"), Some(15));
            assert_eq!(
                timers().of_profile("balanced"),
                Some(60),
                "the others keep theirs"
            );
        }

        {
            run(
                provider,
                &chrome,
                "setting:keyboard.layout",
                arg("layout", ArgValue::Text("de".into())),
            )
            .unwrap();
            let answer = run(provider, &chrome, "setting:keyboard.layout.get", Vec::new()).unwrap();
            assert_eq!(answer["value"], "de");
            assert!(
                run(
                    provider,
                    &chrome,
                    "setting:keyboard.layout",
                    arg("layout", ArgValue::Text("../x".into()))
                )
                .is_err()
            );
        }

        {
            run(
                provider,
                &chrome,
                "setting:appearance.style",
                arg("mode", ArgValue::Text("automatic".into())),
            )
            .unwrap();
            assert_eq!(
                settings_store::appearance::MODE.get().as_deref(),
                Some("automatic")
            );
            assert!(matches!(
                run(
                    provider,
                    &chrome,
                    "setting:appearance.style",
                    arg("mode", ArgValue::Text("neon".into()))
                ),
                Err(ActionError::Arguments(_))
            ));
        }

        {
            let contract = |id: &str| {
                row(provider, &chrome, id)
                    .and_then(|row| row.contract)
                    .unwrap_or_else(|| panic!("{id} has a contract"))
            };
            let read = contract("setting:power.blank.get");
            assert_eq!(read.kind, tilo_actions::contract::ActionKind::Read);
            let write = contract("setting:power.blank");
            assert_eq!(write.kind, tilo_actions::contract::ActionKind::Undoable);
            assert_eq!(write.risk, RiskLevel::Low);
        }
        drop(settings);
    }

    fn index_name(setting: &str) -> String {
        settings_store::index::setting(setting)
            .expect("indexed")
            .name_key()
    }
}
