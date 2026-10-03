//! Incremental re-simulation of a layer as its marks change.
//!
//! A painting changes mostly at its end: a painter adds a passage, or edits
//! something recent. [`LayerPainter`] remembers which marks it has applied
//! and keeps a few checkpoints, so appending a mark costs one mark and
//! editing a recent one replays only from the checkpoint before it.

use std::sync::Arc;

use crate::engine::{Canvas, LayerSim, Mark, MediumEngine, Settings};

/// How many marks apart checkpoints are taken.
const CHECKPOINT_EVERY: usize = 24;
/// How many checkpoints a layer keeps; each is a full copy of its fields.
const CHECKPOINTS_KEPT: usize = 4;

/// One mark as a layer paints it: its content fingerprint, the seed of its
/// randomness, and the mark in cell coordinates.
#[derive(Clone, Debug)]
pub struct Painted {
    pub fingerprint: [u8; 32],
    pub seed: u64,
    pub mark: Mark,
}

impl Painted {
    /// A mark addressed by its stable `id`. The seed follows the id alone,
    /// so recolouring or nudging a stroke keeps its character; the
    /// fingerprint follows the content too, so any edit re-simulates it.
    pub fn new(id: &str, mark: Mark) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(id.as_bytes());
        hasher.update(&[0]);
        hasher.update(serde_json::to_string(&mark).unwrap_or_default().as_bytes());
        Self {
            fingerprint: *hasher.finalize().as_bytes(),
            seed: seed_of(id),
            mark,
        }
    }
}

/// The seed of the randomness of the mark addressed by `id`.
pub fn seed_of(id: &str) -> u64 {
    let hash = blake3::hash(id.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(hash.as_bytes().get(..8).unwrap_or(&[0; 8]));
    u64::from_le_bytes(bytes)
}

/// One layer's simulation, kept in step with its marks.
pub struct LayerPainter {
    engine: Arc<dyn MediumEngine>,
    canvas: Canvas,
    settings: Settings,
    applied: Vec<[u8; 32]>,
    sim: Box<dyn LayerSim>,
    checkpoints: Vec<(usize, Box<dyn LayerSim>)>,
    pixels: Option<(usize, Vec<u8>)>,
}

impl LayerPainter {
    pub fn new(engine: Arc<dyn MediumEngine>, canvas: Canvas, settings: Settings) -> Self {
        let sim = engine.new_layer(&canvas, settings.clone());
        Self {
            engine,
            canvas,
            settings,
            applied: Vec::new(),
            sim,
            checkpoints: Vec::new(),
            pixels: None,
        }
    }

    /// Whether this painter simulates `engine` over `canvas` with `settings`;
    /// otherwise the caller should build a new one.
    pub fn matches(&self, engine: &str, canvas: &Canvas, settings: &Settings) -> bool {
        self.engine.id() == engine
            && self.canvas.width == canvas.width
            && self.canvas.height == canvas.height
            && (self.canvas.scale - canvas.scale).abs() < f32::EPSILON
            && Arc::ptr_eq(&self.canvas.ground, &canvas.ground)
            && self.settings.resolved() == settings.resolved()
    }

    /// How many marks the last [`Self::paint`] had to simulate.
    pub fn applied(&self) -> usize {
        self.applied.len()
    }

    /// The layer with exactly `marks` painted, as straight-alpha sRGB RGBA8.
    pub fn paint(&mut self, marks: &[Painted]) -> &[u8] {
        let common = self
            .applied
            .iter()
            .zip(marks)
            .take_while(|(applied, mark)| **applied == mark.fingerprint)
            .count();
        if common < self.applied.len() {
            self.rewind(common);
        }
        let start = self.applied.len();
        for mark in marks.get(start..).unwrap_or_default() {
            for (piece, seed) in mark.mark.pieces(mark.seed) {
                self.sim.apply(&piece, seed);
            }
            self.applied.push(mark.fingerprint);
            let count = self.applied.len();
            if count.is_multiple_of(CHECKPOINT_EVERY) {
                self.checkpoints.push((count, self.sim.clone_box()));
                if self.checkpoints.len() > CHECKPOINTS_KEPT {
                    self.checkpoints.remove(0);
                }
            }
        }
        let count = self.applied.len();
        if self.pixels.as_ref().is_none_or(|(at, _)| *at != count) || start != count {
            self.pixels = Some((count, self.sim.composite()));
        }
        self.pixels
            .as_ref()
            .map_or(&[], |(_, pixels)| pixels.as_slice())
    }

    /// The layer with `marks` painted and `wet` laid over them, without
    /// keeping `wet`: what a stroke still being painted looks like. The
    /// painted marks stay simulated, so the next preview costs only `wet`.
    pub fn preview(&mut self, marks: &[Painted], wet: &[Painted]) -> Vec<u8> {
        self.paint(marks);
        let mut sim = self.sim.clone_box();
        for mark in wet {
            sim.apply(&mark.mark, mark.seed);
        }
        sim.composite()
    }

    /// Go back to the state after the first `keep` marks.
    fn rewind(&mut self, keep: usize) {
        self.checkpoints.retain(|(count, _)| *count <= keep);
        if let Some((count, sim)) = self.checkpoints.last() {
            self.sim = sim.clone_box();
            self.applied.truncate(*count);
        } else {
            self.sim = self.engine.new_layer(&self.canvas, self.settings.clone());
            self.applied.clear();
        }
        self.pixels = None;
    }
}
