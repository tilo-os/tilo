//! Video recording, one recorder per platform inside the platform layer:
//! [`tilo_media::clip`] encodes Motion-JPEG into an AVI of its own on the
//! desktop, and on the page hands frames to the browser's encoder and muxes
//! the result into `WebM`, a container every browser plays. Both name their clip
//! by what its bytes are, so the capture a person hands to Video is
//! called `VID_<time>.webm` on the page and `VID_<time>.avi` on the desktop.

use std::io;
use std::path::Path;

pub use tilo_media::clip::{CONTAINER, FPS, Frame, Recorder, probe_avi};

/// The name a recorded clip is saved under: the container its bytes are, so
/// the name the handoff carries says what the clip is before it is opened.
pub fn clip_name(stamp: &str, bytes: &[u8]) -> String {
    let (extension, _) = tilo_content::opened_as(bytes);
    let extension = match extension {
        "avi" | "webm" | "mp4" => extension,
        _ => CONTAINER,
    };
    format!("VID_{stamp}.{extension}")
}

/// Persist an encoded clip to `dir` under its container's name, creating the
/// directory on first use. Returns the written file name.
pub fn write_clip(dir: &Path, bytes: &[u8], stamp: &str) -> io::Result<String> {
    let name = clip_name(stamp, bytes);
    crate::files::write(&dir.join(&name), bytes)?;
    Ok(name)
}

/// The JPEG encoder a desktop build records frames with: the shared encoder
/// its toolkit carries, so no second one is compiled into the app.
pub fn encode_jpeg(rgb: &[u8], width: u32, height: u32, quality: u8) -> Option<Vec<u8>> {
    ui::jpeg::encode_rgb8(width, height, rgb, quality).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_is_named_by_the_container_its_bytes_are() {
        let mut avi = Vec::new();
        avi.extend_from_slice(b"RIFF\x00\x00\x00\x00AVI ");
        assert_eq!(
            clip_name("20260101_120000_000", &avi),
            "VID_20260101_120000_000.avi"
        );

        let mut webm = Vec::new();
        webm.extend_from_slice(&[0x1A, 0x45, 0xDF, 0xA3, 0x00]);
        assert_eq!(
            clip_name("20260101_120000_000", &webm),
            "VID_20260101_120000_000.webm"
        );

        // Anything else keeps the platform's own container name rather than
        // claiming a type it is not.
        assert_eq!(clip_name("x", &[]), format!("VID_x.{CONTAINER}"));
    }
}
