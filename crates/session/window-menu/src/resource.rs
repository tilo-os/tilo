//! The view's own record: nothing but its identity.
//!
//! What the menu shows belongs to the session, not here: the reading it
//! draws is `desktop:window-menu`, and the verbs it invokes are that
//! resource's. This record exists so the app is an app — a window with an
//! id, a title and a surface — and holds nothing a swap would lose.

// The derives below name the resource crate by its own name.
use ui::prelude::*;

use crate::surface::WindowMenuSurface;

/// The window menu view.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct WindowMenu {}

#[resource(kind = "desktop.window-menu-view", label = "Window menu")]
impl WindowMenu {}

/// The app the session mounts over the desk while a window's menu is up.
pub struct WindowMenuApp;

impl ResourceApp for WindowMenuApp {
    type Resource = WindowMenu;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Window menu";
    const ICON: Option<&'static str> = Some("menu");
    // A menu over the desktop: what is not the menu shows what is under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    // The menu shows this device's desk: it is nobody's workspace participant
    // and opens before anyone is chosen.
    const ATTACHES_RUNTIME: bool = false;
    const OPENS_WITHOUT_IDENTITY: bool = true;

    fn view(cx: &mut ViewCx<WindowMenu>) -> Element {
        cx.surface(WindowMenuSurface::new())
    }
}
