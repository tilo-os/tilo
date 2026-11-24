//! The pictures a device's engine sends while a stream runs: the newest frame of each stream,
//! shown by its own address.
//!
//! A remote screen, a camera preview and a call tile show what another machine draws, frame after
//! frame. The frame is not a file and is not asked for: the engine that receives it hands it over
//! here as it arrives (`tilo-live://<slot>`, the slot being the stream's job), and a window that
//! shows that address paints the newest one. The engine wakes its window after handing a frame
//! over, so the picture moves on the frame after it arrived, never on a poll.
//!
//! The address carries raw RGBA8 pixels: every source of frames — a screen decoded from a
//! remote framebuffer, a camera, a call — ends in pixels, and nothing here decodes or reads a
//! file. A slot keeps only its newest frame; a window that falls behind skips what it missed
//! rather than growing a queue.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use vello::peniko::ImageData;

/// How a live picture is named: the scheme, then the stream's slot.
pub const LIVE_SCHEME: &str = "tilo-live://";

/// The address a component shows a stream's newest frame by.
#[must_use]
pub fn address(slot: &str) -> String {
    format!("{LIVE_SCHEME}{slot}")
}

/// Whether `url` names a live picture, and the slot it names.
#[must_use]
pub fn slot_of(url: &str) -> Option<&str> {
    url.strip_prefix(LIVE_SCHEME)
}

/// One stream's newest frame, as the engine handed it over.
struct Live {
    generation: u64,
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
}

fn frames() -> &'static Mutex<HashMap<String, Live>> {
    static FRAMES: std::sync::LazyLock<Mutex<HashMap<String, Live>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &FRAMES
}

/// Hand over the newest frame of the stream `slot`: `width`×`height` RGBA8 pixels, the `generation`
///th frame of that stream. A slot holds one frame, so a window that draws late shows the newest
/// and never a backlog.
///
/// The engine calls this from the thread it receives frames on, then wakes its window; the window
/// paints the frame on its next draw.
pub fn publish(slot: &str, generation: u64, width: u32, height: u32, pixels: Arc<Vec<u8>>) {
    let expected = usize::try_from(width)
        .ok()
        .zip(usize::try_from(height).ok())
        .and_then(|(w, h)| w.checked_mul(h))
        .and_then(|wh| wh.checked_mul(4));
    if Some(pixels.len()) != expected {
        tracing::warn!(
            slot,
            width,
            height,
            bytes = pixels.len(),
            "a live picture's pixels do not fit the size it names"
        );
        return;
    }
    frames().lock_recover().insert(
        slot.to_owned(),
        Live {
            generation,
            width,
            height,
            pixels,
        },
    );
}

/// Forget the stream `slot`: a stream that ended shows nothing rather than its last frame.
pub fn forget(slot: &str) {
    frames().lock_recover().remove(slot);
}

/// The newest frame of the stream `slot`, decoded once per frame and kept for the next draw.
#[must_use]
pub fn get(slot: &str) -> Option<ImageData> {
    thread_local! {
        static DECODED: std::cell::RefCell<HashMap<String, (u64, ImageData)>> =
            std::cell::RefCell::new(HashMap::new());
    }
    let (generation, width, height, pixels) = {
        let frames = frames().lock_recover();
        let live = frames.get(slot)?;
        (
            live.generation,
            live.width,
            live.height,
            Arc::clone(&live.pixels),
        )
    };
    let kept = DECODED.with(|decoded| {
        decoded
            .borrow()
            .get(slot)
            .filter(|(kept, _)| *kept == generation)
            .map(|(_, image)| image.clone())
    });
    if kept.is_some() {
        return kept;
    }
    // The pixels are shared with the engine that sent them: the blob takes the same allocation.
    let blob: vello::peniko::Blob<u8> = vello::peniko::Blob::new(pixels);
    let image = crate::raster::rgba8(blob, width, height);
    DECODED.with(|decoded| {
        decoded
            .borrow_mut()
            .insert(slot.to_owned(), (generation, image.clone()));
    });
    Some(image)
}

/// The newest frame of the stream `url` names, when it names a live picture.
#[must_use]
pub fn picture(url: &str) -> Option<ImageData> {
    get(slot_of(url)?)
}

/// Whether `url` names a live picture.
#[must_use]
pub fn is_live(url: &str) -> bool {
    url.starts_with(LIVE_SCHEME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(n: u8) -> Arc<Vec<u8>> {
        Arc::new(vec![n; 16])
    }

    #[test]
    fn a_stream_shows_its_newest_frame_and_forgets_when_it_ends() {
        // A slot of its own, so a test that publishes never shows in another.
        let slot = "stream-shows-newest";
        assert!(
            get(slot).is_none(),
            "a stream that sent nothing shows nothing"
        );
        publish(slot, 1, 2, 2, pixels(1));
        assert_eq!(get(slot).map(|image| image.width), Some(2));
        publish(slot, 2, 2, 2, pixels(2));
        // The newest frame, not the first: decoded afresh when its generation moves.
        let newest = get(slot).expect("the second frame");
        assert_eq!(newest.data.data()[0], 2, "the second frame's pixels");
        assert!(picture("tilo-live://stream-shows-newest").is_some());
        assert_eq!(
            slot_of("tilo-live://stream-shows-newest"),
            Some("stream-shows-newest")
        );
        assert!(!is_live("tilo-blob://abc"));
        forget(slot);
        assert!(get(slot).is_none(), "a stream that ended shows nothing");
    }

    #[test]
    fn a_window_shows_the_newest_frame_at_the_size_it_arrived() {
        use crate::widget::{Image, PictureSource, Row};

        let slot = "stream-draws-newest";
        publish(slot, 1, 4, 2, Arc::new(vec![200_u8; 4 * 2 * 4]));
        let image = Image::from_picture(
            crate::pictures::Pictures::window(),
            PictureSource { src: address(slot) },
        );
        let mut harness = crate::headless::Driver::new(
            crate::State::new(),
            Box::new(Row::new().push(Box::new(image))),
            (40, 40),
        );
        let first = harness.root_widget().children()[0].get_rect();
        assert_eq!(
            (first.width(), first.height()),
            (4.0, 2.0),
            "the window's picture is the frame it was sent, at the size it arrived"
        );
        publish(slot, 2, 2, 4, Arc::new(vec![210_u8; 2 * 4 * 4]));
        harness.recompose().expect("the next frame lays out");
        let next = harness.root_widget().children()[0].get_rect();
        assert_eq!(
            (next.width(), next.height()),
            (2.0, 4.0),
            "a frame that arrived after the last draw is what the window shows"
        );
        forget(slot);
    }

    #[test]
    fn pixels_that_do_not_fit_the_size_it_names_are_refused() {
        let slot = "stream-refuses-short-pixels";
        publish(slot, 1, 2, 2, Arc::new(vec![0; 15]));
        assert!(get(slot).is_none());
    }
}
