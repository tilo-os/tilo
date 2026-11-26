//! What a retained row holds under its text: a part that belongs to the row's item (a picture of
//! it, or the line that says why there is none) and stands in the text's column, under the title
//! and its lines, from the text's edge to the row's own.

use super::ListRow;
use crate::widget::{Element, Text};
use taffy::{NodeId, TaffyTree};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::tokens::TextRole;
use vello::kurbo::Rect;
use vello::peniko::Color;

/// The text band a row with parts of its own lays out beside them: the row's node holds the band's
/// node first, then each part's.
pub(super) struct Band {
    /// Stands for the text band among the row's children, so every walk that pairs a widget's
    /// children with its layout nodes (focus order, the accessibility tree) meets each part at its
    /// own node.
    pub(super) slot: crate::widget::Absent,
    pub(super) node: Option<NodeId>,
    /// How tall the text band stands: the row's text, as tall as its lines.
    pub(super) height: f32,
}

impl Band {
    pub(super) fn new() -> Self {
        Self {
            slot: crate::widget::Absent::new(),
            node: None,
            height: 0.0,
        }
    }
}

/// The part a retained row holds under its text.
pub(super) struct Below {
    pub(super) element: Element,
    pub(super) node: Option<NodeId>,
}

impl ListRow {
    /// What stands under the row's text, in the text's column: a picture of the item, or the
    /// line that says why there is none. It is part of the row, so it moves, hovers and is read
    /// with it.
    #[must_use]
    pub fn below(mut self, part: Element) -> Self {
        self.below = Some(Below {
            element: part,
            node: None,
        });
        self
    }

    /// Whether the row holds parts of its own beside or under its text: it lays them out around a
    /// text band rather than measuring itself as one leaf.
    pub(super) fn has_parts(&self) -> bool {
        self.trailing.is_some() || self.below.is_some() || self.note.is_some()
    }

    /// Whether anything stands under the text in the text's column: the part under it, or the
    /// note on a line of its own.
    pub(super) fn holds_under(&self) -> bool {
        self.below.is_some() || self.note.is_some()
    }

    /// Hold the note line for the note the row carries now, in `ink`: the same line while its
    /// words and ink stay, a new one when they change, none for a row without a note.
    pub(super) fn keep_note_line(&mut self, ink: Color) {
        let Some(note) = &self.note else {
            self.note_line = None;
            return;
        };
        if let Some(line) = &self.note_line
            && line.said == *note
            && line.ink == ink
        {
            return;
        }
        let under = self.note_line.as_ref().is_some_and(|line| line.under);
        self.note_line = Some(NoteLine {
            text: Text::new(note)
                .with_role(TextRole::Caption)
                .with_color(ink)
                .naming_its_control(),
            said: note.clone(),
            ink,
            node: None,
            under,
        });
    }

    /// Whether the note stands whole in the text's column of a band `width` wide, beside what
    /// shares the column's line (the facts, a face at the row's end).
    pub(super) fn note_fits(&self, width: f64, text: &mut TextRenderer) -> bool {
        let (Some(note), Some(theme)) = (&self.note, &self.theme) else {
            return true;
        };
        let m = self.metrics(theme);
        let band = Rect::new(-m.bleed, 0.0, width + m.bleed, f64::from(self.band.height));
        // The band stands beside the trailing control already: the face takes none of its width.
        let (left, right) = self.with_face_over(None, self.secondary_shown, |face| {
            (
                face.text_x(band, &m, text, theme),
                face.geometry(band, &m, text, theme).second_right,
            )
        });
        let needs = f64::from(text.measure_style(note, None, &m.secondary).0);
        needs <= right - left + 0.5
    }

    /// Stand the note on its own line under the row where it does not fit beside the text, and
    /// back in the text's column where it does. True when that changed the row's layout.
    pub(super) fn place_note(
        &mut self,
        taffy: &mut TaffyTree<()>,
        band_width: f64,
        text: &mut TextRenderer,
    ) -> bool {
        if self.note_line.is_none() {
            return false;
        }
        let under = !self.note_fits(band_width, text);
        let Some(line) = &mut self.note_line else {
            return false;
        };
        if line.under == under {
            return false;
        }
        line.under = under;
        self.dirty = true;
        let Some(node) = line.node else {
            return true;
        };
        let Ok(style) = taffy.style(node) else {
            return true;
        };
        let mut style = style.clone();
        style.display = if under {
            taffy::Display::Block
        } else {
            taffy::Display::None
        };
        taffy.set_style(node, style).is_ok()
    }
}

/// A note too long for the text's column, standing whole on a line of its own under the row: a
/// line cut short hides its end, which is often what it is there to say. The row's own node still
/// says it, so the line reads nothing of its own.
pub(super) struct NoteLine {
    pub(super) text: Text,
    /// The words it shows: the note it was made for.
    pub(super) said: String,
    pub(super) ink: Color,
    pub(super) node: Option<NodeId>,
    /// Whether it stands under the row; otherwise it takes no room and the face shows the note.
    pub(super) under: bool,
}
