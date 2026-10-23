//! The machine's own settings as rows of the palette: when the screen locks
//! and blanks, when the machine suspends, the scale and refresh rate of a
//! display, the keyboard layout and the appearance mode.
//!
//! Each setting is addressed `system-settings:<setting>.<verb>`:
//!
//! - `.get` answers the current value and the values that are allowed;
//! - `.set` takes the typed value, offered as the Settings pane offers it,
//!   and changes it through the section's own field edit or verb, which the
//!   session keeps whether or not Settings is open
//!   ([`crate::shell::settings_records`]): a change that weakens the
//!   person's security asks them first (`confirm`);
//! - `.toggle` flips a boolean setting.
//!
//! Display changes are staged through the keep-or-restore handshake every
//! display change goes through, and answer when staged; `display.change.keep`
//! and `display.change.revert` settle them.

use std::rc::Rc;

use serde_json::{Value, json};
use settings_store::display_tuning;
use settings_store::hardware::{DisplayHardwareSnapshot, RichInputConfig};
use settings_store::{keyboard_sources, power};
use tilo_actions::{
    ActionAddress, ActionContract, ConfirmationPolicy, OperationKind, RetryPolicy, RiskLevel,
};
use ui::search::FieldWeight;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{
    ActionReceipt, InvocationDisposition, ResourceKind, SearchContext, SearchProvider, SearchResult,
};

pub const PROVIDER_ID: &str = "system-settings";

/// What a setting is, which decides how it is read, validated and stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    LockAfter,
    BlankAfter,
    SuspendAfter,
    AutoSuspend,
    DisplayScale,
    DisplayRefreshRate,
    KeyboardLayout,
    AppearanceMode,
}

/// One setting: its address, the catalogue key of its name and the words
/// people also use for it.
struct Spec {
    id: &'static str,
    kind: Kind,
    /// Catalogue key of the name; `<name>-words` holds the translated
    /// alternative words.
    name: &'static str,
    /// The stable English alternatives scripts and English speakers type.
    words: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        id: "power.lock-after",
        kind: Kind::LockAfter,
        name: "setting-power-lock-after",
        words: "lock screen after auto lock timeout idle never",
    },
    Spec {
        id: "power.blank-after",
        kind: Kind::BlankAfter,
        name: "setting-power-blank-after",
        words: "screen off blank turn off display after timeout idle dim sleep",
    },
    Spec {
        id: "power.suspend-after",
        kind: Kind::SuspendAfter,
        name: "setting-power-suspend-after",
        words: "suspend sleep standby after timeout idle never",
    },
    Spec {
        id: "power.auto-suspend",
        kind: Kind::AutoSuspend,
        name: "setting-power-auto-suspend",
        words: "automatic suspend sleep when idle on off",
    },
    Spec {
        id: "display.scale",
        kind: Kind::DisplayScale,
        name: "setting-display-scale",
        words: "display screen scale scaling zoom size hidpi percent",
    },
    Spec {
        id: "display.refresh-rate",
        kind: Kind::DisplayRefreshRate,
        name: "setting-display-refresh-rate",
        words: "display screen refresh rate hz hertz frequency framerate",
    },
    Spec {
        id: "keyboard.layout",
        kind: Kind::KeyboardLayout,
        name: "setting-keyboard-layout",
        words: "keyboard layout language input source keymap",
    },
    Spec {
        id: "appearance.mode",
        kind: Kind::AppearanceMode,
        name: "setting-appearance-mode",
        words: "appearance theme dark light mode automatic night",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Get,
    Set,
    Toggle,
}

impl Verb {
    const fn suffix(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Set => "set",
            Self::Toggle => "toggle",
        }
    }
}

/// The setting and verb an action id of this provider names.
/// Whether the setting Settings' index calls `index_id` is one served here
/// with its live value, so search shows it once, in place.
pub(crate) fn offers_in_place(index_id: &str) -> bool {
    SPECS.iter().any(|spec| {
        spec.id.replace('-', "_") == index_id.replace('-', "_") && read(spec.kind).is_some()
    })
}

fn parse(id: &str) -> Option<(&'static Spec, Verb)> {
    let (setting, suffix) = id.rsplit_once('.')?;
    let verb = [Verb::Get, Verb::Set, Verb::Toggle]
        .into_iter()
        .find(|verb| verb.suffix() == suffix)?;
    let spec = SPECS.iter().find(|spec| spec.id == setting)?;
    (verb != Verb::Toggle || spec.kind == Kind::AutoSuspend).then_some((spec, verb))
}

/// The contract of one of this provider's action ids, for the shell's
/// capability gate: reading is a view, changing a setting is a write.
pub(crate) fn contract_for(id: &str) -> Option<ActionContract> {
    let (spec, verb) = parse(id)?;
    let tags = vec!["desktop".into(), "settings".into(), spec.id.into()];
    Some(match verb {
        Verb::Get => ActionContract {
            tags,
            ..ActionContract::view()
        },
        Verb::Set | Verb::Toggle => ActionContract {
            kind: tilo_actions::contract::ActionKind::Undoable,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            retry: if verb == Verb::Toggle {
                RetryPolicy::AtMostOnce
            } else {
                RetryPolicy::Idempotent
            },
            // A display change restores itself unless kept; every other
            // setting is undone by setting it back.
            reversible: true,
            tags,
            ..ActionContract::default()
        },
    })
}

/// A setting as it stands: the value, how a person reads it, the values that
/// are allowed and whatever else describes it.
struct Reading {
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

fn on_off(on: bool) -> String {
    t(if on { "setting-on" } else { "setting-off" })
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
fn read(kind: Kind) -> Option<Reading> {
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
        Kind::AutoSuspend => {
            let on = power::AUTO_SUSPEND.get();
            Reading {
                value: json!(on),
                label: on_off(on),
                choices: vec![json!(true), json!(false)],
                details: Value::Null,
            }
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

/// The typed parameters `.set` takes.
fn set_params(kind: Kind) -> Vec<ActionParam> {
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
        Kind::AutoSuspend => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether the machine suspends when idle.",
        )],
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

/// The arguments that move `spec` one value along its choices (the
/// values its first parameter allows, else the reading's own list), held at
/// either end.
fn stepped_args(spec: &Spec, delta: i8) -> Result<ActionArgs, ActionError> {
    let reading = read(spec.kind).ok_or_else(|| refuse("this setting is not available here"))?;
    let params = set_params(spec.kind);
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

/// What runs a setting action once it is invoked.
type Runner = Rc<dyn Fn(&Chrome, &ActionArgs) -> Result<Value, ActionError>>;

/// What a store write answers: the setting, what it is now and what it was.
fn changed(spec: &Spec, previous: &Reading, value: &Value) -> Value {
    json!({"setting": spec.id, "value": value, "previous": previous.value})
}

/// The `power` choices of `power.suspend-after.set`.
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
        None => st.quick_settings.battery.runs_on_battery(),
        Some(PLUGGED_IN) => false,
        Some(ON_BATTERY) if st.quick_settings.battery.present => true,
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

/// Run a non-display `.set` or `.toggle` through the section's field edit
/// or verb, as the person at this seat.
fn apply(
    chrome: &Chrome,
    spec: &Spec,
    verb: Verb,
    args: &ActionArgs,
) -> Result<Value, ActionError> {
    use crate::shell::settings_records::{address, change, set_field};
    let invoked_by =
        settings_store::InvokingAction::action(format!("{}.{}", spec.id, verb.suffix()));
    let previous = read(spec.kind).ok_or_else(|| refuse("this setting is not available here"))?;
    let of_power = |action: &str| address("settings.power", action);
    let (action, arguments) = match spec.kind {
        Kind::LockAfter => (
            of_power("field-edit"),
            set_field("lock_minutes", &json!(minutes_arg(args)?)),
        ),
        Kind::BlankAfter => (
            of_power("field-edit"),
            set_field("blank_minutes", &json!(minutes_arg(args)?)),
        ),
        Kind::SuspendAfter => suspend_change(&chrome.borrow(), minutes_arg(args)?, args)?,
        Kind::AutoSuspend => {
            let enabled = match verb {
                Verb::Toggle => !power::AUTO_SUSPEND.get(),
                _ => args
                    .boolean("enabled")
                    .ok_or_else(|| refuse("enabled must be true or false"))?,
            };
            (
                of_power("field-edit"),
                set_field("auto_suspend", &json!(enabled)),
            )
        }
        Kind::KeyboardLayout => {
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                "layout".into(),
                json!(args.text("layout").unwrap_or_default()),
            );
            (address("settings.input", "choose-layout"), arguments)
        }
        Kind::AppearanceMode => {
            super::actions::set_mode(
                &mut chrome.borrow_mut(),
                args.text("mode").unwrap_or_default(),
                &invoked_by,
            )?;
            let value = read(spec.kind).map_or(Value::Null, |reading| reading.value);
            return Ok(changed(spec, &previous, &value));
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
    let value = read(spec.kind).map_or(Value::Null, |reading| reading.value);
    Ok(changed(spec, &previous, &value))
}

/// Stage a display change; it is in effect, and waiting to be kept or
/// reverted, once the ticket completes.
fn stage_display(
    chrome: &Chrome,
    spec: &'static Spec,
    args: &ActionArgs,
) -> Result<InvocationDisposition, ActionError> {
    let previous = read(spec.kind).ok_or_else(|| refuse("no display has been published"))?;
    let connector = args.text("display").map(str::to_owned);
    let (value, pending) = if matches!(spec.kind, Kind::DisplayScale) {
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
    let mut answer = changed(spec, &previous, &value);
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

fn answer_of(reading: &Reading, spec: &Spec) -> Value {
    json!({
        "setting": spec.id,
        "value": reading.value,
        "label": reading.label,
        "allowed": reading.choices,
        "details": reading.details,
    })
}

pub struct SystemSettingsProvider {
    ranker: ui::search::ListIndex,
    /// The rows and their searchable text, read once per version of what
    /// they show: a keystroke checks the documents' stamps instead of reading
    /// and parsing every setting again.
    built: std::cell::RefCell<Option<Built>>,
}

/// What [`SystemSettingsProvider`]'s rows were read from: the settings
/// document, the published display hardware, and the language they speak.
type BuiltFrom = (
    Option<settings_store::SettingsStamp>,
    Option<std::time::SystemTime>,
    u64,
    String,
);

struct Built {
    from: BuiltFrom,
    rows: Vec<SearchResult>,
    fields: Vec<Vec<(String, FieldWeight)>>,
}

/// Asked on every keystroke. A process that publishes the display snapshot
/// itself (the compositor) knows when it moved from the published
/// generation, so only a reader of another's snapshot stats the file.
fn built_from() -> BuiltFrom {
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

fn build() -> (Vec<SearchResult>, Vec<Vec<(String, FieldWeight)>>) {
    let mut rows = Vec::new();
    let mut fields = Vec::new();
    for spec in SPECS {
        let Some(reading) = read(spec.kind) else {
            continue;
        };
        let name = t(spec.name);
        let words = format!("{} {}", spec.words, t(&format!("{}-words", spec.name)));
        let verbs: &[Verb] = if spec.kind == Kind::AutoSuspend {
            &[Verb::Get, Verb::Set, Verb::Toggle]
        } else {
            &[Verb::Get, Verb::Set]
        };
        // What Settings' own index calls it, in every language, and the
        // words it is found by there: this row stands in for that one, so a
        // person never gets both. They weigh less than this row's own name:
        // they describe the neighbourhood too ("sperren" is Lock before it
        // is "lock screen after").
        let indexed: Vec<(String, FieldWeight)> =
            settings_store::index::setting(&spec.id.replace('-', "_"))
                .map(|setting| {
                    settings_store::index::names_in_every_locale(&setting.name_key())
                        .into_iter()
                        .map(|name| (name, FieldWeight::SUBTITLE))
                        .chain(
                            settings_store::index::words_in_every_locale(&setting.words_key())
                                .into_iter()
                                .map(|word| (word, FieldWeight::SUBTITLE)),
                        )
                        .collect()
                })
                .unwrap_or_default();
        for &verb in verbs {
            let row = row_for(spec, verb, &name, &reading);
            let mut row_fields = vec![
                (row.title.clone(), FieldWeight::TITLE),
                (name.clone(), FieldWeight::TITLE),
                (row.id.clone(), FieldWeight::IDENTITY),
                (words.clone(), FieldWeight::IDENTITY),
                (
                    row.subtitle.clone().unwrap_or_default(),
                    FieldWeight::SUBTITLE,
                ),
            ];
            row_fields.extend(indexed.iter().cloned());
            fields.push(row_fields);
            rows.push(row);
        }
    }
    (rows, fields)
}

impl Default for SystemSettingsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemSettingsProvider {
    pub fn new() -> Self {
        Self {
            ranker: ui::search::ListIndex::default(),
            built: std::cell::RefCell::default(),
        }
    }
}

impl SearchProvider for SystemSettingsProvider {
    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        use ui::icons::Icon;
        let (spec, _) = parse(&result.id)?;
        Some(match spec.kind {
            Kind::LockAfter => Icon::Lock,
            Kind::BlankAfter | Kind::DisplayScale | Kind::DisplayRefreshRate => Icon::Monitor,
            Kind::SuspendAfter | Kind::AutoSuspend => Icon::Moon,
            Kind::KeyboardLayout => Icon::Keyboard,
            Kind::AppearanceMode => Icon::Palette,
        })
    }
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "System settings"
    }

    fn display_name(&self) -> String {
        t("search-provider-system-settings")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    /// A boolean setting's row is a switch: activating it flips the value
    /// and the row's pill shows the new one where the person can see it.
    fn toggles_in_place(&self, result: &SearchResult) -> bool {
        parse(&result.id).is_some_and(|(_, verb)| verb == Verb::Toggle)
    }

    /// A setting's row steps through the values Settings offers for it, or
    /// flips a switch; a display change goes through its own confirmation
    /// and is chosen, not stepped.
    fn adjustable(&self, result: &SearchResult) -> bool {
        parse(&result.id).is_some_and(|(spec, verb)| {
            verb != Verb::Get && !matches!(spec.kind, Kind::DisplayScale | Kind::DisplayRefreshRate)
        })
    }

    fn adjust(
        &self,
        result: &SearchResult,
        delta: i8,
        _doc: &mut ui::widget::Edits<'_>,
        _root: &automerge::ObjId,
        chrome: &Chrome,
    ) -> Result<(), ActionError> {
        let (spec, verb) =
            parse(&result.id).ok_or_else(|| refuse("this setting is not served here"))?;
        if verb == Verb::Toggle {
            return apply(chrome, spec, verb, &ActionArgs::none()).map(drop);
        }
        let args = stepped_args(spec, delta)?;
        apply(chrome, spec, Verb::Set, &args).map(drop)
    }

    fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        let built = self.built.borrow();
        let built = built.as_ref()?;
        let at = built.rows.iter().position(|row| row.id == result.id)?;
        crate::shell::search::match_reason_in(needle, result, built.fields.get(at)?)
    }

    /// A person sees one row per setting, its value on the row: reading a
    /// value is what the row already does, and a switch is flipped, not set.
    /// Agents and the control socket still reach `.get` and `.set`.
    fn hidden_from_people(
        &self,
        result: &SearchResult,
        _needle: &str,
        _chrome: &crate::shell::chrome_state::ChromeState,
    ) -> bool {
        parse(&result.id).is_some_and(|(spec, verb)| {
            verb == Verb::Get || (verb == Verb::Set && spec.kind == Kind::AutoSuspend)
        })
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let from = built_from();
        let mut built = self.built.borrow_mut();
        if built.as_ref().is_none_or(|built| built.from != from) {
            let (rows, fields) = build();
            *built = Some(Built { from, rows, fields });
        }
        let Some(Built { rows, fields, .. }) = built.as_ref() else {
            return Vec::new();
        };
        crate::shell::search::rank_kept_rows(&self.ranker, ctx.needle.trim(), rows, fields)
    }
}

fn row_for(spec: &'static Spec, verb: Verb, name: &str, reading: &Reading) -> SearchResult {
    let id = format!("{}.{}", spec.id, verb.suffix());
    let (title, subtitle, params) = match verb {
        Verb::Get => (
            t_args(
                "setting-get",
                &[
                    ("name", name.to_owned().into()),
                    ("value", reading.label.clone().into()),
                ],
            ),
            t("setting-get-detail"),
            Vec::new(),
        ),
        // The one row a person sees for a setting: its name, with the live
        // value as the trailing pill; choosing it asks for the new value.
        Verb::Set | Verb::Toggle => (
            name.to_owned(),
            String::new(),
            if verb == Verb::Set {
                set_params(spec.kind)
            } else {
                Vec::new()
            },
        ),
    };
    let display =
        matches!(spec.kind, Kind::DisplayScale | Kind::DisplayRefreshRate) && verb == Verb::Set;
    let run: Runner = match verb {
        Verb::Get => Rc::new(move |_chrome: &Chrome, _args: &ActionArgs| {
            read(spec.kind)
                .map(|reading| answer_of(&reading, spec))
                .ok_or_else(|| refuse("this setting is not available here"))
        }),
        _ => Rc::new(move |chrome: &Chrome, args: &ActionArgs| apply(chrome, spec, verb, args)),
    };
    let open = Rc::clone(&run);
    let invoke = Rc::clone(&run);
    let mut row = SearchResult {
        facts: Vec::new(),
        shortcut: None,
        kind: ResourceKind::Action,
        operation_kind: if verb == Verb::Get {
            OperationKind::View
        } else {
            OperationKind::Effect
        },
        provider_id: PROVIDER_ID.into(),
        id: id.clone(),
        title,
        subtitle: (!subtitle.is_empty()).then_some(subtitle),
        badge: (verb != Verb::Get).then(|| reading.label.clone()),
        score: 0,
        params,
        contract: contract_for(&id),
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
                invoke(chrome, args).map(ActionReceipt::completed_with)
            },
        )),
        on_begin_invoke: None,
    };
    if display {
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
                  args: &ActionArgs| stage_display(chrome, spec, args),
        ));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::ArgValue;
    use automerge::{AutoCommit, ROOT};
    use std::sync::Arc;

    fn rows(chrome: &Chrome, needle: &str) -> Vec<SearchResult> {
        let state = chrome.borrow();
        let mut rows = SystemSettingsProvider::new().query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &AutoCommit::new(),
            doc_root: &ROOT,
        });
        rows.sort_by_key(|row| std::cmp::Reverse(row.score));
        rows
    }

    /// Invoke `id` with `args`, bound against the row's declared parameters
    /// the way a control-socket caller's are.
    fn run(chrome: &Chrome, id: &str, args: Vec<(String, ArgValue)>) -> Result<Value, ActionError> {
        let row = rows(chrome, "")
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| refuse(format!("{id} is not offered")))?;
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
    fn settings_are_typed_actions_reachable_by_name_and_stored_through_the_store() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths).unwrap());
        let settings = crate::shell::test_support::install_settings(session.host_view().unwrap());
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());

        {
            let ids: Vec<String> = rows(&chrome, "").into_iter().map(|row| row.id).collect();
            for expected in [
                "power.lock-after.get",
                "power.lock-after.set",
                "power.blank-after.get",
                "power.blank-after.set",
                "power.suspend-after.get",
                "power.suspend-after.set",
                "power.auto-suspend.get",
                "power.auto-suspend.set",
                "power.auto-suspend.toggle",
                "keyboard.layout.get",
                "keyboard.layout.set",
                "appearance.mode.get",
                "appearance.mode.set",
            ] {
                assert!(ids.iter().any(|id| id == expected), "{expected} in {ids:?}");
            }
            assert!(
                !ids.iter().any(|id| id.starts_with("display.")),
                "no display has been published, so there is none to change"
            );
            let blank = rows(&chrome, "")
                .into_iter()
                .find(|row| row.id == "power.blank-after.set")
                .unwrap();
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
                blank.title, "Turn screen off after",
                "the setting's own name"
            );
            assert_eq!(
                blank.badge.as_deref(),
                Some("10 minutes"),
                "its value is the pill"
            );
            assert_eq!(blank.subtitle, None);
            let provider = SystemSettingsProvider::new();
            let get = rows(&chrome, "")
                .into_iter()
                .find(|row| row.id == "power.blank-after.get")
                .unwrap();
            let state = chrome.borrow();
            assert!(
                provider.hidden_from_people(&get, "", &state),
                "reading is the row's pill"
            );
            assert!(!provider.hidden_from_people(&blank, "", &state));
            drop(state);

            // ←/→ step through the values Settings offers, held at the ends.
            assert!(provider.adjustable(&blank));
            let mut doc = AutoCommit::new();
            let mut step = |delta: i8| {
                provider
                    .adjust(
                        &blank,
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
            for needle in ["blank after", "turn screen off after", "screen off timeout"] {
                let top = rows(&chrome, needle).into_iter().next().map(|row| row.id);
                assert!(
                    top.as_deref()
                        .is_some_and(|id| id.starts_with("power.blank-after")),
                    "{needle}: {top:?}"
                );
            }
            crate::shell::i18n::ensure();
            ui::i18n::I18nManager::set_locale("de-DE");
            let german = rows(&chrome, "Bildschirm ausschalten nach")
                .into_iter()
                .next()
                .map(|row| row.id);
            ui::i18n::I18nManager::set_locale("en-US");
            assert!(
                german
                    .as_deref()
                    .is_some_and(|id| id.starts_with("power.blank-after")),
                "{german:?}"
            );
        }

        {
            let answer = run(&chrome, "power.blank-after.get", Vec::new()).unwrap();
            assert_eq!(answer["value"], 10);
            assert_eq!(answer["allowed"], json!([0, 1, 5, 10, 15, 30]));
        }

        {
            assert!(matches!(
                run(
                    &chrome,
                    "power.blank-after.set",
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
                &chrome,
                "power.blank-after.set",
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
                run(&chrome, "power.lock-after.set", args)
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
                run(&chrome, "power.suspend-after.set", args)
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
            chrome.borrow_mut().quick_settings.battery = tilo_power_mgmt::BatterySnapshot {
                present: true,
                percent: 50,
                state: tilo_power_mgmt::BatteryState::Discharging,
            };
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
            // An untouched machine does not suspend on idle, and the action
            // says so the way the Settings pane and the compositor read it.
            let fresh = run(&chrome, "power.auto-suspend.get", Vec::new()).unwrap();
            assert_eq!(fresh["value"], false);
            assert!(!power::AUTO_SUSPEND.get());
            let toggled = run(&chrome, "power.auto-suspend.toggle", Vec::new()).unwrap();
            assert_eq!(toggled["value"], true);
            assert!(power::AUTO_SUSPEND.get());
            run(
                &chrome,
                "power.auto-suspend.set",
                arg("enabled", ArgValue::Boolean(false)),
            )
            .unwrap();
            assert!(!power::AUTO_SUSPEND.get());
        }

        {
            run(
                &chrome,
                "keyboard.layout.set",
                arg("layout", ArgValue::Text("de".into())),
            )
            .unwrap();
            let answer = run(&chrome, "keyboard.layout.get", Vec::new()).unwrap();
            assert_eq!(answer["value"], "de");
            assert!(
                run(
                    &chrome,
                    "keyboard.layout.set",
                    arg("layout", ArgValue::Text("../x".into()))
                )
                .is_err()
            );
        }

        {
            run(
                &chrome,
                "appearance.mode.set",
                arg("mode", ArgValue::Text("automatic".into())),
            )
            .unwrap();
            assert_eq!(
                settings_store::appearance::MODE.get().as_deref(),
                Some("automatic")
            );
            assert!(matches!(
                run(
                    &chrome,
                    "appearance.mode.set",
                    arg("mode", ArgValue::Text("neon".into()))
                ),
                Err(ActionError::Arguments(_))
            ));
        }

        {
            let read = crate::shell::search::providers::actions::shell_action_contract(
                "power.blank-after.get",
            );
            assert_eq!(read.kind, tilo_actions::contract::ActionKind::Read);
            let write = crate::shell::search::providers::actions::shell_action_contract(
                "power.blank-after.set",
            );
            assert_eq!(write.kind, tilo_actions::contract::ActionKind::Undoable);
            assert_eq!(write.risk, RiskLevel::Low);
        }
        drop(settings);
    }
}
