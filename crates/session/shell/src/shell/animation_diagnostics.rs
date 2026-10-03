//! Allocation-free decomposition of compositor animation demand.

use std::time::{Duration, Instant};

pub(crate) const HOST_SOURCE_NAMES: [&str; 16] = [
    "root-transition",
    "quick-settings",
    "workspace-settings",
    "palette",
    "power-menu",
    "room-activity",
    "follow",
    "room-replay",
    "capture-studio",
    "projection",
    "content",
    "sidebar",
    "capture-mirror",
    "screenshot-markup",
    "notification",
    "workspace-switch",
];

#[allow(
    clippy::struct_excessive_bools,
    reason = "one named flag per independent animation source keeps the bit layout auditable"
)]
pub(crate) struct HostDemandInputs {
    /// First-login setup owns the screen: only it and Capture Studio count.
    pub onboarding: bool,
    /// Whether that setup view has motion in flight. Its mere presence is
    /// not demand: a static setup screen must idle like a static desktop.
    pub onboarding_animating: bool,
    pub capture_studio: bool,
    pub root_transition: bool,
    pub quick_settings: bool,
    pub workspace_settings: bool,
    pub palette: bool,
    pub power_menu: bool,
    pub room_activity: bool,
    pub follow: bool,
    pub room_replay: bool,
    pub projection: bool,
    pub content: bool,
    pub sidebar: bool,
    pub capture_mirror: bool,
    pub screenshot_markup: bool,
    pub notification: bool,
    pub workspace_switch: bool,
}

pub(crate) fn host_bits(inputs: &HostDemandInputs) -> u32 {
    if inputs.onboarding {
        return u32::from(inputs.onboarding_animating) | (u32::from(inputs.capture_studio) << 1);
    }
    u32::from(inputs.root_transition)
        | (u32::from(inputs.quick_settings) << 1)
        | (u32::from(inputs.workspace_settings) << 2)
        | (u32::from(inputs.palette) << 3)
        | (u32::from(inputs.power_menu) << 4)
        | (u32::from(inputs.room_activity) << 5)
        | (u32::from(inputs.follow) << 6)
        | (u32::from(inputs.room_replay) << 7)
        | (u32::from(inputs.capture_studio) << 8)
        | (u32::from(inputs.projection) << 9)
        | (u32::from(inputs.content) << 10)
        | (u32::from(inputs.sidebar) << 11)
        | (u32::from(inputs.capture_mirror) << 12)
        | (u32::from(inputs.screenshot_markup) << 13)
        | (u32::from(inputs.notification) << 14)
        | (u32::from(inputs.workspace_switch) << 15)
}

pub(crate) fn host_labels(bits: u32) -> impl Iterator<Item = &'static str> {
    HOST_SOURCE_NAMES
        .into_iter()
        .enumerate()
        .filter_map(move |(bit, name)| (bits & (1 << bit) != 0).then_some(name))
}

/// Paths to the deepest widgets under `root` that report animation demand:
/// the ones actually holding the loop open, not every container above them.
/// Allocates, so only the paint-stats diagnostic calls it.
pub(crate) fn animating_leaves(root: &dyn ui::widget::Widget) -> Vec<String> {
    fn walk(widget: &dyn ui::widget::Widget, path: &str, out: &mut Vec<String>) {
        if !widget.needs_animation_frame() {
            return;
        }
        // `~` marks a widget that animates without asking to be redrawn: a
        // retained parent cache then never reaches it again.
        let clean = if widget.needs_redraw() { "" } else { "~" };
        let here = match widget.test_id() {
            Some(id) => format!("{path}/{}{clean}#{id}", widget.name()),
            None => format!("{path}/{}{clean}", widget.name()),
        };
        let before = out.len();
        widget.for_each_child(&mut |child| walk(child, &here, out));
        if out.len() == before {
            let rect = widget.get_rect();
            let detail = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<crate::shell::foreign::ForeignSurfaceWidget>())
                .map(|foreign| format!(" {}", foreign.animation_detail()))
                .unwrap_or_default();
            out.push(format!(
                "{here}@{:.0},{:.0} {:.0}x{:.0}{detail}",
                rect.x0,
                rect.y0,
                rect.width(),
                rect.height()
            ));
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

/// Paths to the deepest widgets under `root` whose one-shot wake deadline
/// is `due` or earlier: who will wake an otherwise idle loop next. Allocates,
/// so only diagnostics call it.
pub(crate) fn wake_deadline_leaves(root: &dyn ui::widget::Widget, due: Instant) -> Vec<String> {
    fn walk(widget: &dyn ui::widget::Widget, due: Instant, path: &str, out: &mut Vec<String>) {
        if widget.next_wake_deadline().is_none_or(|at| at > due) {
            return;
        }
        let here = match widget.test_id() {
            Some(id) => format!("{path}/{}#{id}", widget.name()),
            None => format!("{path}/{}", widget.name()),
        };
        let before = out.len();
        widget.for_each_child(&mut |child| walk(child, due, &here, out));
        if out.len() == before {
            out.push(here);
        }
    }
    let mut out = Vec::new();
    walk(
        root,
        due.checked_add(WAKE_MATCH_SLACK).unwrap_or(due),
        "",
        &mut out,
    );
    out
}

/// Deadlines computed against a slightly later clock than the minimum they
/// are compared with still name the same owner.
pub(crate) const WAKE_MATCH_SLACK: Duration = Duration::from_millis(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_labels_decode_stable_source_bits() {
        assert_eq!(
            host_labels((1 << 1) | (1 << 10) | (1 << 14)).collect::<Vec<_>>(),
            ["quick-settings", "content", "notification"],
        );
        assert!(host_labels(0).next().is_none());
    }

    #[test]
    fn a_static_onboarding_screen_demands_no_frames() {
        let inputs = |onboarding_animating, quick_settings| HostDemandInputs {
            onboarding: true,
            onboarding_animating,
            capture_studio: false,
            root_transition: false,
            quick_settings,
            workspace_settings: false,
            palette: false,
            power_menu: false,
            room_activity: false,
            follow: false,
            room_replay: false,
            projection: false,
            content: false,
            sidebar: false,
            capture_mirror: false,
            screenshot_markup: false,
            notification: false,
            workspace_switch: false,
        };
        assert_eq!(host_bits(&inputs(false, false)), 0);
        assert_eq!(host_bits(&inputs(true, false)), 1);
        // Setup owns the screen: the desktop's sources beneath it are hidden.
        assert_eq!(host_bits(&inputs(false, true)), 0);
    }
}
