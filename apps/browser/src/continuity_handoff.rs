//! Browser-owned consumer of an authenticated, identity-bound host selection.
//! Native source acquisition stays behind the platform boundary.

mod native;
pub(crate) use native::Controller;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Status {
    Absent,
    Loading,
    Ready,
    Publishing,
    Complete,
    Failed,
}

/// Whether this platform can consume the host's selected-profile projection.
pub(crate) fn supported() -> bool {
    browser_continuity::handoff::supported()
}
