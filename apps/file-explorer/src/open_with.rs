//! "Open With" — which applications can open a file, and opening it with one.
//!
//! Files decides none of this. The desktop owns the answer: which Tilo
//! packages and host applications claim a type, which one is the person's
//! default, and whether an application may be handed the file at all. The
//! host installs an [`OpenWithRoute`] (the compositor's resolver, over the
//! same candidates it uses to open a file on activation); Files only lists
//! what it is told and asks for what the person picked.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// One application that claims the file's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWithApp {
    /// Opaque application id: a Tilo package id or a host desktop file id.
    pub id: String,
    /// Human name, as the desktop shows it elsewhere.
    pub name: String,
    /// Whether activating the file opens it with this application today.
    pub is_default: bool,
}

/// Every application that can open a file, default first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenWithCandidates {
    /// The type as the person reads it (".pdf files", "PDF documents"), used
    /// to label "Always use for …".
    pub type_label: String,
    pub apps: Vec<OpenWithApp>,
}

impl OpenWithCandidates {
    pub fn find(&self, id: &str) -> Option<&OpenWithApp> {
        self.apps.iter().find(|app| app.id == id)
    }
}

/// The host's answer to "what opens this, and open it with that".
pub trait OpenWithRoute {
    /// True only when this host has linked normal target launch for retained
    /// selections. A collaboration descriptor alone is not availability.
    fn retained_selected_open_available(&self) -> bool {
        false
    }

    /// Applications that claim `path`'s type, the current default first.
    fn candidates(&self, path: &Path) -> OpenWithCandidates;

    /// Open `path` with exactly `app_id`, once.
    ///
    /// # Errors
    ///
    /// A message naming the file when the application is gone, no longer
    /// claims the type, or failed to start.
    fn open_with(&self, path: &Path, app_id: &str) -> Result<(), String>;

    /// Make `app_id` the person's default for `path`'s type.
    ///
    /// # Errors
    ///
    /// A message when the application does not claim the type or the choice
    /// could not be saved.
    fn set_default(&self, path: &Path, app_id: &str) -> Result<(), String>;
}

pub type SharedOpenWithRoute = Rc<dyn OpenWithRoute>;

thread_local! {
    static ROUTE: RefCell<Option<SharedOpenWithRoute>> = const { RefCell::new(None) };
}

/// Install the route every Files view on this thread uses to list and launch
/// "Open With" applications. The compositor installs its resolver here, next
/// to the document opener.
pub fn set_open_with_route(route: SharedOpenWithRoute) {
    ROUTE.with(|slot| *slot.borrow_mut() = Some(route));
}

/// Remove the installed route. Used by tests, which share the thread-local.
#[cfg(test)]
pub(crate) fn clear_open_with_route() {
    ROUTE.with(|slot| *slot.borrow_mut() = None);
}

pub(crate) fn installed_route() -> Option<SharedOpenWithRoute> {
    ROUTE.with(|slot| slot.borrow().clone())
}
