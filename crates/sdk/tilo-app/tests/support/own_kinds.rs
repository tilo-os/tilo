//! A test app's installed surface declaring the kinds each member keeps as
//! their own record in a workspace, as an app's manifest does
//! (`[resources] own`): a record of an undeclared kind is refused, so it
//! never reaches every member.

use std::fmt::Write as _;

/// Install `app`'s surface, declaring `kinds` each member's own.
pub fn declare(app: &str, kinds: &[&str]) {
    let mut surface = String::new();
    for kind in kinds {
        let _ = write!(
            surface,
            "[[resources]]\nkind = \"{kind}\"\nlabel = \"{kind}\"\nversion = 1\nown = true\n"
        );
    }
    collab_types::app_surface::register_package(app, surface.as_bytes());
}
