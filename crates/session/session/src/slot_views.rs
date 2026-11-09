//! The shell's slots whose view is written in tilolang, run in the session's own process.
//! A slot's view is a program of its own (a surface app) unless the app the person bound is a
//! tilolang package this machine installed: then the view is the package's, and the session runs it
//! in an instance of its own ([`tilo_surface_host::slot_view::SlotView`]), over the reading it
//! publishes for the slot. The pointer and the keys reach it while the overlay stack says its
//! surface is the one over the desk; what it asks goes through the session's door, as any caller's
//! does.
//! A package that does not start is no view for the slot: the slot keeps the view the session ships
//! with, as a missing program does.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;
use tilo_compositor::Compositor;
use tilo_desktop::overlays::{Overlays, Slot, Target};
use tilo_desktop_surface::DesktopHandle;
use tilo_surface_host::slot_view::{Asked, SlotView, Spec};

use crate::platform::surfaces::Held;

/// The tilolang views the session runs for the shell's slots.
pub(crate) struct SlotViews {
    /// The desk whose own state, the search it opens, takes the keyboard before the chrome has
    /// published its surface.
    handle: DesktopHandle,
    /// The overlay stack that says which surface holds the pointer and the keys.
    overlays: Overlays,
    /// Physical pixels per logical pixel the desktop draws at.
    scale: f64,
    /// One host per slot the session has run a package for. The compositor keeps what it was given,
    /// so a slot's host stands there for the session's life, drawing nothing while the slot runs
    /// something else.
    hosts: BTreeMap<&'static str, Rc<RefCell<SlotView>>>,
}

impl SlotViews {
    /// The views the session runs itself, feeding the keyboard's target from `overlays` and the
    /// desk `handle` , drawing at `scale` .
    pub(crate) const fn new(handle: DesktopHandle, overlays: Overlays, scale: f64) -> Self {
        Self {
            handle,
            overlays,
            scale,
            hosts: BTreeMap::new(),
        }
    }

    /// The host of `slot` 's tilolang views, put in the compositor the first time the session runs
    /// one.
    fn host(&mut self, slot: &'static str, compositor: &mut Compositor) -> Rc<RefCell<SlotView>> {
        if let Some(host) = self.hosts.get(slot) {
            return Rc::clone(host);
        }
        let overlays = self.overlays.clone();
        let handle = self.handle.clone();
        let surface = Slot::named(slot);
        let focused = Rc::new(move || {
            surface.is_some_and(|surface| {
                crate::chrome::keyboard_target(&handle, &overlays) == Target::Surface(surface)
            })
        });
        let host = Rc::new(RefCell::new(SlotView::new(slot, self.scale, focused)));
        compositor.add_host(Box::new(Held(Rc::clone(&host))));
        self.hosts.insert(slot, Rc::clone(&host));
        host
    }

    /// Run `spec`'s package over `reading` for the slot, for `person`.
    ///
    /// # Errors
    /// The package is no view for the slot: it does not read, it declares no
    /// such view, or the view does not run.
    pub(crate) fn start(
        &mut self,
        slot: &'static str,
        spec: &Spec,
        person: &str,
        reading: &str,
        waker: tilolang_tilo::background::Waker,
        compositor: &mut Compositor,
    ) -> Result<(), String> {
        let host = self.host(slot, compositor);
        host.borrow_mut().open(spec, person, waker, reading)
    }

    /// Stop what runs for `slot` : the part is not up any more, or another view took it.
    pub(crate) fn stop(&self, slot: &str) {
        if let Some(host) = self.hosts.get(slot) {
            host.borrow_mut().close();
        }
    }

    /// Whether a tilolang view stands in `slot` now.
    pub(crate) fn running(&self, slot: &str) -> bool {
        self.hosts
            .get(slot)
            .is_some_and(|host| host.borrow().is_open())
    }

    /// Hand the view in `slot` the reading the session publishes for it now.
    pub(crate) fn publish(&self, slot: &str, reading: &Value) {
        if let Some(host) = self.hosts.get(slot) {
            host.borrow_mut().publish(&reading.to_string());
        }
    }

    /// Run what the view in `slot` asked through `invoke` (the session's door), as any caller of
    /// the action protocol does.
    pub(crate) fn serve(&self, slot: &str, invoke: impl FnMut(&Asked) -> Result<(), String>) {
        if let Some(host) = self.hosts.get(slot) {
            host.borrow().serve(invoke);
        }
    }

    /// Whether the view in `slot` asked something the session has not run yet.
    pub(crate) fn has_asks(&self, slot: &str) -> bool {
        self.hosts
            .get(slot)
            .is_some_and(|host| host.borrow().has_asks())
    }

    /// Take whether a press landed away from what the view in `slot` drew while it held the
    /// pointer: the slot closes.
    pub(crate) fn take_pressed_outside(&self, slot: &str) -> bool {
        self.hosts
            .get(slot)
            .is_some_and(|host| host.borrow_mut().take_pressed_outside())
    }

    /// Where the control the view in `slot` shows as `name` stands, logical `(x, y, w, h)` : what a
    /// test presses it by.
    pub(crate) fn control(&self, slot: &str, name: &str) -> Option<(f64, f64, f64, f64)> {
        let host = self.hosts.get(slot)?;
        host.borrow_mut().control_bounds(name)
    }

    /// What the view in `slot` says, as assistive technology hears it.
    pub(crate) fn labels(&self, slot: &str) -> Vec<String> {
        self.hosts
            .get(slot)
            .map_or_else(Vec::new, |host| host.borrow_mut().labels())
    }
}
