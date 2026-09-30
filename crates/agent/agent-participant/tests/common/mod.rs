//! What the coverage tests share: the fixture catalogue and a generic search
//! for one user-equivalent edit in an app the caller knows nothing about.

#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each test binary uses part of this; a fixture failure is the assertion"
)]

use std::path::Path;
use std::rc::Rc;
use std::sync::OnceLock;

use serde_json::json;
use tilo_actions::door::{ActionDescriptor, InputSpec};
use tilo_actions::{Availability, ConfirmationPolicy, OperationKind, PROMPTED_PARAM, ParamType};
use tilo_agent::surface::Catalogue;
use tilo_agent::tools::{self, Answer, Revealed};
use tilo_agent_participant::apps::is_semantic;
use tilo_agent_participant::{
    AppCatalogue, Control, ControlInput, PlanModel, StepAction, component_apps,
};
use ui::accesskit::Role;

/// Apps whose state is device-local by design, so no input changes the
/// shared record: the agent operates them like anyone, and nothing syncs.
pub const LOCAL_STATE: &[(&str, &str)] = &[
    (
        "so.tilo.clocks",
        "alarms, timers and world clocks live in a device-local file",
    ),
    (
        "so.tilo.hex-editor",
        "edits a local file; the record holds nothing until one is opened",
    ),
    (
        "so.tilo.passwords",
        "secrets live in the device's encrypted secret store",
    ),
    (
        "so.tilo.pdf-viewer",
        "opens a document the user picks; the record holds nothing until one is open",
    ),
    (
        "so.tilo.speed-test",
        "measurements and their history live in the app instance, not the record",
    ),
];

/// The room the tests' records belong to.
pub const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// The text every edit writes.
pub const WRITTEN: &str = "Written by an agent";

/// Keep the apps a test binary mounts off the user's profile: their files
/// under a temporary home, their secrets in memory. Otherwise TOTP and
/// Passwords open the user's real vaults and wait on a Keychain prompt.
pub fn isolate_device() {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::Builder::new()
            .prefix("agent-participant-home")
            .tempdir()
            .unwrap();
        for (var, dir) in [
            ("HOME", ""),
            ("XDG_DATA_HOME", ".local/share"),
            ("XDG_CONFIG_HOME", ".config"),
            ("XDG_STATE_HOME", ".local/state"),
            ("XDG_CACHE_HOME", ".cache"),
        ] {
            // SAFETY: every test reaches this through `catalogue` before it
            // mounts an app, and the `OnceLock` holds the others back, so no
            // thread of this binary reads the environment meanwhile.
            #[allow(
                clippy::disallowed_methods,
                reason = "the binary's environment is set once, before any app reads it"
            )]
            unsafe {
                std::env::set_var(var, home.path().join(dir));
            }
        }
        tilo_secrets::keep_in_memory_for_process();
        home
    });
}

/// Every web app, presented with the fixture fonts and theme, on an
/// [isolated](isolate_device) device.
pub fn catalogue() -> AppCatalogue {
    isolate_device();
    AppCatalogue::new(
        component_apps(),
        Rc::new(|| {
            let fonts = ui::font_catalog::FontCatalog::controlled_fixture(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/assets/fonts"),
                "en-US",
            );
            ui::application::ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::with_font_catalog(&fonts).unwrap(),
            }
        }),
    )
}

/// Something an agent can drive: a mounted app, or a participant's record.
pub trait Driveable {
    /// Every published action, declared and projected.
    fn survey(&mut self) -> Vec<ActionDescriptor>;
    /// Every on-screen control.
    fn controls(&mut self) -> Vec<Control>;
    /// The record's document, for noticing that it changed.
    fn body(&mut self) -> String;
    /// Do one step.
    fn run(&mut self, action: &StepAction) -> Result<(), String>;
}

/// How an edit was made.
#[derive(Clone, Debug)]
pub struct Edit {
    /// `declared`, `control` (projected action) or `input` (direct).
    pub path: &'static str,
    /// The action or control input, as the step log names it.
    pub label: String,
    /// The step, replayable.
    pub action: StepAction,
}

/// Arguments a caller with no knowledge of the app would try: a value of
/// each declared type. `None` when a required parameter has a type no
/// generic value fits.
pub fn sample_arguments(
    descriptor: &ActionDescriptor,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut arguments = serde_json::Map::new();
    if descriptor.params.is_empty()
        && let InputSpec::Text { .. } = descriptor.input
    {
        arguments.insert(PROMPTED_PARAM.into(), WRITTEN.into());
    }
    for param in descriptor.params.iter().filter(|param| param.required) {
        let value = if let Some(choice) = param.choices.first() {
            serde_json::Value::from(choice.as_ref())
        } else {
            match param.ty {
                ParamType::Text => WRITTEN.into(),
                ParamType::Integer => 1.into(),
                ParamType::Number => 1.5.into(),
                ParamType::Boolean => true.into(),
                _ => return None,
            }
        };
        arguments.insert(param.name.to_string(), value);
    }
    Some(arguments)
}

/// Available, without a confirmation step, and not an external effect.
pub fn basic(descriptor: &ActionDescriptor) -> bool {
    matches!(descriptor.contract.availability, Availability::Available)
        && matches!(descriptor.contract.confirmation, ConfirmationPolicy::None)
        && descriptor.kind != OperationKind::Effect
}

fn destructive(label: &str) -> bool {
    let label = label.to_lowercase();
    [
        "delete",
        "remove",
        "close",
        "quit",
        "clear",
        "reset",
        "discard",
        "lock",
        "export",
        "import",
        "restore",
        "back up",
        "open file",
        "cancel",
    ]
    .iter()
    .any(|word| label.contains(word))
}

fn submits(label: &str) -> bool {
    let label = label.to_lowercase();
    [
        "add", "save", "create", "done", "ok", "submit", "apply", "new", "insert",
    ]
    .iter()
    .any(|word| {
        label
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == *word)
    })
}

/// Whether filling this field edits anything: searching and filtering only
/// change the view.
fn fillable(name: &str) -> bool {
    let name = name.to_lowercase();
    !["search", "filter", "find"]
        .iter()
        .any(|word| name.contains(word))
}

/// What a person would type into a field named `name`: a well-formed value
/// of the kind its name asks for.
pub fn sample_value(name: &str, role: Role) -> String {
    let name = name.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| name.contains(word));
    if role == Role::PasswordInput || any(&["secret", "key", "password", "token"]) {
        "JBSWY3DPEHPK3PXP".into()
    } else if any(&[
        "second", "minute", "hour", "period", "digit", "count", "number", "size",
    ]) {
        "30".into()
    } else if any(&["address", "url", "link", "website"]) {
        "https://example.com".into()
    } else if any(&["email", "e-mail"]) {
        "agent@example.com".into()
    } else {
        WRITTEN.into()
    }
}

const TEXT_ROLES: &[Role] = &[Role::TextInput, Role::MultilineTextInput, Role::Document];

/// The attempts a person unfamiliar with the app would make, in order: the
/// app's own document actions, then filling its fields, then typing into
/// its text surfaces, then its other declared actions, then its buttons.
pub fn candidates(app: &mut impl Driveable) -> Vec<Edit> {
    let survey = app.survey();
    let (projected, declared): (Vec<_>, Vec<_>) = survey.into_iter().partition(is_semantic);
    let controls = app.controls();
    let invoke = |path, descriptor: &ActionDescriptor| {
        let arguments = sample_arguments(descriptor)?;
        Some(Edit {
            path,
            label: format!(
                "{}#{}",
                descriptor.address.resource, descriptor.address.action
            ),
            action: StepAction::Invoke {
                address: descriptor.address.clone(),
                arguments,
            },
        })
    };
    let operate = |control: &Control, input: ControlInput, verb: &str| Edit {
        path: "input",
        label: format!(
            "{verb} {:?} ({:?})",
            control.name.as_deref().unwrap_or_default(),
            control.role
        ),
        action: StepAction::Operate {
            control: control.id.clone(),
            input,
        },
    };
    let mut out = Vec::new();
    out.extend(
        declared
            .iter()
            .filter(|d| d.kind == OperationKind::Document && basic(d))
            .filter_map(|d| invoke("declared", d)),
    );
    out.extend(
        projected
            .iter()
            .filter(|d| d.address.action == "set-value" && basic_or_effect(d))
            .filter(|d| fillable(&d.resource_label))
            .filter_map(|d| {
                let mut edit = invoke("control", d)?;
                if let StepAction::Invoke { arguments, .. } = &mut edit.action {
                    arguments.insert(
                        PROMPTED_PARAM.into(),
                        sample_value(&d.resource_label, Role::TextInput).into(),
                    );
                }
                Some(edit)
            }),
    );
    let live = |control: &&Control| !control.disabled;
    let empty_field = |control: &&Control| {
        control.value.as_deref().unwrap_or_default().is_empty()
            && fillable(control.name.as_deref().unwrap_or_default())
    };
    let sample =
        |control: &Control| sample_value(control.name.as_deref().unwrap_or_default(), control.role);
    out.extend(
        controls
            .iter()
            .filter(live)
            .filter(empty_field)
            .filter(|c| c.verbs.iter().any(|verb| verb == "set-value"))
            .map(|c| operate(c, ControlInput::SetValue(sample(c)), "set-value")),
    );
    out.extend(
        controls
            .iter()
            .filter(live)
            .filter(empty_field)
            .filter(|c| TEXT_ROLES.contains(&c.role))
            .map(|c| operate(c, ControlInput::TypeText(sample(c)), "type")),
    );
    out.extend(
        declared
            .iter()
            .filter(|d| d.kind != OperationKind::Document && basic(d))
            .filter(|d| !destructive(&d.action_label))
            .filter_map(|d| invoke("declared", d)),
    );
    let pressable = controls.iter().filter(live).filter(|c| {
        matches!(
            c.role,
            Role::Button | Role::CheckBox | Role::Switch | Role::Tab | Role::ListItem
        ) && !destructive(c.name.as_deref().unwrap_or_default())
    });
    let (submit, other): (Vec<&Control>, Vec<&Control>) =
        pressable.partition(|c| submits(c.name.as_deref().unwrap_or_default()));
    out.extend(
        submit
            .into_iter()
            .chain(other)
            .map(|c| operate(c, ControlInput::Press, "press")),
    );
    out
}

fn basic_or_effect(descriptor: &ActionDescriptor) -> bool {
    matches!(descriptor.contract.availability, Availability::Available)
        && matches!(descriptor.contract.confirmation, ConfirmationPolicy::None)
}

/// Try [`candidates`] until one changes the record, surveying again after
/// every attempt: an attempt that changes only the view (a dialog opens, a
/// search narrows a list) puts new controls on screen. Returns the edit and
/// every attempt before it.
pub fn find_edit(app: &mut impl Driveable, limit: usize) -> (Option<Edit>, Vec<String>) {
    let mut tried = std::collections::HashSet::new();
    let mut refused = Vec::new();
    for _ in 0..limit {
        let Some(edit) = candidates(app)
            .into_iter()
            .find(|edit| !tried.contains(&edit.label))
        else {
            break;
        };
        tried.insert(edit.label.clone());
        let before = app.body();
        match app.run(&edit.action) {
            Ok(()) if app.body() != before => return (Some(edit), refused),
            Ok(()) => refused.push(format!("{}: no change", edit.label)),
            Err(error) => refused.push(format!("{}: {error}", edit.label)),
        }
    }
    (None, refused)
}

/// Chooses the offer sharing the most words with the task (its app,
/// resource and action) and fills its text parameters with the task's
/// quoted phrase. Deterministic, and bound by the same rule as a model: it
/// reveals before it plans.
pub struct KeywordModel;

impl PlanModel for KeywordModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        let task = task.to_lowercase();
        let words: Vec<&str> = task
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.len() > 2)
            .collect();
        let quoted = task.split('"').nth(1).unwrap_or_default().to_owned();
        let score = |offer: &&tilo_agent::surface::Offer| {
            let d = &offer.descriptor;
            let text = format!(
                "{} {} {} {} {}",
                offer.app, d.resource_label, d.address.resource, d.action_label, d.address.action
            )
            .to_lowercase();
            let own: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).collect();
            let verb = usize::from(words.contains(&d.address.action.to_lowercase().as_str()));
            words
                .iter()
                .filter(|word| own.contains(word))
                .count()
                .saturating_add(verb.saturating_mul(2))
        };
        let offer = catalogue
            .offers
            .iter()
            .filter(|offer| score(offer) > 0)
            .min_by_key(|offer| std::cmp::Reverse(score(offer)))
            .ok_or("nothing offered fits")?;
        let mut revealed = Revealed::new();
        tools::answer(
            tools::LIST_OPERATIONS,
            &json!({ "resource": offer.address().resource }),
            catalogue,
            &mut revealed,
        );
        let mut arguments = serde_json::Map::new();
        for param in offer.descriptor.params.iter().filter(|p| p.required) {
            arguments.insert(param.name.to_string(), quoted.clone().into());
        }
        tools::read_plan(
            &json!({"steps": [{"operation": offer.id(), "arguments": arguments}], "why": "keyword"}),
            &revealed,
            most_steps,
        )
    }
}
