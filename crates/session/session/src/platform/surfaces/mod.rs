//! The session's own layers: what its roles place over the windows, on every host.
//!
//! A role draws its panel, its roster or its card as a widget tree of its own and asks the host to
//! show it on a layer above (or below) the windows: this session's compositor on a Linux desktop,
//! the page's canvas on the web. The role keeps the returned [`Layer`] and never names the host
//! that draws it; what it shows to assistive technology and to the session's door is read through
//! it too, so a panel is pressed and read the same on both.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_session_core::door::Published;
use ui::widget::Widget;

pub use tilo_session_core::surface::Placement;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Held, Hosted};

/// What a role places over the windows: the host's layers.
pub trait Surfaces {
    /// Place `tree` on a layer of its own, called `name`, standing at `placement`: it covers the
    /// outputs, and what `heard` says of the layer's role reaches it. The actions `published`
    /// offers are reached through the session's door.
    fn place(
        &mut self,
        name: &str,
        tree: Box<dyn Widget>,
        placement: Placement,
        heard: Heard,
    ) -> Rc<dyn Layer>;

    /// Reach what `published` offers through the session's door from now on: the layer the role
    /// placed is what shows it.
    fn publish(&mut self, published: Rc<RefCell<dyn Published>>);
}

/// What a layered role hears from the host, beside the input its widget tree is given.
#[derive(Clone)]
pub struct Heard {
    /// Whether the role wants the keyboard while its layer stands.
    pub wants_keyboard: Rc<dyn Fn() -> bool>,
    /// The pointer was pressed somewhere other than this layer: what the role has open closes.
    /// It runs before the host holds the tree the role placed, so it may ask that layer to draw
    /// again.
    pub pressed_elsewhere: Rc<dyn Fn()>,
}

/// A layer the host placed, as the role that placed it holds it.
pub trait Layer {
    /// Draw it again: what it shows changed, though no input reached it.
    fn relayout(&self);

    /// Draw it in `theme`.
    fn set_theme(&self, theme: ui::Theme);

    /// What it shows, as a number that moves when its frame changed.
    fn revision(&self) -> u64;

    /// What it shows assistive technology.
    fn a11y_tree(&self) -> Option<ui::accesskit::TreeUpdate>;

    /// Assistive technology acted on `target`.
    fn a11y_action(&self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool;

    /// What it says to assistive technology, as its own widgets name it.
    fn accessible_labels(&self) -> Vec<String>;

    /// The controls it shows, where they stand.
    fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))>;

    /// Press the control `name`.
    ///
    /// # Errors
    /// No control of that name stands on the layer.
    fn press(&self, name: &str) -> Result<(), String>;
}
