//! `tilo-video-render`: renders an agent's timeline, confined. See
//! [`tilo_video_render`].

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(tilo_video_render::run())
}

/// Only Unix hosts confine a helper; elsewhere nothing is rendered.
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
