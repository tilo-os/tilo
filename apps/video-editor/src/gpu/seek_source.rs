//! Decoded pictures from a host that may not wait: each layer's source gets a
//! [`media::edit::FrameSeeker`], asked for the frame under the playhead.
//!
//! This is the frame source of a build without the `GStreamer` sessions — a browser
//! page, where the page's media element decodes. It never blocks: a composite that
//! asks for a frame the seeker has not reached gets the last picture it showed (a
//! preview) or nothing (an export). A running playhead is played along with rather
//! than sought frame by frame. Where seekers do not open, every layer draws its
//! representative fill.

use std::collections::{HashMap, HashSet};

use media::edit::{FrameSeeker, frame_at};
use vello::wgpu;

use crate::model::{MediaId, MediaKind, Project};

use super::{FrameSource, SourceTexture, compositor::upload_rgba};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetch {
    Exact,
    Responsive,
}

/// Past this a playhead is a jump, not running.
const RUNNING_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// The furthest one request may lead the last and still count as the playhead
/// running: a shuttle at 16× moves more than a quarter second a frame.
const RUNNING_STEP_S: f64 = 0.25;
// ratchet-allow(pixel_const): a playback speed multiple, not spacing
const RUNNING_MAX_RATE: f64 = 17.0;

const IDLE: std::time::Duration = std::time::Duration::from_secs(30);

struct Held {
    seeker: FrameSeeker,
    texture: Option<SourceTexture>,
    frame_in_texture: Option<u64>,
    asked: Option<(f64, ui::time::Instant)>,
    rate: Option<f64>,
    used: ui::time::Instant,
}

#[derive(Clone)]
struct Indexed {
    path: String,
    fps: f64,
    last_frame: Option<f64>,
    still: bool,
}

pub struct SeekFrameSource {
    fetch: Fetch,
    strict: bool,
    max_side: u32,
    sources: HashMap<MediaId, Indexed>,
    /// One per source per layer slot: two layers of one source at two instants (a
    /// dissolve within one take) each hold their own frame.
    seekers: HashMap<(MediaId, usize), Held>,
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

    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

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
            // A title's picture is drawn. Seekers open copies kept by their bytes: a
            // page has no paths to open.
            .filter(|item| matches!(item.kind, MediaKind::Video | MediaKind::Image))
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
                        still: item.kind == MediaKind::Image,
                    },
                )
            })
            .collect()
    }

    pub fn set_project(&mut self, project: &Project) {
        self.sources = Self::index(project);
        self.seekers
            .retain(|(media, _), _| self.sources.contains_key(media));
        self.failed.retain(|media| self.sources.contains_key(media));
    }

    pub fn project_signature(project: &Project) -> u64 {
        super::media_signature(project)
    }

    pub fn prune(&mut self) {
        self.seekers.retain(|_, held| held.used.elapsed() < IDLE);
    }

    pub fn has_pending_frames(&self) -> bool {
        self.pending
    }

    pub fn may_serve(&self, media: MediaId) -> bool {
        self.sources.contains_key(&media) && !self.failed.contains(&media)
    }

    pub fn served_real_picture(&self) -> bool {
        self.served_real
    }

    /// With the source time asked for, so an export composites that frame again.
    pub fn missing_pictures(&self) -> &[(MediaId, f64)] {
        &self.missing
    }

    pub fn failure(&self) -> Option<MediaId> {
        self.failed.iter().next().copied()
    }

    /// `None` while its bytes are still being read back, or when it cannot open.
    fn open_seeker(&mut self, media: MediaId, indexed: &Indexed, source_time: f64) -> Option<Held> {
        let Some(source) = crate::files::kept_source(&indexed.path) else {
            if crate::files::kept_missing(&indexed.path) {
                self.failed.insert(media);
            } else {
                self.pending = true;
                self.missing.push((media, source_time));
            }
            return None;
        };
        let opened = if indexed.still {
            FrameSeeker::open_still(&source)
        } else {
            FrameSeeker::open(&source)
        };
        let Some(seeker) = opened else {
            self.failed.insert(media);
            return None;
        };
        Some(Held {
            seeker,
            texture: None,
            frame_in_texture: None,
            asked: None,
            rate: None,
            used: ui::time::now(),
        })
    }

    /// How fast the playhead runs through `held`'s source when asked for `at` now,
    /// while it runs forward (the element plays along at that speed).
    fn running_rate(&self, held: &Held, at: f64, now: ui::time::Instant) -> Option<f64> {
        let (before, when) = held.asked.filter(|_| self.fetch == Fetch::Responsive)?;
        let gap = now.saturating_duration_since(when);
        let moved = at - before;
        if gap > RUNNING_GAP
            || moved <= 0.0
            || moved > RUNNING_STEP_S.max(gap.as_secs_f64() * RUNNING_MAX_RATE)
        {
            return None;
        }
        let rate = moved / gap.as_secs_f64().max(1e-3);
        Some(held.rate.map_or(rate, |was| was * 0.7 + rate * 0.3))
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
        let mut held = match self.seekers.remove(&key) {
            Some(held) => held,
            None => self.open_seeker(media, &indexed, source_time)?,
        };
        if let Some(why) = held.seeker.take_error() {
            tracing::warn!(%media, %why, "video: a source cannot be shown");
            self.failed.insert(media);
            return None;
        }
        let at = if indexed.still {
            0.0
        } else {
            indexed
                .last_frame
                .map_or(source_time, |last| source_time.min(last))
                .max(0.0)
        };
        let now = ui::time::now();
        let step = self.running_rate(&held, at, now);
        held.rate = step;
        let running = step.is_some();
        if let Some(rate) = step {
            held.seeker.follow(at, indexed.fps, rate);
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
