//! Compositor video ingest and the HLS muxer it feeds.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tilo_projector::VideoPacket;

use super::paths::SessionPaths;
use crate::error::ProjectorError;

pub(crate) struct HlsPipeline {
    child: Child,
    input: ChildStdin,
}

impl HlsPipeline {
    pub(crate) fn start(paths: &SessionPaths, audio: bool) -> Result<Self, ProjectorError> {
        let executable =
            std::env::var("TILO_GST_LAUNCH").unwrap_or_else(|_| "gst-launch-1.0".to_owned());
        let segment = paths.root.join("segment-%05d.ts");
        let mut args = vec![
            "-q".to_owned(),
            "fdsrc".to_owned(),
            "fd=0".to_owned(),
            "do-timestamp=true".to_owned(),
            "!".to_owned(),
            "h264parse".to_owned(),
            "config-interval=-1".to_owned(),
            "!".to_owned(),
            "video/x-h264,stream-format=byte-stream,alignment=au".to_owned(),
            "!".to_owned(),
            "queue".to_owned(),
            "!".to_owned(),
            "mux.video".to_owned(),
            "hlssink2".to_owned(),
            "name=mux".to_owned(),
            "target-duration=1".to_owned(),
            "playlist-length=4".to_owned(),
            "max-files=6".to_owned(),
            format!("playlist-location={}", paths.playlist.display()),
            format!("location={}", segment.display()),
        ];
        if audio {
            args.extend([
                "pulsesrc".to_owned(),
                "device=@DEFAULT_MONITOR@".to_owned(),
                "do-timestamp=true".to_owned(),
                "!".to_owned(),
                "audioconvert".to_owned(),
                "!".to_owned(),
                "audioresample".to_owned(),
                "!".to_owned(),
                "audio/x-raw,rate=48000,channels=2".to_owned(),
                "!".to_owned(),
                "avenc_aac".to_owned(),
                "bitrate=192000".to_owned(),
                "!".to_owned(),
                "aacparse".to_owned(),
                "!".to_owned(),
                "queue".to_owned(),
                "!".to_owned(),
                "mux.audio".to_owned(),
            ]);
        }
        let mut child = Command::new(executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ProjectorError::io("cannot start HLS media pipeline"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| ProjectorError::Unavailable("HLS pipeline has no input".to_owned()))?;
        Ok(Self { child, input })
    }

    pub(crate) fn push(&mut self, packet: &VideoPacket) -> Result<(), ProjectorError> {
        self.input
            .write_all(&packet.annex_b)
            .map_err(ProjectorError::io("HLS pipeline stopped accepting video"))
    }
}

impl Drop for HlsPipeline {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn video_ingest_loop(
    paths: &SessionPaths,
    stop: &Arc<AtomicBool>,
    audio: bool,
) -> Result<(), ProjectorError> {
    let listener = UnixListener::bind(&paths.video_socket)
        .map_err(ProjectorError::io("cannot bind compositor video socket"))?;
    fs::set_permissions(&paths.video_socket, fs::Permissions::from_mode(0o600))
        .map_err(ProjectorError::io("cannot secure compositor video socket"))?;
    let mut pipeline = HlsPipeline::start(paths, audio)?;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) if stop.load(Ordering::Acquire) => {
                drop(stream);
                break;
            }
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                while !stop.load(Ordering::Acquire) {
                    match VideoPacket::read_from(&mut stream) {
                        Ok(packet) => pipeline.push(&packet)?,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ProjectorError::io("cannot accept compositor video")(error)),
        }
    }
    Ok(())
}
