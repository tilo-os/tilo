//! The desktop under the windows.
//!
//! [`desktop`] makes the [`ShellDesktop`] a session runs the desktop on: the policy that keeps its
//! windows as tabs of the desktop's workspaces ([`tilo_desktop_policy`]), the desktop's pass, and
//! the shell's published side. The tilolang shell draws the desktop and its wells place the windows
//! ([`super::WellPolicy`]), reading the same desktop; nothing of it is drawn here.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_compositor::LayoutRect;
use tilo_compositor::gestures::GestureConfig;
use tilo_session_core::surface::{ShellSurface, SurfaceLayer};

use tilo_desktop_policy::DesktopPolicy;
use tilo_desktop_policy::displays::Output;

use super::gestures::ShellGesture;

/// The desktop every clone shares.
struct Desktop {
    policy: DesktopPolicy,
    /// Where the desktop begins in the logical layout: the top-left of the outputs' bounding box.
    origin: (i32, i32),
    /// How wide the outputs' bounding box is, as the policy was last told it.
    width: u32,
    /// Something asked for the desktop's next pass: a command queued on its handle, or a change
    /// to its windows the policy made.
    asked: Arc<AtomicBool>,
    /// Moves with every pass the desktop ran and every action run on it.
    revision: u64,
    /// What the gestures the desktop took asked, for the session to carry out.
    gestures: Vec<ShellGesture>,
    /// How the desktop reads touchpad gestures; `None` leaves every one to the clients.
    gesture_config: Option<GestureConfig>,
    /// The monitor on each output, as the compositor knows it.
    monitors: Vec<(LayoutRect, String)>,
    /// The monitor the person made primary, while it is lit.
    primary: Option<String>,
    /// The shell's published side ([`ShellDesktop::run_desktop`]).
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
            self.ask();
        }
    }

    /// Stand over `outputs`' bounding box: the policy lays the workspaces out against it.
    fn cover(&mut self, outputs: &[LayoutRect]) {
        let Some(bounds) = outputs.iter().copied().reduce(LayoutRect::merge) else {
            return;
        };
        let Ok(width) = u32::try_from(bounds.size.w) else {
            return;
        };
        self.origin = (bounds.loc.x, bounds.loc.y);
        let width = width.max(1);
        if width != self.width {
            self.width = width;
            self.policy.set_viewport_width(f64::from(width));
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
            self.ask();
        }
    }

    /// Ask for the desktop's next pass.
    fn ask(&self) {
        self.asked.store(true, Ordering::Release);
    }
}

/// The desktop under every window; a clone is the same desktop.
#[derive(Clone)]
pub struct ShellDesktop {
    desktop: Rc<RefCell<Desktop>>,
}

/// A desktop of one workspace with one empty strip (`salt` seeds its ids): what the session runs
/// the desktop on and the shell's wells place the windows from ([`ShellDesktop::well_policy`]).
#[must_use]
pub fn desktop(salt: u16) -> ShellDesktop {
    let policy = DesktopPolicy::new(salt);
    let handle = policy.handle().clone();
    let asked = Arc::new(AtomicBool::new(true));
    // What another surface commands of the desktop — the chrome's own actions, an agent's — is
    // queued on its handle and taken up by the desktop's next pass: the handle asks for it, so the
    // desktop keeps its windows where the person put them.
    let waking = Arc::clone(&asked);
    handle.set_wake(Arc::new(move || waking.store(true, Ordering::Release)));
    ShellDesktop {
        desktop: Rc::new(RefCell::new(Desktop {
            policy,
            origin: (0, 0),
            width: 0,
            asked,
            revision: 0,
            gestures: Vec::new(),
            gesture_config: Some(GestureConfig::default()),
            monitors: Vec::new(),
            primary: None,
            verbs: None,
        })),
    }
}

impl ShellDesktop {
    /// The desktop's policy, for everything else a session drives on it
    /// (strips and workspaces, the tree it keeps).
    pub fn with_policy<R>(&self, read: impl FnOnce(&mut DesktopPolicy) -> R) -> R {
        read(&mut self.desktop.borrow_mut().policy)
    }

    /// The desktop's policy, held: what the window policy placing its windows changes.
    pub(super) fn policy(&self) -> std::cell::RefMut<'_, DesktopPolicy> {
        std::cell::RefMut::map(self.desktop.borrow_mut(), |desktop| &mut desktop.policy)
    }

    /// Where the desktop begins in the logical layout: what the shell's own layers and the windows'
    /// controls are placed from.
    #[must_use]
    pub fn origin(&self) -> (i32, i32) {
        self.desktop.borrow().origin
    }

    /// Stand over `outputs`, as the compositor lists them now.
    pub fn cover(&self, outputs: &[LayoutRect]) {
        self.desktop.borrow_mut().cover(outputs);
    }

    /// The monitors the compositor holds and the one made primary: the desktop reconciles its
    /// partition from them, so its strips know their display before a window is placed.
    pub(super) fn monitors(&self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        self.desktop.borrow_mut().set_monitors(monitors, primary);
    }

    /// Ask for the desktop's next pass: its model changed, or something queued on its handle
    /// waits for it.
    pub fn ask(&self) {
        self.desktop.borrow().ask();
    }

    /// Run the desktop's pass now ([`DesktopPolicy::step`]): the verbs queued since the last, its
    /// search, the activity the windows are served and the notifications, and the tree saved and
    /// shared.
    pub fn pass(&self) {
        let mut desktop = self.desktop.borrow_mut();
        desktop.asked.store(false, Ordering::Release);
        desktop.revision = desktop.revision.wrapping_add(1);
        desktop.policy.step();
    }

    /// The apps whose windows keep what they show to themselves, as the
    /// session last learnt them: nothing the desktop names for the
    /// compositor to draw shows one of their windows.
    pub fn set_withheld_apps(&self, apps: Vec<String>) {
        self.desktop
            .borrow()
            .policy
            .handle()
            .set_withheld_apps(apps);
    }

    /// Answer `look` and `read` of a window this session does not run, which
    /// the desktop publishes on the window's own resource: the tree holds what
    /// they say, and the session's [`tilo_session_core::surfaces::Look`] makes the picture. `None`
    /// for any other address.
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

    /// Carry out the shell's own verb at `address` ([`Self::run_desktop`]).
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

    /// Answer the platform search API's `open` with the desktop's own search: a window's search
    /// control, its toolbar's magnifier, Ctrl+K and an agent all ask the platform to search, and
    /// the platform's search is the one that knows the person's apps, their files, what apps hold
    /// and the desktop's windows. Opening it here means the same search every way into search
    /// reaches.
    ///
    /// `None` when `address` is not the platform search API.
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

    /// Publish the shell's own verbs `verbs` on this desktop.
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

    /// How the desktop reads touchpad gestures.
    pub(super) fn gesture_config(&self) -> Option<GestureConfig> {
        self.desktop.borrow().gesture_config
    }

    /// The desktop took a gesture, and it asked `gesture` of it.
    pub(super) fn took_gesture(&self, gesture: ShellGesture) {
        self.desktop.borrow_mut().gestures.push(gesture);
    }

    /// The policy that places this desktop's windows where a tilolang shell's wells stand
    /// (`docs/designs/app-language/shell.md` §8.3). The windows keep their tabs in this
    /// desktop, so a place in the person's placement names the window's well.
    #[must_use]
    pub fn well_policy(&self) -> super::WellPolicy {
        super::WellPolicy::over(self.clone())
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

    /// The controls the windows' own content offers, each where it stands on
    /// the desktop ([`ui::window_targets`]): what an app describes of its
    /// window, moved from the window's place to where the desktop shows it.
    #[must_use]
    pub fn window_controls(&self) -> Vec<ui::window_targets::WindowControl> {
        let desktop = self.desktop.borrow();
        let (ox, oy) = desktop.origin;
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

impl tilo_session_core::surfaces::Published for ShellDesktop {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        let desktop = self.desktop.borrow();
        let mut resources = desktop
            .verbs
            .as_ref()
            .map(tilo_session_core::desktop::shell_verbs::ShellVerbs::resources)
            .unwrap_or_default();
        resources.push(ui::search::search_resource());
        resources
    }

    fn revision(&self) -> Option<u64> {
        // What the desktop publishes moves with its passes and with its search, which the bar
        // above the windows moves as the person types there and the rows come in; and the shell's
        // own rows name the windows that stopped answering.
        let desktop = self.desktop.borrow();
        let handle = desktop.policy.handle();
        let unresponsive = handle.shared.borrow().unresponsive_revision;
        Some(
            desktop
                .revision
                .wrapping_add(handle.navigation_revision())
                .wrapping_add(handle.search_revision())
                .wrapping_add(unresponsive),
        )
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError> {
        // What it publishes may move with any action run on it.
        {
            let mut desktop = self.desktop.borrow_mut();
            desktop.revision = desktop.revision.wrapping_add(1);
        }
        if let Some(answered) = self.read_window(address, look)? {
            return Ok(answered);
        }
        if let Some(answered) = self.open_platform_search(address, arguments) {
            return Ok(answered);
        }
        if let Some(answered) = self.invoke_verb(address, arguments)? {
            return Ok(answered);
        }
        Err(ui::tilo_actions::ApiError::Unknown(address.clone()))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// The controls the apps that draw the windows describe of their own content
    /// ([`ui::window_targets`]): a window's app keeps its widget tree to itself, so this is the
    /// one place they are named.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        self.window_controls()
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
        let desktop = self.desktop.borrow();
        // The window in front answers first, where it draws the control: a person pointing at it
        // presses that window's, never one behind it.
        desktop
            .policy
            .handle()
            .press_window_control_doing(identity, action)
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        let desktop = self.desktop.borrow();
        let handle = desktop.policy.handle();
        // The window in front answers for its own content: a person pointing at a control there
        // presses that window's, never one behind it that goes by the same name — another
        // window's sidebar toggle.
        if let Some(front) = handle
            .window_in_front()
            .filter(|tab| handle.window_offers(*tab, name))
        {
            return handle.press_window_control_in(front, name, action);
        }
        // The control is one another window's app draws: the press reaches it as its own
        // assistive technology's activation does.
        if handle
            .window_controls()
            .iter()
            .any(|control| control.name == name)
        {
            return handle.press_window_control(name, action);
        }
        Err(format!("no control named {name}"))
    }

    fn wants_keyboard(&self) -> bool {
        self.desktop.borrow().policy.handle().wants_keyboard()
    }
}

/// The desktop as the compositor holds it: the tilolang shell draws the strips, their frames and the
/// places in them, around the wells the windows stand in, so the desktop draws nothing and takes
/// no pointer and no key. Each frame it takes up its own pass when something asked for one, so a
/// workspace switched or a window closed is the shell's to draw.
impl ShellSurface for ShellDesktop {
    fn layers(&self) -> Vec<SurfaceLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    /// Only what is asked of it brings it a pass: a command queued on its handle, or a change
    /// to its windows. Whether it ran one, so the frame shows what the pass settled.
    fn prepare_frame(&mut self) -> bool {
        let asked = self.desktop.borrow().asked.load(Ordering::Acquire);
        asked && {
            self.pass();
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_session_core::surface::{KeyClaim, KeyModifiers, SurfaceKey};

    /// The tilolang shell draws the desktop and answers the keys, so the desktop under the
    /// windows draws nothing and takes no pointer and no key.
    #[test]
    fn the_desktop_draws_and_takes_nothing() {
        let mut desktop = desktop(7);
        let key = SurfaceKey {
            keycode: 0,
            at: std::time::Instant::now(),
            pressed: true,
            repeat: false,
            keysym: 0xff1b,
            unmodified_keysym: 0xff1b,
            latin_keysym: 0xff1b,
            text: None,
            modifiers: KeyModifiers::default(),
        };
        assert_eq!(
            desktop.claim_key(&key, false, false),
            KeyClaim::Pass,
            "the shell answers the keys"
        );
        assert!(desktop.layers().is_empty(), "it draws nothing");
        assert!(!desktop.accepts_pointer(1.0, 1.0), "it takes no pointer");
    }

    /// What another surface commands of the desktop is queued on its handle, and the desktop's
    /// own pass carries it out in the next frame; a frame nothing asked for runs no pass.
    #[test]
    fn a_command_queued_on_the_handle_is_carried_out_in_the_next_frame() {
        let mut desktop = desktop(7);
        assert!(
            desktop.prepare_frame(),
            "the first frame runs the first pass"
        );
        // A pass that changed the tree asks for one more, which changes nothing.
        let settled = (0..4).any(|_| !desktop.prepare_frame());
        assert!(settled, "with nothing asked, a frame runs no pass");
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        handle.open_search(ui::search::Scope::Workspace, None);
        assert!(!handle.search_open(), "the command waits for the pass");
        assert!(desktop.prepare_frame(), "the command asked for a pass");
        assert!(handle.search_open(), "the pass opened search");
    }
}
