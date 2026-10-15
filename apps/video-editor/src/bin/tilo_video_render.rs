//! `tilo-video-render`: renders an agent's timeline, confined. See
//! [`video_editor::render_helper`].

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(video_editor::render_helper::run())
}
