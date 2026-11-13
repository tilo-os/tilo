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

/// A picture of a window, as a surface answers with one: the address it is
/// kept at, and how large it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    /// Where the picture is kept: a `tilo.content:<hash>` address any caller
    /// of the action protocol reads it by.
    pub content: String,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
}

/// The session's way of picturing a window it does not run, handed to the
/// surface publishing that window's `look` when it is invoked: the picture
/// is made where the compositor is, never by the surface that names the
/// window. Absent ([`Look::none`]) where no compositor stands, which is
/// every caller that is not the session's own loop.
pub struct Look<'a> {
    picture: Option<Picture<'a>>,
}

/// How a surface pictures the window `u64` into content kept in `&str`.
type Picture<'a> = Box<dyn FnMut(u64, &str) -> Result<Shot, String> + 'a>;

impl<'a> Look<'a> {
    /// No picture can be made here.
    #[must_use]
    pub const fn none() -> Self {
        Self { picture: None }
    }

    /// Pictures are made by `picture`: the compositor's own.
    pub fn over(picture: impl FnMut(u64, &str) -> Result<Shot, String> + 'a) -> Self {
        Self {
            picture: Some(Box::new(picture)),
        }
    }

    /// The window the compositor knows as `window`, kept as content of
    /// `workspace`.
    ///
    /// # Errors
    /// No picture can be made here, the window is not on the desk (any more),
    /// or it could not be drawn or kept.
    pub fn window(&mut self, window: u64, workspace: &str) -> Result<Shot, String> {
        match &mut self.picture {
            Some(picture) => picture(window, workspace),
            None => Err("no compositor stands here to picture a window".into()),
        }
    }
}

/// A hosted surface's published side.
pub trait Published {
    /// What it publishes now: its resources, with their actions.
    fn resources(&mut self) -> Vec<ResourceSpec>;

    /// Run its published action `address` with `arguments`; `look` pictures a
    /// window this session does not run, for the surface publishing one.
    ///
    /// # Errors
    /// The surface's refusal: no such action, bad arguments, not allowed.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut Look<'_>,
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
        look: &mut Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.invoke(address, arguments, look)
    }

    /// The labels of its accessibility tree, in reading order.
    fn accessible_labels(&mut self) -> Vec<String>;

    /// The controls it shows by name, each with where it stands, logical
    /// `(x, y, w, h)` of the desktop, and what it does when it runs an app
    /// action: what a test or a tool looks up, by name or by identity.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
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

    /// Act on the control that does `identity` (`so.tilo.lang.notes.Notebook`
    /// and `new_note`): the one `controls` lists, wherever the window's
    /// composition draws it and whatever the person's language calls it.
    ///
    /// # Errors
    /// It shows no control doing that, it answers nothing of `action`, or the
    /// control refused.
    fn press_identity(
        &mut self,
        identity: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        let name = self
            .controls()
            .into_iter()
            .find(|control| control.identity.as_deref() == Some(identity))
            .map(|control| control.name)
            .ok_or_else(|| format!("no control does {identity}"))?;
        self.press_action(&name, action)
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

    /// Where what it publishes stands: equal revisions publish the same, so the door reads it
    /// again only once this moved ([`Doors::revision`]). It may move without a change of what it
    /// publishes (a frame drawn), never the other way round. `None` when it cannot tell, and is
    /// read again every time.
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
    /// What each surface published when it was last read, by its place.
    read: Rc<RefCell<std::collections::HashMap<usize, Read>>>,
}

/// What the door last read of one surface.
struct Read {
    /// The surface's own revision, the language and whether the session was locked when it was
    /// read: the same again, and the surface publishes what it did. `None` for a surface that
    /// cannot tell.
    at: Option<(u64, u64, bool)>,
    /// What it published then.
    resources: Vec<ResourceSpec>,
    /// Moves only when what it publishes did.
    revision: u64,
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

    /// Read again what `surface` at `place` publishes, unless its own revision says it publishes
    /// what it did when it was last read; what it publishes moves the revision kept for it only
    /// when it differs from what was kept. A surface draws far more often than its offer moves,
    /// so its readers read it again only for a change of what it offers.
    fn read(&self, place: usize, surface: &Shared, locale: u64) {
        let locked = self.locked.get();
        let at = surface
            .borrow()
            .revision()
            .map(|revision| (revision, locale, locked));
        if at.is_some() && self.kept(place, |kept| kept.at) == Some(at) {
            return;
        }
        let resources = if locked {
            surface.borrow_mut().locked_resources()
        } else {
            surface.borrow_mut().resources()
        };
        let mut read = self.read.borrow_mut();
        let revision = match read.get(&place) {
            Some(kept) if ui::tilo_actions::door::publishes_same(&kept.resources, &resources) => {
                kept.revision
            }
            kept => kept.map_or(0, |kept| kept.revision.wrapping_add(1)),
        };
        // What was read is what is handed on, bound as the surface bound it now.
        read.insert(
            place,
            Read {
                at,
                resources,
                revision,
            },
        );
    }

    /// `look` at what the door last read of the surface at `place`.
    fn kept<R>(&self, place: usize, look: impl FnOnce(&Read) -> R) -> Option<R> {
        self.read.borrow().get(&place).map(look)
    }

    /// Where everything offered now stands, surface by surface: equal revisions offer the same.
    /// A surface's revision moves only once what it publishes did, whether it drew, ran an action
    /// or changed language in between.
    #[must_use]
    pub fn revision(&self) -> Vec<(usize, u64)> {
        let locale = ui::i18n::I18nManager::revision();
        self.offered()
            .into_iter()
            .map(|(place, surface)| {
                self.read(place, surface, locale);
                (
                    place,
                    self.kept(place, |kept| kept.revision).unwrap_or_default(),
                )
            })
            .collect()
    }

    /// Everything the hosted surfaces offer now, in the order they were
    /// added: what a locked session's lock screen publishes, while it is
    /// locked.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        let locale = ui::i18n::I18nManager::revision();
        self.offered()
            .into_iter()
            .flat_map(|(place, surface)| {
                self.read(place, surface, locale);
                self.kept(place, |kept| kept.resources.clone())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Invoke the published action `address`, on the surface publishing
    /// it; `look` pictures a window this session does not run.
    ///
    /// # Errors
    /// No surface publishes it, or the surface refused.
    pub fn invoke(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.publishing(address)?
            .borrow_mut()
            .invoke(address, arguments, look)
    }

    /// Invoke the published action `address` as `actor` (an agent, a
    /// script), which the surface attributes its change to; `look` pictures
    /// a window this session does not run.
    ///
    /// # Errors
    /// No surface publishes it, or the surface refused.
    pub fn invoke_as(
        &self,
        actor: &str,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.publishing(address)?
            .borrow_mut()
            .invoke_as(actor, address, arguments, look)
    }

    /// The surface offering `address` now.
    fn publishing(&self, address: &ActionAddress) -> Result<Shared, DoorError> {
        let locale = ui::i18n::I18nManager::revision();
        self.offered()
            .into_iter()
            .find(|(place, surface)| {
                self.read(*place, surface, locale);
                self.kept(*place, |kept| {
                    ui::tilo_actions::door::resolve(&kept.resources, address).is_some()
                })
                .unwrap_or_default()
            })
            .map(|(_, surface)| Rc::clone(surface))
            .ok_or_else(|| DoorError::Unknown(address.clone()))
    }

    /// The labels of every hosted surface's accessibility tree.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.offered()
            .into_iter()
            .flat_map(|(_, surface)| surface.borrow_mut().accessible_labels())
            .collect()
    }

    /// Every control the offered surfaces show by name, frontmost surface first — the sheet over
    /// the bar, the bar over the desk — each where it stands: what a test or a tool lists and then
    /// presses. Two controls that go by one name, as each window's sidebar toggle does, are both
    /// listed, so the one a person sees is found where they see it; a press by name reaches the
    /// frontmost surface's ([`Self::press_action`]).
    #[must_use]
    pub fn controls(&self) -> Vec<ui::window_targets::WindowControl> {
        self.offered()
            .into_iter()
            .rev()
            .flat_map(|(_, surface)| surface.borrow_mut().controls())
            .collect()
    }

    /// Act on the control `name` on the surface showing it as `action` says,
    /// as an assistive technology does: the frontmost one, which is what
    /// [`Self::controls`] lists.
    ///
    /// # Errors
    /// No offered surface shows it, or the surface refused.
    pub fn press_action(
        &self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        for (_, surface) in self.offered().into_iter().rev() {
            let mut surface = surface.borrow_mut();
            if surface
                .controls()
                .iter()
                .any(|control| control.name == name)
            {
                return surface.press_action(name, action);
            }
        }
        Err(format!("no control named {name}"))
    }

    /// Act on the control that does `identity` on the surface showing it, as
    /// [`Self::press_action`] acts on one by name: the first one, which is
    /// what [`Self::controls`] lists.
    ///
    /// # Errors
    /// No offered surface shows it, or the surface refused.
    pub fn press_identity(
        &self,
        identity: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        for (_, surface) in self.offered().into_iter().rev() {
            let mut surface = surface.borrow_mut();
            let shows = surface
                .controls()
                .iter()
                .any(|control| control.identity.as_deref() == Some(identity));
            if shows {
                return surface.press_identity(identity, action);
            }
        }
        Err(format!("no control does {identity}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One resource labelled `.0`, drawn `.1` times, described `.2` times.
    #[derive(Default)]
    struct Surface(String, u64, usize);

    impl Published for Surface {
        fn resources(&mut self) -> Vec<ResourceSpec> {
            self.2 = self.2.saturating_add(1);
            vec![ResourceSpec::new("surface", "test", self.0.clone())]
        }

        fn invoke(
            &mut self,
            address: &ActionAddress,
            _: &serde_json::Map<String, serde_json::Value>,
            _: &mut Look<'_>,
        ) -> Result<ResourceOperationStart, DoorError> {
            Err(DoorError::Unknown(address.clone()))
        }

        fn accessible_labels(&mut self) -> Vec<String> {
            Vec::new()
        }

        fn revision(&self) -> Option<u64> {
            Some(self.1)
        }
    }

    #[test]
    fn a_surface_that_drew_without_changing_its_offer_leaves_the_door_where_it_stood() {
        let surface = Rc::new(RefCell::new(Surface::default()));
        let mut doors = Doors::default();
        doors.add(surface.clone());
        let first = doors.revision();
        assert_eq!(doors.revision(), first);
        assert_eq!(
            surface.borrow().2,
            1,
            "a surface that did not move is not described again"
        );
        surface.borrow_mut().1 = 1;
        assert_eq!(doors.revision(), first, "a frame drawn offers the same");
        surface.borrow_mut().0 = "renamed".into();
        surface.borrow_mut().1 = 2;
        assert_ne!(doors.revision(), first, "a new label is a new offer");
        assert_eq!(doors.resources()[0].label, "renamed");
        assert_eq!(surface.borrow().2, 3, "what was read is what is handed on");
    }
}
