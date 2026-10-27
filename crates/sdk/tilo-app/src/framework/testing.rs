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

pub use super::headless::{HeadlessApp, HostedApp, surface_in};

/// What interactions cost an app on the UI thread, phase by phase.
#[cfg(feature = "test-support")]
pub mod perf;

/// Whether every published action is reachable through the door.
#[cfg(feature = "test-support")]
mod reach;
#[cfg(feature = "test-support")]
pub use reach::unreachable_actions;

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
