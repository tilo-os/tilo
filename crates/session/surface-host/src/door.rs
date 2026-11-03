//! What a hosted surface publishes, and the one door the session reaches
//! every hosted surface through.
//!
//! Each surface (the desk, the chrome, quick settings, the banners, …) is
//! an ordinary widget tree or app with its own published resources and
//! actions. [`Doors`] gathers them: a caller of the action protocol (a
//! person's key, the palette, an agent, a test) names an action by address
//! and the door resolves it to the surface publishing it. Nothing calls a
//! surface directly.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::device_local_door::SharedDoor;
use ui::host_operation::ResourceOperationStart;
use ui::tilo_actions::{ActionAddress, DoorError, ResourceSpec};

/// A hosted surface's published side.
pub trait Published {
    /// What it publishes now: its resources, with their actions.
    fn resources(&mut self) -> Vec<ResourceSpec>;

    /// Run its published action `address` with `arguments`.
    ///
    /// # Errors
    /// The surface's refusal: no such action, bad arguments, not allowed.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError>;

    /// Run its published action `address` as `actor` (an agent, a script):
    /// who the change it makes is attributed to. The person's own input is
    /// [`Self::invoke`], and is theirs.
    ///
    /// # Errors
    /// The surface's refusal: no such action, bad arguments, not allowed.
    fn invoke_as(
        &mut self,
        _actor: &str,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.invoke(address, arguments)
    }

    /// The labels of its accessibility tree, in reading order.
    fn accessible_labels(&mut self) -> Vec<String>;

    /// The controls it shows by name, each with where it stands, logical
    /// `(x, y, w, h)` of the desktop: what a test or a tool presses by name.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        Vec::new()
    }

    /// Press the control `name` it shows: the same route a person's click or
    /// an assistive technology's activation takes, never a point aimed at.
    ///
    /// # Errors
    /// It shows no control by that name, or the control refused.
    fn press(&mut self, name: &str) -> Result<(), String> {
        Err(format!("no control named {name}"))
    }

    /// Act on the control `name` it shows as `action` says: the activation
    /// [`Self::press`] performs, or the context menu a person opens on it.
    ///
    /// # Errors
    /// It shows no control by that name, it answers nothing of `action`, or
    /// the control refused.
    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        if action == ui::window_targets::TargetAction::Activate {
            return self.press(name);
        }
        Err(format!("no control named {name} answers {action:?}"))
    }

    /// What an assistive technology reads of it now: its semantic tree, the
    /// same one the toolkit exports for an app's window. `None` while it
    /// shows nothing (down, gone, or a surface with no tree of its own).
    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        None
    }

    /// The name an assistive technology knows its window by.
    fn a11y_name(&self) -> Option<String> {
        None
    }

    /// Perform the accessibility action `action` on `target`, as a client
    /// asked for it: exactly the path of a key or a press on the node.
    /// Whether the node was there to act on.
    fn a11y_action(&mut self, _target: ui::accesskit::NodeId, _action: &ui::a11y::Action) -> bool {
        false
    }

    /// Whether it takes the keyboard now, as the compositor asks it before
    /// handing a key to a client: an assistive technology needs the same
    /// answer to know which of the session's windows is the focused one.
    fn wants_keyboard(&self) -> bool {
        false
    }

    /// Where what it publishes stands: equal revisions publish the same.
    /// `None` when it cannot tell, and is read again every time.
    fn revision(&self) -> Option<u64> {
        None
    }

    /// Whether it offers its actions now: a sheet only while it is up.
    fn offered(&self) -> bool {
        true
    }

    /// Whether, while offered, it withholds every other surface: setup
    /// over the whole desktop until the person finished it.
    fn exclusive(&self) -> bool {
        false
    }

    /// Whether a locked session still offers it: only the lock screen's own
    /// door is, and every other surface is withheld while the session is
    /// locked.
    fn serves_while_locked(&self) -> bool {
        false
    }

    /// What it publishes while the session is locked. Nothing, but for the
    /// door publishing the session's services: the lock's own resource.
    fn locked_resources(&mut self) -> Vec<ResourceSpec> {
        Vec::new()
    }
}

/// Run `address` through `door` (the surface app's own door on this
/// device, which every caller shares), `serve` it at once, and take the
/// answer.
///
/// # Errors
/// The app's refusal, or none came: nothing there publishes `address`.
pub fn through_door(
    door: &SharedDoor,
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    serve: impl FnOnce(),
) -> Result<ResourceOperationStart, DoorError> {
    let answer = door.invoke(address.clone(), arguments.clone());
    serve();
    let answered = answer.borrow_mut().take();
    answered.unwrap_or_else(|| Err(DoorError::Unknown(address.clone())))
}

/// A hosted surface the session keeps beside the compositor's hold of it.
pub type Shared = Rc<RefCell<dyn Published>>;

/// Every hosted surface's published side, the session's action host for
/// them.
#[derive(Default, Clone)]
pub struct Doors {
    surfaces: Vec<Shared>,
    /// Whether the session is locked: only its lock screen is offered then.
    locked: Rc<Cell<bool>>,
}

impl Doors {
    /// Reach `surface` through the door from now on.
    pub fn add(&mut self, surface: Shared) {
        self.surfaces.push(surface);
    }

    /// Offer the lock screen's door alone while the session is locked, and
    /// every other again once it is unlocked.
    pub fn locked(&self, locked: bool) {
        self.locked.set(locked);
    }

    /// The surfaces offering their actions now, with their place: only
    /// the exclusive ones while one is up, and only the lock screen's while
    /// the session is locked.
    fn offered(&self) -> Vec<(usize, &Shared)> {
        let offered: Vec<(usize, &Shared)> = self
            .surfaces
            .iter()
            .enumerate()
            .filter(|(_, surface)| surface.borrow().offered())
            .filter(|(_, surface)| !self.locked.get() || surface.borrow().serves_while_locked())
            .collect();
        if offered
            .iter()
            .any(|(_, surface)| surface.borrow().exclusive())
        {
            offered
                .into_iter()
                .filter(|(_, surface)| surface.borrow().exclusive())
                .collect()
        } else {
            offered
        }
    }

    /// Where everything offered now stands: equal revisions offer the
    /// same. `None` when a surface cannot tell. Each surface's revision
    /// stands beside the language the person reads it in: what a surface
    /// publishes says it in that language, so one that stood still still
    /// offers new words once the language moved.
    #[must_use]
    pub fn revision(&self) -> Option<Vec<(usize, (u64, u64))>> {
        let locale = ui::i18n::I18nManager::revision();
        self.offered()
            .into_iter()
            .map(|(place, surface)| {
                surface
                    .borrow()
                    .revision()
                    .map(|revision| (place, (revision, locale)))
            })
            .collect()
    }

    /// Everything the hosted surfaces offer now, in the order they were
    /// added: what a locked session's lock screen publishes, while it is
    /// locked.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.offered()
            .into_iter()
            .flat_map(|(_, surface)| {
                if self.locked.get() {
                    surface.borrow_mut().locked_resources()
                } else {
                    surface.borrow_mut().resources()
                }
            })
            .collect()
    }

    /// Invoke the published action `address`, on the surface publishing
    /// it.
    ///
    /// # Errors
    /// No surface publishes it, or the surface refused.
    pub fn invoke(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        for (_, surface) in self.offered() {
            let mut surface = surface.borrow_mut();
            let published = if self.locked.get() {
                surface.locked_resources()
            } else {
                surface.resources()
            };
            if ui::tilo_actions::door::resolve(&published, address).is_some() {
                return surface.invoke(address, arguments);
            }
        }
        Err(DoorError::Unknown(address.clone()))
    }

    /// Invoke the published action `address` as `actor` (an agent, a
    /// script), which the surface attributes its change to.
    ///
    /// # Errors
    /// No surface publishes it, or the surface refused.
    pub fn invoke_as(
        &self,
        actor: &str,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        for (_, surface) in self.offered() {
            let mut surface = surface.borrow_mut();
            if ui::tilo_actions::door::resolve(&surface.resources(), address).is_some() {
                return surface.invoke_as(actor, address, arguments);
            }
        }
        Err(DoorError::Unknown(address.clone()))
    }

    /// The labels of every hosted surface's accessibility tree.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.offered()
            .into_iter()
            .flat_map(|(_, surface)| surface.borrow_mut().accessible_labels())
            .collect()
    }

    /// Every control the offered surfaces show by name, in name order: what
    /// a test or a tool lists and then presses. A name two surfaces show is
    /// the earlier surface's, which is the one in front of it.
    #[must_use]
    pub fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        let mut shown: std::collections::BTreeMap<String, (f64, f64, f64, f64)> =
            std::collections::BTreeMap::new();
        for (_, surface) in self.offered() {
            for (name, rect) in surface.borrow_mut().controls() {
                shown.entry(name).or_insert(rect);
            }
        }
        shown.into_iter().collect()
    }

    /// Press the control `name` on the surface showing it, as an assistive
    /// technology activates it: [`Self::press_action`] of its activation.
    ///
    /// # Errors
    /// No offered surface shows it, or the surface refused the press.
    pub fn press(&self, name: &str) -> Result<(), String> {
        self.press_action(name, ui::window_targets::TargetAction::Activate)
    }

    /// Act on the control `name` on the surface showing it as `action` says,
    /// as an assistive technology does.
    ///
    /// # Errors
    /// No offered surface shows it, or the surface refused.
    pub fn press_action(
        &self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        for (_, surface) in self.offered() {
            let mut surface = surface.borrow_mut();
            if surface.controls().iter().any(|(shown, _)| shown == name) {
                return surface.press_action(name, action);
            }
        }
        Err(format!("no control named {name}"))
    }

    /// The surfaces offering now, each with its place in the door: what the
    /// accessibility exporter publishes, the last one topmost.
    #[must_use]
    pub fn offering(&self) -> Vec<(usize, Shared)> {
        self.offered()
            .into_iter()
            .map(|(place, surface)| (place, Rc::clone(surface)))
            .collect()
    }

    /// The surface at `place`, as [`Self::offering`] numbers it.
    #[must_use]
    pub fn surface(&self, place: usize) -> Option<Shared> {
        self.surfaces.get(place).cloned()
    }
}
