//! A written view as a widget: a view a person or an agent wrote in the app language, shown where
//! an app places it (`ui::written_view`).
//!
//! The view runs in an instance of its own ([`tilolang_tilo::instance`]) and is drawn by the UI
//! host's half of a window ([`tilo_app_window::AppWindow`]) as a part: the toolkit's own widgets,
//! built from the component patches the instance sends. The instance holds no record. What the
//! view names are the workspace's apps, each read as the package its published surface declares
//! ([`apps`]) and loaded as an app this runtime does not run:
//!
//! * what it reads of an app is that app's record of the workspace, asked of the session's action
//!   API as the person at the window and asked again when it changed ([`workspace`]), so the view
//!   is drawn again when the record moves and shows nothing the person may not read;
//! * what it does with an action is decided first by the workspace's one policy, with the view's
//!   writer as the author of its code ([`surface`]). An allowed action is sent to the app's own
//!   API. One the policy asks about waits as a suggestion the person allows or dismisses
//!   ([`state`]); nothing is sent before.
//!
//! The view itself arrives as a reading too: a small package this crate compiles once
//! ([`wrapper`]) embeds the definition it reads, with its writer, so a definition written again
//! is one more reading and needs no new instance.
//!
//! A program reaches all of this through [`builtin`] alone, so one that fetches code on first
//! use keeps the language's compiler and interpreter out of its start.

mod apps;
mod platform;
mod state;
mod surface;
mod workspace;
mod wrapper;

#[cfg(test)]
mod sweep;
#[cfg(test)]
mod tests;

use ui::written_view::{MakeWrittenView, Shown, WrittenView};

/// The code that runs written views: what a program installs in `ui::written_view::WRITTEN_VIEWS`.
#[must_use]
pub fn builtin() -> MakeWrittenView {
    MakeWrittenView(make)
}

fn make(view: &WrittenView, api: &ui::RemoteActions) -> Shown {
    surface::shown(view, Box::new(workspace::Session::new(api.clone())))
}
