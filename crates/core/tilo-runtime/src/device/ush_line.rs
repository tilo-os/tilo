//! The daemon's one connection to this login's shell runtime ([`ush::control_line`]): every
//! shell an app's process opens and every question about the workspace's environments is asked
//! on it.

use std::sync::OnceLock;

use ush::control_line::ControlLine;

/// This login's shell runtime, as the daemon reaches it.
pub(crate) fn here() -> &'static ControlLine {
    static HERE: OnceLock<ControlLine> = OnceLock::new();
    HERE.get_or_init(|| {
        ControlLine::at(launcher::services::socket_for(
            launcher::services::USH_SERVICE,
        ))
    })
}
