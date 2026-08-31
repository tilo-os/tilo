//! Crash-recovery persistence and its transient Restore/Discard banner.
//!
//! The editor supplies Markdown snapshots and applies a restored snapshot. This
//! component owns everything else: debounce state, recovery-file lifetime,
//! banner layout/drawing, and pointer hit-testing.

use std::path::{Path, PathBuf};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use ui::text_render::TextRenderer;

use crate::i18n::t;
use crate::num;

/// Write a recovery copy after this many document-modifying edits.
pub(crate) const AUTOSAVE_EDIT_THRESHOLD: u32 = 24;

/// Height reserved for the recovery banner while an offer is active.
pub(crate) const BANNER_HEIGHT: f64 = 40.0;

#[derive(Clone, Debug)]
struct RecoveryOffer {
    path: PathBuf,
    markdown: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RecoveryAction {
    Restore,
    Discard,
}

/// Result of a click claimed by the banner.
#[derive(Debug, PartialEq)]
pub(crate) enum RecoveryDecision {
    Restore(Vec<u8>),
    Discard,
}

/// Theme colours needed by the banner, kept independent of the editor's much
/// larger chrome palette.
#[derive(Clone, Copy)]
pub(crate) struct RecoveryPalette {
    pub(crate) border: Color,
    pub(crate) chip: Color,
    pub(crate) text: Color,
    pub(crate) on_accent: Color,
    pub(crate) accent: Color,
    pub(crate) notice: Color,
}

/// Stateful crash-recovery surface owned by the word processor.
#[derive(Default)]
pub(crate) struct RecoveryBanner {
    offer: Option<RecoveryOffer>,
    buttons: Vec<(RecoveryAction, Rect)>,
    base_dir: Option<PathBuf>,
    edits_since_autosave: u32,
}

impl RecoveryBanner {
    pub(crate) fn is_active(&self) -> bool {
        self.offer.is_some()
    }

    pub(crate) fn height(&self) -> f64 {
        if self.is_active() { BANNER_HEIGHT } else { 0.0 }
    }

    /// Override the recovery store, primarily for isolated tests.
    pub(crate) fn set_base_dir(&mut self, dir: PathBuf) {
        self.base_dir = Some(dir);
    }

    fn base_dir(&self) -> PathBuf {
        self.base_dir
            .clone()
            .unwrap_or_else(crate::recovery::base_dir)
    }

    /// Stable recovery path for `document_path`, re-rooted under any override.
    pub(crate) fn path_for(&self, document_path: Option<&Path>) -> PathBuf {
        let default_path = crate::recovery::recovery_path_for(document_path);
        default_path
            .file_name()
            .map_or_else(|| default_path.clone(), |name| self.base_dir().join(name))
    }

    /// Count an edit and, at the debounce threshold, persist `markdown`.
    pub(crate) fn note_edit(
        &mut self,
        document_path: Option<&Path>,
        snapshot: impl FnOnce() -> Option<Vec<u8>>,
    ) {
        self.edits_since_autosave = self.edits_since_autosave.saturating_add(1);
        if self.edits_since_autosave < AUTOSAVE_EDIT_THRESHOLD {
            return;
        }
        self.edits_since_autosave = 0;
        let Some(markdown) = snapshot() else {
            return;
        };
        let path = self.path_for(document_path);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let _ = std::fs::write(path, markdown);
    }

    /// Remove a superseded recovery copy after a successful open/save.
    pub(crate) fn clear(&mut self, document_path: Option<&Path>) {
        let _ = std::fs::remove_file(self.path_for(document_path));
        self.edits_since_autosave = 0;
        self.offer = None;
        self.buttons.clear();
    }

    /// Stage an on-disk recovery copy for user confirmation.
    pub(crate) fn check(&mut self, document_path: Option<&Path>) -> bool {
        let path = self.path_for(document_path);
        match std::fs::read(&path) {
            Ok(markdown) if !markdown.is_empty() => {
                self.offer = Some(RecoveryOffer { path, markdown });
                true
            }
            _ => false,
        }
    }

    /// Resolve a pointer press against the banner's controls. The recovery file
    /// is consumed here for both outcomes, so file lifetime cannot drift from UI
    /// state. The editor only has to apply Markdown returned by Restore.
    pub(crate) fn handle_click(&mut self, point: Point) -> Option<RecoveryDecision> {
        let action = self
            .buttons
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(action, _)| *action)?;
        let offer = self.offer.take()?;
        let _ = std::fs::remove_file(offer.path);
        self.buttons.clear();
        Some(match action {
            RecoveryAction::Restore => RecoveryDecision::Restore(offer.markdown),
            RecoveryAction::Discard => RecoveryDecision::Discard,
        })
    }

    #[cfg(test)]
    pub(crate) fn restore_button_rect(&self) -> Option<Rect> {
        self.buttons
            .iter()
            .find_map(|(action, rect)| (*action == RecoveryAction::Restore).then_some(*rect))
    }

    #[cfg(test)]
    pub(crate) fn discard_button_rect(&self) -> Option<Rect> {
        self.buttons
            .iter()
            .find_map(|(action, rect)| (*action == RecoveryAction::Discard).then_some(*rect))
    }

    /// Draw the banner and rebuild its button hit map.
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        palette: RecoveryPalette,
        bar: Rect,
    ) {
        self.buttons.clear();
        if !self.is_active() {
            return;
        }
        scene.fill(Fill::NonZero, transform, palette.notice, None, &bar);
        scene.fill(
            Fill::NonZero,
            transform,
            palette.border,
            None,
            &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1),
        );
        let cy = f64::midpoint(bar.y0, bar.y1);
        tr.draw_text(
            scene,
            &t("recovery-banner"),
            13.0,
            palette.text,
            transform * Affine::translate((bar.x0 + 14.0, cy - 8.0)),
            None,
        );
        let mut x = bar.x1 - 12.0;
        for (action, label, width, primary) in [
            (RecoveryAction::Discard, t("recovery-discard"), 80.0, false),
            (RecoveryAction::Restore, t("recovery-restore"), 80.0, true),
        ] {
            x -= width;
            let rect = Rect::new(x, cy - 13.0, x + width, cy + 13.0);
            let (background, foreground) = if primary {
                (palette.accent, palette.on_accent)
            } else {
                (palette.chip, palette.text)
            };
            scene.fill(
                Fill::NonZero,
                transform,
                background,
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, 6.0),
            );
            let tx =
                rect.x0 + (width - num::f64_of_usize(label.chars().count()) * 7.4).max(6.0) / 2.0;
            tr.draw_text(
                scene,
                &label,
                13.0,
                foreground,
                transform * Affine::translate((tx, cy - 8.0)),
                None,
            );
            self.buttons.push((action, rect));
            x -= 8.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "word-processor-recovery-banner-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn threshold_persists_and_clear_removes_the_recovery_copy() {
        let dir = temp_dir("threshold");
        let _ = std::fs::remove_dir_all(&dir);
        let mut banner = RecoveryBanner::default();
        banner.set_base_dir(dir.clone());
        let document = Path::new("draft.md");

        for _ in 0..AUTOSAVE_EDIT_THRESHOLD - 1 {
            banner.note_edit(Some(document), || Some(b"not yet".to_vec()));
        }
        let path = banner.path_for(Some(document));
        assert!(!path.exists());
        banner.note_edit(Some(document), || Some(b"latest".to_vec()));
        assert_eq!(std::fs::read(&path).unwrap(), b"latest");

        banner.clear(Some(document));
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn click_resolution_consumes_the_offer_and_its_file() {
        let dir = temp_dir("click");
        let _ = std::fs::remove_dir_all(&dir);
        let mut banner = RecoveryBanner::default();
        banner.set_base_dir(dir.clone());
        let document = Path::new("draft.md");
        let path = banner.path_for(Some(document));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"recovered").unwrap();
        assert!(banner.check(Some(document)));

        // This focused test supplies the same hit geometry draw() owns, avoiding
        // a font-system dependency while exercising production click routing.
        banner
            .buttons
            .push((RecoveryAction::Restore, Rect::new(10.0, 10.0, 30.0, 30.0)));
        assert_eq!(
            banner.handle_click(Point::new(20.0, 20.0)),
            Some(RecoveryDecision::Restore(b"recovered".to_vec()))
        );
        assert!(!banner.is_active());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
