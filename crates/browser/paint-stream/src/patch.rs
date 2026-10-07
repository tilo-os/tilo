use serde::{Deserialize, Serialize};

use crate::model::{Color, Frame, Item, Layer, LayerId, Point};

/// The change from one [`Frame`] to the next.
///
/// A client holding frame `base` applies it and holds frame `seq`. A client
/// that missed a patch (or joins late) asks for a whole frame instead; a patch
/// never applies to any other base.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub base: u64,
    pub seq: u64,
    pub viewport: Option<Point>,
    pub background: Option<Color>,
    pub changes: Vec<LayerChange>,
    pub removed: Vec<LayerId>,
}

/// One layer's change. `header` carries every field but the items (its
/// `items` are empty); `items` replaces the run between an unchanged prefix
/// and an unchanged suffix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerChange {
    pub id: LayerId,
    pub header: Option<Layer>,
    pub items: Option<Splice>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Splice {
    pub keep_front: u32,
    pub keep_back: u32,
    pub insert: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// The patch was made against a different frame.
    WrongBase { have: u64, patch_base: u64 },
    /// A splice names more kept items than the layer has.
    BadSplice(LayerId),
    /// A change names a layer the frame lacks and does not create it.
    UnknownLayer(LayerId),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongBase { have, patch_base } => {
                write!(f, "patch for frame {patch_base} applied to frame {have}")
            }
            Self::BadSplice(id) => write!(f, "splice out of range for layer {id}"),
            Self::UnknownLayer(id) => write!(f, "patch changes unknown layer {id}"),
        }
    }
}

impl std::error::Error for PatchError {}

impl Patch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.viewport.is_none()
            && self.background.is_none()
            && self.changes.is_empty()
            && self.removed.is_empty()
    }
}

fn header_of(layer: &Layer) -> Layer {
    Layer {
        items: Vec::new(),
        ..layer.clone()
    }
}

fn splice(old: &[Item], new: &[Item]) -> Option<Splice> {
    if old == new {
        return None;
    }
    let front = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_back = old.len().min(new.len()).saturating_sub(front);
    let back = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_back)
        .take_while(|(a, b)| a == b)
        .count();
    let insert = new
        .get(front..new.len().saturating_sub(back))
        .unwrap_or_default()
        .to_vec();
    Some(Splice {
        keep_front: u32::try_from(front).unwrap_or(u32::MAX),
        keep_back: u32::try_from(back).unwrap_or(u32::MAX),
        insert,
    })
}

impl Frame {
    /// The patch that turns `self` into `next`.
    #[must_use]
    pub fn diff(&self, next: &Self) -> Patch {
        let mut changes = Vec::new();
        for (id, layer) in &next.layers {
            match self.layers.get(id) {
                None => changes.push(LayerChange {
                    id: *id,
                    header: Some(header_of(layer)),
                    items: Some(Splice {
                        keep_front: 0,
                        keep_back: 0,
                        insert: layer.items.clone(),
                    }),
                }),
                Some(old) => {
                    let header = (!old.same_header(layer)).then(|| header_of(layer));
                    let items = splice(&old.items, &layer.items);
                    if header.is_some() || items.is_some() {
                        changes.push(LayerChange {
                            id: *id,
                            header,
                            items,
                        });
                    }
                }
            }
        }
        let removed = self
            .layers
            .keys()
            .filter(|id| !next.layers.contains_key(id))
            .copied()
            .collect();
        Patch {
            base: self.seq,
            seq: next.seq,
            viewport: (self.viewport != next.viewport).then_some(next.viewport),
            background: (self.background != next.background).then_some(next.background),
            changes,
            removed,
        }
    }

    /// Apply `patch`. On error the frame is unchanged.
    ///
    /// # Errors
    ///
    /// [`PatchError::WrongBase`] when the patch was made against another
    /// frame; the other variants for a malformed patch.
    pub fn apply(&mut self, patch: &Patch) -> Result<(), PatchError> {
        if patch.base != self.seq {
            return Err(PatchError::WrongBase {
                have: self.seq,
                patch_base: patch.base,
            });
        }
        let mut next = self.clone();
        for id in &patch.removed {
            next.layers.remove(id);
        }
        for change in &patch.changes {
            let layer = match (next.layers.get_mut(&change.id), &change.header) {
                (Some(layer), Some(header)) => {
                    let items = std::mem::take(&mut layer.items);
                    *layer = Layer {
                        items,
                        ..header.clone()
                    };
                    layer
                }
                (Some(layer), None) => layer,
                (None, Some(header)) => next
                    .layers
                    .entry(change.id)
                    .or_insert_with(|| header.clone()),
                (None, None) => return Err(PatchError::UnknownLayer(change.id)),
            };
            if let Some(s) = &change.items {
                let front = usize::try_from(s.keep_front).unwrap_or(usize::MAX);
                let back = usize::try_from(s.keep_back).unwrap_or(usize::MAX);
                let len = layer.items.len();
                let tail_at = len
                    .checked_sub(back)
                    .ok_or(PatchError::BadSplice(change.id))?;
                if front > tail_at {
                    return Err(PatchError::BadSplice(change.id));
                }
                layer.items.splice(front..tail_at, s.insert.iter().cloned());
            }
        }
        if let Some(v) = patch.viewport {
            next.viewport = v;
        }
        if let Some(b) = patch.background {
            next.background = b;
        }
        next.seq = patch.seq;
        *self = next;
        Ok(())
    }
}
