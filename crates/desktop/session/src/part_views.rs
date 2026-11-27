//! The shell's parts whose view is written in tilolang, run in the session's own process.
//! A part's view is a program of its own (a surface app) unless the app the person bound is a
//! tilolang package this machine installed: then the view is the package's, and the session runs it
//! in an instance of its own ([`PartView`]), over the reading it
//! publishes for the part. The pointer and the keys reach it while the overlay stack says its
//! surface is the one over the shell; what it asks goes through the session's API, as any caller's
//! does.
//! A package that does not start is no view for the part: the part keeps the view the session ships
//! with, as a missing program does.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::platform::surfaces::part_view::{Asked, PartView, Spec};
use serde_json::Value;
use tilo_compositor::Compositor;
use tilo_session_core::desktop::overlays::{Overlays, Part, Target};
use tilo_session_core::desktop::state::DesktopHandle;

/// The tilolang views the session runs for the shell's parts.
pub(crate) struct PartViews {
    /// The shell whose own state, the search it opens, takes the keyboard before the chrome has
    /// published its surface.
    handle: DesktopHandle,
    /// The overlay stack that says which surface holds the pointer and the keys.
    overlays: Overlays,
    /// Physical pixels per logical pixel the desktop draws at.
    scale: f64,
    /// One host per part the session has run a package for. The compositor keeps what it was given,
    /// so a part's host stands there for the session's life, drawing nothing while the part runs
    /// something else.
    hosts: BTreeMap<&'static str, Rc<RefCell<PartView>>>,
    /// The catalogue's actions the session publishes as unavailable now, each with its reason, and
    /// the services revision it was read at ([`PartViews::follow_availability`]): what a part's
    /// view draws disabled.
    unavailable: RefCell<Option<(u64, tilolang_tilo::runtime::Unavailable)>>,
    /// The services revision the running views were last told the catalogue's availability at: a
    /// pass that published nothing new hands nothing on.
    told: Cell<Option<u64>>,
}

impl PartViews {
    /// The views the session runs itself, feeding the keyboard's target from `overlays` and the
    /// shell `handle` , drawing at `scale` .
    pub(crate) const fn new(handle: DesktopHandle, overlays: Overlays, scale: f64) -> Self {
        Self {
            handle,
            overlays,
            scale,
            hosts: BTreeMap::new(),
            unavailable: RefCell::new(None),
            told: Cell::new(None),
        }
    }

    /// Hand every view it runs what the catalogue says cannot run now, each action with its
    /// reason: a control bound to one draws disabled, with the reason, and is enabled when the
    /// session publishes it can run. The catalogue is read again only when the session published
    /// it anew.
    pub(crate) fn follow_availability(&self, services: &tilo_service::Services) {
        let revision = services.revision();
        if self.told.get() == Some(revision) {
            return;
        }
        self.told.set(Some(revision));
        let published = {
            let held = self.unavailable.borrow();
            held.as_ref()
                .filter(|(was, _)| *was == revision)
                .map(|(_, map)| map.clone())
        };
        let map = if let Some(map) = published {
            map
        } else {
            let map =
                crate::shell::part::readings::unavailable_actions(services.resources().iter());
            *self.unavailable.borrow_mut() = Some((revision, map.clone()));
            map
        };
        for host in self.hosts.values() {
            host.borrow_mut().set_unavailable_actions(map.clone());
        }
    }

    /// The host of `part` 's tilolang views, put in the compositor the first time the session runs
    /// one.
    fn host(&mut self, part: &'static str, compositor: &mut Compositor) -> Rc<RefCell<PartView>> {
        if let Some(host) = self.hosts.get(part) {
            return Rc::clone(host);
        }
        let overlays = self.overlays.clone();
        let handle = self.handle.clone();
        let surface = Part::named(part);
        let focused = Rc::new(move || {
            surface.is_some_and(|surface| {
                crate::shell::keyboard_target(&handle, &overlays) == Target::Surface(surface)
            })
        });
        let host = Rc::new(RefCell::new(PartView::new(part, self.scale, focused)));
        compositor.add_surface(host.clone());
        self.hosts.insert(part, Rc::clone(&host));
        host
    }

    /// Run `spec`'s package over `reading` for the part, for `person`.
    ///
    /// # Errors
    /// The package is no view for the part: it does not read, it declares no
    /// such view, or the view does not run.
    pub(crate) fn start(
        &mut self,
        part: &'static str,
        spec: &Spec,
        person: &str,
        reading: &str,
        waker: tilolang_tilo::background::Waker,
        compositor: &mut Compositor,
    ) -> Result<(), String> {
        let host = self.host(part, compositor);
        host.borrow_mut().open(spec, person, waker, reading)?;
        // What the catalogue said before the view opened, as a part opened while a menu is up
        // draws its controls the moment it runs.
        if let Some((_, published)) = self.unavailable.borrow().as_ref() {
            host.borrow_mut().set_unavailable_actions(published.clone());
        }
        Ok(())
    }

    /// Stop what runs for `part` : the part is not up any more, or another view took it.
    pub(crate) fn stop(&self, part: &str) {
        if let Some(host) = self.hosts.get(part) {
            host.borrow_mut().close();
        }
    }

    /// The host of `part`'s views, once the session ran one for it: what the session publishes so
    /// the part's controls are reached through its API.
    pub(crate) fn host_of(&self, part: &str) -> Option<Rc<RefCell<PartView>>> {
        self.hosts.get(part).map(Rc::clone)
    }

    /// Dock the view in `part` at `rect` , `(x, y, w, h)` in the desktop's coordinates: it stands
    /// there beside what the person works in, taking only what lands on what it draws.
    pub(crate) fn dock(&self, part: &str, rect: Option<(i32, i32, u32, u32)>) {
        if let Some(host) = self.hosts.get(part) {
            host.borrow_mut().dock(rect);
        }
    }

    /// Where the view in `part` is docked, while it is.
    pub(crate) fn docked(&self, part: &str) -> Option<(i32, i32, u32, u32)> {
        self.hosts.get(part).and_then(|host| host.borrow().docked())
    }

    /// Whether a tilolang view stands in `part` now.
    pub(crate) fn running(&self, part: &str) -> bool {
        self.hosts
            .get(part)
            .is_some_and(|host| host.borrow().is_open())
    }

    /// Hand the view in `part` the reading the session publishes for it now.
    pub(crate) fn publish(&self, part: &str, reading: &Value) {
        if let Some(host) = self.hosts.get(part) {
            host.borrow_mut().publish(&reading.to_string());
        }
    }

    /// Run what the view in `part` asked through `invoke` (the session's API), as any caller of
    /// the action protocol does.
    pub(crate) fn serve(&self, part: &str, invoke: impl FnMut(&Asked) -> Result<(), String>) {
        if let Some(host) = self.hosts.get(part) {
            host.borrow().serve(invoke);
        }
    }

    /// Whether the view in `part` asked something the session has not run yet.
    pub(crate) fn has_asks(&self, part: &str) -> bool {
        self.hosts
            .get(part)
            .is_some_and(|host| host.borrow().has_asks())
    }

    /// Take whether a press landed away from what the view in `part` drew while it held the
    /// pointer: the part closes.
    pub(crate) fn take_pressed_outside(&self, part: &str) -> bool {
        self.hosts
            .get(part)
            .is_some_and(|host| host.borrow_mut().take_pressed_outside())
    }

    /// Where the control the view in `part` shows as `name` stands, logical `(x, y, w, h)` : what a
    /// test presses it by.
    pub(crate) fn control(&self, part: &str, name: &str) -> Option<(f64, f64, f64, f64)> {
        let host = self.hosts.get(part)?;
        host.borrow_mut().control_bounds(name)
    }

    /// What the view in `part` says, as assistive technology hears it.
    pub(crate) fn labels(&self, part: &str) -> Vec<String> {
        self.hosts
            .get(part)
            .map_or_else(Vec::new, |host| host.borrow_mut().labels())
    }
}
