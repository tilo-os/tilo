//! The editor's segmented strips (a transition, a preset, an export
//! option) as the toolkit's segmented control paints them: whole labels, or,
//! where they do not fit, folded into a picker whose menu lists every
//! segment ([`ui::widget::SegmentedFace::folds`]).
//!
//! A strip is painted where its surface lays it out; each paint notes the
//! strips that folded, so a press on one opens its menu instead of landing
//! on a segment too narrow to read. Choosing a row presses that segment
//! through the surface's own handler, so a strip chosen from its menu and
//! one clicked whole take the same path.

use std::cell::RefCell;

use ui::widget::{ContextMenu, SegmentedFace};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::paint::control_size_for;
#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// The surface a strip belongs to: which press handler its segments answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StripHost {
    Inspector,
    Export,
}

/// A strip painted folded this frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FoldedStrip {
    host: StripHost,
    /// Where the strip is: every segment, joined.
    rect: Rect,
    /// Where each segment is: a row chosen presses its centre.
    segments: Vec<Rect>,
    labels: Vec<String>,
    selected: usize,
}

/// The strips painted folded in the latest frame, and the one whose menu is
/// open.
#[derive(Debug, Default)]
pub(super) struct Strips {
    folded: RefCell<Vec<FoldedStrip>>,
    open: Option<FoldedStrip>,
}

impl Strips {
    /// A new frame paints its strips afresh.
    pub(super) fn begin_frame(&self) {
        self.folded.borrow_mut().clear();
    }

    /// Whether the strip of `host` at `rect` was painted folded.
    #[cfg(test)]
    pub(super) fn is_folded(&self, host: StripHost, rect: Rect) -> bool {
        self.folded
            .borrow()
            .iter()
            .any(|strip| strip.host == host && strip.rect == rect)
    }
}

impl VideoEditorApp {
    /// Paint one segmented strip of `labels` in `segments`, `selected` chosen
    /// (or `usize::MAX`), folded into a picker where the labels do not fit.
    pub(super) fn draw_strip(
        &self,
        scene: &mut Scene,
        tf: Affine,
        (tr, theme): (&mut TextRenderer, &Theme),
        (host, segments): (StripHost, &[Rect]),
        (labels, selected): (&[&str], usize),
    ) {
        let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
            return;
        };
        let strip = segments.iter().fold(first.union(*last), |a, b| a.union(*b));
        let size = control_size_for(*first, theme);
        let cursor = self.last_mouse;
        let mut face = SegmentedFace::new(segments, labels, selected).control_size(size);
        if SegmentedFace::folds(segments, labels, size, tr, theme) {
            let open = self.bin_menu.is_open()
                && self.menu_target == Some(MenuTarget::Strip)
                && self
                    .strips
                    .open
                    .as_ref()
                    .is_some_and(|open| open.host == host && open.rect == strip);
            face = face.folded(strip.contains(cursor), open);
            self.strips.folded.borrow_mut().push(FoldedStrip {
                host,
                rect: strip,
                segments: segments.to_vec(),
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
                selected,
            });
        } else {
            face = face.hovered(segments.iter().position(|r| r.contains(cursor)));
        }
        ui::widget::ToggleGroup::paint(scene, tf, tr, theme, &face);
    }

    /// Open the menu of the strip of `host` folded under `p`, if there is
    /// one; whether it did.
    pub(super) fn open_folded_strip(&mut self, host: StripHost, p: Point) -> bool {
        let strip = self
            .strips
            .folded
            .borrow()
            .iter()
            .find(|strip| strip.host == host && strip.rect.contains(p))
            .cloned();
        let Some(strip) = strip else {
            return false;
        };
        let labels: Vec<&str> = strip.labels.iter().map(String::as_str).collect();
        let mut menu = ContextMenu::with_items(SegmentedFace::menu_items(&labels, strip.selected));
        menu.set_align(ui::widget::PanelAlign::Leading);
        menu.open_anchored(strip.rect, self.rect);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Strip);
        self.strips.open = Some(strip);
        true
    }

    /// Row `row` of an open strip's menu: press its segment through the
    /// surface's own handler.
    pub(super) fn choose_strip_row(&mut self, row: usize) {
        let Some(strip) = self.strips.open.take() else {
            return;
        };
        let Some(at) = strip.segments.get(row).map(Rect::center) else {
            return;
        };
        match strip.host {
            StripHost::Inspector => {
                let r = self.regions();
                self.press_inspector(&r, at);
            }
            StripHost::Export => self.press_export(at),
        }
    }
}
