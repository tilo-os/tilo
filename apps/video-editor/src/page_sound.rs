//! Sound for a build without the `GStreamer` sessions — a browser page,
//! whose one thread may never wait.
//!
//! [`PagePcm`] is the mixer's [`PcmSource`] there: each source's sound is
//! decoded once by the page's own audio decoding ([`media::edit::PcmDecoder`])
//! from the copy kept by its bytes, and mixes as silence until it is in.
//! [`PageMonitor`] plays the mix while the transport runs: a frame at a time
//! it mixes the next stretch of the timeline and hands it to the output to
//! play at the time it belongs to, re-anchoring when the playhead jumps. The
//! export mixes through the same source, so the file sounds as the preview.

use std::collections::HashMap;

use media::edit::{PcmDecoder, PcmPlayer, PcmState};

use crate::audio::{CHANNELS, MixMeters, PcmSource, SAMPLE_RATE, mix_block_metered};
use crate::model::{MediaId, MediaKind, Project};

enum Entry {
    /// Its kept copy is still being read back.
    Waiting(String),
    Decoding(PcmDecoder),
    Ready(PcmDecoder),
    /// No sound this page decodes: it mixes as silence.
    Silent,
}

/// The project's sources' sound, decoded by the page.
pub struct PagePcm {
    entries: HashMap<MediaId, Entry>,
}

fn sounding(project: &Project) -> impl Iterator<Item = (MediaId, &str)> {
    project
        .media
        .iter()
        .filter(|item| item.kind == MediaKind::Audio || item.has_audio)
        .filter(|item| crate::files::is_kept(&item.path))
        .map(|item| (item.id, item.path.as_str()))
}

impl PagePcm {
    pub fn new(project: &Project) -> Self {
        let mut pcm = Self {
            entries: HashMap::new(),
        };
        pcm.set_project(project);
        pcm
    }

    /// Index `project`'s sources, keeping what is already decoded.
    pub fn set_project(&mut self, project: &Project) {
        let mut entries = HashMap::new();
        for (media, path) in sounding(project) {
            let entry = self
                .entries
                .remove(&media)
                .unwrap_or_else(|| Entry::Waiting(path.to_string()));
            entries.insert(media, entry);
        }
        self.entries = entries;
    }

    /// Move every source along: open decoders whose copies are in, take the
    /// ones that finished.
    pub fn poll(&mut self) {
        for entry in self.entries.values_mut() {
            let next = match entry {
                Entry::Waiting(path) => match crate::files::kept_source(path) {
                    Some(source) => {
                        PcmDecoder::open(&source).map_or(Entry::Silent, Entry::Decoding)
                    }
                    None if crate::files::kept_missing(path) => Entry::Silent,
                    None => continue,
                },
                Entry::Decoding(decoder) => match decoder.state() {
                    PcmState::Decoding => continue,
                    PcmState::Ready => {
                        let Entry::Decoding(decoder) = std::mem::replace(entry, Entry::Silent)
                        else {
                            continue;
                        };
                        Entry::Ready(decoder)
                    }
                    PcmState::None => Entry::Silent,
                },
                Entry::Ready(_) | Entry::Silent => continue,
            };
            *entry = next;
        }
    }

    /// Whether every source's sound is in or known to be absent, so a mix
    /// now is the whole mix.
    pub fn settled(&mut self) -> bool {
        self.poll();
        self.entries
            .values()
            .all(|entry| matches!(entry, Entry::Ready(_) | Entry::Silent))
    }
}

impl PcmSource for PagePcm {
    fn samples(&mut self, media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>> {
        let Some(Entry::Ready(decoder)) = self.entries.get(&media) else {
            return None;
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rounded and clamped to zero first"
        )]
        let from = (from_secs.max(0.0) * f64::from(SAMPLE_RATE)).round() as u64;
        let samples = decoder.samples(from, frames);
        (!samples.is_empty()).then_some(samples)
    }
}

/// Where the monitor sends what it mixed. The page's output in the editor,
/// a recording in tests.
pub trait Output {
    /// Begin playing timeline second `at`, dropping what was scheduled.
    fn start(&mut self, at: f64) -> bool;
    /// The timeline second playing now.
    fn position(&self) -> f64;
    /// Play `samples` mixed for timeline second `at`.
    fn push(&mut self, samples: &[f32], at: f64);
    fn stop(&mut self);
}

impl Output for PcmPlayer {
    fn start(&mut self, at: f64) -> bool {
        PcmPlayer::start(self, at)
    }
    fn position(&self) -> f64 {
        PcmPlayer::position(self)
    }
    fn push(&mut self, samples: &[f32], at: f64) {
        PcmPlayer::push(self, samples, at);
    }
    fn stop(&mut self) {
        PcmPlayer::stop(self);
    }
}

/// Frames mixed per block: a tenth of a second.
const BLOCK: usize = 4800;

/// How far ahead of the playhead the mix runs. Enough that a slow frame
/// leaves no gap; short enough that an edit is heard almost at once.
const AHEAD_S: f64 = 0.3;

/// A gap this big between what plays and the playhead is a jump (a scrub,
/// a cut), not clock wander.
const RESYNC_S: f64 = 0.35;

/// Plays the mixed timeline on the page while the transport runs.
pub struct PageMonitor {
    output: Option<Box<dyn Output>>,
    opened: bool,
    pcm: Option<PagePcm>,
    sources_sig: u64,
    mix_sig: u64,
    playing: bool,
    /// The timeline second the mix has reached.
    mixed_until: f64,
}

impl Default for PageMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl PageMonitor {
    /// A monitor that opens the page's output the first time it plays.
    pub fn new() -> Self {
        Self {
            output: None,
            opened: false,
            pcm: None,
            sources_sig: 0,
            mix_sig: 0,
            playing: false,
            mixed_until: 0.0,
        }
    }

    /// A monitor playing into `output`.
    #[cfg(test)]
    fn with_output(output: Box<dyn Output>) -> Self {
        Self {
            output: Some(output),
            opened: true,
            ..Self::new()
        }
    }

    /// Keep the output in step with the transport: called once a frame.
    pub fn pump(&mut self, project: &Project, playhead: f64, playing: bool, meters: &MixMeters) {
        let sources_sig = sources_signature(project);
        let pcm = self.pcm.get_or_insert_with(|| PagePcm::new(project));
        if sources_sig != self.sources_sig {
            pcm.set_project(project);
            self.sources_sig = sources_sig;
        }
        // Decoded ahead of the first play, so it is not silent.
        pcm.poll();
        if !playing {
            if self.playing {
                if let Some(output) = self.output.as_mut() {
                    output.stop();
                }
                self.playing = false;
            }
            return;
        }
        if !self.opened {
            self.opened = true;
            self.output = PcmPlayer::open().map(|player| Box::new(player) as Box<dyn Output>);
        }
        let Some(output) = self.output.as_mut() else {
            return;
        };
        // An edit is heard from the next block on, not after what was
        // already mixed ahead plays out.
        let mix_sig = crate::audio::mix_signature(project);
        let jumped = (output.position() - playhead).abs() > RESYNC_S;
        if !self.playing || jumped || mix_sig != self.mix_sig {
            if !output.start(playhead) {
                return;
            }
            self.playing = true;
            self.mixed_until = playhead;
            self.mix_sig = mix_sig;
        }
        #[expect(clippy::cast_precision_loss, reason = "a block's frame count is small")]
        let block_s = BLOCK as f64 / f64::from(SAMPLE_RATE);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a few blocks, clamped to zero first"
        )]
        let due = ((playhead + AHEAD_S - self.mixed_until) / block_s - 1e-9)
            .ceil()
            .max(0.0) as usize;
        for _ in 0..due {
            let at = self.mixed_until;
            let (samples, report) = mix_block_metered(project, at, BLOCK, pcm);
            debug_assert_eq!(samples.len(), BLOCK * CHANNELS);
            meters.record(at, &report);
            output.push(&samples, at);
            self.mixed_until = at + block_s;
        }
    }
}

/// What [`PagePcm::set_project`] would index.
fn sources_signature(project: &Project) -> u64 {
    let mut acc: u64 = 0;
    for (media, path) in sounding(project) {
        let mut hasher = fnv::FnvHasher::default();
        std::hash::Hasher::write(&mut hasher, media.as_bytes());
        std::hash::Hasher::write(&mut hasher, path.as_bytes());
        acc = acc.wrapping_add(std::hash::Hasher::finish(&hasher));
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Heard {
        starts: Vec<f64>,
        blocks: Vec<f64>,
        stops: usize,
        position: f64,
    }

    struct Recording(Rc<RefCell<Heard>>);

    impl Output for Recording {
        fn start(&mut self, at: f64) -> bool {
            let mut heard = self.0.borrow_mut();
            heard.starts.push(at);
            heard.blocks.clear();
            heard.position = at;
            true
        }
        fn position(&self) -> f64 {
            self.0.borrow().position
        }
        fn push(&mut self, samples: &[f32], at: f64) {
            assert_eq!(samples.len(), BLOCK * CHANNELS);
            self.0.borrow_mut().blocks.push(at);
        }
        fn stop(&mut self) {
            self.0.borrow_mut().stops += 1;
        }
    }

    fn monitor() -> (PageMonitor, Rc<RefCell<Heard>>) {
        let heard = Rc::new(RefCell::new(Heard::default()));
        (
            PageMonitor::with_output(Box::new(Recording(heard.clone()))),
            heard,
        )
    }

    #[test]
    fn playing_mixes_ahead_of_the_playhead_in_consecutive_blocks() {
        let project = Project::default();
        let meters = MixMeters::new();
        let (mut monitor, heard) = monitor();
        monitor.pump(&project, 2.0, true, &meters);
        let blocks = heard.borrow().blocks.clone();
        assert_eq!(heard.borrow().starts, vec![2.0]);
        assert_eq!(blocks.len(), 3, "three tenths of a second ahead");
        for (i, at) in blocks.iter().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "a handful of blocks")]
            let want = 2.0 + i as f64 * 0.1;
            assert!((at - want).abs() < 1e-9, "block {i} at {at}");
        }
        // The playhead moves on a frame: only what it now needs is mixed.
        heard.borrow_mut().position = 2.05;
        monitor.pump(&project, 2.05, true, &meters);
        assert_eq!(heard.borrow().blocks.len(), 4);
        assert_eq!(heard.borrow().starts.len(), 1, "wander is not a jump");
    }

    #[test]
    fn a_jump_restarts_from_the_playhead_and_a_pause_silences() {
        let project = Project::default();
        let meters = MixMeters::new();
        let (mut monitor, heard) = monitor();
        monitor.pump(&project, 1.0, true, &meters);
        monitor.pump(&project, 7.0, true, &meters);
        assert_eq!(heard.borrow().starts, vec![1.0, 7.0]);
        assert!((heard.borrow().blocks[0] - 7.0).abs() < 1e-9);
        monitor.pump(&project, 7.0, false, &meters);
        monitor.pump(&project, 7.0, false, &meters);
        assert_eq!(heard.borrow().stops, 1);
        monitor.pump(&project, 7.0, true, &meters);
        assert_eq!(heard.borrow().starts.len(), 3, "playing again starts again");
    }

    #[test]
    fn a_source_without_a_kept_copy_mixes_as_silence() {
        let mut pcm = PagePcm::new(&Project::default());
        assert!(pcm.settled(), "nothing to decode");
        assert_eq!(pcm.samples(MediaId::nil(), 0.0, 10), None);
    }
}
