//! Where a shell remembers the sidebar width the person dragged to: the
//! app's own settings on this device, like the window's content zoom.
//!
//! The window's host says which app's settings those are
//! ([`remember_for_app`]) before it builds the window; every shell built on
//! that thread reads its width from there at its first layout and writes
//! there when a resize ends. A host that says nothing (a test, a capture,
//! a component inside another window) remembers nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::storage::{Scope, Settings, SettingsSchema};

/// The sidebar widths an app's shells were dragged to, authored at 1×,
/// keyed by the shell's id ([`super::NavShell::with_id`]).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct NavShellMemory {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) nav_sidebar_widths: BTreeMap<String, f64>,
}

impl SettingsSchema for NavShellMemory {
    const SCOPE: Scope = Scope::DeviceLocal;
}

thread_local! {
    static MEMORY: RefCell<Option<Settings<NavShellMemory>>> = const { RefCell::new(None) };
}

/// Remember the sidebar widths of the shells this thread builds in
/// `app_id`'s settings on this device. Only the native runner's windows
/// remember; a component build of the toolkit has no app settings to say.
#[cfg(feature = "native")]
pub(crate) fn remember_for_app(app_id: &str) {
    install(Some(Settings::for_app(app_id)));
}

/// Remember the sidebar widths of the shells this thread builds in
/// `settings` (`None`: nowhere). Returns where they were remembered before.
#[cfg(any(feature = "native", test))]
pub(crate) fn install(
    settings: Option<Settings<NavShellMemory>>,
) -> Option<Settings<NavShellMemory>> {
    MEMORY.with(|memory| std::mem::replace(&mut *memory.borrow_mut(), settings))
}

/// Where the shells this thread builds remember their sidebar widths.
pub(super) fn current() -> Option<Settings<NavShellMemory>> {
    MEMORY.with(|memory| memory.borrow().clone())
}

/// The width the shell `key` was dragged to, authored at 1×.
pub(super) fn recall(settings: &Settings<NavShellMemory>, key: &str) -> Option<f64> {
    settings
        .get()
        .nav_sidebar_widths
        .get(key)
        .copied()
        .filter(|width| width.is_finite() && *width > 0.0)
}

/// Keep `width` (authored at 1×) as the shell `key`'s sidebar width.
pub(super) fn keep(settings: &Settings<NavShellMemory>, key: &str, width: f64) {
    if !width.is_finite() || width <= 0.0 {
        return;
    }
    if let Err(error) = settings.update(|memory| {
        memory.nav_sidebar_widths.insert(key.to_owned(), width);
    }) {
        tracing::debug!(%error, "nav shell: could not remember the sidebar width");
    }
}
