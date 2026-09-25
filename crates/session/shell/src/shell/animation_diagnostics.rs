//! Allocation-free decomposition of compositor animation demand.

pub(crate) const HOST_SOURCE_NAMES: [&str; 15] = [
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
];

#[allow(
    clippy::struct_excessive_bools,
    reason = "one named flag per independent animation source keeps the bit layout auditable"
)]
pub(crate) struct HostDemandInputs {
    pub onboarding: bool,
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
}

pub(crate) fn host_bits(inputs: &HostDemandInputs) -> u32 {
    if inputs.onboarding {
        return 1 | (u32::from(inputs.capture_studio) << 1);
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
            host_labels((1 << 1) | (1 << 10) | (1 << 14)).collect::<Vec<_>>(),
            ["quick-settings", "content", "notification"],
        );
        assert!(host_labels(0).next().is_none());
    }
}
