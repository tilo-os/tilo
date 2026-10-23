//! `tilo-video-render`: renders an agent's timeline, confined. See
//! [`video_editor::render_helper`].

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(video_editor::render_helper::run())
}

/// Only Unix hosts confine a helper; elsewhere nothing is rendered.
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
