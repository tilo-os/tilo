//! Testing a framework app the way it runs: the real component runtime,
//! storage, permissions, undo and history, without a window.
//!
//! Prefer [`tilo_resource::Replica`] for the resource's own behaviour (it
//! needs no UI). Use [`HeadlessApp`] to prove what the host adds: that controls
//! invoke actions, that undo reverts them, that a viewer is refused, that
//! state survives a reopen.
//!
//! Nothing here is reached from an app binary's own modes: those use
//! [`super::headless`], so a release app carries none of this module.

use tilo_ui_base::{permissions::MemberId, permissions::Role};
use ui_core::application::{ApplicationOwner, ComponentRunnerInputs};
use ui_core::headless::Driver;
use ui_core::launch::LaunchError;

use tilo_resource::Resource;

use super::app::{ResourceApp, definition, prepare_on, role_permissions};
use super::headless::{AUTHOR, INSTANCE, Kept, surface_of};
use super::root::ResourceRoot;

pub use super::headless::{
    Alone, DATA_DIR_WORKSPACE, HeadlessApp, Host, TEST_WORKSPACE, surface_in,
};

/// The hang bound and bounded waits a test uses, re-exported from the
/// toolkit's `test-support` so an app's tests reach them through `ui`
/// rather than depending on the test environment's own crate.
#[cfg(feature = "test-support")]
pub use ui_core::testing::{hang_bound, hang_deadline, wait_for, wait_until};

/// What interactions cost an app on the UI thread, phase by phase.
#[cfg(feature = "test-support")]
pub mod perf;

/// Whether every published action is reachable through the API.
#[cfg(feature = "test-support")]
mod reach;
#[cfg(feature = "test-support")]
pub use reach::{
    check_scenarios_reach_every_action, unreachable_actions, unreachable_actions_in_share,
};

/// The windows [`check_window_sidebar_button`] opens an app in: one wide
/// enough to dock a sidebar beside any content, and one so narrow the
/// sidebar is a screen of its own or a drawer over the content.
pub const SIDEBAR_BUTTON_WINDOWS: [(u32, u32); 2] = [(1280, 800), (400, 800)];

/// Open the app with `open` in each of [`SIDEBAR_BUTTON_WINDOWS`], shown in
/// the desktop's window bar, and click the bar's sidebar button there: every
/// click flips the sidebar, on the bar, in the window's shell and on screen
/// ([`Driver::window_sidebar_shown`]). A narrow window keeps the button on
/// the sidebar's own screen, behind the screen the person is on, and the
/// check takes the bar's way back to it.
pub fn check_window_sidebar_button<A: ResourceApp, H: Host>(
    mut open: impl FnMut() -> Result<HeadlessApp<A, H>, LaunchError>,
) -> Result<(), String> {
    for viewport in SIDEBAR_BUTTON_WINDOWS {
        let mut app = open().map_err(|error| format!("{viewport:?}: the app opens: {error}"))?;
        let driver = app.harness();
        driver
            .resize(viewport)
            .map_err(|error| format!("{viewport:?}: {error}"))?;
        driver.present_in_window_bar();
        check_sidebar_flips(driver).map_err(|error| format!("{viewport:?}: {error}"))?;
    }
    Ok(())
}

/// Click the window's sidebar button three times, taking the bar's way back
/// to it where the window put it aside, and check that each click flipped
/// the sidebar with the bar, the shell and the frame agreeing every time.
fn check_sidebar_flips(driver: &mut Driver) -> Result<(), String> {
    // Each reading is of the window at rest, as a person sees it once the sidebar's motion
    // landed.
    let rest =
        |driver: &mut Driver| driver.settle_for_capture(ui_core::time::Duration::from_secs(2));
    let mut expected = None;
    for press in 0..=3 {
        rest(driver);
        if !driver.shows_window_sidebar_button() && takes_the_bars_way_back(driver) {
            rest(driver);
        }
        // The flip moves: read it once the window has run the frames that settle it, when bar,
        // shell and frame agree.
        driver.idle();
        let shown = driver.window_sidebar_shown()?;
        let shown = shown.agreed().ok_or_else(|| {
            format!("after {press} clicks the bar, shell and frame disagree: {shown:?}")
        })?;
        if expected.is_some_and(|expected| expected != shown) {
            return Err(format!(
                "click {press} left the sidebar {}",
                if shown { "shown" } else { "hidden" }
            ));
        }
        expected = Some(!shown);
        if press == 3 {
            break;
        }
        driver.click_window_sidebar_button()?;
    }
    Ok(())
}

/// Press the window bar's way back, where it shows one: what leads a narrow
/// window back to the screen that holds its sidebar button.
fn takes_the_bars_way_back(driver: &mut Driver) -> bool {
    let back = driver
        .window_toolbar()
        .and_then(|toolbar| toolbar.navigation)
        .is_some_and(|navigation| navigation.back);
    if back {
        driver.press_window_part(ui_core::window_toolbar::ToolbarEvent::Back);
    }
    back
}

/// Run `visit` on the custom surface of type `S` that `A`'s view hosts in
/// `driver` (what [`ViewCx::surface`](super::ViewCx::surface) mounted), then
/// the frames that follow; `None` when the view hosts no such surface at its
/// root. For a test that drives a component mounted another way than
/// [`HeadlessApp`] (two windows over one record).
pub fn with_surface<A: ResourceApp, S: tilo_ui_base::widget::Widget + 'static, T>(
    driver: &mut Driver,
    visit: impl FnOnce(&mut S) -> T,
) -> Option<T> {
    driver
        .with_widget_mut(|root: &mut ResourceRoot| surface_of::<A, S>(root).map(visit))
        .flatten()
}

/// The custom surface of type `S` that `A`'s view hosts, when `widget` is
/// `A`'s root as a host mounted it: a tab of `A` in a shell, found in the
/// host's own widget map.
pub fn surface_in_root<A: ResourceApp, S: tilo_ui_base::widget::Widget + 'static>(
    widget: &mut dyn tilo_ui_base::widget::Widget,
) -> Option<&mut S> {
    surface_of::<A, S>(widget.as_any_mut()?.downcast_mut::<ResourceRoot>()?)
}

/// A dispatcher outside any host, for a unit test of a surface (or of the
/// model behind it) that edits through one: each immediate action
/// ([`Dispatcher::apply_now`](super::Dispatcher::apply_now)) applies to the document handed to it,
/// attributed to `author`, through the same executor as a hosted one, so
/// the test exercises the surface's real write path. Queued invocations
/// ([`Dispatcher::apply`](super::Dispatcher::apply)) are not taken by anyone: drive those through
/// [`HeadlessApp`].
pub fn detached_dispatcher<R: Resource>(author: &tilo_resource::MemberId) -> super::Dispatcher<R> {
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
/// Its records are kept in the workspace of a fresh private profile, which
/// takes each write as the window sends it.
pub fn drive_live_window<A: ResourceApp, T>(
    drive: impl FnOnce(&mut ui_core::App) -> T,
) -> Result<T, LaunchError> {
    drive_window_on::<A, T>(&Kept::fresh()?, drive)
}

/// [`drive_live_window`] on a workspace slower than the window's frames: while `drive` runs it
/// answers a write only to a writer that waits for it, so the window shows its records as saving
/// throughout, as it does while someone types faster than the workspace answers. What a frame
/// costs is then the same whether or not an answer came in during it. The workspace catches up
/// once `drive` is done, and the window closes as it would.
pub fn drive_live_window_answered_late<A: ResourceApp, T>(
    drive: impl FnOnce(&mut ui_core::App) -> T,
) -> Result<T, LaunchError> {
    drive_window_on::<A, T>(&Kept::slow()?, drive)
}

fn drive_window_on<A: ResourceApp, T>(
    profile: &std::rc::Rc<Kept>,
    drive: impl FnOnce(&mut ui_core::App) -> T,
) -> Result<T, LaunchError> {
    let host = private_host::<A>(profile, TEST_WORKSPACE)?;
    let mut app = live_window::<A>(host, profile.path())?;
    let seen = drive(&mut app);
    // Whatever the window still shows as saving is answered now: its close waits for that.
    profile.catch_up();
    app.close_components();
    Ok(seen)
}

/// The host of `A`'s window on the private `profile`, keeping its records in `workspace` there:
/// nothing carries them anywhere else, so the window writes its own.
fn private_host<A: ResourceApp>(
    profile: &std::rc::Rc<Kept>,
    workspace: &str,
) -> Result<ApplicationOwner, LaunchError> {
    ApplicationOwner::with_port(
        Some((profile.records(), true)),
        definition::<A>().app_id(),
        "test-device".into(),
        workspace,
        MemberId::person(AUTHOR),
        role_permissions(workspace, AUTHOR, Role::Owner),
        tilo_actions::ActionContext::default(),
    )
}

/// The live window `A` opens on `host`, mounted as a real window is: the
/// component, with the content store the host lends every window, so an
/// action past the history budget keeps its large parts there rather than
/// being refused.
fn live_window<A: ResourceApp>(
    mut host: ApplicationOwner,
    directory: &std::path::Path,
) -> Result<ui_core::App, LaunchError> {
    let (component, _, inputs) = live_window_component::<A>(&mut host, directory)?;
    ui_core::App::from_component(component, inputs)
}

/// [`live_window`] as the parts a test reads what a window publishes and
/// shows through: the component, the content store it was lent and what the
/// host hands a window's runtime.
fn live_window_component<A: ResourceApp>(
    host: &mut ApplicationOwner,
    directory: &std::path::Path,
) -> Result<
    (
        ui_core::application::AppComponent,
        tilo_content::ContentStore,
        ComponentRunnerInputs,
    ),
    LaunchError,
> {
    let definition = definition::<A>();
    let mut prepared = prepare_on(
        host,
        definition,
        INSTANCE,
        None,
        ComponentRunnerInputs::from_native_process(),
    )?;
    let content = super::headless::window_content(directory);
    prepared.context.provide_content(content.clone());
    let component = definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
    Ok((component, content, prepared.runner_inputs))
}

/// The live window `A` opens on a host that keeps its records in `workspace`,
/// mounted as a real one is: what it shows and publishes, read through
/// a [`Driver`], and the content store the host lent it.
pub fn live_window_in_workspace<A: ResourceApp>(
    workspace: &str,
) -> Result<(Driver, tilo_content::ContentStore), LaunchError> {
    let profile = Kept::fresh()?;
    let mut host = private_host::<A>(&profile, workspace)?;
    let (component, content, inputs) = live_window_component::<A>(&mut host, profile.path())?;
    let driver = Driver::from_component(component, (1024, 768), inputs)?;
    Ok((driver, content))
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
    let profile = Kept::fresh()?;
    let host = private_host::<A>(&profile, TEST_WORKSPACE)?;
    let app = live_window::<A>(host, profile.path())?;
    let mut tile = ui_core::app::EmbeddedApp::new(app, |_| {})?;
    let seen = drive(&mut tile);
    tile.app_mut().close_components();
    Ok(seen)
}

/// A dispatcher of `R` for testing a surface's own methods without a host:
/// the edits it applies now ([`Dispatcher::apply_now`](super::Dispatcher::apply_now)) go through the same
/// replayable executor a hosted surface's do, attributed to a test author,
/// onto the document the test hands the surface. Invocations it queues are
/// resolved by a host only; test those through [`HeadlessApp`].
pub fn dispatcher<R: Resource>(app_id: &'static str) -> super::Dispatcher<R> {
    super::Dispatcher::new(app_id, R::KIND, false, Some(&MemberId::person(AUTHOR)))
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
                dispatcher.apply_confirmed(&mut tilo_ui_base::widget::Edits::lend(doc), action)
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
        R::APP_ID,
        std::sync::Arc::new(super::root::IndexedEntries::built(entries, 0)),
    )
}
