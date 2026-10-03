//! An engine that only decodes pictures (thumbnails, export, the editor's
//! preview) never connects to the audio server: one too busy to answer
//! cannot hold up, or fail, a picture.

mod common;

#[cfg(feature = "decode")]
#[test]
fn a_frames_only_engine_never_connects_to_the_audio_server() {
    use common::*;
    use tilo_media::{EngineConfig, MediaEngine};

    let dir = tempfile::tempdir().expect("a scratch directory");
    // An audio server that takes connections into its queue and never
    // answers: any audio sink waits on it.
    let socket = dir.path().join("busy");
    let busy = std::os::unix::net::UnixListener::bind(&socket).expect("a busy audio socket");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("PULSE_SERVER", format!("unix:{}", socket.display()));
    env.set("PIPEWIRE_REMOTE", dir.path().join("absent-pw"));

    let mut engine = MediaEngine::with_config(EngineConfig::default().frames_only());
    assert!(engine.is_real_decode(), "this needs the real pipeline");
    engine.open(fixture_source());
    let frame = decode_frame_at(&mut engine, 4_000, "frame with a busy audio server");
    assert!(frame.width > 0 && frame.height > 0);
    busy.set_nonblocking(true).expect("a non-blocking accept");
    assert!(
        matches!(busy.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a frames-only engine connected to the audio server"
    );
}
