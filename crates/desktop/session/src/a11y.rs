//! Every window of the desktop published to assistive technology, by the session.
//!
//! There is one AT-SPI application on a Tilo desktop: this one. A surface the
//! session paints into its compositor — the shell, the bar, the switcher, the
//! banners, the roster, the sheets and the setup browser — has no application
//! window of its own, so the toolkit's window adapter cannot carry it: this
//! module owns one AccessKit Unix adapter per surface. A window an app draws in
//! a process of its own has no adapter there either: the window sends the tree
//! assistive technology reads of it over the `shell-ui` connection it already
//! speaks for its frames and controls
//! ([`ui::floating_surface::FloatingSurfaceClientMessage::AccessibilityTree`]),
//! and this module publishes it as another window of the session's application
//! ([`WindowExports`]). So what a screen reader reads of the desktop is what it
//! reads of an app, and no app process holds an accessibility bus. A surface the
//! shell's own screen stands in for reads as nothing, as it draws nothing.
//!
//! The adapter calls its handlers from its own thread and the surfaces belong
//! to the loop, so every callback only leaves a [`Request`] in a mailbox and
//! wakes the loop; trees are built, and actions run, on the loop in
//! [`Accessibility::step`]. An idle desktop therefore answers an assistive
//! client without waiting for input, which is the endpoint's contract.
//!
//! An action a client takes on an app window's node is not run here: it is
//! left in [`WindowExports`] and the shell-ui service writes it to the window
//! ([`ui::floating_surface::FloatingSurfaceHostMessage::AccessibilityAction`]),
//! which runs it exactly as its own assistive client's action.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use tilo_service::Wake;
use tilo_session_core::surfaces::{Shared, Surfaces};
use ui::accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate,
};
use ui::accesskit_bridge::accesskit_action_to_ours;
use ui::window_targets::TargetAction;

use crate::Session;

/// What an assistive client asks of the loop.
enum Request {
    /// Assistive technology became active on the window at `place` : it needs that window's
    /// complete tree.
    Tree(Place),
    /// A client invoked an action on one exported node of the window at `place` .
    Action(Place, ActionRequest),
    /// The window's last assistive client disconnected.
    Deactivated(Place),
}

/// Where the session publishes a window: a surface it draws here, or a window an app draws in a
/// process of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Place {
    /// The `place`-th surface the session's own API offers.
    Surface(usize),
    /// The window of the app instance (its tab), whose tree its process sent.
    App(u64),
}

/// The place an app window's export is published at, beyond every surface's, so a caller of
/// [`Session::a11y_windows`] names either without knowing which it is.
const APP_PLACE_BASE: usize = 1 << 32;

impl Place {
    /// The number [`Session::a11y_windows`] hands a caller for this window.
    pub(crate) fn exported(self) -> usize {
        match self {
            Self::Surface(place) => place,
            Self::App(instance) => APP_PLACE_BASE
                .saturating_add(usize::try_from(instance).unwrap_or(usize::MAX - APP_PLACE_BASE)),
        }
    }

    /// The window `exported` names, as [`Self::exported`] numbered it.
    pub(crate) fn from_exported(exported: usize) -> Self {
        match exported.checked_sub(APP_PLACE_BASE) {
            Some(instance) => Self::App(u64::try_from(instance).unwrap_or(u64::MAX)),
            None => Self::Surface(exported),
        }
    }
}

/// One window an app draws in a process of its own, as assistive technology reads it: the tree
/// the window sent over its `shell-ui` connection, the name its title gives it, and the
/// connection an action goes back on.
pub(crate) struct AppWindow {
    pub(crate) name: String,
    pub(crate) tree: TreeUpdate,
    /// The shell-ui connection the window speaks on: where a client's action goes.
    pub(crate) connection: u64,
    /// Moves with every tree the window sends.
    pub(crate) revision: u64,
}

/// An action a client of the export asked of one app window's node, waiting for the pass that
/// writes it to the window.
pub(crate) struct AppAction {
    pub(crate) instance: u64,
    pub(crate) request: ActionRequest,
}

/// Every app window published over AT-SPI, by the instance (tab) it is, and the actions clients
/// asked of them: written by the shell-ui service as the windows send their trees, read by the
/// loop's [`Accessibility`], and drained back to the windows by the shell-ui service.
#[derive(Default)]
pub(crate) struct WindowExports {
    windows: BTreeMap<u64, AppWindow>,
    actions: Vec<AppAction>,
}

impl WindowExports {
    /// The window `instance` says what assistive technology reads of it now: `tree`, named
    /// `name`, on the shell-ui connection `connection`.
    pub(crate) fn set(&mut self, instance: u64, connection: u64, name: String, tree: TreeUpdate) {
        let revision = self
            .windows
            .get(&instance)
            .map_or(0, |window| window.revision.wrapping_add(1));
        self.windows.insert(
            instance,
            AppWindow {
                name,
                tree,
                connection,
                revision,
            },
        );
    }

    /// The window `instance` went: its connection closed or its tab did.
    pub(crate) fn remove(&mut self, instance: u64) {
        self.windows.remove(&instance);
    }

    /// Every window that spoke on `connection` went with it.
    pub(crate) fn remove_connection(&mut self, connection: u64) {
        self.windows
            .retain(|_, window| window.connection != connection);
    }

    /// A client acted on a node of the window `instance`: the window runs it on its own loop.
    pub(crate) fn push_action(&mut self, instance: u64, request: ActionRequest) {
        self.actions.push(AppAction { instance, request });
    }

    /// What clients asked since the last pass, oldest first.
    pub(crate) fn take_actions(&mut self) -> Vec<AppAction> {
        std::mem::take(&mut self.actions)
    }

    /// Every window, by the instance it is.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u64, &AppWindow)> {
        self.windows
            .iter()
            .map(|(instance, window)| (*instance, window))
    }

    /// The shell-ui connection the window `instance` sent its tree on: where an action asked of
    /// its nodes goes.
    pub(crate) fn connection_of(&self, instance: u64) -> Option<u64> {
        self.windows.get(&instance).map(|window| window.connection)
    }
}

/// One surface's road from the adapter's thread to the loop: the place the adapter publishes, the
/// channel, and the wake that ends the loop's wait.
/// Waking is part of what an endpoint owes: an idle desktop must answer an activation or an action
/// without waiting for unrelated input.
#[derive(Clone)]
struct Mailbox {
    place: Place,
    requests: Sender<Request>,
    wake: Option<Wake>,
}

impl Mailbox {
    fn send(&self, request: Request) {
        if self.requests.send(request).is_ok()
            && let Some(wake) = &self.wake
        {
            wake();
        }
    }
}

/// The client activated the surface: its tree is the loop's to build, so the documented
/// asynchronous path is taken here.
struct Activation {
    mailbox: Mailbox,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.mailbox.send(Request::Tree(self.mailbox.place));
        None
    }
}

struct Actions {
    mailbox: Mailbox,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        self.mailbox
            .send(Request::Action(self.mailbox.place, request));
    }
}

struct Deactivation {
    mailbox: Mailbox,
}

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        self.mailbox.send(Request::Deactivated(self.mailbox.place));
    }
}

/// A surface's adapter, and where what it shows stood when it last published.
struct Exported {
    adapter: accesskit_unix::Adapter,
    /// Whether the window holds the keyboard, as it was last told.
    focused: bool,
    /// Whether an assistive client reads its window now.
    live: bool,
    /// The revision it published last; `None` before its first tree.
    seen: Option<u64>,
    /// A client asked for its tree and has not been served since.
    wanted: bool,
}

impl Exported {
    /// Tell the adapter whether its window holds the keyboard, as it just changed, so a screen
    /// reader reads the window the keys go to as focused.
    fn focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.adapter.update_window_focus_state(focused);
            self.focused = focused;
        }
    }
}

/// Every hosted surface and every app window published to assistive technology, one adapter each.
pub(crate) struct Accessibility {
    requests: Receiver<Request>,
    /// What each adapter sends through; the loop keeps one to reach itself from a surface's own
    /// handlers.
    sender: Sender<Request>,
    wake: Wake,
    /// The surfaces the shell's own screen stands in for: they draw nothing while it draws, so they
    /// read as nothing too, and the screen's parts are the windows a client finds.
    aside: Vec<Shared>,
    /// The adapters by the place of the surface they publish on the app wire.
    windows: BTreeMap<usize, Exported>,
    /// The adapters of windows apps draw in processes of their own, by the instance (tab) whose
    /// tree the window sent over its shell-ui connection.
    apps: BTreeMap<u64, Exported>,
}

impl Accessibility {
    /// The export of a session whose loop `wake` ends the wait of, and whose `aside` surfaces the
    /// shell's screen stands in for.
    pub(crate) fn new(wake: Wake, aside: Vec<Shared>) -> Self {
        let (sender, requests) = mpsc::channel();
        Self {
            requests,
            sender,
            wake,
            aside,
            windows: BTreeMap::new(),
            apps: BTreeMap::new(),
        }
    }

    /// Answer what assistive clients asked, publish every surface and every app window offering
    /// now whose tree moved, and mark the window holding the keyboard focused.
    ///
    /// `exports` is what the windows in processes of their own sent, keyed by their instance, and
    /// `focused_app` the instance the desktop works in; an action a client asked of such a window
    /// is left in `exports` for the shell-ui service to write to it.
    pub(crate) fn step(
        &mut self,
        surfaces: &Surfaces,
        exports: &mut WindowExports,
        focused_app: Option<u64>,
    ) {
        let asked = {
            let mut asked = Vec::new();
            while let Ok(request) = self.requests.try_recv() {
                asked.push(request);
            }
            asked
        };
        for request in asked {
            self.answer(surfaces, exports, request);
        }
        let offering: Vec<(usize, Shared)> = surfaces
            .offering()
            .into_iter()
            .filter(|(_, surface)| !self.aside.iter().any(|aside| Rc::ptr_eq(aside, surface)))
            .collect();
        // The surface holding the keyboard is its focused window, as the compositor routes that
        // key: the topmost one, when several would take it.
        let focused = offering
            .iter()
            .rev()
            .find(|(_, surface)| surface.borrow().wants_keyboard())
            .map(|(place, _)| *place);
        self.publish(offering, focused);
        // A surface over the windows takes the keys from the window in front; with none up, the
        // app window the desktop works in holds the focus.
        self.publish_apps(exports, focused.is_none().then_some(focused_app).flatten());
    }

    /// Carry out one request against the live surfaces and app windows.
    fn answer(&mut self, surfaces: &Surfaces, exports: &mut WindowExports, request: Request) {
        match request {
            Request::Tree(place) => {
                if let Some(window) = self.exported_mut(place) {
                    window.wanted = true;
                    window.live = true;
                }
            }
            Request::Action(Place::App(instance), request) => {
                // The window runs it on its own loop, exactly as its own assistive client would:
                // the session is the bus, not the actor.
                exports.push_action(instance, request);
            }
            Request::Action(Place::Surface(place), request) => {
                let Some(action) = accesskit_action_to_ours(request.action, request.data.as_ref())
                else {
                    return;
                };
                Self::perform(surfaces, place, request.target_node, &action);
                if let Some(window) = self.windows.get_mut(&place) {
                    // What the action changed is published this pass, whatever the surface says of
                    // its revision.
                    window.wanted = true;
                }
            }
            Request::Deactivated(place) => {
                if let Some(window) = self.exported_mut(place) {
                    window.live = false;
                    window.wanted = false;
                }
            }
        }
    }

    /// The adapter publishing the window at `place`, once it is one.
    fn exported_mut(&mut self, place: Place) -> Option<&mut Exported> {
        match place {
            Place::Surface(place) => self.windows.get_mut(&place),
            Place::App(instance) => self.apps.get_mut(&instance),
        }
    }

    /// Perform `action` on `target` of the surface at `place` , on the loop that owns it: the one
    /// road a client's request takes. Answers whether the surface took it.
    pub(crate) fn perform(
        surfaces: &Surfaces,
        place: usize,
        target: ui::accesskit::NodeId,
        action: &ui::a11y::Action,
    ) -> bool {
        surfaces
            .surface(place)
            .is_some_and(|surface| surface.borrow_mut().a11y_action(target, action))
    }

    /// The window the surface at `place` shows now, as an assistive client reads it: the name its
    /// window carries, and its tree. `None` while it shows nothing to read.
    pub(crate) fn window_at(surfaces: &Surfaces, place: usize) -> Option<(String, TreeUpdate)> {
        Self::window_of(&surfaces.surface(place)?)
    }

    /// [`Self::window_at`] of a surface the caller already holds.
    fn window_of(surface: &Shared) -> Option<(String, TreeUpdate)> {
        let name = surface.borrow().a11y_name()?;
        let mut tree = surface.borrow_mut().a11y_tree()?;
        ui::accesskit_bridge::name_window(&mut tree, &name);
        Some((name, tree))
    }

    /// Publish what the offering surfaces show now: a window appears with a surface that has a tree
    /// a client asked for, and goes with one that has none.
    fn publish(&mut self, offering: Vec<(usize, Shared)>, focused: Option<usize>) {
        if !self.windows.is_empty() {
            self.windows
                .retain(|place, _| offering.iter().any(|(offered, _)| offered == place));
        }
        let sender = self.sender.clone();
        let wake = Arc::clone(&self.wake);
        for (place, surface) in offering {
            let hosts_keyboard = focused == Some(place);
            if let Some(window) = self.windows.get_mut(&place) {
                window.focus(hosts_keyboard);
            }
            // A surface with no name is no window of its own: a surface over services or commands
            // publishes actions, nothing to read.
            if surface.borrow().a11y_name().is_none() {
                continue;
            }
            let revision = surface.borrow().revision();
            let reads = self.windows.get(&place).is_some_and(|window| {
                window.wanted || (window.live && (revision.is_none() || window.seen != revision))
            });
            if self.windows.contains_key(&place) && !reads {
                continue;
            }
            let Some((_, tree)) = Self::window_of(&surface) else {
                // It shows nothing to read: its window goes until it does.
                self.windows.remove(&place);
                continue;
            };
            let window = self.windows.entry(place).or_insert_with(|| {
                let mailbox = Mailbox {
                    place: Place::Surface(place),
                    requests: sender.clone(),
                    wake: Some(Arc::clone(&wake)),
                };
                Exported {
                    adapter: accesskit_unix::Adapter::new(
                        Activation {
                            mailbox: mailbox.clone(),
                        },
                        Actions {
                            mailbox: mailbox.clone(),
                        },
                        Deactivation { mailbox },
                    ),
                    focused: false,
                    live: false,
                    seen: None,
                    wanted: false,
                }
            });
            window.focus(hosts_keyboard);
            // Only a client attached to the window makes this build anything: an adapter nobody
            // activated keeps the closure unrun.
            window.adapter.update_if_active(|| tree);
            window.seen = revision;
            window.wanted = false;
        }
    }

    /// Publish every app window that sent a tree now: one adapter each, named as the window names
    /// itself, focused when the desktop works in it. A window whose app stopped sending, or whose
    /// connection closed, goes with its last tree.
    fn publish_apps(&mut self, exports: &WindowExports, focused: Option<u64>) {
        if !self.apps.is_empty() {
            self.apps
                .retain(|instance, _| exports.windows.contains_key(instance));
        }
        let sender = self.sender.clone();
        let wake = Arc::clone(&self.wake);
        for (instance, window) in &exports.windows {
            let hosts_keyboard = focused == Some(*instance);
            if let Some(exported) = self.apps.get_mut(instance) {
                exported.focus(hosts_keyboard);
            }
            let reads = self.apps.get(instance).is_some_and(|exported| {
                exported.wanted || (exported.live && exported.seen != Some(window.revision))
            });
            if self.apps.contains_key(instance) && !reads {
                continue;
            }
            let mut tree = window.tree.clone();
            ui::accesskit_bridge::name_window(&mut tree, &window.name);
            let exported = self.apps.entry(*instance).or_insert_with(|| {
                let mailbox = Mailbox {
                    place: Place::App(*instance),
                    requests: sender.clone(),
                    wake: Some(Arc::clone(&wake)),
                };
                Exported {
                    adapter: accesskit_unix::Adapter::new(
                        Activation {
                            mailbox: mailbox.clone(),
                        },
                        Actions {
                            mailbox: mailbox.clone(),
                        },
                        Deactivation { mailbox },
                    ),
                    focused: false,
                    live: false,
                    seen: None,
                    wanted: false,
                }
            });
            exported.focus(hosts_keyboard);
            // Only a client attached to the window makes this build anything: an adapter nobody
            // activated keeps the closure unrun.
            exported.adapter.update_if_active(|| tree);
            exported.seen = Some(window.revision);
            exported.wanted = false;
        }
    }
}

/// The controls of the windows the session shows, by name and place, for anything that reads the
/// shell as an assistive technology does.
impl Session {
    /// Every control the session shows by name, each with where it stands on the shell: the shell's
    /// own chrome, and the controls the apps that draw the windows describe of their content
    /// ([`ui::window_targets`]).
    #[must_use]
    pub fn controls(&self) -> Vec<ui::window_targets::WindowControl> {
        self.shared.published.controls()
    }

    /// Act on the control `name` as `action` says, as an assistive technology
    /// does on the surface showing it.
    ///
    /// # Errors
    /// No surface shows a control by that name, or it answers nothing of
    /// `action`.
    pub fn press_action(&self, name: &str, action: TargetAction) -> Result<(), String> {
        self.shared.published.press_action(name, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use ui::accesskit::{Action, Node, NodeId, Role, TreeId, TreeInfo};

    /// A surface in place of one the session paints: the window name it carries, and whether it
    /// would take the keys.
    struct Face {
        name: &'static str,
        keyboard: bool,
    }

    impl tilo_session_core::surfaces::Published for Face {
        fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
            Vec::new()
        }

        fn invoke(
            &mut self,
            address: &ui::tilo_actions::ActionAddress,
            _arguments: &serde_json::Map<String, serde_json::Value>,
            _look: &mut tilo_session_core::surfaces::Look<'_>,
        ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError>
        {
            Err(ui::tilo_actions::ApiError::Unknown(address.clone()))
        }

        fn accessible_labels(&mut self) -> Vec<String> {
            Vec::new()
        }

        fn a11y_tree(&mut self) -> Option<TreeUpdate> {
            let mut window = Node::new(Role::Window);
            window.set_label(self.name);
            Some(TreeUpdate {
                nodes: vec![(NodeId(0), window)],
                tree: Some(TreeInfo::new(NodeId(0))),
                tree_id: TreeId::ROOT,
                focus: NodeId(0),
            })
        }

        fn a11y_name(&self) -> Option<String> {
            Some(self.name.to_owned())
        }

        fn wants_keyboard(&self) -> bool {
            self.keyboard
        }
    }

    /// A surface `name` that would take the keys when `keyboard` .
    fn face(name: &'static str, keyboard: bool) -> Shared {
        Rc::new(RefCell::new(Face { name, keyboard }))
    }

    /// The export of a session whose loop has nothing to wake.
    fn export(aside: Vec<Shared>) -> Accessibility {
        Accessibility::new(Arc::new(|| {}), aside)
    }

    tilo_test_env::scenario! {
        fn the_export_publishes_the_surface_that_draws_and_focuses_the_one_the_keys_go_to;

        phase a_surface_the_shells_screen_stands_in_for_reads_as_nothing;
        // What the shell's own screen stands in for draws nothing, so it reads as nothing: a client
        // finds the window that draws, not the one behind it.
        {
            let mut surfaces = Surfaces::default();
            let drawn = face("Drawn", false);
            let stood_aside = face("Stood aside", true);
            surfaces.add(Rc::clone(&drawn));
            surfaces.add(Rc::clone(&stood_aside));
            let mut export = export(vec![Rc::clone(&stood_aside)]);

            export.step(&surfaces, &mut WindowExports::default(), None);

            assert!(
                export.windows.contains_key(&0),
                "the surface that draws is the window a client finds"
            );
            assert!(
                !export.windows.contains_key(&1),
                "the surface the screen stands in for publishes no window"
            );
        }

        phase the_window_the_keys_go_to_reads_as_focused;
        // The topmost surface that would take the keys is the window a screen reader reads as focused,
        // as the compositor routes a key to it.
        {
            let mut surfaces = Surfaces::default();
            surfaces.add(face("Shell", false));
            surfaces.add(face("Question", true));
            let mut export = export(Vec::new());

            export.step(&surfaces, &mut WindowExports::default(), None);

            assert!(
                export.windows.get(&1).is_some_and(|window| window.focused),
                "the surface that takes the keys holds the focus"
            );
            assert!(
                export.windows.get(&0).is_some_and(|window| !window.focused),
                "the one below it does not"
            );
        }
    }

    /// Every callback the adapter makes from its own thread leaves its request for the loop and
    /// wakes it: an idle desktop answers assistive technology without waiting for the next
    /// unrelated input.
    #[test]
    fn every_adapter_callback_leaves_its_request_and_wakes_the_loop() {
        let (sender, requests) = mpsc::channel();
        let woken = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&woken);
        let mailbox = Mailbox {
            place: Place::Surface(7),
            requests: sender,
            wake: Some(Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            })),
        };

        assert!(
            Activation {
                mailbox: mailbox.clone()
            }
            .request_initial_tree()
            .is_none(),
            "the tree is the loop's to build, so the documented asynchronous path is taken"
        );
        Actions {
            mailbox: mailbox.clone(),
        }
        .do_action(ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: NodeId(4),
            data: None,
        });
        Deactivation { mailbox }.deactivate_accessibility();

        match requests.try_recv().expect("the activation request") {
            Request::Tree(place) => assert_eq!(place, Place::Surface(7)),
            _ => panic!("the activation asked for a tree"),
        }
        match requests.try_recv().expect("the action request") {
            Request::Action(place, request) => {
                assert_eq!(place, Place::Surface(7));
                assert_eq!(request.target_node, NodeId(4));
            }
            _ => panic!("the action reached the loop"),
        }
        match requests.try_recv().expect("the deactivation request") {
            Request::Deactivated(place) => assert_eq!(place, Place::Surface(7)),
            _ => panic!("the deactivation reached the loop"),
        }
        assert_eq!(
            woken.load(Ordering::SeqCst),
            3,
            "each callback woke the loop once"
        );
    }
}
