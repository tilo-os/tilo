//! A window for a workspace that does not know who is working in it yet.
//!
//! An app joins its workspace as the identity selected there. Until someone
//! has chosen one, the identity service refuses
//! ([`crate::launch::LaunchError::IdentityRequired`]) and the launch keeps
//! the app's window open on [`required_state`]: who is working here, and a
//! button that opens the session shell's identity chooser. The window waits
//! for the choice through the shell, and once it is made the launch opens
//! the app in its place: the same window, the same process.
//!
//! An app that does useful work without its workspace (Settings, whose
//! device settings belong to nobody's workspace) runs without it instead
//! ([`crate::launch::LaunchSpec::runs_without_identity`]): [`missing`] says
//! so, and the parts of it that need the workspace show [`required_state`]
//! where they would be. When the identity is chosen it too opens again in
//! its window, now joined to its workspace.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::icons::Icon;
use crate::widget::EmptyState;

/// The control id of the button that opens the identity chooser.
pub const CHOOSE_ACTION_ID: &str = "workspace-identity.choose";

thread_local! {
    /// Whether the window on this thread runs without its workspace's
    /// identity (see [`missing`]).
    static MISSING: Cell<bool> = const { Cell::new(false) };
}

/// Set from the shell's answer, on the chooser connection's thread: the
/// workspace has an identity now, so the window gives its place to the app
/// joined to its workspace.
static CHOSEN: AtomicBool = AtomicBool::new(false);

/// Whether this app runs without its workspace's identity: the workspace has
/// none selected yet, and the app opened what works without one. Its parts
/// that need the workspace show [`required_state`] in place.
#[must_use]
pub fn missing() -> bool {
    MISSING.get()
}

/// Say whether the window on this thread runs without its workspace's
/// identity. The launch says it; a test stands in for the launch.
#[doc(hidden)]
pub fn set_missing(missing: bool) {
    MISSING.set(missing);
}

/// What stands where an app needs its workspace: who is working here, and
/// the button that opens the shell's identity chooser. `app` is the app's
/// name as people read it ("Settings").
#[must_use]
pub fn required_state(app: &str) -> EmptyState {
    required_state_choosing(app, choose)
}

/// [`required_state`] whose button runs `choose`.
pub(crate) fn required_state_choosing(app: &str, choose: impl FnMut() + 'static) -> EmptyState {
    EmptyState::new("workspace-identity-required-title")
        .with_icon(Icon::User)
        .with_message(crate::i18n!("workspace-identity-required-body", "app" => app))
        .with_action("workspace-identity-required-choose")
        .with_action_id(CHOOSE_ACTION_ID)
        .on_view(choose)
}

/// Open the session shell's identity chooser for this app's workspace. The
/// person chooses there; the window hears the choice through the wait the
/// launch started.
pub fn choose() {
    if let Some(link) = tilo_host::shell_choosers::ShellLink::inherited() {
        link.choose_identity(|outcome| {
            if let Err(reason) = outcome {
                tracing::warn!(%reason, "the identity chooser did not open");
            }
        });
        return;
    }
    tracing::warn!("no session shell to choose this workspace's identity with");
}

/// Wait, through the session shell, for the workspace's identity to be
/// chosen; once it is, the window in front leaves its place for the app
/// joined to its workspace ([`chosen`]). Call inside that window's wake
/// scope: the answer wakes it.
#[cfg_attr(
    not(feature = "native"),
    expect(dead_code, reason = "only a native launch waits for the identity")
)]
pub(crate) fn await_choice() {
    if let Some(link) = tilo_host::shell_choosers::ShellLink::inherited() {
        let wake = crate::wake::WakeHandle::new();
        link.await_identity(move |outcome| match outcome {
            Ok(()) => {
                CHOSEN.store(true, Ordering::Release);
                wake.wake();
            }
            Err(reason) => {
                tracing::warn!(%reason, "cannot hear when this workspace's identity is chosen");
            }
        });
        return;
    }
    tracing::warn!("no session shell to hear this workspace's identity from");
}

/// Whether the workspace's identity was chosen while the window waited: the
/// window leaves its event loop so the launch opens the app in its place.
#[must_use]
#[cfg_attr(
    not(feature = "native"),
    expect(dead_code, reason = "only a native window gives its place")
)]
pub(crate) fn chosen() -> bool {
    CHOSEN.load(Ordering::Acquire)
}

/// [`chosen`], answered once: the launch takes it when it opens the app.
#[cfg_attr(
    not(feature = "native"),
    expect(dead_code, reason = "only a native launch opens the app in place")
)]
pub(crate) fn take_chosen() -> bool {
    CHOSEN.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;
    use std::rc::Rc;

    // USER REQUIREMENT (2026-10-19): "It shows the app's window with an
    // EmptyState: a user icon from the Lucide set, the title "Choose who you
    // are here", the body "This workspace needs to know who's working in it
    // before {app} can open.", and a primary button "Choose identity"."
    #[test]
    fn the_window_says_who_is_needed_and_its_button_opens_the_chooser() {
        let asked = Rc::new(Cell::new(0));
        let counted = Rc::clone(&asked);
        let state = required_state_choosing("Settings", move || counted.set(counted.get() + 1));
        assert_eq!(Icon::User.name(), "user", "the Lucide user icon");
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            state,
            (800, 600),
            Theme::dark(),
        );
        let root = harness.root::<EmptyState>();
        let node = crate::widget::Widget::accessibility(root).expect("the state is semantic");
        assert_eq!(node.name.as_deref(), Some("Choose who you are here"));
        assert_eq!(
            node.description.as_deref(),
            Some("This workspace needs to know who’s working in it before Settings can open.")
        );
        assert_eq!(node.children.len(), 1, "one action");
        assert_eq!(node.children[0].role, crate::a11y::Role::Button);
        assert_eq!(node.children[0].name.as_deref(), Some("Choose identity"));

        let action = harness.root::<EmptyState>().action_rect();
        assert!(action.area() > 0.0, "the button has a target");
        harness.click_at(action.center().x, action.center().y);
        assert_eq!(asked.get(), 1, "the button opens the identity chooser");
    }
}
