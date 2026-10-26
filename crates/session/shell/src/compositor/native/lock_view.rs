//! Live lock-view inputs and the semantic tree published with them. Scene
//! painting stays pure; the native host owns the clock and credential state.

use super::{
    FrameBuilder, LockController, NativeAccessibility, NodeId, Output, focused_display_id,
};
use crate::compositor::lock_screen::{self, LockView, lock_outputs};

/// A lock-state edge replaces every pixel on screen. Keep that edge pending
/// until a frame is actually accepted by the presenter; consuming it while
/// the asynchronous render queue is full would leave the previous lock or
/// desktop frame visible until unrelated input happens to trigger a repaint.
pub(super) fn retain_session_redraw(session_changed: bool, frame_submitted: bool) -> bool {
    session_changed && !frame_submitted
}

pub(super) struct LockClock {
    clock: String,
    date: String,
}

impl LockClock {
    pub(super) fn now() -> Self {
        let now = ui::clock::local_now();
        Self {
            clock: ui::clock::time_of_day(&now),
            date: ui::format::day_heading(now.date_naive(), false),
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        lock: &'a LockController,
        notification_count: usize,
        now_playing: Option<&'a crate::shell::now_playing::PlayerSnapshot>,
        keyboard: Option<ui::passcode_keyboard::PasscodeKeyboard>,
        alert: Option<&'a str>,
    ) -> LockView<'a> {
        LockView {
            alert,
            keyboard,
            password_len: lock.buffer_len(),
            preedit: lock.preedit(),
            error: lock.error(),
            clock: &self.clock,
            date: &self.date,
            notification_count,
            verifying: lock.is_verifying(),
            now_playing,
        }
    }
}

/// Hand AccessKit the current semantic tree. It needs a laid-out widget tree,
/// not a presented frame: an activation or action is answered in the tick
/// that drained it, whether or not that tick paints.
pub(super) fn publish_accessibility_tree(
    accessibility: &mut NativeAccessibility,
    frame_builder: &FrameBuilder,
    outputs: &[Output],
    lock: Option<&LockView<'_>>,
    focus: Option<NodeId>,
) {
    accessibility.set_window_focus(true);
    if let Some(view) = lock {
        let outputs = lock_outputs(
            outputs,
            frame_builder.scene_size(),
            focused_display_id(frame_builder),
        );
        accessibility.update_if_active(|| {
            lock_screen::build_accessibility_tree(&outputs, frame_builder.theme(), view)
        });
    } else {
        accessibility.update_if_active(|| frame_builder.build_a11y_tree_update(focus, 1.0));
    }
}
