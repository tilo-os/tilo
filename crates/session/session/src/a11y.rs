//! The session's own surfaces published to assistive technology.
//!
//! Every surface the session paints into its compositor — the desk, the bar,
//! the switcher, the banners, the roster, the sheets and the setup browser —
//! has no application window of its own, so the toolkit's window adapter
//! cannot carry it: this module owns one AccessKit Unix adapter per surface
//! instead. Each appears as a window of the session's AT-SPI application and
//! shows the surface's own semantic tree — the same tree a Tilo app publishes
//! for its window, built by the same bridge — so what a screen reader reads of
//! the desktop is what it reads of an app. A surface the shell's own screen
//! stands in for reads as nothing, as it draws nothing.
//!
//! The adapter calls its handlers from its own thread and the surfaces belong
//! to the loop, so every callback only leaves a [`Request`] in a mailbox and
//! wakes the loop; trees are built, and actions run, on the loop in
//! [`Accessibility::step`]. An idle desktop therefore answers an assistive
//! client without waiting for input, which is the endpoint's contract.
//!
//! The controls it finds and presses are the desk's own chrome and those of
//! the windows in processes of their own, whose apps describe their content
//! over their `shell-ui` connection ([`ui::window_targets`]) and take a
//! press as an assistive technology's activation does.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use tilo_service::Wake;
use tilo_surface_host::{Doors, Shared};
use ui::accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate,
};
use ui::accesskit_bridge::accesskit_action_to_ours;
use ui::window_targets::TargetAction;

use crate::Session;

/// What an assistive client asks of the loop.
enum Request {
    /// Assistive technology became active on the surface at `place` : it needs that surface's
    /// complete tree.
    Tree(usize),
    /// A client invoked an action on one exported node of the surface at `place` .
    Action(usize, ActionRequest),
    /// The surface's last assistive client disconnected.
    Deactivated(usize),
}

/// One surface's road from the adapter's thread to the loop: the place the adapter publishes, the
/// channel, and the wake that ends the loop's wait.
/// Waking is part of what an endpoint owes: an idle desktop must answer an activation or an action
/// without waiting for unrelated input.
#[derive(Clone)]
struct Mailbox {
    place: usize,
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

/// Every hosted surface published to assistive technology, one adapter each.
pub(crate) struct Accessibility {
    requests: Receiver<Request>,
    /// What each adapter sends through; the loop keeps one to reach itself from a surface's own
    /// handlers.
    sender: Sender<Request>,
    wake: Wake,
    /// The surfaces the shell's own screen stands in for: they draw nothing while it draws, so they
    /// read as nothing too, and the screen's parts are the windows a client finds.
    aside: Vec<Shared>,
    /// The adapters by the place of the surface they publish in the door.
    windows: BTreeMap<usize, Exported>,
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
        }
    }

    /// Answer what assistive clients asked, publish every surface offering now whose tree moved,
    /// and mark the window holding the keyboard focused.
    pub(crate) fn step(&mut self, doors: &Doors) {
        let asked = {
            let mut asked = Vec::new();
            while let Ok(request) = self.requests.try_recv() {
                asked.push(request);
            }
            asked
        };
        for request in asked {
            self.answer(doors, request);
        }
        let offering: Vec<(usize, Shared)> = doors
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
    }

    /// Carry out one request against the live surfaces.
    fn answer(&mut self, doors: &Doors, request: Request) {
        match request {
            Request::Tree(place) => {
                if let Some(window) = self.windows.get_mut(&place) {
                    window.wanted = true;
                    window.live = true;
                }
            }
            Request::Action(place, request) => {
                let Some(action) = accesskit_action_to_ours(request.action, request.data.as_ref())
                else {
                    return;
                };
                Self::perform(doors, place, request.target_node, &action);
                if let Some(window) = self.windows.get_mut(&place) {
                    // What the action changed is published this pass, whatever the surface says of
                    // its revision.
                    window.wanted = true;
                }
            }
            Request::Deactivated(place) => {
                if let Some(window) = self.windows.get_mut(&place) {
                    window.live = false;
                    window.wanted = false;
                }
            }
        }
    }

    /// Perform `action` on `target` of the surface at `place` , on the loop that owns it: the one
    /// road a client's request takes. Answers whether the surface took it.
    pub(crate) fn perform(
        doors: &Doors,
        place: usize,
        target: ui::accesskit::NodeId,
        action: &ui::a11y::Action,
    ) -> bool {
        doors
            .surface(place)
            .is_some_and(|surface| surface.borrow_mut().a11y_action(target, action))
    }

    /// The window the surface at `place` shows now, as an assistive client reads it: the name its
    /// window carries, and its tree. `None` while it shows nothing to read.
    pub(crate) fn window_at(doors: &Doors, place: usize) -> Option<(String, TreeUpdate)> {
        Self::window_of(&doors.surface(place)?)
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
            // A surface with no name is no window of its own: a door over services or commands
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
                    place,
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
}

/// The controls of the windows the session shows, by name and place, for anything that reads the
/// desk as an assistive technology does.
impl Session {
    /// Every control the session shows by name, each with where it stands on the desk: the desk's
    /// own chrome, and the controls the apps that draw the windows describe of their content
    /// ([`ui::window_targets`]).
    #[must_use]
    pub fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        self.shared.doors.controls()
    }

    /// Act on the control `name` as `action` says, as an assistive technology
    /// does on the surface showing it.
    ///
    /// # Errors
    /// No surface shows a control by that name, or it answers nothing of
    /// `action`.
    pub fn press_action(&self, name: &str, action: TargetAction) -> Result<(), String> {
        self.shared.doors.press_action(name, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use ui::accesskit::{Action, Node, NodeId, Role, TreeId, TreeInfo};

    /// A surface standing in for one the session paints: the window name it carries, and whether it
    /// would take the keys.
    struct Face {
        name: &'static str,
        keyboard: bool,
    }

    impl tilo_surface_host::Published for Face {
        fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
            Vec::new()
        }

        fn invoke(
            &mut self,
            address: &ui::tilo_actions::ActionAddress,
            _arguments: &serde_json::Map<String, serde_json::Value>,
            _look: &mut tilo_surface_host::Look<'_>,
        ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError>
        {
            Err(ui::tilo_actions::DoorError::Unknown(address.clone()))
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

    /// What the shell's own screen stands in for draws nothing, so it reads as nothing: a client
    /// finds the window that draws, not the one behind it.
    #[test]
    fn a_surface_the_shells_screen_stands_in_for_reads_as_nothing() {
        let mut doors = Doors::default();
        let drawn = face("Drawn", false);
        let stood_aside = face("Stood aside", true);
        doors.add(Rc::clone(&drawn));
        doors.add(Rc::clone(&stood_aside));
        let mut export = export(vec![Rc::clone(&stood_aside)]);

        export.step(&doors);

        assert!(
            export.windows.contains_key(&0),
            "the surface that draws is the window a client finds"
        );
        assert!(
            !export.windows.contains_key(&1),
            "the surface the screen stands in for publishes no window"
        );
    }

    /// The topmost surface that would take the keys is the window a screen reader reads as focused,
    /// as the compositor routes a key to it.
    #[test]
    fn the_window_the_keys_go_to_reads_as_focused() {
        let mut doors = Doors::default();
        doors.add(face("Desk", false));
        doors.add(face("Question", true));
        let mut export = export(Vec::new());

        export.step(&doors);

        assert!(
            export.windows.get(&1).is_some_and(|window| window.focused),
            "the surface that takes the keys holds the focus"
        );
        assert!(
            export.windows.get(&0).is_some_and(|window| !window.focused),
            "the one below it does not"
        );
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
            place: 7,
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
            Request::Tree(place) => assert_eq!(place, 7),
            _ => panic!("the activation asked for a tree"),
        }
        match requests.try_recv().expect("the action request") {
            Request::Action(place, request) => {
                assert_eq!(place, 7);
                assert_eq!(request.target_node, NodeId(4));
            }
            _ => panic!("the action reached the loop"),
        }
        match requests.try_recv().expect("the deactivation request") {
            Request::Deactivated(place) => assert_eq!(place, 7),
            _ => panic!("the deactivation reached the loop"),
        }
        assert_eq!(
            woken.load(Ordering::SeqCst),
            3,
            "each callback woke the loop once"
        );
    }
}
