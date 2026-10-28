//! What the window repaints. The editor and the preview each say what of
//! them changed ([`crate::editor::SourceEditor::collect_damage_for`],
//! [`crate::preview::PreviewPane::collect_damage_for`]); around them the
//! bar's controls, the outline, the problems panel, the status bar and the
//! divider repaint where they changed. A frame laid out
//! otherwise, or with a card or an open list floating over the window,
//! repaints the window.

use ui::theme::Theme;
use vello::kurbo::Rect;
use vello::peniko::Color;

use super::{CompactPane, TypstApp};

/// How one frame showed what lies around the two panes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WindowShown {
    layout: Layout,
    /// A hover card or an open list floats over the window.
    floating: bool,
    divider: (Rect, bool, bool),
    status: (String, String),
    problems: u64,
}

/// Where everything is and what the window shows.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    rects: [Rect; 6],
    panes: (bool, bool, bool, bool),
    colors: [Color; 3],
}

impl TypstApp {
    /// Note what this frame shows around the panes.
    pub(super) fn note_window(&self, theme: &Theme) {
        let colors = &theme.tokens.colors;
        self.window_shown.note(WindowShown {
            layout: Layout {
                rects: [
                    self.rect,
                    self.editor_rect,
                    self.preview_rect,
                    self.diag_rect,
                    self.status_rect,
                    self.outline.rect(),
                ],
                panes: (
                    self.compact,
                    matches!(self.compact_view, CompactPane::Preview),
                    self.show_diagnostics,
                    self.outline.visible(),
                ),
                colors: [colors.surfaces.content, colors.text_primary, theme.text_dim],
            },
            floating: self.editor.shows_tooltip() || self.chrome.captures_input(),
            divider: (
                self.splitter.hit_rect(),
                self.splitter.is_hovered(),
                self.splitter.is_dragging(),
            ),
            status: (self.status_left_label(), self.status_right_label()),
            problems: self.diagnostics.look(),
        });
    }

    /// The window's damage since the frame on screen.
    pub(super) fn window_damage(&self, regions: &mut Vec<Rect>) {
        let whole = match self.window_shown.frames() {
            Some((drawing, presented)) => {
                drawing.layout != presented.layout || drawing.floating || presented.floating
            }
            None => true,
        };
        if whole {
            return regions.push(self.rect);
        }
        if let Some((drawing, presented)) = self.window_shown.frames() {
            if drawing.divider != presented.divider {
                regions.extend([drawing.divider.0, presented.divider.0]);
            }
            if drawing.status != presented.status {
                regions.push(self.status_rect);
            }
            if drawing.problems != presented.problems {
                regions.push(self.diag_rect);
            }
        }
        for child in self.chrome.children() {
            child.collect_damage(regions);
        }
        if let Some(outline) = self.outline.widget() {
            outline.collect_damage(regions);
        }
        if self.editor_rect.area() > 0.0 {
            self.editor.collect_damage_for(self, regions);
        }
        if self.preview_rect.area() > 0.0 {
            self.preview.collect_damage_for(self, regions);
        }
    }

    /// From `mark_drawn`: the frame just drawn is on screen.
    pub(super) fn mark_window_shown(&mut self) {
        self.window_shown.present();
        self.editor.mark_shown();
        self.preview.mark_shown();
    }
}
