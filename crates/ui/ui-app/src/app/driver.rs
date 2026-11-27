//! An [`App`] on a surface with no display: what a test, a capture, a
//! scenario and a browser page drive. The app runs exactly as it does in a
//! window of its own — its frames, input routing, focus, palette, History and
//! the actions it publishes are the app's — and the [`Driver`] only plays the
//! display: it says the surface's size and scale, hands the app the input a
//! person would make, runs the frames a display would ask for on a clock the
//! caller controls, and rasterises the scene a frame painted.

use std::any::TypeId;
use std::collections::HashMap;

use taffy::NodeId;
use vello::Scene;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, WindowEvent};

use super::{App, AppSurface, SurfaceDamage};

mod apps_a;
mod capture;
mod cards;
pub(crate) mod chrome;
pub(crate) mod controls;
pub(crate) mod drive;
pub(crate) mod fixtures;
pub(crate) mod frame;
mod jobs;
#[cfg(not(target_arch = "wasm32"))]
mod launch;
mod layout_check;
pub(crate) mod metrics;
mod palette;
#[cfg(feature = "model")]
mod peer;
mod pictures;
mod session;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod uicore;
mod wake;
mod widget;
mod window_bar;
#[cfg(feature = "model")]
mod workspace_side;
use crate::headless::{
    AccessibilityAudit, InspectionNode, InspectionSnapshot, NativeAppAudit, NativeAppIssue,
    SemanticExpectation,
};
use crate::state::State;
pub use apps_a::{WindowBarNotice, WindowBarNoticeButton};
#[cfg(feature = "model")]
pub use palette::FollowedView;
#[cfg(feature = "model")]
use tilo_permissions::PermissionContext;
use tilo_ui_base::a11y::AccessNode;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
pub use wake::OwedSettle;
#[cfg(feature = "model")]
pub use workspace_side::{ProvidedInvocation, ServedProvider, ServedReads, ShownHost};

/// The surface a [`Driver`] shows its app on: a size and a scale, with
/// nothing behind them. What the app asks of it (another frame, a pointer
/// shape) is kept for the driver to read.
struct Surface {
    viewport: std::cell::Cell<(u32, u32)>,
    scale: std::cell::Cell<f64>,
    redraw: std::cell::Cell<bool>,
    cursor: std::cell::Cell<winit::window::CursorIcon>,
    /// The window's title, which its bar wears.
    title: std::cell::RefCell<String>,
    maximized: std::cell::Cell<bool>,
    /// The host leaves the window's frame to the app: its bar carries the
    /// window's own controls ([`Driver::host_window_controls`]).
    frames_itself: std::cell::Cell<bool>,
    /// What the window's own controls asked of the window, oldest first.
    window_actions: std::cell::RefCell<Vec<crate::part_row::WindowAction>>,
}

impl Surface {
    fn new(viewport: (u32, u32), scale: f64) -> Self {
        Self {
            viewport: std::cell::Cell::new(viewport),
            scale: std::cell::Cell::new(scale),
            redraw: std::cell::Cell::new(true),
            cursor: std::cell::Cell::new(winit::window::CursorIcon::Default),
            title: std::cell::RefCell::new(String::new()),
            maximized: std::cell::Cell::new(false),
            frames_itself: std::cell::Cell::new(false),
            window_actions: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The surface's size in device pixels.
    fn device_size(&self) -> (u32, u32) {
        let (width, height) = self.viewport.get();
        let scale = self.scale.get();
        (
            tilo_ui_base::num::u32_of_f64((f64::from(width) * scale).round()),
            tilo_ui_base::num::u32_of_f64((f64::from(height) * scale).round()),
        )
    }
}

impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        self.scale.get()
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        let (width, height) = self.device_size();
        winit::dpi::PhysicalSize::new(width, height)
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
    fn set_cursor(&self, cursor: winit::window::CursorIcon) {
        self.cursor.set(cursor);
    }
    fn hosts_tab_chords(&self) -> bool {
        // No shell stands around this surface: the chords are the app's.
        true
    }
}

impl super::part_bar::BarWindow for Surface {
    fn fullscreen(&self) -> bool {
        false
    }

    fn maximized(&self) -> bool {
        self.maximized.get()
    }

    fn device_size(&self) -> (u32, u32) {
        Surface::device_size(self)
    }

    fn scale_factor(&self) -> f64 {
        self.scale.get()
    }

    fn title(&self) -> String {
        self.title.borrow().clone()
    }

    fn app_draws_frame(&self, _platform: tilo_ui_base::platform::PlatformConventions) -> bool {
        self.frames_itself.get()
    }

    fn act(&self, action: crate::part_row::WindowAction) {
        if action == crate::part_row::WindowAction::ToggleMaximize {
            self.maximized.set(!self.maximized.get());
        }
        self.window_actions.borrow_mut().push(action);
    }

    fn request_redraw(&self) {
        self.redraw.set(true);
    }
}

/// How the [`Driver`]'s first frames are made: the theme, the fonts and who
/// the person at the surface is.
pub struct DriverInputs {
    pub theme: Theme,
    pub text: TextRenderer,
    pub author: tilo_actions::MemberId,
}

impl DriverInputs {
    /// The theme `theme`, the process's fonts and the process's person.
    #[must_use]
    pub fn themed(theme: Theme) -> Self {
        Self {
            theme,
            text: TextRenderer::new(),
            author: super::local_person(),
        }
    }
}

/// Construction options for [`Driver::for_root_with`].
#[derive(Default)]
pub struct FrameOptions {
    /// Defaults to [`Theme::dark`].
    pub theme: Option<Theme>,
    /// Defaults to [`TextRenderer::new`]; its text scale follows the theme.
    pub text: Option<TextRenderer>,
    /// The style of the node the root is laid out in, for a root that must not be stretched by the
    /// default row: its size is replaced by the viewport.
    pub host_style: Option<taffy::Style>,
    /// Lay the root out at its content height against unbounded height, as a scrolling host lays the
    /// document inside it; the viewport still bounds culling and rasterisation.
    pub content_height: bool,
}

/// An [`App`] run on a surface with no display. See the module documentation.
pub struct Driver {
    app: Box<App>,
    surface: Surface,
    scene: Scene,
    damage: SurfaceDamage,
    touch: tilo_ui_base::interaction::TouchPointerEmulator,
    /// Concrete type of the root when it was mounted through [`Driver::for_root`]; checks
    /// [`Driver::root`].
    root_type: Option<TypeId>,
    /// The app a concrete root is ([`Driver::for_app`]): its kind and title, which its window's bar
    /// wears.
    window_app: Option<(crate::widget::AppKind, String)>,
    /// Frames run since construction.
    frames: u64,
    /// The window's bar, once the app is shown as a window of its own presents it
    /// ([`Self::present_in_window_bar`]). Boxed: the bar is tens of kilobytes inline, and a driver
    /// a test moves by value through its helpers would carry all of it on the stack.
    bar: Option<Box<super::part_bar::PartBar>>,
    /// The bar stands at rest ([`Self::present_window_bar_at_rest`]): it hosts the app's parts but
    /// stands out of the way over the window's top edge, and the app's frame fills the window.
    bar_at_rest: bool,
    /// The window the app declares its parts on while its bar is presented, so that drivers in
    /// one thread keep their bars apart; current while the driver lives and entered again around
    /// each frame and input.
    window: Option<(
        crate::hosted_window::HostedWindow,
        crate::window_toolbar::WindowScope,
    )>,
    /// The peer standing for the workspace's broker, once connected ([`Self::connect_peer`]).
    #[cfg(feature = "model")]
    peer: Option<peer::Peer>,
    metrics: metrics::Metrics,
    /// What the driver keeps of the calls it made and the requests it read ([`jobs`]).
    jobs: jobs::Jobs,
    /// What a host that poses and compares frames keeps of them ([`session`]).
    records: session::FrameRecords,
    /// The owner models a worker woke for this tree's components ([`session`]).
    wakes: session::OwnerWakes,
    /// What a host placed over the app ([`uicore`]).
    overlay: uicore::Overlay,
    /// The pictures last handed to a page's composition ([`pictures`]).
    pictures: pictures::Pictures,
    /// The cards the app stands over its window ([`cards`]).
    cards: cards::Cards,
}

/// The name the window's bar goes by in a scenario's `perform` step: its parts are performed on it
/// by their targets.
pub const WINDOW_PARTS: &str = "window-parts";

/// The frames a `press` waits for the control it names to be shown: a menu the step before opened
/// draws a frame or two later, and the row an action made is laid out in the frame after it.
const PRESS_FRAMES: usize = 8;

/// How long a step that reads a call's answer — what it binds, or the refusal it expects — waits
/// for an app that runs the call off this window's thread. A hosted app answers in a frame or two;
/// one that never answers fails the step, naming the action, rather than holding the drive.
#[cfg(feature = "model")]
const ANSWER_WAIT: crate::time::Duration = crate::time::Duration::from_secs(30);

impl Driver {
    /// The driver over `app`, a window the launch built
    /// ([`crate::launch::LaunchSpec::build_window`]), in a surface of `viewport` logical pixels,
    /// after its first frame.
    #[must_use]
    pub fn showing(app: App, viewport: (u32, u32)) -> Self {
        Self::on_app(Box::new(app), viewport)
    }

    fn on_app(app: Box<App>, viewport: (u32, u32)) -> Self {
        let mut driver = Self::built(app, viewport);
        // A freshly built app has everything to take up: its first frame syncs the tree, as a
        // window's first frame after mounting does.
        driver.first_frame();
        driver
    }

    /// The driver over `app`, before its first frame runs: a window that mounts an app puts its
    /// workspace in place first, so what the app asks of it while mounting is asked through it.
    fn built(app: Box<App>, viewport: (u32, u32)) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let scale = f64::from(tilo_ui_base::offscreen::capture_device_scale());
        #[cfg(target_arch = "wasm32")]
        let scale = 1.0;
        Self {
            app,
            surface: Surface::new(viewport, scale),
            scene: Scene::new(),
            // A window of its own paints members' pointers and activity over the widgets.
            damage: SurfaceDamage::default().painting_peers(),
            touch: tilo_ui_base::interaction::TouchPointerEmulator::default(),
            root_type: None,
            window_app: None,
            frames: 0,
            bar: None,
            bar_at_rest: false,
            window: None,
            #[cfg(feature = "model")]
            peer: None,
            metrics: metrics::Metrics::default(),
            jobs: jobs::Jobs::default(),
            records: session::FrameRecords::default(),
            wakes: session::OwnerWakes::default(),
            overlay: uicore::Overlay::default(),
            pictures: pictures::Pictures::default(),
            cards: cards::Cards::default(),
        }
    }

    /// The frame a window runs after mounting: the tree syncs, and what it asks of its workspace
    /// goes to the peer standing by.
    fn first_frame(&mut self) {
        self.app.needs_sync = true;
        self.run_frame();
    }

    /// Show `root` over `state` in a surface of `viewport` logical pixels, dark.
    pub fn new(state: State, root: Box<dyn Widget>, viewport: (u32, u32)) -> Self {
        Self::new_with_theme(state, root, viewport, Theme::dark())
    }

    /// [`Self::new`] under `theme`.
    pub fn new_with_theme(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        Self::with_inputs(state, root, viewport, DriverInputs::themed(theme))
    }

    /// [`Self::new`] with caller-owned fonts and identity, without ambient discovery.
    pub fn new_with_controlled_inputs(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
        text: TextRenderer,
        author: tilo_actions::MemberId,
    ) -> Self {
        Self::with_inputs(
            state,
            root,
            viewport,
            DriverInputs {
                theme,
                text,
                author,
            },
        )
    }

    /// Show `root` over `state` drawn with `inputs`.
    pub fn with_inputs(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        inputs: DriverInputs,
    ) -> Self {
        Self::on_app(App::displayless(state, root, inputs), viewport)
    }

    /// Show the concrete `root`, which [`Self::root`] and [`Self::root_mut`] then read as itself.
    pub fn for_root<W: Widget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        let mut driver = Self::new_with_theme(state, Box::new(root), viewport, theme);
        driver.root_type = Some(TypeId::of::<W>());
        driver
    }

    /// [`Self::for_root`] with every construction option spelled out.
    pub fn for_root_with<W: Widget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        options: FrameOptions,
    ) -> Self {
        let FrameOptions {
            theme,
            text,
            host_style,
            content_height,
        } = options;
        let theme = theme.unwrap_or_else(Theme::dark);
        let inputs = DriverInputs {
            text: text.unwrap_or_default(),
            ..DriverInputs::themed(theme)
        };
        let mut app = App::displayless(state, Box::new(root), inputs);
        app.frame_root_in(host_style, content_height);
        let mut driver = Self::on_app(app, viewport);
        driver.root_type = Some(TypeId::of::<W>());
        driver
    }

    /// Show the app `root` is, whose kind and title its window's bar wears.
    pub fn for_app<W: crate::widget::AppWidget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        let kind = root.app_kind();
        let title = root.app_title().to_owned();
        let mut driver = Self::for_root(state, root, viewport, theme);
        driver.window_app = Some((kind, title));
        driver
    }

    /// Mount a prepared component without a display, GPU or service discovery.
    ///
    /// # Errors
    /// As [`App::from_component`].
    #[cfg(feature = "model")]
    pub fn from_component(
        component: crate::application::AppComponent,
        viewport: (u32, u32),
        inputs: crate::application::ComponentRunnerInputs,
    ) -> Result<Self, crate::launch_error::LaunchError> {
        let mut driver = Self::built(
            App::from_component_using(component, inputs, super::AppConstructionInputs::component)?,
            viewport,
        );
        // The workspace is the window's before its first frame: what mounting asks of the
        // workspace is asked through the peer, and kept for whoever reads the window's requests.
        driver.app.needs_sync = true;
        driver.connect_peer();
        Ok(driver)
    }

    /// Mount `component` in a surface of [`crate::headless::MOUNT_VIEWPORT`] with the fixture fonts
    /// and the dark theme.
    ///
    /// # Errors
    /// The setup failure a live window fails with when the component cannot be mounted.
    #[cfg(feature = "model")]
    #[cfg(any(test, feature = "test-support"))]
    pub fn mount(
        component: crate::application::AppComponent,
    ) -> Result<Self, crate::launch_error::LaunchError> {
        // A bare mount, not a window: no workspace stands behind it, so what the component asks
        // of one waits, as an unlaunched component's does.
        let viewport = crate::headless::MOUNT_VIEWPORT;
        let app = App::from_component_using(
            component,
            crate::headless::fixture_runner_inputs(),
            super::AppConstructionInputs::component,
        )?;
        let mut driver = Self::built(app, viewport);
        driver.first_frame();
        Ok(driver)
    }

    /// Stand a managed root up and run its first frame.
    ///
    /// # Errors
    /// Refuses invalid scope metadata or unavailable mounted document ownership.
    #[cfg(feature = "model")]
    pub fn from_managed_root(
        state: State,
        root: crate::history_mode::ManagedRoot,
        viewport: (u32, u32),
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        Self::from_managed_root_with(state, root, viewport, DriverInputs::themed(Theme::dark()))
    }

    /// [`Self::from_managed_root`] with caller-owned fonts and identity.
    ///
    /// # Errors
    /// As [`Self::from_managed_root`].
    #[cfg(feature = "model")]
    pub fn from_managed_root_with_controlled_inputs(
        state: State,
        root: crate::history_mode::ManagedRoot,
        viewport: (u32, u32),
        theme: Theme,
        text: TextRenderer,
        author: tilo_actions::MemberId,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        Self::from_managed_root_with(
            state,
            root,
            viewport,
            DriverInputs {
                theme,
                text,
                author,
            },
        )
    }

    #[cfg(feature = "model")]
    fn from_managed_root_with(
        state: State,
        root: crate::history_mode::ManagedRoot,
        viewport: (u32, u32),
        inputs: DriverInputs,
    ) -> Result<Self, crate::history_mode::HistoryRegistrationError> {
        root.history.validate()?;
        root.history.validate_root(root.widget.as_ref())?;
        let mut app = App::displayless(state, root.widget, inputs);
        app.install_history(root.history)?;
        Ok(Self::on_app(app, viewport))
    }

    /// The app this driver runs.
    #[must_use]
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The app this driver runs, mutable.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// The app this driver runs, taken out of it: for a host that goes on to show it on a display.
    #[must_use]
    pub fn into_app(self) -> App {
        *self.app
    }

    // ---- frames ----

    /// One frame of the app, as a display asks for one: its model pumped, synchronized, laid out and
    /// painted into the driver's scene. A frame whose sync or layout left more to settle runs again
    /// at once, so what the driver shows is the app at rest. Whether the app wants another frame.
    fn run_frame(&mut self) -> bool {
        let _running = self.running();
        // What the widgets' update moves — a pan, a fade, a closure that answers what it waits for —
        // advances in the surface frame's own model pump, which a window runs before it synchronizes.
        let mut again = true;
        for _ in 0..tilo_ui_base::widget::SETTLE_FRAMES {
            // The bar first, as a window's frame refreshes it: its height is part of the app's
            // top inset.
            if self.bar_at_rest {
                self.rest_bar();
            } else if let Some(bar) = self.bar.as_mut() {
                let _shown = bar.refresh(&mut self.app, &self.surface);
            }
            self.app.surface_accessibility();
            self.scene.reset();
            self.surface.redraw.set(false);
            let declared = crate::window_toolbar::revision();
            let recording = self.records.begin(&self.scene);
            again = self.app.surface_frame_with_damage(
                &self.surface,
                &mut self.scene,
                &mut self.damage,
            );
            // The cards the app asked for in this frame's update stand, and draw over it, before
            // the next frame takes input: a card that came, went or moved runs that frame now.
            if self.follow_cards() {
                again = true;
            }
            self.draw_cards();
            self.records.finish(recording, &self.scene);
            // What the app declared for its bar in this frame's sync (a part's new name after a
            // press) shows in the next frame, which runs at once, as a window's bar shows it in
            // the frame after the declaration.
            let redeclared = self.bar.is_some() && crate::window_toolbar::revision() != declared;
            if let Some(bar) = self.bar.as_mut().filter(|_| !self.bar_at_rest) {
                let transform = vello::kurbo::Affine::scale(self.surface.scale.get());
                // The app has laid out this frame: read again where its row holds the bar, so
                // the window's controls follow a width this frame changed.
                bar.rehold(&self.app);
                bar.draw(&mut self.app, &mut self.scene, transform);
                let _ = bar.take_repaint();
                again |= bar.needs_frame();
            }
            // The pointers a host placed over the app are drawn last, as a window draws the
            // members' pointers over everything.
            self.paint_placed_pointers();
            self.frames = self.frames.wrapping_add(1);
            if redeclared {
                continue;
            }
            // What the peer answered during the frame (the broker's reply to a call the app
            // made) is taken up at once, as a window's next frame takes up a socket's.
            if (!again || !self.app.unsettled()) && !self.peer_holds_delivery() {
                break;
            }
        }
        let _ = self.damage.take();
        // The semantics, and the placing of every widget far from any view they read, are built
        // when something reads them ([`Self::semantic_tree`]), as a window builds them when an
        // assistive technology asks: never on every frame, where a walk of every widget costs a
        // frame of a scroll more than drawing it.
        self.note_retained_frame();
        self.check_retained_geometry_if_due();
        // What the app asked of its broker in this frame is answered in the frame after.
        #[cfg(feature = "model")]
        if let Some(serve) = self.peer.as_ref().map(|peer| peer.serve)
            && serve(self)
        {
            return self.run_frame();
        }
        again
    }

    /// Run a frame now, whatever asked for it, as a caller asks for one after changing anything:
    /// the model is refreshed whole ([`Self::refresh_model`]) and every component pass runs
    /// whatever its observation says ([`tilo_ui_base::component_inputs::forcing`]), so what was written
    /// past the framework is shown and published.
    pub fn relayout(&mut self) {
        self.take_up_reported_layout();
        tilo_ui_base::component_inputs::forcing(|| self.refresh_model());
    }

    /// Run one frame at `viewport`.
    ///
    /// # Errors
    /// Never: a frame whose layout fails is skipped, as every host skips it, and the next frame lays
    /// out again.
    pub fn relayout_at(
        &mut self,
        viewport: (u32, u32),
    ) -> Result<(), tilo_ui_base::widget::WidgetLayoutError> {
        self.surface.viewport.set(viewport);
        self.take_up_reported_layout();
        self.run_frame();
        Ok(())
    }

    /// A frame the host asked for places the tree as it stands: a caller asks for one after changing
    /// what its widgets read from outside them (a row slid down, a pane the host moved), which the
    /// tree reports nothing of, so the frame's own pass would keep the old geometry. Placing it here
    /// leaves the frame quiet: nothing was rebuilt, solved or marked, and a window at rest still owes
    /// nothing after it. What the widgets report since the last frame — a size that moved, a region
    /// that asks for a measure — is taken up as a frame after an event takes it.
    fn take_up_reported_layout(&mut self) {
        if let Some(root) = self.app.layout.root_node {
            // Placed on the surface's device pixels, as its frames lay the widgets out: a list's
            // offset snapped to another grid stands its rows between pixels.
            let _grid = tilo_ui_base::widget::layout::enter_device_scale(self.surface.scale.get());
            let tree = tilo_ui_base::widget::layout::TreeId::of(&self.app.layout.taffy);
            tilo_ui_base::widget::layout::placing_again_in(tree, || {
                self.app.apply_history_presentation(root);
            });
            // What stood under a still pointer may have moved: the next press replays where the
            // pointer is, as a frame that placed moved geometry does.
            self.app.input.pointer_replay.mark_unsettled();
        }
        let root = self.app.root_widget.as_ref();
        if root.is_layout_dirty() || root.layout_needs_measure() || root.needs_redraw() {
            self.app.needs_sync = true;
        }
    }

    /// Run one frame that rebuilds the whole layout tree, as a structural change does.
    ///
    /// # Errors
    /// As [`Self::relayout_at`].
    pub fn recompose(&mut self) -> Result<(), tilo_ui_base::widget::WidgetLayoutError> {
        self.app.layout.request_rebuild();
        self.run_frame();
        Ok(())
    }

    /// [`Self::relayout_at`] under `theme`, which later frames keep.
    ///
    /// # Errors
    /// As [`Self::relayout_at`].
    pub fn relayout_themed(
        &mut self,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Result<(), tilo_ui_base::widget::WidgetLayoutError> {
        // The theme is the window's whole appearance, as it is when the window opens: its scale
        // and its comforts are worn as composed, not as a base the person's settings paint over.
        self.app.adopt_theme(theme);
        self.relayout_at(viewport)
    }

    /// Resize the surface to `viewport` logical pixels and run the frame that lays the app out at it.
    ///
    /// # Errors
    /// As [`Self::relayout_at`].
    pub fn resize(
        &mut self,
        viewport: (u32, u32),
    ) -> Result<(), tilo_ui_base::widget::WidgetLayoutError> {
        if self.surface.viewport.get() == viewport {
            return Ok(());
        }
        self.relayout_at(viewport)
    }

    /// The surface's size in logical pixels.
    #[must_use]
    pub fn viewport(&self) -> (u32, u32) {
        self.surface.viewport.get()
    }

    /// The next instant the app asks for a frame: a widget's own deadline, and what presence
    /// keeps moving on its behalf — a member's typing going quiet, the fade of what they wrote.
    /// The live host reads the presence deadlines beside the tree's, as a window's frame clock
    /// does; a driver that read only the tree would never wake to clear them.
    fn next_frame_deadline(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
        self.next_frame_deadline_moving(now, self.app.surface_moving(now))
    }

    /// [`Self::next_frame_deadline`] with whether the tree moves (`moving`, [`App::surface_moving`])
    /// already found: a caller that asked it for the schedule shares that walk rather than
    /// walking the tree for it again.
    fn next_frame_deadline_moving(
        &self,
        now: crate::time::Instant,
        moving: bool,
    ) -> Option<crate::time::Instant> {
        [
            self.app.surface_next_frame_moving(now, moving),
            self.app.presence_ctx.interaction_next_wake_deadline(now),
            self.app.presence_ctx.widgets_change_at(now),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// What an idle window does for presence between frames: a "just typed" mark steps down, a
    /// composer gone quiet drops out of "… is typing", and the step that came due paints once.
    /// The live host's idle tick does the same before it asks whether to draw
    /// ([`crate::app::App::service_native_idle_model`]'s presence pass).
    fn presence_tick(&mut self, now: crate::time::Instant) {
        let step_due = self
            .app
            .presence_ctx
            .widgets_change_at(now)
            .is_some_and(|due| due <= now)
            || self
                .app
                .presence_ctx
                .interaction_next_wake_deadline(now)
                .is_some_and(|due| due <= now);
        if step_due {
            self.surface.redraw.set(true);
        }
        self.app.presence_ctx.prune_typing(now);
        self.app.presence_ctx.prune_composing(now);
    }

    /// Whether a frame is owed: the app asked for one, its background work woke it, a deadline it
    /// set is due, or a worker woke one of its components' owner models for an idle pass to serve.
    #[must_use]
    pub fn frame_owed(&self) -> bool {
        let now = crate::time::now();
        let moving = self.app.surface_moving(now);
        self.frame_owed_by(now, moving, || self.next_frame_deadline_moving(now, moving))
    }

    /// [`Self::frame_owed`] at `now`, with the tree's next deadline asked of `deadline` only when
    /// nothing cheaper already owes the frame: a caller that has the deadline at hand passes it
    /// rather than walking the tree for it again. `moving` is [`App::surface_moving`] at `now`, as
    /// the caller found it: a tree that moves owes the frame, so the redraw walk is skipped.
    fn frame_owed_by(
        &self,
        now: crate::time::Instant,
        moving: bool,
        deadline: impl FnOnce() -> Option<crate::time::Instant>,
    ) -> bool {
        self.has_pending_owner_models()
            || self.surface.redraw.get()
            || self
                .bar
                .as_deref()
                .is_some_and(super::part_bar::PartBar::needs_frame)
            || self.app.wake_pending()
            || self.app.unsettled()
            // A widget that says it is dirty asks for the frame that paints it, as a display's
            // frame clock asks the tree what is due. A measure demand is no such ask: a window
            // reads it in a frame something else brought, and a leaf that keeps the default
            // `is_layout_dirty` reports one for as long as it lives.
            || moving
            || self
                .app
                .presentation_roots()
                .iter()
                .any(|root| root.needs_redraw())
            || self.peer_holds_delivery()
            || self.peer_owes_app_frame()
            || deadline().is_some_and(|at| at <= now)
    }

    /// Run the frame a deadline made due, if one is: a window's loop draws it before the next
    /// event, so input reaches what the clock places rather than the frame its deadline left.
    fn serve_due_frame(&mut self) {
        let now = crate::time::now();
        if self.next_frame_deadline(now).is_some_and(|at| at <= now) {
            let _ = self.poll_frame();
        }
    }

    /// Run a frame only when one is owed ([`Self::frame_owed`]), as a display's frame clock does,
    /// then serve the owner models a worker woke for this tree's components: an idle pass, as a
    /// live window's is. Whether a frame ran.
    ///
    /// # Errors
    /// As [`Self::relayout_at`].
    pub fn poll_frame(&mut self) -> Result<bool, tilo_ui_base::widget::WidgetLayoutError> {
        let now = crate::time::now();
        self.presence_tick(now);
        if let Some((device, events)) = self.touch.poll(now) {
            for event in events {
                self.app
                    .surface_input(&self.surface, event.window_event(device), true);
            }
        }
        if !self.frame_owed() {
            return Ok(false);
        }
        self.run_frame();
        self.service_idle_owner_models();
        Ok(true)
    }

    /// Serve one batch of the owner models a worker woke, as a live host's idle pass does: a
    /// refusal keeps the rest queued and the next pass retries.
    fn service_idle_owner_models(&mut self) {
        // Only Tilo's model hosts components whose models a worker wakes.
        #[cfg(feature = "model")]
        self.service_woken_owner_models();
    }

    #[cfg(feature = "model")]
    fn service_woken_owner_models(&mut self) {
        if !self.has_pending_owner_models() {
            return;
        }
        if let Err(error) = self.service_pending_owner_models() {
            tracing::warn!(
                ?error,
                "ui: a woken component's model was refused; retry remains pending"
            );
        }
    }

    /// A wake reached the host: run the frame that takes up what landed. The wake is the app's own,
    /// as a window's host turns the event a wake sends it into the app's pending sync.
    pub fn woken(&mut self) {
        self.wake_handle().wake();
        self.run_frame();
    }

    /// The app's wake, for a source outside it (a handle the host writes, a store, a worker) to
    /// call when something the app reads changed: the app then owes a frame
    /// ([`Self::frame_owed`]), and that frame syncs and takes the change up. What a window's host
    /// hands such a source.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "every driven app is built with a wake scope of its own (`App::displayless`)"
    )]
    pub fn wake_handle(&self) -> crate::wake::WakeHandle {
        self.app
            .wake_scope
            .as_ref()
            .map(crate::wake::WakeScope::handle)
            .expect("a driven app has a wake scope of its own")
    }

    /// What a window does once input stops: the frames owed run until none is.
    pub fn idle(&mut self) {
        for _ in 0..tilo_ui_base::widget::SETTLE_FRAMES {
            if !matches!(self.poll_frame(), Ok(true)) {
                break;
            }
        }
    }

    /// Offer the hosted component's store what the app changed and run the frame that shows it.
    /// Whether anything was written: false when no component is hosted.
    pub fn persist_tick(&mut self) -> bool {
        let hosted = self.tick_writes();
        self.run_frame();
        hosted
    }

    /// Offer the hosted component's store what the app changed, laying nothing out: the write the
    /// next frame shows. Whether a component is hosted.
    fn tick_writes(&self) -> bool {
        #[cfg(feature = "model")]
        let hosted = {
            let binding = crate::bound_dispatch::callback_binding(&*self.app.root_widget);
            if let Some(binding) = &binding {
                binding.records.tick();
            }
            binding.is_some()
        };
        // Only Tilo's model hosts a component with a store of its own.
        #[cfg(not(feature = "model"))]
        let hosted = false;
        hosted
    }

    /// Whether the workspace peer holds something for the app it has not taken yet. Only Tilo's
    /// model gives the driver a peer.
    fn peer_holds_delivery(&self) -> bool {
        #[cfg(feature = "model")]
        let delivering = self.peer_delivering();
        #[cfg(not(feature = "model"))]
        let delivering = false;
        delivering
    }

    /// Whether the workspace peer owes the app a frame.
    fn peer_owes_app_frame(&self) -> bool {
        #[cfg(feature = "model")]
        let owes = self.peer_owes_frame();
        #[cfg(not(feature = "model"))]
        let owes = false;
        owes
    }

    /// When the app next wants a frame with no input to bring one.
    #[must_use]
    pub fn next_frame(&self) -> Option<crate::time::Instant> {
        self.app.surface_next_frame(crate::time::now())
    }

    /// Run frames until `condition` holds or `timeout` passes on the current
    /// [`crate::background::Background`]'s clock, parking between them as an idle window does: a
    /// pumped executor runs its queue and advances its virtual clock instead of waiting in real
    /// time. Whether the condition held.
    ///
    /// Each pass is an idle window's ([`Self::idle_pass`]), not a plain frame: a caller waits for
    /// what it changed outside the tree (a document written past it, a widget it visited), and
    /// only a frame that refreshes the model whole takes that up. The first pass builds the views
    /// again around the model it took up; the passes after it wait for the app's own work.
    pub fn wait_until(
        &mut self,
        timeout: crate::time::Duration,
        mut condition: impl FnMut(&Self) -> bool,
    ) -> bool {
        let background = crate::background::Background::current();
        self.idle_pass(true);
        if condition(self) {
            return true;
        }
        let Some(deadline) = background.now().checked_add(timeout) else {
            return false;
        };
        loop {
            let now = background.now();
            if now >= deadline {
                return false;
            }
            let slice = now
                .checked_add(crate::time::Duration::from_millis(1))
                .unwrap_or(deadline);
            background.park(slice.min(deadline));
            self.idle_pass(false);
            if condition(self) {
                return true;
            }
        }
    }

    /// What an idle window does between frames: the frame the last input asked for is run, with
    /// the model refreshed whole ([`Self::relayout`]); what the widgets queued and the jobs in
    /// flight are taken up and the record's writes offered to its writer
    /// ([`Self::take_up_acted`]); and the frame after shows all of it. With `rebuild`, that frame
    /// builds the views again around the model the first one took up: a caller writes what an
    /// app's views are built from (a document behind the tree), and only a composition built after
    /// the frame read it shows the caller's change.
    fn idle_pass(&mut self, rebuild: bool) {
        self.relayout();
        self.take_up_acted();
        if rebuild {
            self.app.layout.request_rebuild();
        }
        self.relayout();
    }

    /// Park until the app's background work wakes it or `deadline` passes. Whether it woke.
    pub fn park_until(&self, deadline: crate::time::Instant) -> bool {
        let background = crate::background::Background::current();
        while !self.app.wake_pending() {
            if background.now() >= deadline {
                return false;
            }
            let slice = background
                .now()
                .checked_add(crate::time::Duration::from_millis(1))
                .unwrap_or(deadline);
            background.park(slice.min(deadline));
        }
        true
    }

    /// Wait until a widget named `name` is laid out.
    ///
    /// # Errors
    /// When none is within `timeout`.
    pub fn wait_for_named(
        &mut self,
        name: &str,
        timeout: crate::time::Duration,
    ) -> Result<(), String> {
        if self.wait_until(timeout, |driver| driver.targets().contains_key(name)) {
            Ok(())
        } else {
            Err(format!(
                "timed out after {} ms waiting for widget {name:?}",
                timeout.as_millis()
            ))
        }
    }

    /// Run frames until the window has settled, as a person sees it a moment after it opened: no
    /// motion running and no image load in flight. Presents at most `bound`'s frames at a live
    /// window's pace ([`crate::headless::capture_frames`]) and answers whether it settled.
    ///
    /// A frame runs only when the window asks for one, as a live window draws none between two
    /// interactions: a window already at rest costs no frame.
    pub fn settle_for_capture(&mut self, bound: crate::time::Duration) -> bool {
        let mut frames = crate::headless::capture_frames(bound);
        loop {
            if self.frame_owed() {
                self.run_frame();
            }
            self.service_idle_owner_models();
            self.tick_writes();
            if self.frame_owed() {
                self.run_frame();
            }
            if self.capture_is_ready() {
                return true;
            }
            if frames == 0 {
                return false;
            }
            frames = frames.saturating_sub(1);
            let background = crate::background::Background::current();
            let now = background.now();
            background.park(
                now.checked_add(crate::headless::CAPTURE_FRAME_INTERVAL)
                    .unwrap_or(now),
            );
        }
    }

    fn capture_is_ready(&self) -> bool {
        let now = crate::time::now();
        !self.app.surface_moving(now)
            && self.app.root_widget.holds_frame_until(now).is_none()
            && !self.app.root_widget.has_pending_visual_work()
            && tilo_ui_base::image_store::loads_in_flight() == 0
    }

    /// Run frames until the visual work the widgets asked for (an image decode, a fixture read) is
    /// done.
    ///
    /// A widget advances that work in its sync, so each frame run here is a whole one
    /// ([`Self::refresh_model`]): a window waiting for its picture syncs until the picture is
    /// there, whether or not anything else asked for a sync.
    ///
    /// # Errors
    /// When it is not within `timeout`.
    pub fn wait_for_visuals(&mut self, timeout: crate::time::Duration) -> Result<(), String> {
        let background = crate::background::Background::current();
        let Some(deadline) = background.now().checked_add(timeout) else {
            return Err("Timed out waiting for visual work to finish".into());
        };
        loop {
            self.refresh_model();
            if !self.app.root_widget.has_pending_visual_work() {
                return Ok(());
            }
            let now = background.now();
            if now >= deadline {
                return Err("Timed out waiting for visual work to finish".into());
            }
            background.park(
                now.checked_add(crate::time::Duration::from_millis(1))
                    .unwrap_or(deadline),
            );
        }
    }

    /// The scene the last frame painted, in the surface's device pixels.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Frames run since construction.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The surface's size in device pixels: the viewport at the surface's scale.
    #[must_use]
    pub fn device_size(&self) -> (u32, u32) {
        self.surface.device_size()
    }

    /// The pointer shape the app last asked its surface for.
    #[must_use]
    pub fn pointer_cursor(&self) -> winit::window::CursorIcon {
        self.surface.cursor.get()
    }

    // ---- input ----

    /// Hand `event` to the app as its surface would, then run the frame it asks for. Positions are in
    /// the surface's device pixels. A pen sample is kept for the pointer event it describes and
    /// never reaches a widget as an event of its own.
    pub fn send(&mut self, event: &WindowEvent) {
        let _ = self.send_handled(event);
    }

    /// [`Self::send`], and whether the tree took the event: the answer a host needs for a key its
    /// tree left over, as a shell does when a direction leaves its views.
    pub fn send_handled(&mut self, event: &WindowEvent) -> bool {
        let Some(_pen) = self.app.input.pen.route(event) else {
            return false;
        };
        // A frame the clock made due runs before the input, as a window's loop draws what is due
        // between events: the press lands on what that frame placed, not on the layout the
        // deadline found.
        self.serve_due_frame();
        tilo_ui_base::focus_visible::note(event);
        self.note_pointer(event);
        // A chord typed on a layout that is not Latin is the Latin chord it is on the keys: a
        // person with a Russian layout selects all with the same keys as anyone else.
        let renamed = tilo_ui_base::latin_chord::renamed(event, self.app.input.modifiers.state());
        let event = renamed.as_ref().unwrap_or(event);
        let running = self.running();
        // The window's bar takes what is its own before the app sees it, as a window's frame
        // routes it.
        if let Some(bar) = self.bar.as_mut().filter(|_| !self.bar_at_rest) {
            let took = bar.route(&mut self.app, &self.surface, event);
            if bar.take_close() {
                self.surface
                    .window_actions
                    .borrow_mut()
                    .push(crate::part_row::WindowAction::Close);
            }
            if took {
                drop(running);
                self.run_frame();
                return true;
            }
        }
        // A card standing over the window takes what is on it, and the window under it does not
        // see it, as on a desktop.
        let handled = if self.route_to_card(event) {
            true
        } else {
            self.app.surface_input(&self.surface, event.clone(), false)
        };
        drop(running);
        if !self.coalesces_input() {
            self.run_frame();
        }
        handled
    }

    /// [`Self::send`].
    pub fn send_host_event(&mut self, event: &WindowEvent) {
        self.send(event);
    }

    /// Note where a pointer event puts the pointer, in the toolkit's own logical pixels: what a
    /// surface opening now comes out of (the control the person pressed).
    fn note_pointer(&self, event: &WindowEvent) {
        let Some(at) = tilo_ui_base::motion::pointer_of(event) else {
            return;
        };
        let scale = self.surface.scale.get().max(f64::EPSILON);
        tilo_ui_base::motion::note_pointer(Point::new(at.x / scale, at.y / scale));
    }

    /// Hand `event` to the app without running a frame: what lands before the next one. Whether
    /// anything handled it; a pen sample is kept for the pointer event it describes.
    pub fn dispatch_to_root(&mut self, event: &WindowEvent) -> bool {
        let Some(_pen) = self.app.input.pen.route(event) else {
            return false;
        };
        tilo_ui_base::focus_visible::note(event);
        self.app.surface_input(&self.surface, event.clone(), false)
    }

    /// How many device pixels each logical pixel of input is.
    #[must_use]
    pub fn input_scale(&self) -> f64 {
        self.surface.scale.get()
    }

    /// Set the surface's scale: device pixels per logical pixel, by which input is scaled and frames
    /// drawn.
    ///
    /// # Errors
    /// When `scale` is not finite and positive.
    pub fn set_input_scale(&mut self, scale: f64) -> Result<(), String> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err("input scale must be finite and positive".into());
        }
        if scale.to_bits() != self.surface.scale.get().to_bits() {
            self.surface.scale.set(scale);
            self.run_frame();
        }
        Ok(())
    }

    fn device(&self, x: f64, y: f64) -> (f64, f64) {
        let scale = self.surface.scale.get();
        (x * scale, y * scale)
    }

    /// Move the pointer to logical `(x, y)` with no button held.
    pub fn hover_at(&mut self, x: f64, y: f64) {
        let (x, y) = self.device(x, y);
        self.send(&tilo_ui_base::synthetic_input::cursor_moved(x, y));
    }

    fn press_at(&mut self, point: Point, button: winit::event::MouseButton) {
        let (x, y) = self.device(point.x, point.y);
        // A press and its release are one interaction: the frame the pointer move asks for runs
        // after both reach the app, as a window's next frame takes them up together, so a widget
        // a frame replaces between them does not lose the press to its successor.
        let coalesced = self.coalesces_input();
        if !coalesced {
            self.set_coalesce_input_frames(true);
        }
        self.send(&tilo_ui_base::synthetic_input::cursor_moved(x, y));
        self.send(&tilo_ui_base::synthetic_input::mouse_input(
            ElementState::Pressed,
            button,
        ));
        self.send(&tilo_ui_base::synthetic_input::mouse_input(
            ElementState::Released,
            button,
        ));
        if !coalesced {
            self.set_coalesce_input_frames(false);
            let _ = self.poll_frame();
        }
    }

    /// Move the pointer to logical `(x, y)` and press and release the left button there.
    pub fn click_at(&mut self, x: f64, y: f64) {
        self.press_at(Point::new(x, y), winit::event::MouseButton::Left);
    }

    /// [`Self::click_at`] with the secondary button: what opens a context menu.
    pub fn right_click_at(&mut self, x: f64, y: f64) {
        self.press_at(Point::new(x, y), winit::event::MouseButton::Right);
    }

    /// Two left clicks at `(x, y)` that count as one double-click however long the frames between
    /// the presses take.
    pub fn double_click_at(&mut self, x: f64, y: f64) {
        tilo_ui_base::gesture::with_capture_multi_click_interval(
            crate::time::Duration::from_secs(3600),
            || {
                self.click_at(x, y);
                self.click_at(x, y);
            },
        );
    }

    /// Three left clicks at logical `(x, y)` that count as one triple-click.
    pub fn triple_click_at(&mut self, x: f64, y: f64) {
        tilo_ui_base::gesture::with_capture_multi_click_interval(
            crate::time::Duration::from_secs(3600),
            || {
                for _ in 0..3 {
                    self.click_at(x, y);
                }
            },
        );
    }

    /// Press the left button at `from`, move through intermediate points to `to` and release there.
    pub fn drag_at(&mut self, from: (f64, f64), to: (f64, f64)) {
        let (x, y) = self.device(from.0, from.1);
        self.send(&tilo_ui_base::synthetic_input::cursor_moved(x, y));
        self.send(&tilo_ui_base::synthetic_input::mouse_press());
        for fraction in [0.25, 0.5, 0.75, 1.0] {
            let (x, y) = self.device(
                from.0 + (to.0 - from.0) * fraction,
                from.1 + (to.1 - from.1) * fraction,
            );
            self.send(&tilo_ui_base::synthetic_input::cursor_moved(x, y));
        }
        self.send(&tilo_ui_base::synthetic_input::mouse_release());
    }

    /// Send wheel line deltas at the last pointer position.
    pub fn scroll(&mut self, dx: f64, dy: f64) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "line-delta counts are small; winit's `LineDelta` is f32"
        )]
        let (dx, dy) = (dx as f32, dy as f32);
        self.send(&tilo_ui_base::synthetic_input::mouse_wheel(dx, dy));
    }

    /// Deliver one finger's contact at logical `(x, y)`; `id` tells simultaneous fingers apart.
    pub fn touch(&mut self, id: u64, phase: winit::event::TouchPhase, x: f64, y: f64) {
        let (x, y) = self.device(x, y);
        self.send_touch(id, phase, x, y);
        self.run_frame();
    }

    /// Deliver one finger's contact at physical `(x, y)` as a window's loop hands it on: the frame
    /// it moves is drawn on the host's own clock, as [`Self::send`] leaves it.
    pub fn send_touch(&mut self, id: u64, phase: winit::event::TouchPhase, x: f64, y: f64) {
        self.serve_due_frame();
        let touch = winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id,
        };
        self.app
            .surface_touch(&self.surface, touch, &mut self.touch);
    }

    /// A tap at logical `(x, y)`.
    pub fn tap_at(&mut self, x: f64, y: f64) {
        self.touch(0, winit::event::TouchPhase::Started, x, y);
        self.touch(0, winit::event::TouchPhase::Ended, x, y);
    }

    /// Two taps at logical `(x, y)` that count as one double tap.
    pub fn double_tap_at(&mut self, x: f64, y: f64) {
        tilo_ui_base::gesture::with_capture_multi_click_interval(
            crate::time::Duration::from_secs(3600),
            || {
                for id in [u64::MAX - 1, u64::MAX] {
                    self.touch(id, winit::event::TouchPhase::Started, x, y);
                    self.touch(id, winit::event::TouchPhase::Ended, x, y);
                }
            },
        );
    }

    /// One finger moved from `start` to `end` through intermediate points.
    pub fn swipe_at(&mut self, start: (f64, f64), end: (f64, f64)) {
        self.touch(0, winit::event::TouchPhase::Started, start.0, start.1);
        for fraction in [0.25, 0.5, 0.75, 1.0] {
            let x = start.0 + (end.0 - start.0) * fraction;
            let y = start.1 + (end.1 - start.1) * fraction;
            self.touch(0, winit::event::TouchPhase::Moved, x, y);
        }
        self.touch(0, winit::event::TouchPhase::Ended, end.0, end.1);
    }

    /// One finger put down at logical `(x, y)` and held past the long-press delay, left down.
    pub fn touch_down_and_hold_at(&mut self, x: f64, y: f64) {
        self.touch(0, winit::event::TouchPhase::Started, x, y);
        if let Some(deadline) = self.touch.long_press_deadline() {
            self.app
                .surface_touch_hold(&self.surface, &mut self.touch, deadline);
        }
        self.run_frame();
    }

    /// One finger held still at logical `(x, y)` past the long-press delay, then lifted: the
    /// touchscreen's secondary click.
    pub fn touch_hold_at(&mut self, x: f64, y: f64) {
        self.touch_down_and_hold_at(x, y);
        self.touch(0, winit::event::TouchPhase::Ended, x, y);
    }

    /// The centre of the widget named `name`.
    fn named_center(&self, name: &str) -> Result<Point, String> {
        let rect = self
            .targets()
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(format!("widget {name:?} has an empty rect: {rect:?}"));
        }
        Ok(rect.center())
    }

    /// Click the centre of the widget named `name`.
    ///
    /// # Errors
    /// When no laid-out widget has that name, or it has no area.
    pub fn click_named(&mut self, name: &str) -> Result<(), String> {
        self.serve_due_frame();
        let center = self.named_center(name)?;
        self.click_at(center.x, center.y);
        Ok(())
    }

    /// Tap the centre of the widget named `name`.
    ///
    /// # Errors
    /// As [`Self::click_named`].
    pub fn tap_named(&mut self, name: &str) -> Result<(), String> {
        self.serve_due_frame();
        let center = self.named_center(name)?;
        self.tap_at(center.x, center.y);
        Ok(())
    }

    /// Press and release one key, holding the modifiers a `+` chord names (`Ctrl+Z`, `Shift+F10`)
    /// for as long as the press lasts.
    pub fn key(&mut self, spec: &str) {
        let (spec, held) = crate::app::driver::chord(spec);
        let key = tilo_ui_base::keys::parse(spec);
        if !held.is_empty() {
            self.set_modifiers(&held);
        }
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send(&tilo_ui_base::synthetic_input::key_input(&key, state));
        }
        if !held.is_empty() {
            self.set_modifiers(&[]);
        }
    }

    /// Type `text` one character at a time.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.key(&ch.to_string());
        }
    }

    /// Commit `text` through the input-method path in one event.
    pub fn commit_text(&mut self, text: &str) {
        self.send(&WindowEvent::Ime(winit::event::Ime::Commit(text.into())));
    }

    /// Hold the modifiers `names` names (`ctrl`, `shift`, `alt`, `logo`, `command`), and no others.
    pub fn set_modifiers(&mut self, names: &[String]) {
        let mut state = winit::keyboard::ModifiersState::empty();
        let platform = self.app.platform_conventions();
        for name in names {
            match name.to_ascii_lowercase().as_str() {
                "command" => state |= platform.command_modifier(),
                "ctrl" | "control" => state |= winit::keyboard::ModifiersState::CONTROL,
                "shift" => state |= winit::keyboard::ModifiersState::SHIFT,
                "alt" => state |= winit::keyboard::ModifiersState::ALT,
                "logo" | "super" | "meta" => state |= winit::keyboard::ModifiersState::SUPER,
                _ => {}
            }
        }
        let modifiers = winit::event::Modifiers::from(state);
        self.app.set_surface_modifiers(modifiers);
        self.send(&WindowEvent::ModifiersChanged(modifiers));
    }

    /// Move keyboard focus as Tab does (Shift+Tab when `back`). Where it landed.
    pub fn tab(&mut self, back: bool) -> Option<NodeId> {
        self.key(if back { "Shift+Tab" } else { "Tab" });
        self.app.focused_node
    }

    // ---- the document, the root and its widgets ----

    /// The live document.
    #[must_use]
    pub fn state(&self) -> &State {
        &self.app.state
    }

    /// The live document, to change between frames. A change here schedules no frame of its
    /// own: whoever changed it runs the frame that shows it, as a host does for its own writes.
    pub fn state_mut(&mut self) -> &mut State {
        &mut self.app.state
    }

    /// The theme frames are drawn with.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.app.theme
    }

    /// Draw with `theme` from the next frame on, and run that frame.
    pub fn set_theme(&mut self, theme: Theme) {
        // A window shown on this surface wears the theme whole, as one it opened with.
        self.app.adopt_theme(theme);
        self.run_frame();
    }

    /// The root widget, whatever it was mounted as.
    #[must_use]
    pub fn root_widget(&self) -> &dyn Widget {
        self.app.root_widget.as_ref()
    }

    /// The root widget, mutable: what the next frame takes up.
    pub fn root_widget_mut(&mut self) -> &mut dyn Widget {
        self.app.needs_sync = true;
        self.app.layout.layout_dirty = true;
        self.app.root_widget.as_mut()
    }

    /// Hand the root widget to `change`, which says whether it changed the tree: only a change
    /// is what the next frame takes up. A host that hands its window what arrived on every turn
    /// owes no frame for a turn that brought nothing, so an idle tree does no work.
    pub fn change_root(&mut self, change: impl FnOnce(&mut dyn Widget) -> bool) -> bool {
        let changed = change(self.app.root_widget.as_mut());
        if changed {
            self.app.needs_sync = true;
            self.app.layout.layout_dirty = true;
        }
        changed
    }

    /// The first widget of type `T` in the tree, changed by `visit`, and the frame that shows it.
    pub fn with_widget_mut<T: 'static, O>(&mut self, visit: impl FnOnce(&mut T) -> O) -> Option<O> {
        let output = visit(self.widget_mut::<T>()?);
        // The widget changed by hand: the frame takes the change up, as one after an event does.
        self.app.needs_sync = true;
        self.relayout();
        Some(output)
    }

    /// The first widget of type `T` in the tree, without running a frame.
    pub fn widget_mut<T: 'static>(&mut self) -> Option<&mut T> {
        /// The component a widget belongs to, the nearest above it: with Tilo's model only.
        #[cfg(feature = "model")]
        type Owner = Option<crate::bound_dispatch::CallbackBinding>;
        #[cfg(not(feature = "model"))]
        type Owner = ();
        fn find(
            widget: &mut dyn Widget,
            ty: TypeId,
            owner: Owner,
        ) -> Option<(&mut dyn Widget, Owner)> {
            #[cfg(feature = "model")]
            let owner = crate::bound_dispatch::callback_binding(widget).or(owner);
            if widget
                .as_any_mut()
                .is_some_and(|any| (*any).type_id() == ty)
            {
                return Some((widget, owner));
            }
            widget
                .children_mut()
                .into_iter()
                .find_map(|child| find(child, ty, owner.clone()))
        }
        self.app.needs_sync = true;
        self.app.layout.layout_dirty = true;
        // What the caller changes may be anything the widget publishes: nothing the app derived
        // from it before stands.
        self.app.invalidate_widget_resource_cache();
        let (found, _owner) = find(
            self.app.root_widget.as_mut(),
            TypeId::of::<T>(),
            Owner::default(),
        )?;
        #[cfg(feature = "model")]
        if let Some(owner) = _owner {
            owner.note_input();
        }
        found.as_any_mut()?.downcast_mut::<T>()
    }

    /// The person's presence and their peers', as the frames draw them.
    #[must_use]
    pub fn presence(&self) -> &PresenceCtx {
        &self.app.presence_ctx
    }

    /// The presence the frames draw, to change.
    pub fn presence_mut(&mut self) -> &mut PresenceCtx {
        self.app.layout.layout_dirty = true;
        &mut self.app.presence_ctx
    }

    /// Replace the presence the frames draw, and run the frame that draws it.
    pub fn set_presence_context(&mut self, presence: PresenceCtx) {
        self.app.presence_ctx = presence;
        self.run_frame();
    }

    /// Change the presence the frames draw with `update`; when it says it changed something, the
    /// next frame draws it. Whether it did.
    pub fn update_presence(&mut self, update: impl FnOnce(&mut PresenceCtx) -> bool) -> bool {
        let changed = update(&mut self.app.presence_ctx);
        if changed {
            self.surface.redraw.set(true);
        }
        changed
    }

    /// Replace the permissions the app's own document is acted on under.
    #[cfg(feature = "model")]
    pub fn set_permission_ctx(&mut self, permissions: PermissionContext) {
        self.app.set_permission_ctx(permissions);
    }

    /// How the person reaches the surface: pointer, touch, keyboard.
    pub fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.app.set_interaction_environment(environment);
        self.run_frame();
    }

    #[must_use]
    pub fn interaction_environment(&self) -> tilo_ui_base::interaction::InteractionEnvironment {
        self.app.interaction_environment()
    }

    /// The platform whose conventions the app follows (its command key, its shortcuts).
    pub fn set_platform_conventions(
        &mut self,
        conventions: tilo_ui_base::platform::PlatformConventions,
    ) {
        self.app.set_platform_conventions(conventions);
        self.run_frame();
    }

    #[must_use]
    pub fn platform_conventions(&self) -> tilo_ui_base::platform::PlatformConventions {
        self.app.platform_conventions()
    }

    /// The root's native input diagnostic, as the native runner samples it.
    #[must_use]
    pub fn native_input_snapshot(&self) -> Option<serde_json::Value> {
        self.app
            .root_widget
            .native_input_snapshot(&self.app.state.doc, &self.app.state.root)
    }

    /// The pointer shape the laid-out app asks for at logical `point`.
    #[must_use]
    pub fn cursor_at(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let mut pick = self.app.tree().cursor_pick_at(point);
        for root in self.app.presentation_roots() {
            let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(root) else {
                continue;
            };
            pick.offer_tree(root, point);
        }
        pick.icon()
    }

    /// What the app wants its process to do (open another window, quit), once.
    #[cfg(feature = "model")]
    pub fn take_process_intent(&mut self) -> Option<crate::widget::AppProcessIntent> {
        let bound = self
            .app
            .root_widget
            .as_any_mut()?
            .downcast_mut::<crate::widget::BoundAppWidget>()?;
        crate::AppWidget::take_process_intent(bound)
    }

    /// The document as JSON: the mounted component's record when one is, else the app's own
    /// document.
    pub fn dump_json(&mut self) -> String {
        #[cfg(feature = "model")]
        if let Some(binding) = crate::bound_dispatch::callback_binding(&*self.app.root_widget) {
            let records = &binding.records;
            let Some(mut dump) = records.read(&binding.checkout.id, |doc| {
                serde_json::to_value(automerge::AutoSerde::from(doc))
            }) else {
                return "{\"error\":\"canonical record unavailable\"}".into();
            };
            // The parts of a chain the record keeps beside it (a notebook's notes) are dumped with
            // it, each under its record's id.
            let parts: serde_json::Map<String, serde_json::Value> = records
                .open_resources()
                .into_iter()
                .filter(|held| held.id.chain_part().is_some())
                .filter_map(|held| {
                    let part = records
                        .read(&held.id, |doc| {
                            serde_json::to_value(automerge::AutoSerde::from(doc))
                        })?
                        .ok()?;
                    Some((held.id.as_str().to_owned(), part))
                })
                .collect();
            if !parts.is_empty()
                && let Ok(serde_json::Value::Object(fields)) = &mut dump
            {
                fields.insert("parts".into(), serde_json::Value::Object(parts));
            }
            return dump
                .ok()
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or_else(|| "{}".into());
        }
        serde_json::to_string_pretty(&automerge::AutoSerde::from(&self.app.state.doc))
            .unwrap_or_else(|_| "{}".into())
    }

    // ---- what a capture reads ----

    /// The frame drawn last, rasterised over the window ground as RGBA rows the surface's device
    /// width wide.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn render_rgba(&self) -> Vec<u8> {
        self.render_rgba_on(self.app.theme.tokens.colors.surfaces.content)
    }

    /// [`Self::render_rgba`] over a transparent background.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn render_rgba_transparent(&self) -> Vec<u8> {
        self.render_rgba_on(vello::peniko::Color::TRANSPARENT)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn render_rgba_on(&self, background: vello::peniko::Color) -> Vec<u8> {
        let (width, height) = self.device_size();
        let mut renderer = tilo_ui_base::offscreen::OffscreenRenderer::new(width, height);
        self.render_rgba_with(&mut renderer, background)
    }

    /// Rasterise the frame drawn last with a caller-owned renderer of the surface's device size.
    ///
    /// # Panics
    /// When the renderer is another size.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn render_rgba_with(
        &self,
        renderer: &mut tilo_ui_base::offscreen::OffscreenRenderer,
        background: vello::peniko::Color,
    ) -> Vec<u8> {
        assert_eq!(
            (renderer.width, renderer.height),
            self.device_size(),
            "the renderer must match the surface's device size"
        );
        renderer.set_backdrop(Some(self.records.captured_backdrops()));
        renderer.render_resilient(&self.scene, background)
    }

    /// The frame drawn last, written as a PNG over the window ground.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_png(&self, out: &std::path::Path) {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (width, height) = self.device_size();
        let mut renderer = tilo_ui_base::offscreen::OffscreenRenderer::new(width, height);
        renderer.set_backdrop(Some(self.records.captured_backdrops()));
        renderer.render_png(
            &self.scene,
            self.app.theme.tokens.colors.surfaces.content,
            out,
        );
    }

    /// The window's ground alone, written as a PNG: what a capture of a protected window keeps.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_ground_png(&self, out: &std::path::Path) {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (width, height) = self.device_size();
        let mut renderer = tilo_ui_base::offscreen::OffscreenRenderer::new(width, height);
        renderer.render_png(
            &Scene::new(),
            self.app.theme.tokens.colors.surfaces.content,
            out,
        );
    }
}

// What the window's semantic tree and the document hold, for an assertion: kept beside the driver
// so a test reads them the way an assistive technology and an agent do.
impl Driver {
    /// The widget the keyboard is on.
    #[must_use]
    pub fn focused(&self) -> Option<NodeId> {
        self.app.focused_node
    }

    /// Take keyboard focus from every control.
    pub fn clear_focus(&mut self) {
        self.app.set_focused_node(None);
        self.run_frame();
    }

    /// The keyboard focus order, as Tab walks it.
    #[must_use]
    pub fn focus_order(&self) -> Vec<NodeId> {
        self.app.focusable_order.clone()
    }

    /// The accessibility node of every keyboard-focusable control, in focus order.
    #[must_use]
    pub fn focus_access_nodes(&self) -> Vec<AccessNode> {
        self.focus_order()
            .into_iter()
            .filter_map(|id| {
                self.app
                    .inspect_node(id, |widget| widget.accessibility())
                    .ok()
                    .flatten()
            })
            .collect()
    }

    /// Where the bar of the window frame named `name` stands, in the layout's logical pixels:
    /// the frame the tree holds, read where its layout put it
    /// ([`tilo_ui_base::widget::Widget::get_rect`]'s coordinates).
    ///
    /// A gesture that carries the strip under the window in front aims at its bar every frame it
    /// moves; this walks the laid-out tree for it, never the accessibility tree, which is built
    /// for a reader and collects every widget's role, name and bounds.
    #[must_use]
    pub fn window_bar(&self, name: &str) -> Option<vello::kurbo::Rect> {
        crate::widget::window_frame::bar_named(self.app.root_widget.as_ref(), name)
    }

    /// The accessibility node of the focused widget: what a screen reader says when Tab lands there.
    #[must_use]
    pub fn focused_access_node(&self) -> Option<AccessNode> {
        let id = self.app.focused_node?;
        self.app
            .inspect_node(id, |widget| widget.accessibility())
            .ok()
            .flatten()
    }

    /// Tab through the whole focus order from the top: the accessible `(role, name)` said at each
    /// stop.
    /// A stop whose control says nothing to a reader is passed over, as a reader passes it.
    pub fn tab_through(&mut self) -> Vec<(tilo_ui_base::a11y::Role, Option<String>)> {
        self.clear_focus();
        let count = self.focus_order().len();
        let mut stops = Vec::with_capacity(count);
        for _ in 0..count {
            self.tab(false);
            if let Some(node) = self.focused_access_node() {
                stops.push((node.role, node.name));
            }
        }
        stops
    }

    /// Answer what the expectation asks of the named widget's accessibility.
    ///
    /// # Errors
    /// Every mismatched field in one diagnostic, or a missing target.
    pub fn expect(&mut self, expected: &SemanticExpectation) -> Result<(), String> {
        let Some(timeout) = expected.timeout_ms.map(crate::time::Duration::from_millis) else {
            return self.expect_now(expected);
        };
        let mut last = Err(String::new());
        if self.wait_until(timeout, |driver| {
            last = driver.expect_now(expected);
            last.is_ok()
        }) {
            Ok(())
        } else {
            last
        }
    }

    fn expect_now(&self, expected: &SemanticExpectation) -> Result<(), String> {
        let node = self.access_node_named(&expected.target, expected.semantic_target.as_deref())?;
        crate::headless::compare_expectation(&node, expected)
    }

    /// The widget the inspection names `name`, its accessibility node, or the synthetic node under it
    /// that `semantic_target` names.
    pub(crate) fn access_node_named(
        &self,
        name: &str,
        semantic_target: Option<&str>,
    ) -> Result<AccessNode, String> {
        let targets = self.named_target_nodes();
        let node = targets
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        let access = self
            .app
            .inspect_node(node, |widget| {
                widget
                    .accessibility()
                    .map(|access| tilo_ui_base::a11y::with_synthetic_bounds(access, widget))
            })
            .ok()
            .flatten()
            .ok_or_else(|| format!("widget {name:?} exposes no accessibility semantics"))?;
        let Some(target) = semantic_target else {
            return Ok(access);
        };
        crate::headless::find_semantic_target(&access, target)
            .cloned()
            .ok_or_else(|| format!("widget {name:?} has no semantic target {target:?}"))
    }

    /// Every laid-out widget by the names a test and a scenario address it by: its path, its bare
    /// path and its id.
    pub(crate) fn named_target_nodes(&self) -> HashMap<String, NodeId> {
        let mut registered = HashMap::with_capacity(self.app.layout.node_map.len());
        for &node in self.app.layout.node_map.keys() {
            let Ok(key) = self.app.inspect_node(node, |widget| {
                (
                    std::ptr::from_ref(widget).cast::<()>() as usize,
                    widget.name().to_owned(),
                )
            }) else {
                continue;
            };
            registered
                .entry(key)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(node));
        }
        let mut retained = Vec::new();
        for root in self.app.presentation_roots() {
            crate::headless::collect_named_targets(root, "", &mut HashMap::new(), &mut retained);
        }
        let mut unique = HashMap::new();
        for target in &retained {
            unique
                .entry(target.key.clone())
                .and_modify(|entry| *entry = false)
                .or_insert(true);
        }
        let mut targets = HashMap::new();
        for target in retained {
            if unique.get(&target.key) != Some(&true) {
                continue;
            }
            let Some(Some(node)) = registered.get(&target.key).copied() else {
                continue;
            };
            targets.insert(target.path, node);
            if let Some(bare) = target.bare {
                targets.entry(bare).or_insert(node);
            }
            if let Some(id) = target.id {
                targets.insert(id, node);
            }
        }
        targets
    }

    /// Every addressable widget and its rect, by its path, its bare path and its id.
    #[must_use]
    pub fn targets(&self) -> HashMap<String, Rect> {
        let mut out = HashMap::new();
        let mut counts = HashMap::new();
        for root in self.app.presentation_roots() {
            crate::headless::collect_targets(root, "", &mut counts, &mut out);
        }
        out
    }

    /// Widget identity, geometry, hierarchy and accessibility, from the same laid-out frame.
    #[must_use]
    pub fn inspection(&self) -> InspectionSnapshot {
        let mut sibling_counts = HashMap::new();
        // What the window presents: History stands for the window while it is open, with the
        // app's own surface beside it as long as that surface is visible, and the palette takes
        // the window while it is up.
        let mut root = if let Some(presentation) = self.history_inspection(&mut sibling_counts) {
            presentation
        } else if let Some(palette) = self.app.presented_palette() {
            crate::headless::collect_inspection(palette, "", &mut sibling_counts, None)
        } else {
            crate::headless::collect_inspection(
                self.app.root_widget.as_ref(),
                "",
                &mut sibling_counts,
                None,
            )
        };
        // The tooltip is painted over the tree rather than being one of its widgets.
        if let Some(tooltip) = self.tooltip_inspection() {
            root.children.push(tooltip);
        }
        // The people and agents a frame paints over the widgets are no widgets either.
        root.children.extend(self.presence_inspection());
        root.children.extend(self.cards_inspection());
        fn owner_path(
            node: &InspectionNode,
            targets: &HashMap<String, NodeId>,
            focused: NodeId,
        ) -> Option<String> {
            if targets.get(&node.path) == Some(&focused) {
                return Some(node.path.clone());
            }
            node.children
                .iter()
                .find_map(|child| owner_path(child, targets, focused))
        }
        let keyboard_focus_owner = self
            .app
            .focused_node
            // A control keeps the keyboard only while it still holds it: what a window the person
            // left held names nothing, as the host's own focus pass finds.
            .filter(|&focused| {
                self.app
                    .inspect_node(focused, |widget| {
                        !widget.is_focusable() || widget.is_focused()
                    })
                    .unwrap_or(false)
            })
            .and_then(|focused| owner_path(&root, &self.named_target_nodes(), focused));
        let (width, height) = self.viewport();
        InspectionSnapshot {
            viewport: [width, height],
            keyboard_focus_owner,
            root,
        }
    }

    /// [`Self::inspection`] as pretty JSON, the `--headless-drive` inspector's form.
    ///
    /// # Errors
    /// When it does not serialize.
    pub fn inspection_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.inspection())
    }

    /// Audit the semantic tree the frame exports to assistive technology.
    #[must_use]
    pub fn accessibility_audit(&self) -> AccessibilityAudit {
        let inspection = self.inspection();
        let mut issues = Vec::new();
        crate::headless::audit_inspection_node(&inspection.root, &mut issues);
        AccessibilityAudit {
            version: crate::headless::ACCESSIBILITY_AUDIT_VERSION,
            issues,
        }
    }

    /// What the window presents while its History is open: History, with the palette while it is
    /// up, else the app's own surface while it stays visible.
    fn history_inspection(
        &self,
        sibling_counts: &mut HashMap<String, usize>,
    ) -> Option<InspectionNode> {
        #[cfg(feature = "model")]
        let presentation = {
            let history = self.app.history.as_deref()?;
            let mut presentation =
                crate::headless::collect_inspection(history, "", sibling_counts, None);
            if let Some(palette) = self
                .app
                .actions
                .action_palette
                .as_deref()
                .filter(|palette| palette.is_presented())
            {
                presentation
                    .children
                    .push(crate::headless::collect_inspection(
                        palette,
                        "",
                        sibling_counts,
                        None,
                    ));
            } else if history.live_visible() {
                presentation
                    .children
                    .push(crate::headless::collect_inspection(
                        self.app.root_widget.as_ref(),
                        "",
                        sibling_counts,
                        None,
                    ));
            }
            Some(presentation)
        };
        #[cfg(not(feature = "model"))]
        let presentation = {
            let _ = sibling_counts;
            None
        };
        presentation
    }

    /// Certify the laid-out app against the action-native contract.
    #[must_use]
    pub fn native_app_audit(&self) -> NativeAppAudit {
        use std::collections::HashSet;
        use tilo_ui_base::widget::ControlBindingKind;
        let mut resources = self.resources();
        // Shared chrome invokes the host's own APIs rather than the app's: search, and History
        // where the window serves one. A control bound to either is not stale.
        resources.push(crate::widget::search_request::search_resource());
        #[cfg(feature = "model")]
        {
            resources.push(self.app.history.as_ref().map_or_else(
                || crate::history_mode::api::history_resource(false, false),
                |history| history.api_resource(),
            ));
        }
        let bindings =
            tilo_ui_base::widget::controls::control_action_bindings(self.app.root_widget.as_ref());
        let inventoried_controls = bindings
            .iter()
            .map(|binding| binding.control_id.clone())
            .collect::<HashSet<_>>();
        let application = crate::headless::collect_inspection(
            self.app.root_widget.as_ref(),
            "",
            &mut HashMap::new(),
            None,
        );
        let has_semantics = crate::headless::inspection_has_semantics(&application);
        let semantic_effect_controls = crate::headless::effect_control_ids(&application);
        let mut issues = bindings
            .into_iter()
            .filter_map(|binding| match binding.kind {
                ControlBindingKind::Action(address) => {
                    tilo_actions::api::resolve(&resources, &address)
                        .is_none()
                        .then(|| NativeAppIssue {
                            control: binding.control_id,
                            code: "stale-action-binding".into(),
                            message: format!(
                                "bound action {}/{} is absent from the live resource tree",
                                address.resource, address.action
                            ),
                        })
                }
                ControlBindingKind::CollaborativeDocument(_)
                | ControlBindingKind::SemanticAction(_) => None,
            })
            .collect::<Vec<_>>();
        if !has_semantics && inventoried_controls.is_empty() {
            issues.push(NativeAppIssue {
                control: "application-root".into(),
                code: "empty-native-contract".into(),
                message: "application exposes neither accessibility semantics nor a control/action inventory".into(),
            });
        }
        for control in inventoried_controls.difference(&semantic_effect_controls) {
            issues.push(NativeAppIssue {
                control: control.clone(),
                code: "orphan-action-binding".into(),
                message: "action binding has no matching effect-capable semantic control".into(),
            });
        }
        NativeAppAudit {
            version: crate::headless::NATIVE_APP_AUDIT_VERSION,
            accessibility: self.accessibility_audit(),
            issues,
        }
    }
}

/// A [`Driver`] over one concrete root that reads as that root. Dereferencing yields the widget.
pub struct Framed<W: Widget + 'static> {
    driver: Driver,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> Framed<W> {
    /// Mount `root` over `state` and run its first frame at `viewport`.
    pub fn new(state: State, root: W, viewport: (u32, u32), theme: Theme) -> Self {
        Self::from_driver(Driver::for_root(state, root, viewport, theme))
    }

    /// [`Self::new`] with every construction option spelled out.
    pub fn with_options(
        state: State,
        root: W,
        viewport: (u32, u32),
        options: FrameOptions,
    ) -> Self {
        Self::from_driver(Driver::for_root_with(state, root, viewport, options))
    }

    fn from_driver(driver: Driver) -> Self {
        let _: &W = driver.root();
        Self {
            driver,
            root: std::marker::PhantomData,
        }
    }

    pub fn harness(this: &Self) -> &Driver {
        &this.driver
    }

    pub fn harness_mut(this: &mut Self) -> &mut Driver {
        &mut this.driver
    }

    /// The root and the live document at once; the next frame builds the tree again, as after a
    /// mutable dereference.
    pub fn split(this: &mut Self) -> (&mut W, &mut State) {
        this.driver.root_and_state_rebuilt()
    }
}

impl<W: Widget + 'static> std::ops::Deref for Framed<W> {
    type Target = W;

    fn deref(&self) -> &W {
        self.driver.root()
    }
}

impl<W: Widget + 'static> std::ops::DerefMut for Framed<W> {
    fn deref_mut(&mut self) -> &mut W {
        self.driver.root_and_state_rebuilt::<W>().0
    }
}

/// The root of a [`Driver`], to change ([`Driver::root_mut`]); let go of, the frame that takes up
/// the change runs, as after an event.
pub struct RootMut<'a, W: Widget + 'static> {
    driver: &'a mut Driver,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> std::ops::Deref for RootMut<'_, W> {
    type Target = W;

    fn deref(&self) -> &W {
        self.driver.root()
    }
}

impl<W: Widget + 'static> std::ops::DerefMut for RootMut<'_, W> {
    fn deref_mut(&mut self) -> &mut W {
        // SAFETY: `root_mut` checked the type; the frame run on drop takes up what changed before
        // anything calls through the layout maps.
        unsafe { self.driver.lent_root() }
    }
}

impl<W: Widget + 'static> RootMut<'_, W> {
    /// Put `root` in the root's place, as a view that builds its widget anew does: the next frame
    /// builds the tree again.
    pub fn replace(self, root: W) {
        // SAFETY: as in `deref_mut`.
        *unsafe { self.driver.lent_root::<W>() } = root;
        self.driver.app.layout.node_map.clear();
        self.driver.app.layout.request_rebuild();
    }
}

impl<W: Widget + 'static> Drop for RootMut<'_, W> {
    fn drop(&mut self) {
        // The host let go of a root it changed: the frame it runs takes the change up, as one
        // after an event does.
        self.driver.app.needs_sync = true;
        self.driver.run_frame();
    }
}

/// The root of a [`Driver`] and its live document, to change ([`Driver::root_and_state_mut`]); let
/// go of as a [`RootMut`].
pub struct RootAndStateMut<'a, W: Widget + 'static> {
    driver: &'a mut Driver,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> RootAndStateMut<'_, W> {
    /// The root and the document, at once.
    pub fn split(&mut self) -> (&mut W, &mut State) {
        let app = &mut *self.driver.app;
        // SAFETY: `root_and_state_mut` checked the type, and `drop` runs the frame that takes up
        // what changed.
        let root =
            unsafe { &mut *std::ptr::from_mut::<dyn Widget>(app.root_widget.as_mut()).cast::<W>() };
        (root, &mut app.state)
    }
}

impl<W: Widget + 'static> Drop for RootAndStateMut<'_, W> {
    fn drop(&mut self) {
        // The host let go of a root and document it changed: the frame takes both up.
        self.driver.app.needs_sync = true;
        self.driver.run_frame();
    }
}

impl Driver {
    /// The root mounted by [`Self::for_root`].
    ///
    /// # Panics
    /// When it was mounted as another type.
    #[must_use]
    pub fn root<W: Widget + 'static>(&self) -> &W {
        self.lend_root::<W>("Driver::root");
        // SAFETY: `root_type` records the concrete type the root was boxed from.
        unsafe { &*std::ptr::from_ref::<dyn Widget>(self.app.root_widget.as_ref()).cast::<W>() }
    }

    /// The root mounted by [`Self::for_root`], to change.
    pub fn root_mut<W: Widget + 'static>(&mut self) -> RootMut<'_, W> {
        self.lend_root::<W>("Driver::root_mut");
        self.app.needs_sync = true;
        RootMut {
            driver: self,
            root: std::marker::PhantomData,
        }
    }

    /// The root mounted by [`Self::for_root`] and the live document, for calls that take both.
    pub fn root_and_state_mut<W: Widget + 'static>(&mut self) -> RootAndStateMut<'_, W> {
        self.lend_root::<W>("Driver::root_and_state_mut");
        self.app.needs_sync = true;
        RootAndStateMut {
            driver: self,
            root: std::marker::PhantomData,
        }
    }

    fn lend_root<W: Widget + 'static>(&self, caller: &str) {
        assert_eq!(
            self.root_type,
            Some(TypeId::of::<W>()),
            "{caller}: the root was not mounted as this type"
        );
    }

    /// # Safety
    /// `root_type` is `W` ([`Self::lend_root`]), and a frame runs once the borrow ends.
    unsafe fn lent_root<W: Widget + 'static>(&mut self) -> &mut W {
        // SAFETY: as in `root`.
        unsafe { &mut *std::ptr::from_mut::<dyn Widget>(self.app.root_widget.as_mut()).cast::<W>() }
    }

    /// The root and the document as plain references: nothing runs when they are let go of, so the
    /// layout maps are dropped now and the next frame builds the tree again.
    fn root_and_state_rebuilt<W: Widget + 'static>(&mut self) -> (&mut W, &mut State) {
        self.lend_root::<W>("Framed");
        let app = &mut *self.app;
        app.needs_sync = true;
        app.layout.node_map.clear();
        app.layout.request_rebuild();
        // SAFETY: as in `root`; the layout maps that pointed into this tree were dropped above.
        let root =
            unsafe { &mut *std::ptr::from_mut::<dyn Widget>(app.root_widget.as_mut()).cast::<W>() };
        (root, &mut app.state)
    }
}

// What an assistive technology and an agent reach: the semantic tree the frame exports and the
// actions the app publishes, each through the app's own route.
impl Driver {
    /// The accessibility nodes, with bounds in logical pixels, as the frame exports them.
    #[must_use]
    pub fn a11y_nodes(&self) -> Vec<(accesskit::NodeId, accesskit::Node)> {
        self.a11y_tree_update_at(1.0).nodes
    }

    /// The whole accessibility tree as one update, bounds in logical pixels.
    #[must_use]
    pub fn a11y_tree_update(&self) -> accesskit::TreeUpdate {
        self.a11y_tree_update_at(1.0)
    }

    /// The accessibility tree as one update at `scale`, with what stands over the app (History, the
    /// palette) read with it.
    #[must_use]
    pub fn a11y_tree_update_at(&self, scale: f64) -> accesskit::TreeUpdate {
        let app = &*self.app;
        let exported = app.bindings.exported_nodes(&app.layout.node_map);
        let palette = app.palette_presented();
        let mut update = if !app.presents_history() && !palette && app.window_box.is_drawn() {
            // The box with no window around it is read over the app, as it is drawn.
            let roots: [&dyn Widget; 2] = [app.root_widget.as_ref(), &app.window_box];
            tilo_ui_base::accesskit_bridge::build_presentation_tree_bound(
                &roots,
                &exported.nodes,
                exported.focus(app.focused_node),
                scale,
            )
        } else if !app.presents_history() && !palette {
            tilo_ui_base::accesskit_bridge::build_tree_update_bound(
                app.root_widget.as_ref(),
                &exported.nodes,
                exported.focus(app.focused_node),
                scale,
            )
        } else {
            tilo_ui_base::accesskit_bridge::build_presentation_tree_bound(
                &app.presentation_roots(),
                &exported.nodes,
                exported.focus(app.focused_node),
                scale,
            )
        };
        // The window's bar is first in reading order, as the window's tree carries it.
        if let Some(row) = self
            .bar
            .as_ref()
            .filter(|_| !self.bar_at_rest)
            .and_then(|bar| bar.accessibility(app))
        {
            tilo_ui_base::accesskit_bridge::publish_host_part(&mut update, &row, scale);
        }
        update
    }

    /// The exported node `name` addresses, as [`Self::a11y_nodes_named`] reads it: by its accessible
    /// name, or by the line a list row is titled with.
    #[must_use]
    pub fn a11y_node_named(&self, name: &str) -> Option<accesskit::NodeId> {
        self.a11y_nodes_named(name).into_iter().next()
    }

    /// The node the exported tree marks focused: where a screen reader sits.
    #[must_use]
    pub fn a11y_focus(&self) -> Option<(accesskit::NodeId, accesskit::Node)> {
        let update = self.a11y_tree_update();
        let focus = update.focus;
        update.nodes.into_iter().find(|(id, _)| *id == focus)
    }

    /// Perform an accessibility action on an exported node, as an assistive technology does: the
    /// app routes it, and what its widget leaves to the pointer is pressed where the node is drawn.
    ///
    /// # Errors
    /// When the node is not in the current frame: a stale reference is reported, never retargeted.
    pub fn perform_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &tilo_ui_base::a11y::Action,
    ) -> Result<(), String> {
        if !self.a11y_nodes().iter().any(|(id, _)| *id == target) {
            return Err(format!(
                "no accessibility node {target:?} in the current frame"
            ));
        }
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Action);
        let running = self.running();
        let bar_took = self
            .bar
            .as_mut()
            .is_some_and(|bar| bar.perform_accessibility(&mut self.app, target, action));
        drop(running);
        if bar_took {
            self.apply_window_actions();
            self.run_frame();
            return Ok(());
        }
        if let Some((x, y)) = self.app.dispatch_a11y_action_checked(target, action)? {
            self.click_at(x, y);
        }
        self.run_frame();
        Ok(())
    }

    /// Press the control a person finds by `press`'s address — the name it answers to, what it
    /// does, or the resource a list row presents — as a pointer or an assistive technology presses
    /// it: the same route [`Self::perform_a11y_action`] takes, so a row opens its own item rather
    /// than a widget's id standing in for the press.
    ///
    /// # Errors
    /// When no control answers to the address, or the one that does takes no such press.
    pub fn press_control(&mut self, press: &crate::headless::Press) -> Result<(), String> {
        use tilo_ui_base::a11y::Action;
        let action = match press.verb {
            crate::headless::PressVerb::Activate => Action::Activate,
            crate::headless::PressVerb::ShowContextMenu => Action::ShowContextMenu,
        };
        let wanted = tilo_ui_base::accesskit_bridge::map_action(&action);
        // The control a press finds is one the next few frames show: a menu the step before
        // opened, the row an action just made, the window's own bar as it settles.
        let mut target = None;
        for _ in 0..PRESS_FRAMES {
            target = self.control_for(press, wanted);
            if target.is_some() {
                break;
            }
            self.run_frame();
        }
        let target =
            target.ok_or_else(|| format!("no control {} is offered", address_of(press)))?;
        self.perform_a11y_action(target, &action)
    }

    /// The exported node `press` addresses, as the assistive technology acts on it: by what the
    /// control does, by the address of the resource a row presents, or by the words it answers
    /// to.
    fn control_for(
        &self,
        press: &crate::headless::Press,
        wanted: accesskit::Action,
    ) -> Option<accesskit::NodeId> {
        if let Some(identity) = &press.identity {
            return self
                .a11y_nodes()
                .into_iter()
                .find(|(_, described)| {
                    described.author_id() == Some(identity.as_str())
                        && described.supports_action(wanted)
                })
                .map(|(id, _)| id);
        }
        let Some(resource) = &press.resource else {
            let name = press.name.as_deref().unwrap_or_default();
            return self.a11y_nodes_named(name).into_iter().find(|id| {
                self.a11y_node(*id)
                    .is_some_and(|described| described.supports_action(wanted))
            });
        };
        // The address is one this window publishes, read the way an action's resource argument
        // is ([`Self::reference_arguments`]): the published resource names the app the run
        // installed, and the row that presents it is the one a person opens — by the line it is
        // titled with, which is how that row is reached by name too.
        let resources = self.resources();
        let published = find_resource(&resources, resource)?;
        let node = self.control_node_of_resource(published.id.as_str()).ok()?;
        let title = self
            .app
            .inspect_node(node, |widget| {
                widget
                    .accessibility()
                    .and_then(|access| access.title.or(access.name))
            })
            .ok()
            .flatten()?;
        let by_title = self.a11y_nodes_named(&title).into_iter().find(|id| {
            self.a11y_node(*id)
                .is_some_and(|described| described.supports_action(wanted))
        });
        if by_title.is_some() {
            return by_title;
        }
        let id = accesskit::NodeId(u64::from(node).saturating_add(1));
        self.a11y_nodes()
            .iter()
            .any(|(exported, described)| *exported == id && described.supports_action(wanted))
            .then_some(id)
    }

    /// The laid-out widget whose own words or target name the resource `resource`: the row of a
    /// list that presents it, addressed as the page a browser draws addresses one.
    fn control_node_of_resource(&self, resource: &str) -> Result<NodeId, String> {
        for &node in self.app.layout.node_map.keys() {
            let Ok(Some(access)) = self.app.inspect_node(node, |widget| widget.accessibility())
            else {
                continue;
            };
            let names = [
                access.item.as_deref(),
                access.action_target.as_deref(),
                access.action.as_deref(),
            ];
            if names
                .into_iter()
                .flatten()
                .any(|name| tilo_ui_base::a11y::names_one_resource(name, resource))
            {
                return Ok(node);
            }
        }
        Err(format!("no control shows {resource:?} in this window"))
    }

    /// The window's semantics tree as it stands now: every node a person, a reader and an agent
    /// reach, with its role, name, state, bounds and the actions it offers. A renderer that
    /// presents the window from its semantics reads it here and runs a node's action with
    /// [`Self::perform_semantic`].
    ///
    /// It is built when it is read, as a window builds it when an assistive technology asks:
    /// a frame nobody reads the semantics of builds none, so a scroll's frames cost the window
    /// what it draws, not a walk of every widget it holds.
    pub fn semantic_tree(&mut self) -> &crate::a11y_tree::SemanticTree {
        let _ = self.app.refresh_semantic_tree();
        self.app.semantic_tree()
    }

    /// Perform `action` on the semantic node `id`, as an assistive technology's request does: the
    /// widget that owns the node takes it, and what it leaves to the pointer is pressed where the
    /// node stands. Whether anything took it; the frame after shows what it did.
    ///
    /// # Errors
    /// When the widget's component refuses the dispatch.
    pub fn perform_semantic(
        &mut self,
        id: &tilo_ui_base::a11y::SemanticId,
        action: &tilo_ui_base::a11y::Action,
    ) -> Result<bool, String> {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Action);
        let running = self.running();
        let handled = self.app.perform_semantic_verb(id, action.clone(), true);
        drop(running);
        self.run_frame();
        handled
    }

    /// Perform `action` on the widget named `name`.
    ///
    /// # Errors
    /// As [`Self::perform_semantic_named`].
    pub fn perform_named(
        &mut self,
        name: &str,
        action: &tilo_ui_base::a11y::Action,
    ) -> Result<(), String> {
        self.perform_semantic_named(name, None, action)
    }

    /// Perform `action` on the widget named `name`, or on the synthetic node under it that
    /// `semantic_target` names, through the app's semantic route.
    ///
    /// # Errors
    /// When nothing has that name, or what it names does not take the action.
    pub fn perform_semantic_named(
        &mut self,
        name: &str,
        semantic_target: Option<&str>,
        action: &tilo_ui_base::a11y::Action,
    ) -> Result<(), String> {
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Action);
        let id = self.semantic_id_named(name, semantic_target)?;
        let handled = self.app.perform_semantic_verb(&id, action.clone(), true)?;
        self.run_frame();
        if handled || matches!(action, tilo_ui_base::a11y::Action::Focus) {
            Ok(())
        } else {
            Err(format!(
                "widget {name:?} does not support semantic action {action:?}"
            ))
        }
    }

    /// The node of the app's semantic tree that `name` (and `semantic_target` under it) names.
    fn semantic_id_named(
        &mut self,
        name: &str,
        semantic_target: Option<&str>,
    ) -> Result<tilo_ui_base::a11y::SemanticId, String> {
        let _ = self.app.refresh_semantic_tree();
        let targets = self.named_target_nodes();
        let tree = self.app.semantic_tree();
        let by_target = |target: &str| {
            tree.iter()
                .find(|node| node.semantic_target.as_deref() == Some(target))
                .map(|node| node.id.clone())
        };
        let Some(&node) = targets.get(name) else {
            // A name no widget has is a target a widget publishes for a node of its own.
            if let Some(found) = by_target(semantic_target.unwrap_or(name)) {
                return Ok(found);
            }
            // A widget that paints rows itself (a menu's items) has no widget for each: a node
            // it publishes with its own target and a name is reached by that name, as a screen
            // reader and a tool reach it. One name that is two nodes is none of them in
            // particular.
            let mut named = tree.iter().filter(|candidate| {
                candidate.semantic_target.is_some()
                    && !candidate.state.hidden
                    && candidate.name.as_deref().map(str::trim) == Some(name)
            });
            return match (named.next(), named.next()) {
                (Some(found), None) => Ok(found.id.clone()),
                _ => Err(format!("no addressable widget named {name:?}")),
            };
        };
        let raw = u64::from(node);
        let found = tree.iter().find(|candidate| {
            candidate.widget_taffy_id == Some(raw)
                && candidate.semantic_target.as_deref() == semantic_target
        });
        if let Some(found) = found {
            return Ok(found.id.clone());
        }
        if let Some(target) = semantic_target {
            // A widget may name its own node with a target; a row's element may be published by
            // the list around it.
            return tree
                .iter()
                .find(|candidate| {
                    candidate.widget_taffy_id == Some(raw)
                        && candidate.semantic_target.is_none()
                        && self
                            .app
                            .inspect_node(node, |widget| {
                                widget
                                    .accessibility()
                                    .and_then(|access| access.action_target)
                            })
                            .ok()
                            .flatten()
                            .as_deref()
                            == Some(target)
                })
                .map(|candidate| candidate.id.clone())
                .or_else(|| by_target(target))
                .ok_or_else(|| format!("widget {name:?} has no semantic target {target:?}"));
        }
        tree.iter()
            .find(|candidate| candidate.widget_taffy_id == Some(raw))
            .map(|candidate| candidate.id.clone())
            .ok_or_else(|| format!("widget {name:?} exposes no accessibility semantics"))
    }

    /// The resources the app publishes now, as an agent lists them.
    ///
    /// Read from the tree the window shows, as a host reads its own tree: what a widget publishes
    /// only when it is asked for (a row, a part built for an answer, an item past the page it
    /// shows) is published here as it is not in a cached surface, which names what a frame put
    /// there.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let _entry = self.app.enter_wake_scope();
        let mut resources = Vec::new();
        crate::bound_dispatch::collect_widget_resources(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            &mut resources,
        );
        // The controls the app shows publish actions of their own, as a window hosts them: a
        // chip's press and its removal are one control with both.
        let mut semantics = crate::a11y_tree::SemanticTree::new();
        semantics.refresh_action_projection(
            self.app.root_widget.as_ref(),
            &self.app.layout.node_map,
            &self
                .app
                .bindings
                .semantic_node_names(&self.app.layout.node_map),
            self.app.focused_node,
        );
        resources.extend(semantics.action_resources());
        resources
    }

    /// The published resource `id`, resolved by the widget that publishes it, as a host resolves
    /// one address without building every resource the tree holds.
    #[must_use]
    pub fn resource(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        let _entry = self.app.enter_wake_scope();
        crate::bound_dispatch::find_resource(self.app.root_widget.as_ref(), &self.app.state.doc, id)
    }

    /// Invoke the published action at `address` as the person at the surface, as the call sent
    /// again with their approve when `approval` holds it ([`Self::approval`]), then run the frame
    /// that shows it.
    ///
    /// # Errors
    /// The app's refusals: an unknown, unavailable or denied action, or its failure.
    #[cfg(feature = "model")]
    pub fn invoke_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let actor = tilo_actions::Actor::Member(self.app.local_author.clone());
        self.invoke_action_as(&actor, address, arguments, approval)
    }

    /// [`Self::invoke_action`] for `actor`, who need not be the person at the surface: another
    /// member's or an agent's call reaches the app as the broker delivers it
    /// ([`Self::start_bound_job_within`]).
    ///
    /// # Errors
    /// As [`Self::invoke_action`].
    #[cfg(feature = "model")]
    pub fn invoke_action_as(
        &mut self,
        actor: &tilo_actions::Actor,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        // Search is the window's own API, served by its host rather than the app: a call to it
        // opens the window's search as its magnifier does.
        if crate::widget::search_request::is_search_address(address) {
            let mut resources = self.resources();
            resources.push(crate::widget::search_request::search_resource());
            let (_, spec) = tilo_actions::api::locate(&resources, address)
                .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
            let encoded = spec
                .build_json(arguments)
                .map_err(|error| tilo_actions::ApiError::InvalidArguments(error.to_string()))?;
            let request = crate::widget::search_request::SearchRequest::decode(&encoded.payload)
                .map_err(|error| tilo_actions::ApiError::InvalidArguments(error.to_string()))?;
            self.app.open_search(request);
            self.run_frame();
            return Ok(tilo_ui_base::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // Anyone but the person at the window reaches it through the workspace: their call is
        // admitted and delivered to the app for them, as the broker delivers it, never handed to
        // the window as one of its own.
        if let Some(member) = actor.member_id()
            && *member != self.app.local_author
        {
            let invoker = tilo_actions::ActionInvoker::member(member.clone());
            return self
                .start_job_within(Some(invoker), None, address, arguments, approval, None)
                .map(|(_, started)| started);
        }
        let argument = crate::app_core::json_argument(arguments);
        // The person's yes for a call the app sends on to the provider that runs it, as the device
        // hands it back when they answer: the frame carrying it is admitted once.
        if let Some(approval) = approval {
            self.hand_approval(address, approval);
        }
        // A view operation that shows an item first (the render of one, the lines of one) asks
        // for the view to be laid out before it reads it: a frame lays it out, and the
        // operation runs again, reading the item where the window shows it.
        let started = tilo_ui_base::widget::resource_operation::laying_out_first(|| {
            self.invoke_resource_as(actor, address, &argument, approval)
        });
        let started = match started {
            Err(error)
                if tilo_ui_base::widget::resource_operation::asks_to_lay_out_first(&error) =>
            {
                self.relayout();
                self.invoke_resource_as(actor, address, &argument, approval)
            }
            other => other,
        };
        self.run_frame();
        // A call the app hands the workspace carries no yes: the workspace keeps it asked, and the
        // approve the caller brings is the person's answer to that ask, admitted again by the
        // device ([`Self::confirm_asked`]).
        if let Some(approval) = approval
            && started.is_ok()
            && self.asking().is_some_and(|(asked, _)| asked == *address)
        {
            self.resolve_asked(approval.clone());
        }
        started
    }

    #[cfg(feature = "model")]
    fn invoke_resource_as(
        &mut self,
        actor: &tilo_actions::Actor,
        address: &tilo_actions::ActionAddress,
        argument: &str,
        approval: Option<&tilo_actions::remote::asked::Approval>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.app.invoke_resource_action(
            crate::app_core::ResourceInvocation::new(address, actor)
                .argument(Some(argument))
                .approval(approval),
        )
    }

    /// [`Self::invoke_action`], and the answer of a call the app does not run now: it hands its
    /// peer the call first (an external effect crosses the workspace before it touches anything),
    /// and answers when the peer delivers it back. The frames that take up that answer run here,
    /// and what the call ended as is returned. A call that answers at once, or runs on as a job,
    /// answers as [`Self::invoke_action`] does.
    ///
    /// # Errors
    /// As [`Self::invoke_action`], and the refusal a call the answer ends refused or failed with.
    #[cfg(feature = "model")]
    pub fn invoke_action_answering(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let sent = self.peer_requests().len();
        let started = self.invoke_action(address, arguments, approval)?;
        if matches!(
            started,
            tilo_ui_base::widget::ResourceOperationStart::Completed(_)
        ) {
            return Ok(started);
        }
        // What the app handed its peer before running it is answered in the frames after the
        // call: run them here, so a call refused as it starts answers its caller now. A call
        // taken as a job still runs on, and answers as [`Self::invoke_action`] does.
        self.idle();
        let Some(invocation) = self.invocation_sent_since(sent, address) else {
            return Ok(started);
        };
        // A call the workspace would not admit never runs: its caller hears why.
        if let Some(refusal) = self.own_refusal(invocation) {
            return Err(refusal);
        }
        // Another provider's action ends as that provider answered it.
        match self.provider_answer(invocation) {
            Some(tilo_actions::remote::ReceiptStatus::Completed { output }) => {
                return Ok(tilo_ui_base::widget::ResourceOperationStart::Completed(
                    output.clone(),
                ));
            }
            Some(tilo_actions::remote::ReceiptStatus::Failed { error }) => {
                return Err(jobs::failure_refusal(address, error.clone()));
            }
            Some(tilo_actions::remote::ReceiptStatus::Cancelled { message }) => {
                return Err(tilo_actions::ApiError::Failed(
                    message.clone().unwrap_or_else(|| "cancelled".to_owned()),
                ));
            }
            Some(tilo_actions::remote::ReceiptStatus::Rejected { error }) => {
                return Err(crate::session_api::api_error(error));
            }
            Some(tilo_actions::remote::ReceiptStatus::Accepted) | None => {}
        }
        let ended = self
            .call_updates(invocation)
            .into_iter()
            .find(tilo_actions::InvocationUpdate::is_terminal);
        match ended {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(
                tilo_ui_base::widget::ResourceOperationStart::Completed(output),
            ),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(jobs::failure_refusal(address, error))
            }
            Some(tilo_actions::InvocationUpdate::Cancelled { message, .. }) => Err(
                tilo_actions::ApiError::Failed(message.unwrap_or_else(|| "cancelled".to_owned())),
            ),
            _ => Ok(started),
        }
    }

    /// The invocation the app handed its peer since `sent` of its requests were made
    /// ([`Self::peer_requests`]) for a call of `address`, whose answers [`Self::job_updates`]
    /// reads. The call names the action the caller gave it: what an app publishes beside its
    /// own record (a surface's rows) reaches the peer under the alias that record binds, not
    /// under the id its view knows.
    #[cfg(feature = "model")]
    fn invocation_sent_since(
        &self,
        sent: usize,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::InvocationId> {
        self.peer_requests()
            .into_iter()
            .skip(sent)
            .find_map(|request| match request {
                tilo_ui_base::collaboration::CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                    ..
                } if request.target.address.action == address.action => Some(request.invocation),
                _ => None,
            })
    }

    /// Invoke `action` on the resource `resource` the app publishes, with JSON `arguments`, the
    /// way an agent's call reaches it, then run the frame that shows it.
    ///
    /// # Errors
    /// When `arguments` is no object, and as [`Self::invoke_action`].
    #[cfg(feature = "model")]
    pub fn call_on(
        &mut self,
        resource: &str,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.call_resource(resource, action, arguments, false)
    }

    /// [`Self::call_on`] as the call sent again once the person approved the asked record it was
    /// kept as ([`Self::approval`]).
    ///
    /// # Errors
    /// As [`Self::call_on`], except that it does not wait for the person's yes.
    #[cfg(feature = "model")]
    pub fn call_on_approved(
        &mut self,
        resource: &str,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.call_resource(resource, action, arguments, true)
    }

    #[cfg(feature = "model")]
    fn call_resource(
        &mut self,
        resource: &str,
        action: &str,
        arguments: serde_json::Value,
        approved: bool,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::ApiError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        let approval = approved.then(|| self.approval());
        let started = self.invoke_action(
            &tilo_actions::ActionAddress::new(resource, action),
            &arguments,
            approval.as_ref(),
        );
        self.idle();
        started
    }

    /// Invoke the published action `action` of `resource`, without the person's approve.
    ///
    /// # Errors
    /// As [`Self::invoke_action`].
    #[cfg(feature = "model")]
    pub fn invoke_published(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_action(
            &tilo_actions::ActionAddress::new(resource, action),
            arguments,
            None,
        )
    }

    /// What the app said of an action it refused, at the control that asked for it.
    #[must_use]
    pub fn control_notice(&self) -> Option<&crate::control_notice::ControlNotice> {
        self.app.control_notice()
    }

    /// Open the window's search as `request` asks, as the person's search key does.
    ///
    /// # Errors
    /// While a dialog the app holds open keeps the window to itself. An app that offers no search
    /// ignores the request, as a window does.
    #[cfg(feature = "model")]
    pub fn open_search(
        &mut self,
        request: &crate::widget::search_request::SearchRequest,
    ) -> Result<(), String> {
        if self.app.root_widget.modal_capture().is_some() {
            return Err("Finish the current dialog first".into());
        }
        self.app.open_search(request.clone());
        self.run_frame();
        Ok(())
    }

    /// Undo the focused document's last change, as the person's undo key does. Whether anything
    /// was undone.
    pub fn perform_undo(&mut self) -> bool {
        let moved = self
            .app
            .perform_undo_request(tilo_ui_base::widget::undo_access::UndoRequest::Undo);
        self.run_frame();
        moved
    }

    /// Redo what was undone last. Whether anything was redone.
    pub fn perform_redo(&mut self) -> bool {
        let moved = self
            .app
            .perform_undo_request(tilo_ui_base::widget::undo_access::UndoRequest::Redo);
        self.run_frame();
        moved
    }

    /// Whether undo and redo are possible for the focused document.
    pub fn undo_status(&mut self) -> tilo_ui_base::widget::undo_access::UndoStatus {
        self.app.focused_undo_status()
    }

    /// The pending controls History owes and what each waits on.
    #[cfg(feature = "model")]
    #[must_use]
    pub fn pending_history_controls(
        &self,
    ) -> Vec<crate::history_mode::pending_controls::PendingControlReview> {
        self.app.pending_history_controls()
    }

    /// Close every component the app hosts, as closing its window does.
    pub fn close_components(&mut self) {
        self.app.close_components();
    }

    /// What the components closed reported, without taking it.
    #[cfg(feature = "model")]
    #[must_use]
    pub fn component_close_reports(&self) -> &[crate::application::ComponentCloseReport] {
        self.app.component_close_reports()
    }

    /// The instance id of the hosted application component, the `instance_id` its bound actions
    /// are addressed by.
    #[cfg(feature = "model")]
    #[must_use]
    pub fn component_instance_id(&self) -> Option<crate::application::InstanceId> {
        crate::bound_dispatch::callback_binding(&*self.app.root_widget)
            .map(|binding| binding.instance_id)
    }

    /// The record the hosted component was launched on.
    #[cfg(feature = "model")]
    #[must_use]
    pub fn component_resource(&self) -> Option<crate::resource::DocumentCheckout> {
        crate::bound_dispatch::callback_binding(&*self.app.root_widget)
            .map(|binding| binding.checkout)
    }

    /// Run `steps` in order, as a scenario file runs its actions: the answers earlier steps bound
    /// fill later `{{name.path}}` placeholders, and the run stops at the first step that cannot be
    /// carried out. What the app shows afterwards, step by step.
    pub fn drive(&mut self, steps: &[crate::headless::Step]) -> crate::headless::Outcome {
        crate::app::driver::drive::drive(self, steps)
    }

    /// Have the record writer acknowledge every pending edit of the hosted component, then run
    /// the frame that shows the record saved: an explicit save boundary for a capture.
    ///
    /// # Errors
    /// When no component is hosted, or the write fails.
    #[cfg(feature = "model")]
    pub fn flush_component_resources(&mut self) -> Result<(), String> {
        let result = crate::bound_dispatch::callback_binding(&*self.app.root_widget)
            .ok_or_else(|| "the driver hosts no component".to_owned())?
            .records
            .try_flush();
        self.run_frame();
        result
    }

    /// Whether the controls the app pressed on its own (a startup refresh, a restored selection)
    /// have all run.
    #[must_use]
    pub fn controls_settled(&self) -> bool {
        !tilo_ui_base::widget::controls::has_pending_control_invocation(
            self.app.root_widget.as_ref(),
        ) && !self.app.actions.bound_calls_running()
    }

    /// Why what was done last did not run, as the window says it at the control used: none when
    /// it ran.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        self.app.control_notice().map(|notice| notice.text.clone())
    }

    /// What the components closed reported, taken.
    #[cfg(feature = "model")]
    pub fn take_component_close_reports(
        &mut self,
    ) -> Vec<crate::application::ComponentCloseReport> {
        self.app.take_component_close_reports()
    }

    /// The registry the app decodes its own actions with.
    pub fn action_registry_mut(&mut self) -> &mut tilo_actions::ActionRegistry {
        self.app.action_registry_mut()
    }

    /// Where the app sends the actions it does not run itself.
    ///
    /// # Errors
    /// While the app has actions running: replacing the transport then would lose their answers.
    /// What its widgets queued but no pass has dispatched yet is not such a call: it runs on the
    /// transport the app holds when it is dispatched, which may be this one.
    pub fn set_action_transport(
        &mut self,
        transport: Box<dyn tilo_actions::ActionTransport>,
    ) -> Result<(), String> {
        if self.app.actions.bound_calls_running() {
            return Err("cannot replace transport while actions are pending".into());
        }
        self.app.set_action_transport(transport);
        Ok(())
    }

    /// Present History in the window's header, as its History control does.
    pub fn present_history_in_header(&mut self) {
        self.app.present_history_in_header();
        self.run_frame();
    }

    /// Show the app's History.
    ///
    /// # Errors
    /// When the app has none.
    #[cfg(feature = "model")]
    pub fn open_history(&mut self) -> Result<(), String> {
        if self.app.history.is_none() {
            return Err("this app registered no History".into());
        }
        self.present_history_in_header();
        Ok(())
    }
}

// What a display's frame clock reads of the app between frames.
impl Driver {
    /// What the app asks of the next frame, without running one.
    #[must_use]
    pub fn frame_schedule(&self) -> crate::headless::FrameSchedule {
        let now = crate::time::now();
        let queued_work =
            self.app.wake_pending() || self.app.unsettled() || self.has_pending_owner_models();
        // Each of these walks the whole tree, so each is asked once and the answers shared: a tree
        // that moves owes a frame without the redraw walk, and the deadline needs no motion walk
        // of its own.
        let moving = self.app.surface_moving(now);
        let next_wake_deadline = self.next_frame_deadline_moving(now, moving);
        crate::headless::FrameSchedule {
            needs_redraw: self.frame_owed_by(now, moving, || next_wake_deadline),
            queued_work,
            needs_animation_frame: moving,
            next_wake_deadline,
        }
    }

    /// The widgets behind the app's animation demand and its due deadlines, each as the path of
    /// names from the root to the deepest widget whose demand none of its children explains.
    #[must_use]
    pub fn frame_requesters(&self) -> Vec<(String, crate::headless::FrameDemand)> {
        fn visit(
            widget: &dyn Widget,
            now: crate::time::Instant,
            path: &mut Vec<String>,
            out: &mut Vec<(String, crate::headless::FrameDemand)>,
        ) -> (bool, bool) {
            let animating = widget.needs_animation_frame();
            let due = widget.next_wake_deadline().is_some_and(|at| at <= now);
            if !animating && !due {
                return (false, false);
            }
            path.push(widget.name().to_owned());
            let (mut child_animating, mut child_due) = (false, false);
            widget.for_each_child(&mut |child| {
                let (animating, due) = visit(child, now, path, out);
                child_animating |= animating;
                child_due |= due;
            });
            if animating && !child_animating {
                out.push((path.join("/"), crate::headless::FrameDemand::Animation));
            }
            if due && !child_due {
                out.push((path.join("/"), crate::headless::FrameDemand::Deadline));
            }
            path.pop();
            (animating, due)
        }
        let now = crate::time::now();
        let mut out = Vec::new();
        for root in self.app.presentation_roots() {
            visit(root, now, &mut Vec::new(), &mut out);
        }
        out
    }

    /// The frame drawn last, with what the app exports and asks of the next one.
    #[must_use]
    pub fn frame_output(&self) -> crate::headless::FrameOutput<'_> {
        let schedule = self.frame_schedule();
        crate::headless::FrameOutput {
            scene: &self.scene,
            background: self.app.theme.tokens.colors.surfaces.content,
            logical_size: self.viewport(),
            device_size: self.device_size(),
            semantics: self.inspection(),
            ime: self.ime_window(),
            needs_animation_frame: schedule.needs_animation_frame,
            next_wake_deadline: schedule.next_wake_deadline,
            needs_redraw: schedule.needs_redraw,
        }
    }

    /// What the frame drawn last hands a renderer.
    #[must_use]
    pub fn frame_render_input(&self) -> crate::headless::FrameRenderInput<'_> {
        crate::headless::FrameRenderInput {
            scene: &self.scene,
            background: self.app.theme.tokens.colors.surfaces.content,
        }
    }

    /// Where the app's text input stands, as an input method is told.
    #[must_use]
    pub fn ime_window(&self) -> Option<tilo_ui_base::ime::ImeWindowState> {
        self.app.ime_window_state()
    }
}

// A mounted component, called the way an agent calls it.
impl Driver {
    /// Invoke `address` with `arguments` as the person at the surface.
    ///
    /// # Errors
    /// As [`Self::invoke_action`].
    #[cfg(feature = "model")]
    pub fn call(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_action(address, &arguments, None)
    }

    /// [`Self::call`] once the person allowed this exact request: it comes back with their
    /// approve, as the device sends it again.
    ///
    /// # Errors
    /// As [`Self::invoke_action`].
    #[cfg(feature = "model")]
    pub fn call_confirmed(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let approval = self.approval();
        self.invoke_action(address, &arguments, Some(&approval))
    }

    /// Whether the app publishes `action` on `resource`.
    #[must_use]
    pub fn publishes(&self, resource: &str, action: &str) -> bool {
        self.contract(resource, action).is_some()
    }

    /// The contract of `action` on `resource`, as the app publishes it.
    #[must_use]
    pub fn contract(&self, resource: &str, action: &str) -> Option<tilo_actions::ActionSpec> {
        tilo_actions::api::resolve(
            &self.resources(),
            &tilo_actions::ActionAddress::new(resource, action),
        )
        .cloned()
    }

    /// Run the frame owed and offer the record's writer what changed.
    pub fn settle(&mut self) {
        self.run_frame();
        self.persist_tick();
    }
}

// The `--headless-drive` grammar, step by step.
impl Driver {
    /// Run one action of the headless grammar.
    ///
    /// # Errors
    /// When the action cannot be carried out.
    pub fn perform(&mut self, action: &crate::headless::Action) -> Result<(), String> {
        use crate::headless::Action;
        match action {
            Action::ClickNamed(name) => self.click_named(name),
            Action::ClickAt(x, y) => {
                self.click_at(*x, *y);
                Ok(())
            }
            Action::TapNamed(name) => self.tap_named(name),
            Action::TapAt(x, y) => {
                self.tap_at(*x, *y);
                Ok(())
            }
            Action::SwipeAt(x0, y0, x1, y1) => {
                self.swipe_at((*x0, *y0), (*x1, *y1));
                Ok(())
            }
            Action::Key(key) => {
                self.key(key);
                Ok(())
            }
            Action::Type(text) => {
                self.type_text(text);
                Ok(())
            }
            Action::Modifiers(names) => {
                self.set_modifiers(names);
                Ok(())
            }
            Action::WaitForNamed { target, timeout_ms } => {
                self.wait_for_named(target, crate::time::Duration::from_millis(*timeout_ms))
            }
            Action::Expect(expectation) => self.expect(expectation),
            Action::Perform {
                target,
                semantic_target,
                action,
            } => {
                let action = crate::app::driver::semantic_action_from_wire(action.clone());
                // The window's bar is the window's, not a widget's: its parts are performed on
                // the bar, its search's parts by their own targets.
                if target == WINDOW_PARTS {
                    let part = semantic_target
                        .as_deref()
                        .ok_or("performing on the window's bar names one of its parts")?;
                    return self.perform_window_part(part, &action);
                }
                if target.starts_with("window-search-") {
                    return self.perform_window_part(target, &action);
                }
                self.perform_semantic_named(target, semantic_target.as_deref(), &action)
            }
            Action::Invoke(invocation) => self.perform_invocation(invocation).map(drop),
            Action::Press(press) => self.press_control(press),
            Action::Search(query) => self.search_app(query),
        }
    }

    /// Open the system's search scoped to this app with `query` typed in it, as the app's search
    /// button or the window's search does: what the app holds, found where a person finds it.
    ///
    /// # Errors
    /// While a dialog the app holds open keeps the window to itself; without Tilo's model there
    /// is no search to open.
    fn search_app(&mut self, query: &str) -> Result<(), String> {
        #[cfg(feature = "model")]
        {
            let request =
                crate::widget::search_request::SearchRequest::this_app().with_query(query);
            self.open_search(&request)
        }
        #[cfg(not(feature = "model"))]
        {
            let _ = query;
            Err("opening the app's search needs Tilo's model".into())
        }
    }

    /// Run one scenario step with the answers earlier steps bound: an invocation's resource and
    /// arguments get their `{{name.path}}` placeholders filled, and one that names `bind` keeps
    /// its answer (`{"value": ..., "resources": [...]}`) for the steps after it.
    ///
    /// # Errors
    /// When the step is no action, a placeholder does not resolve, or the action fails.
    pub fn perform_bound(
        &mut self,
        step: &crate::headless::Step,
        bindings: &mut crate::app::driver::drive::Bindings,
    ) -> Result<(), String> {
        use crate::headless::Action;
        let Some(action) = step.action() else {
            return Err(format!("{} is not an action to run", step.name()));
        };
        if let Action::Press(press) = action {
            // A press names what an earlier step bound as readily as a word it was written with:
            // the row of a resource an action answered is the row this step opens.
            let mut press = press.clone();
            let filled = |value: &Option<String>| -> Result<Option<String>, String> {
                let Some(value) = value else {
                    return Ok(None);
                };
                match tilo_headless_schema::substitute(
                    &serde_json::Value::String(value.clone()),
                    bindings,
                )? {
                    serde_json::Value::String(found) => Ok(Some(found)),
                    _ => Err("a press placeholder must name a string".into()),
                }
            };
            press.name = filled(&press.name)?;
            press.identity = filled(&press.identity)?;
            press.resource = filled(&press.resource)?;
            return self.press_control(&press);
        }
        let Action::Invoke(invocation) = action else {
            return self.perform(action);
        };
        let mut invocation = invocation.clone();
        if let serde_json::Value::String(resource) = tilo_headless_schema::substitute(
            &serde_json::Value::String(invocation.resource.clone()),
            bindings,
        )? {
            invocation.resource = resource;
        } else {
            return Err("a resource placeholder must name a string".into());
        }
        let serde_json::Value::Object(args) = tilo_headless_schema::substitute(
            &serde_json::Value::Object(invocation.args.clone()),
            bindings,
        )?
        else {
            return Err("invocation arguments must stay an object".into());
        };
        invocation.args = args;
        self.reference_arguments(&mut invocation.args)?;
        let answer = self.perform_invocation(&invocation)?;
        if let Some(name) = &invocation.bind {
            let answer = answer.ok_or_else(|| {
                format!("{name}: the action answered nothing to bind (it was refused or runs on)")
            })?;
            bindings.insert(
                name.clone(),
                serde_json::json!({ "value": answer.value, "resources": answer.resources }),
            );
        }
        Ok(())
    }

    /// What `invocation` names is a published action, and only Tilo's model dispatches those.
    #[cfg(not(feature = "model"))]
    #[expect(
        clippy::unused_self,
        reason = "the model's build of this step invokes through the driver"
    )]
    fn perform_invocation(
        &mut self,
        invocation: &crate::headless::Invocation,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        Err(format!(
            "{}#{}: invoking a published action needs Tilo's model",
            invocation.resource, invocation.action
        ))
    }

    /// The invocation's arguments with each resource a plan names (`{"resource": "<address>"}`)
    /// answered as the reference its action takes: the published resource's app, kind, id and
    /// label. The address is the published one, so it names the app the run installed, a
    /// development build's included — what a plan writes without knowing the run.
    ///
    /// # Errors
    /// No resource this window publishes has that address.
    fn reference_arguments(
        &self,
        args: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        // The published tree is read once a step, and only when one of its arguments is a resource.
        let mut published: Option<Vec<tilo_actions::ResourceSpec>> = None;
        for (name, value) in args.iter_mut() {
            let Some(named) = value
                .as_object()
                .filter(|fields| fields.len() == 1)
                .and_then(|fields| fields.get("resource"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let address = named.to_owned();
            let resources = published.get_or_insert_with(|| self.resources());
            let spec = find_resource(resources, &address).ok_or_else(|| {
                format!(
                    "argument `{name}` names {address:?}, which this window publishes nothing of"
                )
            })?;
            let app = spec
                .app_id
                .clone()
                .or_else(|| {
                    address
                        .strip_prefix("tl:")
                        .and_then(|rest| rest.split_once(':'))
                        .map(|(app, _)| app.to_owned())
                })
                .filter(|app| !app.is_empty())
                .ok_or_else(|| {
                    format!("argument `{name}` names {address:?}, which no app publishes")
                })?;
            *value = serde_json::json!({
                "app": app,
                "kind": spec.kind,
                "id": address,
                "label": spec.label,
            });
        }
        Ok(())
    }

    /// Invoke what `invocation` names; what a completed action returned (`None` when it runs on or
    /// was refused as expected).
    ///
    /// An app hosted beside this window runs a call off the frame, so a step that binds what the
    /// call made, or expects the refusal it ended as, waits for the answer the app keeps
    /// ([`App::take_window_invocation_answer`]) instead of reading the call's start.
    #[cfg(feature = "model")]
    fn perform_invocation(
        &mut self,
        invocation: &crate::headless::Invocation,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        use tilo_actions::InvocationUpdate;
        let address = tilo_actions::ActionAddress::new(&invocation.resource, &invocation.action);
        // A step the person allowed comes back with their approve, carrying the phrase they typed.
        let approval = (invocation.confirm || invocation.confirmation_phrase.is_some())
            .then(|| self.approval_typing(invocation.confirmation_phrase.as_deref()));
        // A press the call causes may start a call of its own: the one this step reads is the
        // call it made, whatever ran beside it.
        let _ = self.app.take_started_window_invocation();
        let result = self.invoke_action(&address, &invocation.args, approval.as_ref());
        // A call the window hands the workspace is addressed to the provider as that window
        // publishes it (its own alias for the app's resource), so what tells this step's call
        // apart is the action it names.
        let runs_on = self
            .app
            .take_started_window_invocation()
            .filter(|(_, started)| started.action == address.action)
            .map(|(invocation, _)| invocation);
        self.retain_invocation_outcome(&address, &result)?;
        match result {
            Ok(tilo_ui_base::widget::ResourceOperationStart::Completed(output)) => {
                if let Some(code) = invocation.expect_error.as_deref() {
                    return Err(format!(
                        "expected the API to refuse with {code}, but the action ran"
                    ));
                }
                Ok(Some(output))
            }
            Ok(tilo_ui_base::widget::ResourceOperationStart::Accepted) => {
                // Only a call this window started keeps its answer here: one it cannot read
                // leaves a step that binds nothing to bind, and one that expects a refusal with
                // the call it made.
                let reads_the_answer =
                    invocation.bind.is_some() || invocation.expect_error.is_some();
                let Some(runs_on) = runs_on.filter(|_| reads_the_answer) else {
                    if let Some(code) = invocation.expect_error.as_deref() {
                        return Err(format!(
                            "expected the API to refuse with {code}, but the action ran"
                        ));
                    }
                    return Ok(None);
                };
                let answer = self.await_answer(runs_on, &address)?;
                match (&answer, invocation.expect_error.as_deref()) {
                    (InvocationUpdate::Succeeded { output, .. }, None) => Ok(Some(output.clone())),
                    (InvocationUpdate::Succeeded { .. }, Some(code)) => Err(format!(
                        "expected the API to refuse with {code}, but the action ran"
                    )),
                    (InvocationUpdate::Failed { error, .. }, Some(code)) if error.code == code => {
                        Ok(None)
                    }
                    (InvocationUpdate::Failed { error, .. }, _) => Err(format!(
                        "{} on {:?}: {}: {}",
                        address.action, address.resource, error.code, error.message
                    )),
                    (InvocationUpdate::Cancelled { message, .. }, _) => {
                        let why = message
                            .as_deref()
                            .map(|message| format!(": {message}"))
                            .unwrap_or_default();
                        Err(format!(
                            "{} on {:?} was cancelled{why}",
                            address.action, address.resource
                        ))
                    }
                    _ => Err(format!(
                        "{} on {:?} answered no end a step reads",
                        address.action, address.resource
                    )),
                }
            }
            Err(error) => match invocation.expect_error.as_deref() {
                Some(code) if error.code() == code => Ok(None),
                _ => Err(format!("{}: {error}", error.code())),
            },
        }
    }

    /// Wait, bounded ([`ANSWER_WAIT`]), for what the call `invocation` ended as: an app hosted
    /// beside this window takes the call to run it off the frame, and keeps its answer for a
    /// caller that cannot wait where it invoked.
    #[cfg(feature = "model")]
    fn await_answer(
        &mut self,
        invocation: tilo_actions::InvocationId,
        address: &tilo_actions::ActionAddress,
    ) -> Result<tilo_actions::InvocationUpdate, String> {
        let unanswered = || {
            format!(
                "{} on {:?} answered nothing within {} ms; the app may still be running it",
                address.action,
                address.resource,
                ANSWER_WAIT.as_millis()
            )
        };
        if !self.wait_until(ANSWER_WAIT, |driver| {
            driver.app.window_invocation_answer(invocation).is_some()
        }) {
            return Err(unanswered());
        }
        self.app
            .take_window_invocation_answer(invocation)
            .ok_or_else(unanswered)
    }
}

/// The address a `press` was given, in the words a failed step names it by.
fn address_of(press: &crate::headless::Press) -> String {
    if let Some(name) = &press.name {
        return format!("named {name:?}");
    }
    if let Some(identity) = &press.identity {
        return format!("doing {identity:?}");
    }
    press.resource.as_ref().map_or_else(
        || "at that address".to_owned(),
        |r| format!("showing {r:?}"),
    )
}

/// The published resource `address` names in `resources`, at any depth: a document's items stand
/// under it as children, and a window publishes each action on the resource it is about.
fn find_resource<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    address: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    fn walk<'a>(
        resource: &'a tilo_actions::ResourceSpec,
        address: &str,
    ) -> Option<&'a tilo_actions::ResourceSpec> {
        if resource.id.as_str() == address {
            return Some(resource);
        }
        resource
            .children
            .iter()
            .find_map(|child| walk(child, address))
    }
    resources
        .iter()
        .find_map(|resource| walk(resource, address))
}

/// A logical `(width, height)` as a viewport: fractions truncate and negative or NaN sizes
/// become zero, so a float size names the same viewport it would as an integer.
#[must_use]
pub fn viewport<T: Into<f64>>((w, h): (T, T)) -> (u32, u32) {
    let (w, h) = (w.into(), h.into());
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "`as` saturates and maps NaN to zero, which is the documented contract"
    )]
    (w as u32, h as u32)
}

/// A key chord written `Ctrl+Shift+Key`: the key itself and the modifiers held for it.
pub(crate) fn chord(spec: &str) -> (&str, Vec<String>) {
    let mut held = Vec::new();
    let mut rest = spec;
    while let Some((name, after)) = rest.split_once('+') {
        let name = name.trim();
        if !matches!(
            name.to_ascii_lowercase().as_str(),
            "ctrl" | "control" | "shift" | "alt" | "logo" | "super" | "meta" | "command"
        ) {
            break;
        }
        held.push(name.to_owned());
        rest = after;
    }
    (rest, held)
}

/// The accessibility action a semantic step names, as the app's own action.
pub(crate) fn semantic_action_from_wire(
    action: tilo_headless_schema::SemanticAction,
) -> tilo_ui_base::a11y::Action {
    use tilo_headless_schema::SemanticAction as Wire;
    match action {
        Wire::Activate => tilo_ui_base::a11y::Action::Activate,
        Wire::Focus => tilo_ui_base::a11y::Action::Focus,
        Wire::ToggleCheck => tilo_ui_base::a11y::Action::ToggleCheck,
        Wire::Increment => tilo_ui_base::a11y::Action::Increment,
        Wire::Decrement => tilo_ui_base::a11y::Action::Decrement,
        Wire::Expand => tilo_ui_base::a11y::Action::Expand,
        Wire::Collapse => tilo_ui_base::a11y::Action::Collapse,
        Wire::ScrollIntoView => tilo_ui_base::a11y::Action::ScrollIntoView,
        Wire::ShowContextMenu => tilo_ui_base::a11y::Action::ShowContextMenu,
        Wire::SetValue(value) => tilo_ui_base::a11y::Action::SetValue(value),
        Wire::SetSelection { anchor, focus } => {
            tilo_ui_base::a11y::Action::SetSelection { anchor, focus }
        }
    }
}
