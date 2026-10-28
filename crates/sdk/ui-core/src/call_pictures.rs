//! A call's pictures, decoded for the tiles that show them.
//!
//! The app says which pictures it shows (`native_media::Command::Show`) and
//! collects them as they arrive (`native_media::Command::Pictures`), coded
//! (tilo-video/1). [`CallPictures`] decodes them off the UI thread, hardware
//! first (`media::RealtimeDecoder`), one decoder per stream, and keeps each
//! person's newest camera and screen picture for their tile. A stream whose
//! pictures had to be dropped starts over at its next keyframe, so a tile
//! never shows a picture decoded against a reference it lacks.
//!
//! A build without real-time media decodes nothing: tiles keep showing the
//! person, never a made-up picture. In a browser the decoder is the page's
//! (WebCodecs), which finishes pictures on its own schedule: [`CallPictures::poll`]
//! collects them. A page may also decode pictures itself and hand them over
//! with [`CallPictures::insert`].

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, TryLockError};

use runtime_collab::native_media::{Picture, PictureSource};
use tilo_lock::MutexRecover as _;
use vello::peniko::ImageData;

use crate::wake::WakeHandle;

/// Coded pictures waiting for the decoder at most: past them a stream starts
/// over at its next keyframe rather than fall further behind.
const QUEUED: usize = 32;

#[derive(Default)]
struct Decoded {
    /// The newest picture of each person's camera and screen.
    shown: HashMap<(String, PictureSource), ImageData>,
    changed: bool,
    /// Pictures handed to the decoder and not yet decoded.
    pending: usize,
}

/// Decodes a call's pictures; see the [module docs](self).
pub struct CallPictures {
    decoded: Arc<Mutex<Decoded>>,
    /// Streams that dropped a picture: nothing of them until a keyframe.
    behind: BTreeSet<(String, PictureSource)>,
    work: Arc<Work>,
    wake: WakeHandle,
}

/// The pictures waiting for the decoder and its decoders: decoded by a
/// background job that runs while there are any, and ends when there are
/// none, so a quiet call holds no thread. In a page the job runs on the
/// page's one thread, after the frame that queued the pictures.
#[derive(Default)]
struct Work {
    queue: Mutex<std::collections::VecDeque<Picture>>,
    running: std::sync::atomic::AtomicBool,
    decoders: Mutex<Decoders>,
}

/// One decoder per person's source, for the stream it last showed: a
/// person's newer stream (another device, back after leaving) replaces it.
#[derive(Default)]
struct Decoders {
    by_person: HashMap<(String, PictureSource), (String, media::RealtimeDecoder)>,
    /// Streams whose decoder could not be built: not tried again.
    refused: BTreeSet<(String, PictureSource)>,
}

impl Default for CallPictures {
    fn default() -> Self {
        Self::new()
    }
}

impl CallPictures {
    /// A decoder that wakes the component being built (or handling an event)
    /// when a picture is ready.
    pub fn new() -> Self {
        Self {
            decoded: Arc::new(Mutex::new(Decoded::default())),
            behind: BTreeSet::new(),
            work: Arc::new(Work::default()),
            wake: WakeHandle::new(),
        }
    }

    /// Hand over the pictures of one `Pictures` answer, oldest first.
    pub fn receive(&mut self, pictures: Vec<Picture>) {
        for picture in pictures {
            let Some(unit) = picture.unit() else {
                continue;
            };
            let key = (picture.stream.clone(), picture.source);
            if unit.keyframe {
                self.behind.remove(&key);
            } else if self.behind.contains(&key) {
                continue;
            }
            let mut queue = self.work.queue.lock_recover();
            if queue.len() >= QUEUED {
                drop(queue);
                self.behind.insert(key);
                continue;
            }
            // Counted before it is queued: a running job may decode it at once.
            self.decoded.lock_recover().pending += 1;
            queue.push_back(picture);
            drop(queue);
        }
        self.start_decoding();
    }

    /// Collect pictures a decoder finished after the unit that completed
    /// them was handed over (a page's decoder works on its own schedule; a
    /// quiet stream's last picture). Skipped while the decoding job holds
    /// the decoders: the UI thread never waits for it, and the job hands
    /// over what it decodes itself.
    pub fn poll(&mut self) {
        let mut decoders = match self.work.decoders.try_lock() {
            Ok(decoders) => decoders,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let finished: Vec<_> = decoders
            .by_person
            .iter_mut()
            .filter_map(|((person, source), (_, decoder))| {
                let picture = decoder.take_late_picture().ok()??;
                Some((person.clone(), *source, image_of(picture)?))
            })
            .collect();
        drop(decoders);
        for (person, source, image) in finished {
            self.insert(&person, source, image);
        }
    }

    /// Decode what is queued, on a background job, unless one runs.
    fn start_decoding(&self) {
        use std::sync::atomic::Ordering;
        if self.work.queue.lock_recover().is_empty()
            || self.work.running.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let work = Arc::clone(&self.work);
        let decoded = Arc::clone(&self.decoded);
        let wake = self.wake.clone();
        if crate::background::Background::current()
            .spawn("call-pictures", move || decode(&work, &decoded, &wake))
            .is_err()
        {
            self.work.running.store(false, Ordering::Release);
            let dropped = std::mem::take(&mut *self.work.queue.lock_recover()).len();
            let mut decoded = self.decoded.lock_recover();
            decoded.pending = decoded.pending.saturating_sub(dropped);
            tracing::warn!("call pictures have nothing to decode them on");
        }
    }

    /// Pictures handed over and still being decoded: a screenshot waits for
    /// them.
    pub fn pending(&self) -> usize {
        self.decoded.lock_recover().pending
    }

    /// Code a short run of a moving test picture (colour bars with a block
    /// sweeping across, shaded by `variant`) exactly as a call codes a
    /// camera or screen, and decode it for `person`'s `source`: screenshots
    /// and tests show pictures that went through the real encoder and
    /// decoder. False when this build or machine has no encoder (a page
    /// has none).
    pub fn feed_test_pattern(
        &mut self,
        person: &str,
        source: PictureSource,
        (width, height): (u16, u16),
        variant: u8,
    ) -> bool {
        use runtime_collab::native_media::{PictureCodec, PictureLayer, PictureUnit};
        const FRAMES: u8 = 8;
        let mut config = media::EncoderConfig::new(
            media::StreamCodec::H264,
            u32::from(width),
            u32::from(height),
            30,
        );
        config.bitrate_bps = 1_500_000;
        let Ok(mut encoder) = media::RealtimeEncoder::new(&config) else {
            return false;
        };
        let stream = format!("test/{person}/{variant}");
        let mut pictures = Vec::new();
        for frame in 0..FRAMES {
            let rgba = test_pattern(width, height, variant, frame);
            let pts = std::time::Duration::from_millis(u64::from(frame).saturating_mul(33));
            let Ok(units) = encoder.encode(media::InputFrame::Packed(&rgba), pts) else {
                return false;
            };
            for unit in units {
                let object = PictureUnit {
                    keyframe: unit.keyframe,
                    codec: PictureCodec::H264,
                    width,
                    height,
                    payload: &unit.data,
                }
                .to_object(
                    PictureLayer::Mid,
                    u64::from(frame),
                    u64::try_from(pts.as_micros()).unwrap_or(u64::MAX),
                );
                pictures.push(Picture {
                    person: person.to_owned(),
                    stream: stream.clone(),
                    source,
                    object,
                });
            }
        }
        let fed = !pictures.is_empty();
        self.receive(pictures);
        fed
    }

    /// Put a picture decoded elsewhere in place (a browser's page decodes
    /// its own).
    pub fn insert(&mut self, person: &str, source: PictureSource, image: ImageData) {
        let mut decoded = self.decoded.lock_recover();
        decoded.shown.insert((person.to_owned(), source), image);
        decoded.changed = true;
    }

    /// The newest picture of `person`'s camera or screen.
    pub fn picture(&self, person: &str, source: PictureSource) -> Option<ImageData> {
        self.decoded
            .lock_recover()
            .shown
            .get(&(person.to_owned(), source))
            .cloned()
    }

    pub fn pictures(&self) -> Vec<(String, PictureSource, ImageData)> {
        self.decoded
            .lock_recover()
            .shown
            .iter()
            .map(|((person, source), image)| (person.clone(), *source, image.clone()))
            .collect()
    }

    /// Whether a picture arrived since this was last asked.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.decoded.lock_recover().changed)
    }

    /// Keep only the pictures `keep` still shows (a camera turned off, a
    /// person who left): their tiles go back to showing the person.
    pub fn retain(&mut self, keep: impl Fn(&str, PictureSource) -> bool) {
        let mut decoded = self.decoded.lock_recover();
        let before = decoded.shown.len();
        decoded
            .shown
            .retain(|(person, source), _| keep(person, *source));
        if decoded.shown.len() != before {
            decoded.changed = true;
        }
    }

    /// Forget everything: the call ended.
    pub fn clear(&mut self) {
        self.retain(|_, _| false);
        self.behind.clear();
        let dropped = std::mem::take(&mut *self.work.queue.lock_recover()).len();
        let mut decoded = self.decoded.lock_recover();
        decoded.pending = decoded.pending.saturating_sub(dropped);
        drop(decoded);
        *self.work.decoders.lock_recover() = Decoders::default();
    }
}

/// The decoding job: every queued picture through its person's decoder,
/// the newest picture of each person's source kept, the component woken for
/// each; it ends once nothing is queued.
fn decode(work: &Work, decoded: &Mutex<Decoded>, wake: &WakeHandle) {
    use std::sync::atomic::Ordering;
    loop {
        let next = work.queue.lock_recover().pop_front();
        let Some(picture) = next else {
            work.running.store(false, Ordering::Release);
            // A picture queued after the queue read empty, before this job
            // said it ended, is taken here rather than left waiting.
            if work.queue.lock_recover().is_empty() || work.running.swap(true, Ordering::AcqRel) {
                return;
            }
            continue;
        };
        let decoded_one = decode_one(&picture, &mut work.decoders.lock_recover());
        let mut shown = decoded.lock_recover();
        shown.pending = shown.pending.saturating_sub(1);
        if let Some(image) = decoded_one {
            shown.shown.insert((picture.person, picture.source), image);
            shown.changed = true;
        }
        drop(shown);
        wake.wake();
    }
}

/// Decode `picture` with its person's decoder (made for its stream when
/// needed): the picture it completes, if any.
fn decode_one(picture: &Picture, decoders: &mut Decoders) -> Option<ImageData> {
    let Decoders {
        by_person: decoders,
        refused,
    } = decoders;
    use runtime_collab::native_media::PictureCodec;
    let unit = picture.unit()?;
    let codec = match unit.codec {
        PictureCodec::H264 => media::StreamCodec::H264,
        PictureCodec::Vp9 => media::StreamCodec::Vp9,
        PictureCodec::Av1 => media::StreamCodec::Av1,
        PictureCodec::Vp8 => return None,
    };
    if refused.contains(&(picture.stream.clone(), picture.source)) {
        return None;
    }
    let key = (picture.person.clone(), picture.source);
    if decoders
        .get(&key)
        .is_some_and(|(stream, _)| *stream != picture.stream)
    {
        // A new stream starts at a keyframe; until then the old one shows.
        if !unit.keyframe {
            return None;
        }
        decoders.remove(&key);
    }
    let decoder = match decoders.entry(key) {
        std::collections::hash_map::Entry::Occupied(entry) => &mut entry.into_mut().1,
        std::collections::hash_map::Entry::Vacant(entry) => {
            let mut config = media::DecoderConfig::new(codec);
            config.output = media::DecodeOutput::Rgba;
            config.display_size = Some((u32::from(unit.width), u32::from(unit.height)));
            match media::RealtimeDecoder::new(config) {
                Ok(decoder) => &mut entry.insert((picture.stream.clone(), decoder)).1,
                Err(error) => {
                    tracing::warn!(%error, "a call picture stream has no decoder");
                    refused.insert((picture.stream.clone(), picture.source));
                    return None;
                }
            }
        }
    };
    image_of(decoder.decode(unit.payload, Some(unit.keyframe)).ok()??)
}

/// A decoded picture as a tile paints it.
fn image_of(decoded: media::DecodedPicture) -> Option<ImageData> {
    let rgba = decoded.frame.to_rgba()?;
    Some(ImageData {
        data: vello::peniko::Blob::new(rgba),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::Alpha,
        width: decoded.frame.width,
        height: decoded.frame.height,
    })
}

/// One frame of the moving test picture: colour bars, shaded by `variant`,
/// with a light block sweeping across by `frame`.
fn test_pattern(width: u16, height: u16, variant: u8, frame: u8) -> Vec<u8> {
    const BARS: [[u8; 3]; 7] = [
        [192, 192, 192],
        [192, 192, 0],
        [0, 192, 192],
        [0, 192, 0],
        [192, 0, 192],
        [192, 0, 0],
        [0, 0, 192],
    ];
    let (width, height) = (usize::from(width), usize::from(height));
    let bar_width = width.checked_div(BARS.len()).unwrap_or(0).max(1);
    let block = (height / 4).max(1);
    let block_x = usize::from(frame)
        .saturating_mul(width / 16)
        .checked_rem(width)
        .unwrap_or(0);
    let shade = u16::from(variant % 4).saturating_mul(24);
    let mut rgba = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4));
    for y in 0..height {
        for x in 0..width {
            let bar = BARS
                .get(
                    x.checked_div(bar_width)
                        .unwrap_or(0)
                        .min(BARS.len().saturating_sub(1)),
                )
                .copied()
                .unwrap_or([0, 0, 0]);
            let in_block = y >= height / 2
                && y < (height / 2).saturating_add(block)
                && x >= block_x
                && x < block_x.saturating_add(block);
            let [r, g, b] = if in_block {
                [240, 240, 240]
            } else {
                bar.map(|channel| {
                    u8::try_from(u16::from(channel).saturating_sub(shade)).unwrap_or(0)
                })
            };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    rgba
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn the_test_picture_is_colour_bars_with_a_moving_block() {
        let first = test_pattern(70, 40, 0, 0);
        let later = test_pattern(70, 40, 0, 4);
        assert_eq!(first.len(), 70 * 40 * 4);
        // The top-left pixel is the first bar, light grey.
        assert_eq!(&first[..4], &[192, 192, 192, 255]);
        // The last bar is blue.
        let last = 69 * 4;
        assert_eq!(&first[last..last + 4], &[0, 0, 192, 255]);
        assert_ne!(first, later, "the block moves");
        assert_ne!(
            test_pattern(70, 40, 1, 0),
            first,
            "each variant has its own shade"
        );
    }

    #[test]
    fn a_dropped_picture_holds_its_stream_back_until_a_keyframe() {
        use runtime_collab::native_media::{PictureCodec, PictureLayer, PictureUnit};
        let object = |keyframe| {
            PictureUnit {
                keyframe,
                codec: PictureCodec::H264,
                width: 2,
                height: 2,
                payload: &[0, 0, 0, 1],
            }
            .to_object(PictureLayer::Low, 0, 0)
        };
        let mut pictures = CallPictures::new();
        pictures.behind.insert(("s".into(), PictureSource::Camera));
        // A picture that is not a keyframe is not even handed over.
        pictures.receive(vec![Picture {
            person: "ab".repeat(32),
            stream: "s".into(),
            source: PictureSource::Camera,
            object: object(false),
        }]);
        assert_eq!(pictures.pending(), 0, "nothing reached a decoder");
        pictures.receive(vec![Picture {
            person: "ab".repeat(32),
            stream: "s".into(),
            source: PictureSource::Camera,
            object: object(true),
        }]);
        assert!(
            pictures.behind.is_empty(),
            "a keyframe catches the stream up"
        );
    }

    #[test]
    fn a_picture_decoded_elsewhere_shows_until_its_person_stops_showing_it() {
        let mut pictures = CallPictures::new();
        let image = ImageData {
            data: vello::peniko::Blob::new(Arc::new(vec![0; 4])),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        };
        pictures.insert("ana", PictureSource::Camera, image);
        assert!(pictures.take_changed());
        assert!(pictures.picture("ana", PictureSource::Camera).is_some());
        assert!(!pictures.take_changed());
        pictures.retain(|person, _| person != "ana");
        assert!(pictures.take_changed());
        assert!(pictures.picture("ana", PictureSource::Camera).is_none());
    }
}
