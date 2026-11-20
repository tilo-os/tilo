//! The desktop's part of a frame, whichever way the mount presents it: on
//! its canvas or as HTML.

use super::{Desktop, Mount};

impl Mount {
    /// Whether the desktop asked for a frame: an app it was asked to open,
    /// or a wake from one of its components, which [`Self::follow_desktop`]
    /// takes.
    pub(crate) fn desktop_wants_frame(desktop: Option<&Desktop>) -> bool {
        desktop.is_some_and(|desktop| {
            desktop.open_requested_apps()
                || desktop
                    .shared
                    .woken
                    .load(std::sync::atomic::Ordering::Acquire)
        })
    }

    /// The shell took a key or a click, and what it asked of the page for it (a card chosen on
    /// the empty shell, a menu's row, a command) is carried out on its next pass. That pass comes
    /// in the frame the input asks for and only there: a frame the shell's widgets asked for does
    /// theirs, and one a press itself asked for may find none, leaving the choice waiting for
    /// whatever frame the page came to next. The input's own frame follows at once
    /// (`Self::present_after_input`), so the shell is told without asking the page for a second
    /// frame of its own: that frame found nothing left to do and drew every key twice. A key let
    /// go asks nothing of the page (a shortcut acts as it is pressed), so its frame leaves the
    /// shell's pass alone.
    pub(crate) fn wake_shell_for_input(&self, event: &winit::event::WindowEvent) {
        let released = matches!(
            event,
            winit::event::WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Released
        );
        if let Some(desktop) = self.desktop.as_ref().filter(|_| !released) {
            desktop.shared.wake_in_input_frame();
        }
    }

    /// The desktop's part of a frame, whichever way it is presented: its
    /// width, its agents, uploads, notices, mentions, activity, governance,
    /// search and holders, and the wakes its components sent since the last frame.
    /// Returns whether what the windows show changed.
    pub(crate) fn follow_desktop(&self, width: f64, reasons: Option<&mut Vec<String>>) -> bool {
        let held = self.desktop.as_ref().is_some_and(|desktop| {
            desktop.handle.set_viewport_width(width);
            desktop.offer_uploads(false);
            desktop.tell_mentions();
            desktop.serve_activity();
            let named = desktop.project_governance(self.peer);
            desktop.follow_search();
            let holders = desktop.refresh_holders(self.peer.is_some());
            named || holders
        });
        // Desktop commands and peer changes need an update even when the
        // harness itself is idle. Let a scheduled poll consume them first:
        // eagerly relayouting here and then polling animated widgets runs
        // the same frame pipeline twice before one presentation.
        let mut host_changed = held;
        if let Some(desktop) = self.desktop.as_ref().filter(|d| d.shared.take_wake()) {
            let wakers = desktop.shared.take_wakers();
            if let Some(reasons) = reasons {
                if wakers.is_empty() {
                    reasons.push("desktop wake".into());
                }
                reasons.extend(wakers.iter().map(|app| format!("wake: {app}")));
            }
            if desktop.shared.take_arrived() {
                desktop.shared.retry_waiting(&desktop.handle);
            }
            host_changed = true;
        }
        host_changed
    }
}
