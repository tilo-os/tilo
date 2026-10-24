//! Testing a framework app the way it runs: the real component runtime,
//! storage, permissions, undo and history, without a window.
//!
//! Prefer [`tilo_resource::Replica`] for the resource's own behaviour (it
//! needs no UI). Use [`TestApp`] to prove what the host adds: that controls
//! invoke actions, that undo reverts them, that a viewer is refused, that
//! state survives a reopen.
//!
//! Nothing here is reached from an app binary's own modes: those use
//! [`super::headless`], so a release app carries none of this module.

use ui_core::application::{ApplicationHost, ComponentRunnerInputs};
use ui_core::headless::FrameHarness;
use ui_core::launch::LaunchError;
use ui_core::permissions::{Role, UserId};
use ui_core::record::RecordId;

use tilo_resource::Resource;

use super::app::{ResourceApp, definition, prepare_on, role_permissions};
use super::headless::{AUTHOR, INSTANCE, TEST_ROOM, operation, profile_directory, surface_of};
use super::root::ResourceRoot;

pub use super::headless::{HeadlessApp, surface_in};

/// What interactions cost an app on the UI thread, phase by phase.
#[cfg(feature = "test-support")]
pub mod perf;

/// The name tests use for [`HeadlessApp`].
pub type TestApp<A> = HeadlessApp<A>;

/// Run `visit` on the custom surface of type `S` that `A`'s view hosts in
/// `harness` (what [`ViewCx::surface`](super::ViewCx::surface) mounted), then
/// the frames that follow; `None` when the view hosts no such surface at its
/// root. For a test that drives a component mounted another way than
/// [`HeadlessApp`] (two windows over one record).
pub fn with_surface<A: ResourceApp, S: 'static, T>(
    harness: &mut FrameHarness,
    visit: impl FnOnce(&mut S) -> T,
) -> Option<T> {
    harness
        .with_widget_mut(|root: &mut ResourceRoot| surface_of::<A, S>(root).map(visit))
        .flatten()
}

/// The custom surface of type `S` that `A`'s view hosts, when `widget` is
/// `A`'s root as a host mounted it: a tab of `A` in a shell, found in the
/// host's own widget map.
pub fn surface_in_root<A: ResourceApp, S: 'static>(
    widget: &mut dyn ui_core::widget::Widget,
) -> Option<&mut S> {
    surface_of::<A, S>(widget.as_any_mut()?.downcast_mut::<ResourceRoot>()?)
}

/// A dispatcher outside any host, for a unit test of a surface (or of the
/// model behind it) that edits through one: each immediate action
/// ([`Dispatcher::apply_now`](super::Dispatcher::apply_now)) applies to the document handed to it,
/// attributed to `author`, through the same executor as a hosted one, so
/// the test exercises the surface's real write path. Queued invocations
/// ([`Dispatcher::apply`](super::Dispatcher::apply)) are not taken by anyone: drive those through
/// [`TestApp`].
pub fn detached_dispatcher<R: Resource>(author: &str) -> super::Dispatcher<R> {
    super::Dispatcher::new("test", R::KIND, false, Some(author))
}

/// The live state of `doc`, for a surface a unit test attaches outside any
/// host (see [`detached_dispatcher`]); the surface keeps it current from the
/// document its `sync` receives, as in a host.
pub fn detached_live<R: Resource>(
    doc: &tilo_resource::Doc,
) -> Result<super::Live<R>, tilo_resource::ModelError> {
    Ok(super::Live::new(tilo_resource::Reader::read(doc)?))
}

/// Inspect the window `A`'s live launch opens: the same definition and
/// component, mounted in a native [`ui_core::App`] on a fresh private
/// profile, without running its event loop. For what the window itself
/// declares, such as capture protection.
pub fn inspect_live_window<A: ResourceApp, T>(
    inspect: impl FnOnce(&ui_core::App) -> T,
) -> Result<T, LaunchError> {
    drive_live_window::<A, T>(|app| inspect(app))
}

/// Drive the window `A`'s live launch opens, mounted as
/// [`inspect_live_window`] mounts it: the real [`ui_core::App`], which a
/// test or benchmark feeds input through [`ui_core::App::surface_input`].
pub fn drive_live_window<A: ResourceApp, T>(
    drive: impl FnOnce(&mut ui_core::App) -> T,
) -> Result<T, LaunchError> {
    let profile = profile_directory().map_err(operation)?;
    let mut host = ApplicationHost::new(
        profile.path().join("state"),
        "test-device".into(),
        "test-workspace",
        UserId(AUTHOR.into()),
        role_permissions(AUTHOR, Role::Owner),
        tilo_actions::ActionContext::default(),
    )?;
    let definition = definition::<A>();
    let prepared = prepare_on(
        &mut host,
        definition,
        INSTANCE,
        None,
        ComponentRunnerInputs::from_native_process(),
    )?;
    let component = definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
    let mut app = ui_core::App::from_component(component, prepared.runner_inputs)?;
    let seen = drive(&mut app);
    app.close_components();
    Ok(seen)
}

/// The record window `instance` of `A` kept in the test room while `A` kept
/// one per window: stamped with that window's default identity, initialized
/// as `A` creates a record, then edited with `actions`. Open the app over
/// such records with [`HeadlessApp::with_window_records`].
pub fn window_record<A: ResourceApp>(
    instance: u64,
    actions: &[<A::Resource as Resource>::Action],
) -> Result<tilo_resource::Doc, LaunchError> {
    let mut doc = tilo_resource::Doc::new();
    let kind = definition::<A>().record_kind.clone();
    if !ui_core::record::init_scoped(
        &mut doc,
        &RecordId::window_default(instance),
        &kind,
        TEST_ROOM,
        Some(TEST_ROOM),
    ) {
        return Err(operation("the window record could not be stamped"));
    }
    <A::Resource as Resource>::initialize(&mut doc).map_err(operation)?;
    for action in actions {
        tilo_actions::Action::apply(action, &mut doc).map_err(operation)?;
    }
    Ok(doc)
}

/// [`drive_live_window`] with the window shown in a region of a host's
/// scene, as the shell shows an in-process app: the real app in a
/// [`ui_core::app::EmbeddedApp`], which the drive lays out, draws into its
/// own scene and feeds input through
/// `ui_core::app::EmbeddedApp::input_for_test`.
#[cfg(feature = "native")]
pub fn drive_live_embedded<A: ResourceApp, T>(
    drive: impl FnOnce(&mut ui_core::app::EmbeddedApp) -> T,
) -> Result<T, LaunchError> {
    let profile = profile_directory().map_err(operation)?;
    let mut host = ApplicationHost::new(
        profile.path().join("state"),
        "test-device".into(),
        "test-workspace",
        UserId(AUTHOR.into()),
        role_permissions(AUTHOR, Role::Owner),
        tilo_actions::ActionContext::default(),
    )?;
    let definition = definition::<A>();
    let prepared = prepare_on(
        &mut host,
        definition,
        INSTANCE,
        None,
        ComponentRunnerInputs::from_native_process(),
    )?;
    let component = definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
    let app = ui_core::App::from_component(component, prepared.runner_inputs)?;
    let mut tile = ui_core::app::EmbeddedApp::new(app, |_| {})?;
    let seen = drive(&mut tile);
    tile.app_mut().close_components();
    Ok(seen)
}

/// A dispatcher of `R` for testing a surface's own methods without a host:
/// the edits it applies now ([`Dispatcher::apply_now`](super::Dispatcher::apply_now)) go through the same
/// replayable executor a hosted surface's do, attributed to a test author,
/// onto the document the test hands the surface. Invocations it queues are
/// resolved by a host only; test those through [`TestApp`].
pub fn dispatcher<R: Resource>(app_id: &'static str) -> super::Dispatcher<R> {
    super::Dispatcher::new(app_id, R::KIND, false, Some(AUTHOR))
}

/// Resolve what `dispatcher` ([`dispatcher`]) queued since the last call —
/// its actions, typed values and groups — onto `doc` as a host does after an
/// event, as confirmed: each invocation one step, attributed to the test
/// author. An action its resource refuses (its clip is gone) changes
/// nothing and is reported through [`Dispatcher::refused`](super::Dispatcher::refused), as the host
/// reports it. Returns what each applied invocation applied, in order: one
/// entry per undo step (a group's entry holds all of its actions).
pub fn resolve<R: Resource>(
    dispatcher: &super::Dispatcher<R>,
    doc: &mut tilo_resource::Doc,
) -> Result<Vec<Vec<R::Action>>, String> {
    let mut applied = Vec::new();
    let mut any_refused = false;
    while let Some((invocation, _)) = dispatcher.core().take_attributed() {
        if invocation.address.resource != dispatcher.core().resource_id() {
            return Err(format!(
                "`{}` on `{}` is not an action of {}",
                invocation.address.action,
                invocation.address.resource,
                R::LABEL
            ));
        }
        let now_ms = tilo_resource::origin_now_ms();
        let actions = if invocation.address.action == tilo_resource::GROUP {
            tilo_resource::group::build_group::<R>(&invocation.arguments, now_ms)?.actions
        } else {
            vec![tilo_resource::build_action::<R>(
                &invocation.address.action,
                &invocation.arguments,
                now_ms,
            )?]
        };
        let mut refused = None;
        for action in &actions {
            if let Err(error) =
                dispatcher.apply_confirmed(&mut ui_core::widget::Edits::lend(doc), action)
            {
                refused = Some(error.to_string());
                break;
            }
        }
        match refused {
            Some(reason) => {
                any_refused = true;
                dispatcher.core().refuse(&invocation.address.action, reason);
            }
            None => applied.push(actions),
        }
    }
    // `doc` is decided by nobody else: what the views said they did is
    // kept at once (`Dispatcher::say_when_kept`), unless it was refused.
    for text in dispatcher.core().take_notices() {
        if !any_refused {
            dispatcher.core().kept(&text);
        }
    }
    Ok(applied)
}

/// The search source the framework builds for `R` from `entries` — its
/// resource's and its surfaces' — for testing what an app's entries find and
/// how their hits read without a window.
pub fn search_source<R: Resource>(
    entries: Vec<tilo_resource::SearchEntry>,
) -> impl ui_core::search::SearchProvider {
    super::root::ResourceSearch::new(
        R::LABEL,
        std::sync::Arc::new(super::root::IndexedEntries::built(entries, 0)),
    )
}

/// Invoke every action `app` publishes, one after another, as a headless
/// drive's `invoke` steps do (`FrameHarness::invoke_action`: the door's own
/// resolution, availability, confirmation and strict binding), each with
/// [`sample_arguments`] and confirmed. Answers the ones the door could not
/// address, as `resource action: why`: published yet reachable only by
/// clicking. An action an earlier one withdrew (its row was removed) is not
/// asked; one that is unavailable now, refuses the sample as arguments (a
/// payload only another answer can build, such as a review to restore), or
/// ran and failed at its job, was reached.
pub fn unreachable_actions<A: ResourceApp>(app: &mut HeadlessApp<A>) -> Vec<String> {
    let surveyed = tilo_actions::door::survey(&app.harness().resources());
    let mut unreachable = Vec::new();
    for descriptor in surveyed {
        let address = descriptor.address.clone();
        let arguments = sample_arguments(&descriptor);
        let resources = app.harness().resources();
        let Some(spec) = tilo_actions::door::resolve(&resources, &address) else {
            continue;
        };
        let phrase = match spec.confirmation_for_json(&arguments) {
            tilo_actions::ConfirmationPolicy::Phrase { phrase } => Some(phrase),
            tilo_actions::ConfirmationPolicy::None | tilo_actions::ConfirmationPolicy::Confirm => {
                None
            }
        };
        let answer = app
            .harness()
            .invoke_action(&address, &arguments, true, phrase.as_deref());
        if let Err(error) = answer
            && error.code() == "unknown_action"
        {
            unreachable.push(format!("{} {}: {error}", address.resource, address.action));
        }
    }
    unreachable
}

/// Arguments `descriptor`'s parameters accept, for a test that invokes an
/// action without knowing it: each required parameter gets its first
/// choice, else what its schema names (a default, a constant, the first
/// enumerated value) or the least value of its type the schema admits, and
/// a resource parameter the action's own resource. Optional parameters are
/// left out, as a caller who knows no better leaves them.
pub fn sample_arguments(
    descriptor: &tilo_actions::ActionDescriptor,
) -> serde_json::Map<String, serde_json::Value> {
    descriptor
        .params
        .iter()
        .filter(|param| param.required)
        .map(|param| (param.name.to_string(), sample_param(descriptor, param)))
        .collect()
}

fn sample_param(
    descriptor: &tilo_actions::ActionDescriptor,
    param: &tilo_actions::ActionParam,
) -> serde_json::Value {
    use tilo_actions::ParamType;
    let numeric = matches!(param.ty, ParamType::Integer | ParamType::Number);
    if let Some(choice) = param.choices.first() {
        return numeric
            .then(|| serde_json::from_str(choice).ok())
            .flatten()
            .unwrap_or_else(|| serde_json::Value::String(choice.to_string()));
    }
    if let Some(value) = param.schema.as_deref().and_then(sample_schema) {
        return value;
    }
    let schema = param.schema.as_deref().unwrap_or(&serde_json::Value::Null);
    match param.ty {
        ParamType::Text | ParamType::Secret => sample_string(schema),
        ParamType::Integer => sample_integer(schema),
        ParamType::Number => sample_number(schema),
        ParamType::Boolean => serde_json::Value::Bool(false),
        ParamType::Object => serde_json::Value::Object(serde_json::Map::new()),
        ParamType::Array => serde_json::Value::Array(Vec::new()),
        ParamType::Resource => serde_json::Value::String(descriptor.address.resource.clone()),
        ParamType::DateTime => serde_json::Value::String(SAMPLE_DATE_TIME.to_owned()),
    }
}

const SAMPLE_DATE_TIME: &str = "2026-01-01T09:00:00Z";

/// A value `schema` admits, when it says enough to make one.
fn sample_schema(schema: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(value) = schema.get("default").or_else(|| schema.get("const")) {
        return Some(value.clone());
    }
    let first = |key: &str| {
        schema
            .get(key)
            .and_then(serde_json::Value::as_array)
            .and_then(|values| values.first())
    };
    if let Some(value) = first("examples").or_else(|| first("enum")) {
        return Some(value.clone());
    }
    if let Some(variant) = first("oneOf").or_else(|| first("anyOf")) {
        return sample_schema(variant);
    }
    let ty = match schema.get("type")? {
        serde_json::Value::String(ty) => ty.as_str(),
        serde_json::Value::Array(types) => types
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|ty| *ty != "null")?,
        _ => return None,
    };
    Some(match ty {
        "string" => sample_string(schema),
        "integer" => sample_integer(schema),
        "number" => sample_number(schema),
        "boolean" => serde_json::Value::Bool(false),
        "array" => {
            let items = schema.get("items").and_then(sample_schema);
            let least = schema
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let count = usize::try_from(least).unwrap_or(0);
            serde_json::Value::Array(items.map_or_else(Vec::new, |item| vec![item; count]))
        }
        "object" => {
            let properties = schema.get("properties");
            let required = schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            serde_json::Value::Object(
                required
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|name| {
                        let value = properties
                            .and_then(|properties| properties.get(name))
                            .and_then(sample_schema)
                            .unwrap_or_else(|| serde_json::Value::String("sample".into()));
                        (name.to_owned(), value)
                    })
                    .collect(),
            )
        }
        "null" => serde_json::Value::Null,
        _ => return None,
    })
}

fn sample_string(schema: &serde_json::Value) -> serde_json::Value {
    let text = match schema.get("format").and_then(serde_json::Value::as_str) {
        Some("date-time") => SAMPLE_DATE_TIME.to_owned(),
        Some("date") => "2026-01-01".to_owned(),
        Some("uri" | "url") => "https://example.org/".to_owned(),
        _ => {
            let pattern = schema
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .and_then(sample_pattern);
            if let Some(text) = pattern {
                return serde_json::Value::String(text);
            }
            let length = |key: &str| {
                schema
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|length| usize::try_from(length).ok())
            };
            let least = length("minLength").unwrap_or(0);
            let most = length("maxLength").unwrap_or(usize::MAX);
            "s".repeat(if most < 6 { most } else { least.max(6) })
        }
    };
    serde_json::Value::String(text)
}

/// Text a pattern of one repeated character class admits (`^[0-9a-f]{64}$`,
/// a hash): the class's first character, as many times as it asks.
fn sample_pattern(pattern: &str) -> Option<String> {
    let class = pattern.strip_prefix("^[")?.strip_suffix('$')?;
    let (class, count) = class.split_once("]{")?;
    let count: usize = count.strip_suffix('}')?.parse().ok()?;
    let first = class
        .chars()
        .next()
        .filter(|first| !matches!(first, '^' | '\\'))?;
    Some(first.to_string().repeat(count))
}

fn sample_integer(schema: &serde_json::Value) -> serde_json::Value {
    let bound = |key: &str| schema.get(key).and_then(serde_json::Value::as_i64);
    let least = bound("minimum")
        .or_else(|| bound("exclusiveMinimum").and_then(|bound| bound.checked_add(1)))
        .unwrap_or(0);
    let most = bound("maximum")
        .or_else(|| bound("exclusiveMaximum").and_then(|bound| bound.checked_sub(1)))
        .unwrap_or(i64::MAX);
    // Zero where the bounds admit it, else the bound nearest it.
    serde_json::Value::from(if least > 0 {
        least
    } else if most < 0 {
        most
    } else {
        0
    })
}

fn sample_number(schema: &serde_json::Value) -> serde_json::Value {
    let bound = |key: &str| schema.get(key).and_then(serde_json::Value::as_f64);
    let least = bound("minimum")
        .or_else(|| bound("exclusiveMinimum").map(f64::next_up))
        .unwrap_or(0.0);
    let most = bound("maximum")
        .or_else(|| bound("exclusiveMaximum").map(f64::next_down))
        .unwrap_or(f64::MAX);
    serde_json::Value::from(if least > 0.0 {
        least
    } else if most < 0.0 {
        most
    } else {
        0.0
    })
}
