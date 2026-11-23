//! The desktop under the windows.
//!
//! [`desktop`] makes the [`ShellDesktop`] a session runs the desktop on: the policy that keeps its
//! windows as tabs of the desktop's strips ([`tilo_desktop_policy`]), the desktop's pass each
//! frame, and the shell's published side. The tilolang shell's wells place the windows
//! ([`super::WellPolicy`]), reading the same desktop.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::gestures::GestureConfig;
use tilo_compositor::{LayoutRect, WindowInfo};
use tilo_session_core::surface::{
    Scroll, ShellSurface, SurfaceIme, SurfaceKey, SurfaceLayer, SurfaceTextInput,
};

use tilo_desktop_policy::DesktopPolicy;
use tilo_desktop_policy::displays::Output;
use tilo_session_core::desktop::state::DesktopHandle;

use super::gestures::ShellGesture;
use super::{Placement, WidgetSurface};

/// The desktop both halves share.
struct Desktop {
    policy: DesktopPolicy,
    surface: WidgetSurface,
    /// What the gestures the desktop took asked, for its surface to carry out.
    gestures: Vec<ShellGesture>,
    /// How the desktop reads touchpad gestures; `None` leaves every one to
    /// the clients.
    gesture_config: Option<GestureConfig>,
    /// The monitor on each output, as the compositor knows it.
    monitors: Vec<(LayoutRect, String)>,
    /// The monitor the person made primary, while it is lit.
    primary: Option<String>,
    /// The shell's published side, while the session runs the desktop itself rather than a widget
    /// drawing it ([`ShellDesktop::run_desktop`]): each frame then takes the desktop's own pass.
    verbs: Option<tilo_session_core::desktop::shell_verbs::ShellVerbs>,
}

impl Desktop {
    /// The monitors the compositor holds, and the one the person made primary: told before any
    /// window is placed, so the desktop knows how many displays it spreads over (and its strips
    /// which display each is shown on) even while there is no window to place.
    fn set_monitors(&mut self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        monitors.clone_into(&mut self.monitors);
        self.primary = primary.map(str::to_owned);
        let Some(bounds) = monitors
            .iter()
            .map(|(rect, _)| *rect)
            .reduce(LayoutRect::merge)
        else {
            return;
        };
        let displays: Vec<Output> = monitors
            .iter()
            .map(|(rect, monitor)| Output {
                primary: primary == Some(monitor.as_str()),
                monitor: monitor.clone(),
                rect: ui::kurbo::Rect::new(
                    f64::from(rect.loc.x.saturating_sub(bounds.loc.x)),
                    f64::from(rect.loc.y.saturating_sub(bounds.loc.y)),
                    f64::from(rect.loc.x.saturating_sub(bounds.loc.x)) + f64::from(rect.size.w),
                    f64::from(rect.loc.y.saturating_sub(bounds.loc.y)) + f64::from(rect.size.h),
                ),
            })
            .collect();
        if self.policy.set_displays(&displays) {
            self.surface.relayout();
        }
    }

    /// Cover `outputs`' bounding box, the surface laid out against it.
    fn cover(&mut self, outputs: &[LayoutRect]) {
        let Some(bounds) = outputs.iter().copied().reduce(LayoutRect::merge) else {
            return;
        };
        let (Ok(width), Ok(height)) = (u32::try_from(bounds.size.w), u32::try_from(bounds.size.h))
        else {
            return;
        };
        let placement = Placement {
            origin: (bounds.loc.x, bounds.loc.y),
            size: (width.max(1), height.max(1)),
            scale: self.surface.placement().scale,
        };
        if placement != self.surface.placement() {
            self.policy.set_viewport_width(f64::from(placement.size.0));
            self.surface.set_placement(placement);
        }
        // Each display holds its own windows. A monitor is known by where
        // it sits, as the policy is told it, and the primary one is the
        // display the person chose for their bar, windows and sheets.
        let primary = self.primary.clone();
        let displays: Vec<Output> = outputs
            .iter()
            .map(|output| {
                let x = f64::from(output.loc.x.saturating_sub(bounds.loc.x));
                let y = f64::from(output.loc.y.saturating_sub(bounds.loc.y));
                let monitor = self
                    .monitors
                    .iter()
                    .find(|(rect, _)| rect == output)
                    .map_or_else(
                        || format!("output:{},{}", output.loc.x, output.loc.y),
                        |(_, monitor)| monitor.clone(),
                    );
                Output {
                    primary: primary.as_deref() == Some(monitor.as_str()),
                    monitor,
                    rect: ui::kurbo::Rect::new(
                        x,
                        y,
                        x + f64::from(output.size.w),
                        y + f64::from(output.size.h),
                    ),
                }
            })
            .collect();
        if self.policy.set_displays(&displays) {
            self.surface.relayout();
        }
    }
}

/// `value` rounded to a whole logical pixel.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a window's place on a screen"
)]
fn round(value: f64) -> i32 {
    value.round() as i32
}

/// `rect` of the surface, as it stands in the logical layout where the desktop's origin is
/// `(ox, oy)`.
/// A toolkit shadow round a card of `radius` , as the compositor draws it.
fn frame_of(shadow: &ui::tokens::Shadow, radius: f64) -> tilo_compositor::WindowFrame {
    let colour = |color: vello::peniko::Color| color.to_rgba8().to_u8_array();
    tilo_compositor::WindowFrame {
        radius,
        shadow: (shadow.blur > 0.0).then(|| tilo_compositor::Shadow {
            offset_y: f64::from(shadow.offset_y),
            blur: f64::from(shadow.blur),
            rgba: colour(shadow.color),
        }),
        edge: shadow.outline.map(|line| tilo_compositor::Edge {
            width: f64::from(line.width),
            rgba: colour(line.color),
        }),
    }
}

fn placed(rect: ui::kurbo::Rect, (ox, oy): (i32, i32)) -> (i32, i32, i32, i32) {
    (
        ox.saturating_add(round(rect.x0)),
        oy.saturating_add(round(rect.y0)),
        round(rect.width()).max(1),
        round(rect.height()).max(1),
    )
}

/// The desktop drawn under every window; a clone is the same desktop.
#[derive(Clone)]
pub struct ShellDesktop {
    desktop: Rc<RefCell<Desktop>>,
}

/// Told about the windows a program opens and closes, by the policy: what
/// finds the program behind a window that named itself.
pub trait WindowObserver: Send + Sync {
    /// `window` named itself and is about to map.
    fn opened(&self, window: &WindowInfo);
    /// The window `id` is gone.
    fn closed(&self, id: u64);
}

/// A desktop of one workspace with one empty strip (`salt` seeds its ids), drawn at `scale`: the
/// surface under the windows, which the session runs the desktop on and the shell's wells place the
/// windows from ([`ShellDesktop::well_policy`]).
#[must_use]
pub fn desktop(salt: u16, scale: f64) -> ShellDesktop {
    let policy = DesktopPolicy::new(salt);
    let handle = policy.handle().clone();
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale,
    };
    // What the desktop itself types into takes the keys; everything else on it passes them on to
    // the window the person is in.
    let waking_handle = handle.clone();
    let keyboard = Rc::new(move || handle.wants_keyboard());
    let desktop = Rc::new(RefCell::new(Desktop {
        policy,
        surface: WidgetSurface::new(Box::new(ui::widget::Stack::new()), placement, true)
            .named(crate::i18n::t("desktop"))
            .with_keyboard(keyboard),
        gestures: Vec::new(),
        gesture_config: Some(GestureConfig::default()),
        monitors: Vec::new(),
        primary: None,
        verbs: None,
    }));
    // What another surface commands of the desktop — the chrome's own actions, an agent's — is
    // queued on its handle and taken up by the desktop's next pass: the handle asks for it, so the
    // desktop keeps its windows where the person put them.
    let asked = desktop.borrow().surface.asking();
    waking_handle.set_wake(std::sync::Arc::new(move || {
        asked.store(true, std::sync::atomic::Ordering::Release);
    }));
    ShellDesktop { desktop }
}

impl ShellDesktop {
    /// The desktop's policy, for everything else a session drives on it
    /// (strips and workspaces, the tree it keeps).
    pub fn with_policy<R>(&self, read: impl FnOnce(&mut DesktopPolicy) -> R) -> R {
        read(&mut self.desktop.borrow_mut().policy)
    }

    /// The apps whose windows keep what they show to themselves, as the
    /// session last learnt them: nothing the desktop draws, or names for the
    /// compositor to draw, shows one of their windows.
    pub fn set_withheld_apps(&self, apps: Vec<String>) {
        self.desktop
            .borrow()
            .policy
            .handle()
            .set_withheld_apps(apps);
    }

    /// Answer `look` and `read` of a window this session does not run, which
    /// the desktop publishes on the window's own resource: the tree holds what
    /// they say, and the session's [`tilo_session_core::surfaces::Look`] makes the picture. Any
    /// other address is left to the desktop's tree.
    fn read_window(
        &self,
        address: &ui::tilo_actions::ActionAddress,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, ui::tilo_actions::ApiError>
    {
        use tilo_session_core::desktop::window;
        let reads = address.action == "look" || address.action == "read";
        let Some(tab) = window::resource_tab(&address.resource).filter(|_| reads) else {
            return Ok(None);
        };
        let desktop = self.desktop.borrow();
        let handle = desktop.policy.handle();
        let Some(shown) = handle.read(|tree| window::shown(tree, tab)) else {
            // Not a window this session does not run: the tree publishes no
            // read action of it, so the address names nothing.
            return Err(ui::tilo_actions::ApiError::Unknown(address.clone()));
        };
        if address.action == "look" {
            // A window whose app keeps what it shows to itself is never
            // pictured, however it was asked: the address is not offered
            // while it is withheld, and a catalogue read before then does not
            // outlive it.
            if handle.withholds(&shown.app_id) {
                return Err(ui::tilo_actions::ApiError::Unavailable {
                    code: "withheld".into(),
                    reason: "this app's window keeps what it shows to itself".into(),
                });
            }
            let shot = look
                .window(shown.window, &shown.workspace)
                .map_err(|reason| ui::tilo_actions::ApiError::Unavailable {
                    code: "no_picture".into(),
                    reason,
                })?;
            return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "title": shown.title,
                        "app": shown.app_id,
                        "width": shot.width,
                        "height": shot.height,
                        "content": shot.content,
                    })),
                    resources: vec![shot.content],
                },
            )));
        }
        Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
            ui::tilo_actions::ActionOutput {
                value: Some(serde_json::json!({
                    "title": shown.title,
                    "app": shown.app_id,
                })),
                resources: Vec::new(),
            },
        )))
    }

    /// Answer the platform search API's `open` with the
    /// desktop's own search: a window's search control, its toolbar's magnifier,
    /// Ctrl+K and an agent all ask the platform to search, and the platform's
    /// search is the one that knows the person's apps, their files, what apps
    /// hold and the desktop's windows. The desktop's palette is drawn over the
    /// windows by the chrome ([`crate::platform::surfaces::ShellDesktop`]'s
    /// `SearchOverlay`), which is the one surface that may cover a window in
    /// front; opening it here means the same search every way into search
    /// reaches.
    ///
    /// `None` when `address` is not the platform search API.
    /// Carry out the shell's own verb at `address`, when the session runs the desktop
    /// ([`Self::run_desktop`]).
    fn invoke_verb(
        &self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, ui::tilo_actions::ApiError>
    {
        let desktop = self.desktop.borrow();
        let Some(verbs) = desktop.verbs.as_ref() else {
            return Ok(None);
        };
        Ok(verbs
            .invoke(address, arguments)?
            .map(ui::host_operation::ResourceOperationStart::Completed))
    }

    fn open_platform_search(
        &self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<ui::host_operation::ResourceOperationStart> {
        if !ui::search::is_search_address(address) {
            return None;
        }
        let request = ui::search::SearchRequest::from_arguments(arguments);
        let handle = self.desktop.borrow().policy.handle().clone();
        handle.open_search(ui::search::Scope::Workspace, request.query);
        Some(ui::host_operation::ResourceOperationStart::Completed(
            ui::tilo_actions::ActionOutput::default(),
        ))
    }

    /// Run the desktop here, each frame: its pass over the tree
    /// ([`DesktopPolicy::step`]) and the shell's published side `verbs`, for a shell that draws
    /// the screen itself and builds no widget of the desktop.
    pub fn run_desktop(&self, verbs: tilo_session_core::desktop::shell_verbs::ShellVerbs) {
        self.desktop.borrow_mut().verbs = Some(verbs);
    }

    /// The action a row chosen in search runs, for the session's action API to run as the app's
    /// own control would.
    pub fn take_unrouted_control(&self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.desktop
            .borrow_mut()
            .policy
            .pass_mut()
            .take_unrouted_control()
    }

    /// Read touchpad gestures by `config` from the next one on; `None`
    /// leaves them all to the clients.
    pub fn set_gestures(&self, config: Option<GestureConfig>) {
        self.desktop.borrow_mut().gesture_config = config;
    }

    /// The policy that places this desktop's windows where a tilolang shell's wells stand
    /// (`docs/designs/app-language/shell.md` §8.3), in place of the strips': what a session
    /// runs with when the shell draws the whole screen. The windows keep their tabs in this
    /// desktop, so a place in the person's placement names the window's well.
    #[must_use]
    pub fn well_policy(&self) -> super::WellPolicy {
        super::WellPolicy::over(Rc::new(self.clone()))
    }

    /// Draw the desktop with `theme`: the person's palette, tokens and fonts.
    pub fn set_theme(&self, theme: ui::Theme) {
        self.desktop.borrow_mut().surface.set_theme(theme);
    }

    /// What the touchpad gestures the desktop took asked since the last
    /// call, oldest first.
    pub fn take_gestures(&self) -> Vec<ShellGesture> {
        std::mem::take(&mut self.desktop.borrow_mut().gestures)
    }

    /// The windows whose bar asked for its menu since the last call, oldest
    /// first: a secondary click on a bar, and the place on the desktop it was
    /// pressed.
    pub fn take_tab_menus(&self) -> Vec<(u64, f64, f64)> {
        self.desktop
            .borrow()
            .policy
            .handle()
            .take_tab_menus()
            .into_iter()
            .map(|(tab, at)| (tab, at.x, at.y))
            .collect()
    }

    /// What the desktop asked to open since the last call, oldest first:
    /// the apps the session plans and starts, and the tab each window the
    /// desktop held for them will take.
    pub fn take_launches(&self) -> Vec<tilo_desktop_policy::LaunchAsk> {
        self.with_policy(DesktopPolicy::take_launches)
    }

    /// The desktop's widget tree as hosted: what it publishes, its
    /// accessibility tree, the actions it accepts.
    pub fn with_widgets<R>(&self, read: impl FnOnce(&mut WidgetSurface) -> R) -> R {
        read(&mut self.desktop.borrow_mut().surface)
    }

    /// The controls the windows' own content offers, each where it stands on
    /// the desktop ([`ui::window_targets`]): what an app describes of its
    /// window, moved from the window's place to where the desktop shows it.
    ///
    /// A shell that draws the screen replaced what the desktop draws, never
    /// the windows standing in it, so this is what it lists beside its own
    /// controls: the desktop's own widgets are not drawn there.
    #[must_use]
    pub fn window_controls(&self) -> Vec<ui::window_targets::WindowControl> {
        let desktop = self.desktop.borrow();
        let (ox, oy) = desktop.surface.placement().origin;
        desktop
            .policy
            .handle()
            .window_controls()
            .into_iter()
            .map(|mut control| {
                control.rect.0 += f64::from(ox);
                control.rect.1 += f64::from(oy);
                control
            })
            .collect()
    }
}

impl super::well_policy::WindowModel for ShellDesktop {
    fn policy(&self) -> std::cell::RefMut<'_, DesktopPolicy> {
        std::cell::RefMut::map(self.desktop.borrow_mut(), |desktop| &mut desktop.policy)
    }

    fn relayout(&self) {
        self.desktop.borrow_mut().surface.relayout();
    }

    fn cover(&self, outputs: &[LayoutRect]) {
        self.desktop.borrow_mut().cover(outputs);
    }

    fn monitors(&self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        self.desktop.borrow_mut().set_monitors(monitors, primary);
    }

    fn gesture_config(&self) -> Option<GestureConfig> {
        self.desktop.borrow().gesture_config
    }

    fn took_gesture(&self, gesture: ShellGesture) {
        self.desktop.borrow_mut().gestures.push(gesture);
    }
}

impl tilo_session_core::surfaces::Published for ShellDesktop {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        let mut desktop = self.desktop.borrow_mut();
        let mut resources = desktop
            .verbs
            .as_ref()
            .map(tilo_session_core::desktop::shell_verbs::ShellVerbs::resources)
            .unwrap_or_default();
        resources.extend(tilo_session_core::surfaces::Published::resources(
            &mut desktop.surface,
        ));
        resources
    }

    fn revision(&self) -> Option<u64> {
        // What the desktop publishes is its tree's as drawn — the windows, its fields and its
        // controls — and its search, which the bar above the windows moves as the person types
        // there and the rows come in. A frame it drew may have moved any of them, and the API
        // tells a frame that moved nothing it publishes from one that did.
        let desktop = self.desktop.borrow();
        let handle = desktop.policy.handle();
        // The shell's own rows also name the windows that stopped answering.
        let unresponsive = handle.shared.borrow().unresponsive_revision;
        tilo_session_core::surfaces::Published::revision(&desktop.surface).map(|drawn| {
            drawn
                .wrapping_add(handle.navigation_revision())
                .wrapping_add(handle.search_revision())
                .wrapping_add(unresponsive)
        })
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError> {
        if let Some(answered) = self.read_window(address, look)? {
            return Ok(answered);
        }
        if let Some(answered) = self.open_platform_search(address, arguments) {
            return Ok(answered);
        }
        if let Some(answered) = self.invoke_verb(address, arguments)? {
            return Ok(answered);
        }
        tilo_session_core::surfaces::Published::invoke(
            &mut self.desktop.borrow_mut().surface,
            address,
            arguments,
            look,
        )
    }

    fn invoke_as(
        &mut self,
        actor: &ui::tilo_actions::Actor,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError> {
        if let Some(answered) = self.read_window(address, look)? {
            return Ok(answered);
        }
        if let Some(answered) = self.open_platform_search(address, arguments) {
            return Ok(answered);
        }
        if let Some(answered) = self.invoke_verb(address, arguments)? {
            return Ok(answered);
        }
        tilo_session_core::surfaces::Published::invoke_as(
            &mut self.desktop.borrow_mut().surface,
            actor,
            address,
            arguments,
            look,
        )
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        tilo_session_core::surfaces::Published::accessible_labels(
            &mut self.desktop.borrow_mut().surface,
        )
    }

    /// The desktop's own controls — its strips, its windows' bars — and, behind
    /// them, the controls the apps that draw the windows describe of their
    /// own content ([`ui::window_targets`]): a window's app keeps its widget
    /// tree to itself, so this is the one place they are named.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        let mut shown = tilo_session_core::surfaces::Published::controls(
            &mut self.desktop.borrow_mut().surface,
        );
        // Every window's own controls stand where they stand, beside the desktop's: two windows go
        // by the same names — each window's sidebar toggle, each window's "More actions" — and
        // each is named where it is drawn. A window's control is never the desktop's because they
        // share a name; a press by name reaches the window in front ([`Self::press_action`]). The
        // windows' controls stand where the desktop draws them, in its coordinates: moved, as its
        // own are, to where the desktop is placed in the logical layout.
        for control in self.window_controls() {
            if !shown
                .iter()
                .any(|seen| seen.name == control.name && seen.rect == control.rect)
            {
                shown.push(control);
            }
        }
        shown
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        self.press_action(name, ui::window_targets::TargetAction::Activate)
    }

    /// Act on the control that does `identity`, in whichever window draws it: the one the desktop
    /// lists, so what the window composed around the control and what the person's language
    /// calls it are not in the way.
    fn press_identity(
        &mut self,
        identity: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        let mut desktop = self.desktop.borrow_mut();
        // The window in front answers first, where it draws the control: a person pointing at it
        // presses that window's, never one behind it.
        let front = desktop.policy.handle().window_in_front();
        if front.is_some_and(|tab| desktop.policy.handle().window_does(tab, identity)) {
            return desktop
                .policy
                .handle()
                .press_window_control_doing(identity, action);
        }
        if desktop
            .policy
            .handle()
            .press_window_control_doing(identity, action)
            .is_ok()
        {
            return Ok(());
        }
        tilo_session_core::surfaces::Published::press_identity(
            &mut desktop.surface,
            identity,
            action,
        )
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        let mut desktop = self.desktop.borrow_mut();
        // Where the window in front stands, and whether it draws a control of that name itself:
        // taken before the desktop's own tree is built, which changes it.
        let (front, front_window, front_draws_it, a_window_draws_it) = {
            let handle = desktop.policy.handle();
            let front = handle.window_in_front();
            (
                front,
                front.and_then(|tab| handle.window_rect(tab)),
                front.is_some_and(|tab| handle.window_offers(tab, name)),
                handle
                    .window_controls()
                    .iter()
                    .any(|control| control.name == name),
            )
        };
        let chrome = tilo_session_core::surfaces::Published::controls(&mut desktop.surface);
        // A control the desktop draws in the front window's own chrome — its bar's buttons — is that
        // window's: it answers for it as its app's content does.
        let in_fronts_chrome = front_window.is_some_and(|window| {
            chrome.iter().any(|control| {
                let (x, y, width, height) = control.rect;
                let (centre_x, centre_y) = (x + width / 2.0, y + height / 2.0);
                control.name == name
                    && centre_x >= window.x0
                    && centre_x < window.x1
                    && centre_y >= window.y0
                    && centre_y < window.y1
            })
        });
        if in_fronts_chrome {
            return tilo_session_core::surfaces::Published::press_action(
                &mut desktop.surface,
                name,
                action,
            );
        }
        // The window in front answers for its own content: a person pointing at a control there
        // presses that window's, never one behind it that goes by the same name — another
        // window's sidebar toggle.
        if front_draws_it && let Some(front) = front {
            return desktop
                .policy
                .handle()
                .press_window_control_in(front, name, action);
        }
        if chrome.iter().any(|control| control.name == name) {
            return tilo_session_core::surfaces::Published::press_action(
                &mut desktop.surface,
                name,
                action,
            );
        }
        // The control is one a window's app draws: the press reaches it as its
        // own assistive technology's activation does.
        if a_window_draws_it {
            return desktop.policy.handle().press_window_control(name, action);
        }
        Err(format!("no control named {name}"))
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        tilo_session_core::surfaces::Published::a11y_tree(&mut self.desktop.borrow_mut().surface)
    }

    fn a11y_name(&self) -> Option<String> {
        tilo_session_core::surfaces::Published::a11y_name(&self.desktop.borrow().surface)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        tilo_session_core::surfaces::Published::a11y_action(
            &mut self.desktop.borrow_mut().surface,
            target,
            action,
        )
    }

    fn wants_keyboard(&self) -> bool {
        ShellSurface::wants_keyboard(self)
    }
}

impl ShellSurface for ShellDesktop {
    fn layers(&self) -> Vec<SurfaceLayer> {
        self.desktop.borrow().surface.layers()
    }

    fn withholds_captures(&self) -> bool {
        self.desktop.borrow().surface.withholds_captures()
    }

    fn below_windows(&self) -> bool {
        true
    }

    fn drawn_over_windows(&self) -> Vec<tilo_session_core::surface::WindowCover> {
        // The desktop draws a floating window's card itself, under the windows, so the windows it
        // covers are drawn around it, and under the shadows it casts.
        let desktop = self.desktop.borrow();
        let origin = desktop.surface.placement().origin;
        let theme = desktop.surface.host().theme();
        desktop
            .policy
            .float_covers()
            .into_iter()
            .map(|(window, card)| {
                let radius = ui::widget::floating_surface::card_radius(card, theme);
                tilo_session_core::surface::WindowCover {
                    window,
                    rect: placed(card, origin),
                    shadows: ui::surface::floating_lift(theme)
                        .iter()
                        .map(|shadow| frame_of(shadow, radius))
                        .collect(),
                }
            })
            .collect()
    }

    fn drawn_over_window(&self) -> Option<(u64, (i32, i32, i32, i32))> {
        // A window covering its display shows its own bar over itself while the person reaches for
        // the bar over the display; the desktop draws that bar, under the windows.
        let desktop = self.desktop.borrow();
        let origin = desktop.surface.placement().origin;
        desktop
            .policy
            .covering_bar()
            .map(|(window, bar)| (window, placed(bar, origin)))
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.desktop.borrow().surface.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.desktop.borrow_mut().surface.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.desktop.borrow_mut().surface.pointer_leave();
    }

    fn carries_window_at(&mut self, x: f64, y: f64) -> bool {
        let desktop = self.desktop.borrow();
        let (ox, oy) = desktop.surface.placement().origin;
        let point = ui::kurbo::Point::new(x - f64::from(ox), y - f64::from(oy));
        desktop.policy.handle().carry_next_press_at(point)
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.desktop
            .borrow_mut()
            .surface
            .pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, scroll: Scroll) {
        self.desktop.borrow_mut().surface.pointer_axis(scroll);
    }

    fn pointer_modifiers(&mut self, held: tilo_session_core::surface::KeyModifiers) {
        self.desktop.borrow_mut().surface.pointer_modifiers(held);
    }

    fn wants_keyboard(&self) -> bool {
        ShellSurface::wants_keyboard(&self.desktop.borrow().surface)
    }

    fn key(&mut self, key: &SurfaceKey) {
        self.desktop.borrow_mut().surface.key(key);
    }

    fn text_input(&self) -> Option<SurfaceTextInput> {
        self.desktop.borrow().surface.text_input()
    }

    fn ime(&mut self, event: SurfaceIme) {
        self.desktop.borrow_mut().surface.ime(event);
    }

    fn prepare_frame(&mut self) -> bool {
        let stepped = {
            let mut desktop = self.desktop.borrow_mut();
            desktop.verbs.is_some() && desktop.policy.step()
        };
        {
            let mut desktop = self.desktop.borrow_mut();
            // Something outside the desktop's tree asked for a pass — an app
            // described the controls of its window's content, a service
            // published what the desktop shows. The desktop reads what it changed in
            // that frame, as it does under the shell
            // ([`crate::shell::Aside`]), rather than whenever the next frame
            // comes for another reason. A desktop nobody asked draws nothing.
            if desktop
                .surface
                .asking()
                .load(std::sync::atomic::Ordering::Acquire)
            {
                desktop.surface.relayout();
            }
        }
        self.desktop.borrow_mut().surface.prepare_frame() || stepped
    }

    fn advance(&mut self) -> bool {
        self.desktop.borrow_mut().surface.advance()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.desktop.borrow().surface.next_deadline()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.desktop
            .borrow_mut()
            .surface
            .set_frame_interval(interval);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::SurfaceOutput]) {
        // The desktop draws at the scale of the first display there is, the one
        // it covers: its strips and the windows' bars paint the pixels that
        // display shows, rather than one scale's worth stretched over a
        // denser panel (which is what made everything grainy on a HiDPI
        // laptop). A desktop spanning displays of different scales draws at the
        // first's, as its layout is the bounding box of all of them.
        let scale = outputs.first().map_or(1.0, |output| output.scale);
        let mut desktop = self.desktop.borrow_mut();
        let placement = desktop.surface.placement();
        if (placement.scale - scale).abs() > f64::EPSILON {
            desktop
                .surface
                .set_placement(Placement { scale, ..placement });
        }
    }

    fn window_pictures(&self) -> Vec<tilo_session_core::surface::WindowPicture> {
        self.desktop.borrow().surface.window_pictures()
    }

    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        self.desktop.borrow().surface.backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        self.desktop.borrow_mut().surface.set_backdrops(images);
    }
}

/// Who a window policy tells of the windows the desktop gives a place, and
/// of the ones gone: the same for every policy placing the desktop's
/// windows ([`StripPolicy`], [`crate::platform::surfaces::WellPolicy`]).
#[derive(Default)]
pub(crate) struct WindowFollowers {
    /// Told the process of each window the compositor maps, the moment the
    /// desktop gives it a place.
    map_observer: Option<Rc<dyn Fn(u32)>>,
    /// Who follows the windows, when the session has someone that does.
    observer: Option<std::sync::Arc<dyn WindowObserver>>,
    /// The windows it was told about, so none is told of twice.
    announced: std::collections::HashSet<u64>,
}

impl WindowFollowers {
    /// Tell `observer` the process of each window the desktop gives a place.
    pub(crate) fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.map_observer = Some(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`.
    pub(crate) fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.observer = Some(observer);
    }

    /// `window` was given its place: tell its process's observer, and the
    /// window's observer once.
    pub(crate) fn mapped(&mut self, window: &WindowInfo) {
        if let (Some(observer), Some(pid)) = (self.map_observer.as_ref(), window.pid) {
            observer(pid);
        }
        let Some(observer) = self.observer.clone() else {
            return;
        };
        if !self.announced.insert(window.id) {
            return;
        }
        observer.opened(window);
    }

    /// Tell the observer that `id` is gone, once.
    pub(crate) fn gone(&mut self, id: u64) {
        if !self.announced.remove(&id) {
            return;
        }
        if let Some(observer) = &self.observer {
            observer.closed(id);
        }
    }
}

impl StripPolicy {
    /// Tell `observer` the process of each window the compositor maps, as the
    /// desktop gives it a place: the app's API opens to it from then on.
    pub fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        self.followers.set_window_mapped(observer);
    }

    /// Follow the windows the compositor opens and closes with `observer`:
    /// how a session learns which programs the person runs.
    pub fn observe_windows(&mut self, observer: std::sync::Arc<dyn WindowObserver>) {
        self.followers.observe_windows(observer);
    }

    /// Send every pointer sample and touch to `samples` too, wherever they
    /// go: the chrome that keeps out of the way until reached for.
    pub fn with_samples(&mut self, samples: Rc<RefCell<dyn PointerSamples>>) {
        self.samples = Some(samples);
    }

    /// Name apps with `names`, the session's: the installed package's
    /// localised name, else the machine's desktop entry, else the registry's.
    /// A tab opens with the name of its app, never with the app's id.
    pub fn reads_app_names(&mut self, names: Rc<dyn Fn(&str) -> String>) {
        self.desktop.borrow_mut().policy.reads_app_names(names);
    }

    /// Give `window` its tab, once, and lay the desktop out so its place
    /// is known.
    fn map(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) {
        {
            let mut desktop = self.desktop.borrow_mut();
            desktop.cover(outputs);
            if self.mapped.contains(&window.id) {
                return;
            }
            let mapped = desktop.policy.map(Window {
                id: window.id,
                app_id: window.app_id.clone(),
                title: window.title.clone(),
                parent: window.parent,
                min_size: window.min_size,
                max_size: window.max_size,
                pid: window.pid,
                tag: window.tag.clone(),
            });
            if let Err(error) = mapped {
                tracing::warn!(%error, window = window.id, "the desktop has no strip to open a window in");
                return;
            }
            self.mapped.push(window.id);
            for missed in desktop.policy.take_missed_holds() {
                tracing::warn!(
                    window = missed.window,
                    held = missed.held,
                    "the tab held for a window's launch left the layout before the window mapped: it took another"
                );
            }
            desktop.surface.relayout();
            // A window launched into a workspace not in front goes there once its tab is made.
            desktop.policy.carry_launched();
        }
        self.followers.mapped(window);
    }
}

impl WindowPolicy for StripPolicy {
    /// Focus follows the pointer onto another display: to the window last
    /// focused there, else to none, so the next window opens there.
    fn pointer_moved(&mut self, x: f64, y: f64) {
        if let Some(samples) = &self.samples {
            samples.borrow_mut().moved(x, y);
        }
        let mut desktop = self.desktop.borrow_mut();
        let (ox, oy) = desktop.surface.placement().origin;
        let (x, y) = (x - f64::from(ox), y - f64::from(oy));
        let handle = desktop.policy.handle().clone();
        let under = handle.read(|tree| {
            if !tree.is_partitioned() {
                return None;
            }
            tree.outputs
                .displays
                .iter()
                .find(|d| x >= d.x && x < d.x + d.width && y >= d.y && y < d.y + d.height)
                .map(|d| d.id)
        });
        if let Some(display) = under
            && handle.focused_display() != Some(display)
            && handle.focus_display(display)
        {
            desktop.surface.relayout();
        }
    }

    fn pointer_pressed(&mut self, x: f64, y: f64) {
        if let Some(samples) = &self.samples {
            samples.borrow_mut().pressed(x, y);
        }
    }

    fn touch(&mut self, touch: TouchSample) -> bool {
        self.samples
            .as_ref()
            .is_some_and(|samples| samples.borrow_mut().touch(touch))
    }

    fn monitors(&mut self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        self.desktop.borrow_mut().set_monitors(monitors, primary);
    }

    fn overlay(&self, window: &WindowInfo) -> bool {
        self.desktop.borrow().policy.draws_card(window.id)
    }

    /// A card's window takes no focus: the person works in the workspace it
    /// floats over, and the card is a view of the activity, not a place to
    /// type.
    fn focus_on_map(&mut self, window: &WindowInfo) -> bool {
        !self.desktop.borrow().policy.draws_card(window.id)
    }

    fn initial_size(&mut self, window: &WindowInfo, outputs: &[LayoutRect]) -> Option<(i32, i32)> {
        self.map(window, outputs);
        if !self.pending.contains(&window.id) {
            self.pending.push(window.id);
        }
        // A dialog picks its own size, and a card the size its app asked for.
        if let Some(size) = self.desktop.borrow().policy.card_size(window.id) {
            return Some(size);
        }
        let rect = self.desktop.borrow().shown(window.id)?;
        Some((rect.size.w, rect.size.h))
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[LayoutRect],
    ) -> LayoutPoint {
        self.map(window, outputs);
        self.pending.retain(|id| *id != window.id);
        let desktop = self.desktop.borrow();
        desktop
            .shown(window.id)
            .map(|rect| rect.loc)
            .or_else(|| desktop.floating(window.id, size))
            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0)))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, LayoutRect)],
        outputs: &[LayoutRect],
    ) -> Option<Vec<WindowPlacement>> {
        // A launched window whose tab came with the last layout goes to its own workspace.
        self.desktop.borrow_mut().policy.carry_launched();
        // A window the compositor lists has shown its buffer.
        self.pending
            .retain(|id| !windows.iter().any(|(window, _)| window.id == *id));
        let gone: Vec<u64> = self
            .mapped
            .iter()
            .copied()
            .filter(|id| {
                !self.pending.contains(id) && !windows.iter().any(|(window, _)| window.id == *id)
            })
            .collect();
        {
            let mut desktop = self.desktop.borrow_mut();
            desktop.cover(outputs);
            for id in &gone {
                desktop.policy.unmap(*id);
            }
            for (window, _) in windows {
                if let Some(title) = &window.title {
                    desktop.policy.retitle(window.id, title);
                }
                if let Some(app_id) = &window.app_id {
                    desktop.policy.set_app_id(window.id, app_id);
                }
            }
            if !gone.is_empty() {
                desktop.surface.relayout();
            }
        }
        self.mapped.retain(|id| !gone.contains(id));
        for id in &gone {
            self.followers.gone(*id);
        }
        for (window, _) in windows {
            self.map(window, outputs);
        }
        // A tab is never narrower than the smallest width of a window in it: a fixed-size
        // window shows whole, and the strip scrolls to it.
        if self.desktop.borrow_mut().policy.honour_minimum_widths(
            windows
                .iter()
                .map(|(window, _)| (window.id, window.min_size)),
        ) {
            self.desktop.borrow_mut().surface.relayout();
        }
        let desktop = self.desktop.borrow();
        let placement = desktop.policy.placement();
        let origin = desktop.surface.placement().origin;
        let of = |rects: &[(u64, ui::kurbo::Rect)], window: u64| {
            rects
                .iter()
                .find(|(id, _)| *id == window)
                .map(|(_, rect)| layout_rect(*rect, origin))
        };
        // The desktop decides which window covers its display: one that asked for it and was
        // given it back (the window's own control, its menu, a key) is told so.
        let covering = desktop.policy.covering_window();
        Some(
            windows
                .iter()
                .map(|(window, current)| {
                    if let Some(rect) = of(&placement.shown, window.id) {
                        return WindowPlacement {
                            id: window.id,
                            location: rect.loc,
                            size: Some((rect.size.w, rect.size.h)),
                            clip: of(&placement.clips, window.id),
                            fullscreen: Some(covering == Some(window.id)),
                        };
                    }
                    let size = (current.size.w, current.size.h);
                    WindowPlacement {
                        id: window.id,
                        location: desktop
                            .floating(window.id, size)
                            .unwrap_or_else(|| LayoutPoint::from((OFFSTAGE, 0))),
                        size: None,
                        clip: None,
                        fullscreen: None,
                    }
                })
                .collect(),
        )
    }

    fn withdrawn(&mut self, window: &WindowInfo) {
        self.pending.retain(|id| *id != window.id);
        self.mapped.retain(|id| *id != window.id);
        self.followers.gone(window.id);
        let mut desktop = self.desktop.borrow_mut();
        desktop.policy.unmap(window.id);
        desktop.surface.relayout();
    }

    fn focus_on_click(&mut self, window: &WindowInfo) -> bool {
        let mut desktop = self.desktop.borrow_mut();
        // Focus moves on the surface's next pass: run it, so the window's
        // place and focus are known when the compositor asks.
        if desktop.policy.focus(window.id) {
            desktop.surface.relayout();
        }
        true
    }

    fn focused(&mut self) -> Option<u64> {
        self.desktop.borrow().policy.focused()
    }

    /// The keyboard waits for the window in front of the person while it has not shown: one the
    /// shell placed that has drawn nothing yet, or the one a launch is bringing to the place.
    fn keyboard_awaits_window(&mut self) -> bool {
        let desktop = self.desktop.borrow();
        match desktop.policy.focused() {
            Some(window) => self.pending.contains(&window),
            None => desktop.policy.awaits_launched_window(),
        }
    }

    fn activate(&mut self, window: &WindowInfo, fresh: bool) -> bool {
        let mut desktop = self.desktop.borrow_mut();
        if fresh {
            // What the person just did opened it: bring it forward.
            if desktop.policy.focus(window.id) {
                desktop.surface.relayout();
            }
        } else if desktop.policy.ask_attention(window.id) {
            desktop.surface.relayout();
        }
        fresh
    }

    fn gesture(&mut self, gesture: &GestureEvent) -> bool {
        let config = self.desktop.borrow().gesture_config;
        let desktop = &self.desktop;
        self.gestures.take(gesture, config, |asked| {
            desktop.borrow_mut().gestures.push(asked);
        })
    }

    fn take_close_requests(&mut self) -> Vec<u64> {
        self.desktop.borrow_mut().policy.take_closed()
    }

    /// A window's own asking. A tab has no free geometry: the strip lays it
    /// out, so a move or a resize asks for nothing. Covering the display is
    /// the desktop's either way, the strip filling or the whole display, and
    /// the compositor is told the desktop took it rather than covering the
    /// window itself.
    fn request(&mut self, window: &WindowInfo, request: &WindowRequest) -> bool {
        let mut desktop = self.desktop.borrow_mut();
        let answered = match request {
            WindowRequest::Maximize(on) => desktop.policy.set_maximized(window.id, *on),
            WindowRequest::Fullscreen { on, .. } => desktop.policy.set_fullscreen(window.id, *on),
            WindowRequest::Move | WindowRequest::Resize(_) | WindowRequest::Minimize => {
                return false;
            }
        };
        if answered {
            desktop.surface.relayout();
        }
        false
    }
}

#[cfg(test)]
mod hidpi {
    use super::*;

    /// The desktop draws at the scale of the display it covers: its strips and
    /// the windows' bars paint the pixels a `HiDPI` panel shows, rather than
    /// one scale's worth stretched over twice the area.
    #[test]
    fn the_shell_draws_at_its_displays_scale() {
        let mut desktop = desktop(7, 1.0);
        ShellSurface::outputs_changed(
            &mut desktop,
            &[tilo_session_core::surface::SurfaceOutput {
                rect: (0, 0, 1472, 920),
                scale: 2.0,
            }],
        );
        let scale = desktop.with_widgets(|widgets| widgets.placement().scale);
        assert!(
            (scale - 2.0).abs() < f64::EPSILON,
            "the desktop on a 2 display draws at 2: it drew at {scale}"
        );
    }
}
