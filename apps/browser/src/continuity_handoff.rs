//! Browser's consumer of an authenticated, identity-bound host selection.

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

pub(crate) fn supported() -> bool {
    browser_continuity::handoff::supported()
}
