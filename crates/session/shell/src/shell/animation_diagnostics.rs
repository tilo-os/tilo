//! Allocation-free decomposition of compositor animation demand.
//!
//! This is the demand itself, not a report beside it: the shell asks for a
//! frame exactly when one of these sources is set
//! (`ShellOverlayHost::needs_animation_frame_at`), so every motion the shell
//! draws is named here while it moves, and a source missing from this list
//! is a motion that gets no frames.

pub(crate) const HOST_SOURCE_NAMES: [&str; 32] = [
    "root-transition",
    "quick-settings",
    "workspace-settings",
    "palette",
    "power-menu",
    "follow",
    "capture-studio",
    "projection",
    "content",
    "strip_switcher",
    "capture-mirror",
    "screenshot-markup",
    "notification",
    "workspace-switch",
    "desk",
    "strip-indicators",
    "cursor-chat",
    "collaborators",
    "osd",
    "acknowledgement",
    "window-previews",
    "peer-sync",
    "directory-sheet",
    "tab-menu",
    "window-switcher",
    "dictation",
    "emoji-picker",
    "display-confirm",
    "feedback-picker",
    "control-hints",
    "onboarding",
    "fullscreen-hint",
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
    pub follow: bool,
    pub projection: bool,
    pub content: bool,
    pub strip_switcher: bool,
    pub capture_mirror: bool,
    pub screenshot_markup: bool,
    pub notification: bool,
    pub workspace_switch: bool,
    pub desk: bool,
    pub strip_indicators: bool,
    pub cursor_chat: bool,
    pub collaborators: bool,
    pub osd: bool,
    pub acknowledgement: bool,
    pub window_previews: bool,
    pub peer_sync: bool,
    pub directory_sheet: bool,
    pub tab_menu: bool,
    pub window_switcher: bool,
    pub dictation: bool,
    pub emoji_picker: bool,
    pub display_confirm: bool,
    /// Pointing at what is wrong, to report it.
    pub feedback_picker: bool,
    pub control_hints: bool,
    /// The way out of full screen, fading out over the window.
    pub fullscreen_hint: bool,
}

/// The bit of [`host_bits`] that says the workspace content animates.
pub(crate) const CONTENT_BIT: u32 = 1 << 8;

/// The bit of [`host_bits`] that says the way out of full screen animates.
pub(crate) const FULLSCREEN_HINT_BIT: u32 = 1 << 31;

pub(crate) fn host_bits(inputs: &HostDemandInputs) -> u32 {
    if inputs.onboarding {
        return (u32::from(inputs.capture_studio) << 6)
            | (u32::from(inputs.onboarding_animating) << 30);
    }
    u32::from(inputs.root_transition)
        | (u32::from(inputs.quick_settings) << 1)
        | (u32::from(inputs.workspace_settings) << 2)
        | (u32::from(inputs.palette) << 3)
        | (u32::from(inputs.power_menu) << 4)
        | (u32::from(inputs.follow) << 5)
        | (u32::from(inputs.capture_studio) << 6)
        | (u32::from(inputs.projection) << 7)
        | (if inputs.content { CONTENT_BIT } else { 0 })
        | (u32::from(inputs.strip_switcher) << 9)
        | (u32::from(inputs.capture_mirror) << 10)
        | (u32::from(inputs.screenshot_markup) << 11)
        | (u32::from(inputs.notification) << 12)
        | (u32::from(inputs.workspace_switch) << 13)
        | (u32::from(inputs.desk) << 14)
        | (u32::from(inputs.strip_indicators) << 15)
        | (u32::from(inputs.cursor_chat) << 16)
        | (u32::from(inputs.collaborators) << 17)
        | (u32::from(inputs.osd) << 18)
        | (u32::from(inputs.acknowledgement) << 19)
        | (u32::from(inputs.window_previews) << 20)
        | (u32::from(inputs.peer_sync) << 21)
        | (u32::from(inputs.directory_sheet) << 22)
        | (u32::from(inputs.tab_menu) << 23)
        | (u32::from(inputs.window_switcher) << 24)
        | (u32::from(inputs.dictation) << 25)
        | (u32::from(inputs.emoji_picker) << 26)
        | (u32::from(inputs.display_confirm) << 27)
        | (u32::from(inputs.feedback_picker) << 28)
        | (u32::from(inputs.control_hints) << 29)
        | if inputs.fullscreen_hint {
            FULLSCREEN_HINT_BIT
        } else {
            0
        }
}

pub(crate) fn host_labels(bits: u32) -> impl Iterator<Item = &'static str> {
    HOST_SOURCE_NAMES
        .into_iter()
        .enumerate()
        .filter_map(move |(bit, name)| (bits & (1 << bit) != 0).then_some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_labels_decode_stable_source_bits() {
        assert_eq!(
            host_labels((1 << 1) | (1 << 8) | (1 << 12)).collect::<Vec<_>>(),
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
            follow: false,
            projection: false,
            content: false,
            strip_switcher: false,
            capture_mirror: false,
            screenshot_markup: false,
            notification: false,
            workspace_switch: false,
            desk: false,
            strip_indicators: false,
            cursor_chat: false,
            collaborators: false,
            osd: false,
            acknowledgement: false,
            window_previews: false,
            peer_sync: false,
            directory_sheet: false,
            tab_menu: false,
            window_switcher: false,
            dictation: false,
            emoji_picker: false,
            display_confirm: false,
            feedback_picker: false,
            control_hints: false,
            fullscreen_hint: false,
        };
        assert_eq!(host_bits(&inputs(false, false)), 0);
        assert_eq!(
            host_labels(host_bits(&inputs(true, false))).collect::<Vec<_>>(),
            ["onboarding"],
        );
        // Setup owns the screen: the desktop's sources beneath it are hidden.
        assert_eq!(host_bits(&inputs(false, true)), 0);
    }
}
