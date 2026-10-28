//! Decoded pictures for the compositor from a host that may not wait: each
//! layer's source gets a [`media::edit::FrameSeeker`], asked for the frame
//! under the playhead and read once it holds that frame.
//!
//! This is the frame source of a build without the `GStreamer` sessions — a
//! browser page, where the page's media element decodes. It never blocks: a
//! composite that asks for a frame the seeker has not reached yet gets the
//! last picture it showed (a preview) or nothing (an export, which asks again
//! next time and writes no stand-in frame in between).
//!
//! On a host where seekers do not open, every layer draws its representative
//! fill, exactly as with [`super::NoSources`].

use std::collections::{HashMap, HashSet};

use media::edit::{FrameSeeker, frame_at};
use vello::wgpu;

use crate::model::{MediaId, MediaKind, Project};

use super::{FrameSource, SourceTexture, compositor::upload_rgba};

/// How a caller wants frames fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch {
    /// Only the frame at the requested instant will do. What an export
    /// wants: every frame must be the right one.
    Exact,
    /// Show the last picture while the seeker moves, and let it play along
    /// with a running playhead. What a preview wants.
    Responsive,
}

/// How long a playhead may go between two requests and still count as
/// running, rather than as a jump that wants an exact seek.
const RUNNING_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// The furthest one request may lead the last and still count as the
/// playhead running forward.
const RUNNING_STEP_S: f64 = 0.25;

/// A seeker left unused this long is closed.
const IDLE: std::time::Duration = std::time::Duration::from_secs(30);

struct Held {
    seeker: FrameSeeker,
    /// The texture its last read picture is in, and which frame that was.
    texture: Option<SourceTexture>,
    frame_in_texture: Option<u64>,
    /// The source time last asked for, and when.
    asked: Option<(f64, ui::time::Instant)>,
    used: ui::time::Instant,
}

/// What one decodable source is: where its bytes are, how fast it runs, and
/// the time of its last frame.
#[derive(Clone)]
struct Indexed {
    path: String,
    fps: f64,
    last_frame: Option<f64>,
}

/// A [`FrameSource`] over [`FrameSeeker`]s.
pub struct SeekFrameSource {
    fetch: Fetch,
    strict: bool,
    /// The largest picture side read back, in pixels.
    max_side: u32,
    sources: HashMap<MediaId, Indexed>,
    /// One seeker per source per layer slot: two layers of one source at two
    /// instants (a dissolve within one take) each hold their own frame.
    seekers: HashMap<(MediaId, usize), Held>,
    /// Requests per source in the composite being resolved.
    uses: HashMap<MediaId, usize>,
    failed: HashSet<MediaId>,
    served_real: bool,
    pending: bool,
    missing: Vec<(MediaId, f64)>,
}

impl SeekFrameSource {
    pub fn new(project: &Project, fetch: Fetch) -> Self {
        Self {
            fetch,
            strict: false,
            max_side: 960,
            sources: Self::index(project),
            seekers: HashMap::new(),
            uses: HashMap::new(),
            failed: HashSet::new(),
            served_real: false,
            pending: false,
            missing: Vec::new(),
        }
    }

    /// For an export: no stand-in pictures, every frame the exact one.
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// Read pictures back at up to `side` pixels on their longer side.
    #[must_use]
    pub fn with_max_side(mut self, side: u32) -> Self {
        self.max_side = side.max(1);
        self
    }

    fn index(project: &Project) -> HashMap<MediaId, Indexed> {
        if !FrameSeeker::available() {
            return HashMap::new();
        }
        project
            .media
            .iter()
            // A title's picture is drawn, and a still is not something a
            // media element plays. Seekers open copies kept by their bytes:
            // a page has no paths to open.
            .filter(|item| item.kind == MediaKind::Video)
            .filter(|item| crate::files::is_kept(&item.path))
            .map(|item| {
                let fps = if item.fps > 1.0 { item.fps } else { 30.0 };
                let last_frame =
                    (item.duration > 0.0).then(|| (item.duration - 1.0 / fps).max(0.0));
                (
                    item.id,
                    Indexed {
                        path: item.path.clone(),
                        fps,
                        last_frame,
                    },
                )
            })
            .collect()
    }

    /// Re-index against `project`, keeping the seekers of media still in it.
    pub fn set_project(&mut self, project: &Project) {
        self.sources = Self::index(project);
        self.seekers
            .retain(|(media, _), _| self.sources.contains_key(media));
        self.failed.retain(|media| self.sources.contains_key(media));
    }

    /// A cheap signature of what [`Self::set_project`] would index.
    pub fn project_signature(project: &Project) -> u64 {
        super::media_signature(project)
    }

    /// Close the seekers nothing asked for lately.
    pub fn prune(&mut self) {
        self.seekers.retain(|_, held| held.used.elapsed() < IDLE);
    }

    /// Whether the last composite asked for a frame not shown yet, so the
    /// caller should composite again shortly.
    pub fn has_pending_frames(&self) -> bool {
        self.pending
    }

    /// Whether `media` may still get a decoded picture.
    pub fn may_serve(&self, media: MediaId) -> bool {
        self.sources.contains_key(&media) && !self.failed.contains(&media)
    }

    /// Whether the last composite put a decoded picture on any layer.
    pub fn served_real_picture(&self) -> bool {
        self.served_real
    }

    /// Media the last composite got no exact picture of, with the source
    /// time asked for (an export composites that frame again).
    pub fn missing_pictures(&self) -> &[(MediaId, f64)] {
        &self.missing
    }

    /// Why a source cannot be decoded, when one could not.
    pub fn failure(&self) -> Option<MediaId> {
        self.failed.iter().next().copied()
    }

    /// Put `held`'s current picture in its texture, reusing the texture
    /// while the size holds.
    fn read_picture(&self, device: &wgpu::Device, queue: &wgpu::Queue, held: &mut Held) -> bool {
        let Some(picture) = held.seeker.picture(self.max_side, self.max_side) else {
            return false;
        };
        let same_size = held
            .texture
            .as_ref()
            .is_some_and(|t| t.width == picture.width && t.height == picture.height);
        if same_size && let Some(texture) = &held.texture {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &picture.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(picture.width * 4),
                    rows_per_image: Some(picture.height),
                },
                wgpu::Extent3d {
                    width: picture.width,
                    height: picture.height,
                    depth_or_array_layers: 1,
                },
            );
            return true;
        }
        if let Some(old) = held.texture.take() {
            old.texture.destroy();
        }
        held.texture = upload_rgba(device, queue, picture.width, picture.height, &picture.rgba);
        held.texture.is_some()
    }
}

impl FrameSource for SeekFrameSource {
    fn begin_frame(&mut self) {
        self.uses.clear();
        self.served_real = false;
        self.pending = false;
        self.missing.clear();
    }

    fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        media: MediaId,
        source_time: f64,
    ) -> Option<SourceTexture> {
        if self.failed.contains(&media) {
            return None;
        }
        let indexed = self.sources.get(&media)?.clone();
        let slot = {
            let uses = self.uses.entry(media).or_insert(0);
            *uses += 1;
            *uses - 1
        };
        let key = (media, slot);
        if !self.seekers.contains_key(&key) {
            let Some(source) = crate::files::kept_source(&indexed.path) else {
                if crate::files::kept_missing(&indexed.path) {
                    self.failed.insert(media);
                    return None;
                }
                // Its bytes are still being read from where they were kept.
                self.pending = true;
                self.missing.push((media, source_time));
                return None;
            };
            let Some(seeker) = FrameSeeker::open(&source) else {
                self.failed.insert(media);
                return None;
            };
            let now = ui::time::now();
            self.seekers.insert(
                key,
                Held {
                    seeker,
                    texture: None,
                    frame_in_texture: None,
                    asked: None,
                    used: now,
                },
            );
        }
        let mut held = self.seekers.remove(&key)?;
        if let Some(why) = held.seeker.take_error() {
            tracing::warn!(%media, %why, "video: a source cannot be shown");
            self.failed.insert(media);
            return None;
        }
        let at = indexed
            .last_frame
            .map_or(source_time, |last| source_time.min(last))
            .max(0.0);
        let now = ui::time::now();
        let running = self.fetch == Fetch::Responsive
            && held.asked.is_some_and(|(before, when)| {
                at > before
                    && at - before <= RUNNING_STEP_S
                    && now.saturating_duration_since(when) <= RUNNING_GAP
            });
        if running {
            held.seeker.follow(at, indexed.fps);
        } else {
            held.seeker.seek(at, indexed.fps);
        }
        held.asked = Some((at, now));
        held.used = now;
        let target = frame_at(at, indexed.fps);
        let shown = held.seeker.shown_frame();
        let current = shown.filter(|&frame| running || frame == target);
        let served = match current {
            Some(frame) => {
                if held.frame_in_texture != Some(frame) {
                    if self.read_picture(device, queue, &mut held) {
                        held.frame_in_texture = Some(frame);
                    } else {
                        held.frame_in_texture = None;
                    }
                }
                held.frame_in_texture
                    .is_some()
                    .then(|| held.texture.clone())
                    .flatten()
            }
            None => None,
        };
        let served = if served.is_some() {
            served
        } else {
            self.pending = true;
            if self.strict {
                self.missing.push((media, source_time));
                None
            } else {
                // The last picture this layer showed, while the next lands.
                held.texture.clone()
            }
        };
        self.seekers.insert(key, held);
        if served.is_some() {
            self.served_real = true;
        }
        served
    }
}
