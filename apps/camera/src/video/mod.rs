//! Video recording, one recorder per platform: the desktop encodes
//! Motion-JPEG into an AVI of its own ([`avi`]), and the page encodes VP8
//! through the browser and muxes `WebM` (the `web` module), a container every browser
//! plays. Both name their clip by what its bytes are, so the capture a
//! person hands to Video Player is called `VID_<time>.webm` on the page and
//! `VID_<time>.avi` on the desktop.

use std::io;
use std::path::Path;

/// Frames per second written into the container timing. A clip plays back
/// whatever rate frames actually arrive; 30 is the standard assumption used
/// for the containers' rate and duration maths.
pub const FPS: u32 = 30;

#[cfg(not(target_arch = "wasm32"))]
mod avi;

#[cfg(not(target_arch = "wasm32"))]
pub use avi::Recorder;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub use avi::probe_avi;

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
pub use web::Recorder;

/// The name a recorded clip is saved under: the container its bytes are, so
/// the name the handoff carries says what the clip is before it is opened.
pub fn clip_name(stamp: &str, bytes: &[u8]) -> String {
    let (extension, _) = tilo_content::opened_as(bytes);
    let extension = match extension {
        "avi" | "webm" | "mp4" => extension,
        _ => {
            if cfg!(target_arch = "wasm32") {
                "webm"
            } else {
                "avi"
            }
        }
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
        assert!(
            clip_name("x", &[]).starts_with(if cfg!(target_arch = "wasm32") {
                "VID_x.webm"
            } else {
                "VID_x.avi"
            })
        );
    }
}
