//! Crash-recovery persistence and its transient Restore/Discard banner.
//!
//! The editor supplies Markdown snapshots and applies a restored snapshot. This
//! component owns everything else: debounce state, recovery-file lifetime,
//! banner layout/drawing, and pointer hit-testing.

use std::path::{Path, PathBuf};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Banner, BannerContent, BannerKind, Button, ButtonFace, ButtonRole, ControlSize};

use crate::i18n::t;

/// Write a recovery copy after this many document-modifying edits.
pub(crate) const AUTOSAVE_EDIT_THRESHOLD: u32 = 24;

#[derive(Clone, Debug)]
struct RecoveryOffer {
    slot: (tilo_files::Dir, tilo_files::VPath),
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

/// Stateful crash-recovery surface owned by the word processor.
#[derive(Default)]
pub(crate) struct RecoveryBanner {
    offer: Option<RecoveryOffer>,
    buttons: Vec<(RecoveryAction, Rect)>,
    hovered: Option<RecoveryAction>,
    base_dir: Option<PathBuf>,
    edits_since_autosave: u32,
}

impl RecoveryBanner {
    pub(crate) fn is_active(&self) -> bool {
        self.offer.is_some()
    }

    /// The band the banner reserves: an information banner one regular
    /// control row tall, its own padding inside it and the same step around
    /// it, or nothing while no offer is staged.
    pub(crate) fn height(&self, theme: &Theme) -> f64 {
        if self.is_active() {
            ButtonFace::height(ControlSize::Regular, theme)
                + 4.0 * f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        }
    }

    /// Track the pointer for the buttons' hover state; returns whether it
    /// changed.
    pub(crate) fn pointer_moved(&mut self, point: Point) -> bool {
        let hovered = self
            .buttons
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(action, _)| *action);
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    /// Override the recovery store, primarily for isolated tests.
    pub(crate) fn set_base_dir(&mut self, dir: PathBuf) {
        self.base_dir = Some(dir);
    }

    #[cfg(test)]
    fn base_dir(&self) -> PathBuf {
        self.base_dir
            .clone()
            .unwrap_or_else(crate::recovery::base_dir)
    }

    /// The recovery copy's file for `document_path`, in any override.
    fn slot(&self, document_path: Option<&Path>) -> Option<(tilo_files::Dir, tilo_files::VPath)> {
        let name = tilo_files::VPath::new(&crate::recovery::slot_name(document_path)).ok()?;
        if let Some(base) = &self.base_dir {
            let dir = tilo_files::Dir::open(tilo_files::Location::Native(base.clone())).ok()?;
            Some((dir, name))
        } else {
            let (dir, prefix) = crate::recovery::store()?;
            Some((dir, prefix.join(&name)))
        }
    }

    /// Stable recovery path for `document_path`, re-rooted under any override.
    #[cfg(test)]
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
        if let Some((dir, name)) = self.slot(document_path) {
            let _ = tilo_files::blocking::write_atomic(&dir, &name, &markdown);
        }
    }

    /// Remove a superseded recovery copy after a successful open/save.
    pub(crate) fn clear(&mut self, document_path: Option<&Path>) {
        if let Some((dir, name)) = self.slot(document_path) {
            let _ = tilo_files::blocking::remove(&dir, &name);
        }
        self.edits_since_autosave = 0;
        self.offer = None;
        self.buttons.clear();
    }

    /// Stage an on-disk recovery copy for user confirmation.
    pub(crate) fn check(&mut self, document_path: Option<&Path>) -> bool {
        // Switching documents must dismiss the previous offer without deleting
        // its on-disk recovery. Only the new document's copy may be restored.
        self.offer = None;
        self.buttons.clear();
        self.edits_since_autosave = 0;
        let Some(slot) = self.slot(document_path) else {
            return false;
        };
        match tilo_files::blocking::read(&slot.0, &slot.1) {
            Ok(markdown) if !markdown.is_empty() => {
                self.offer = Some(RecoveryOffer { slot, markdown });
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
        let _ = tilo_files::blocking::remove(&offer.slot.0, &offer.slot.1);
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

    /// Draw the banner and rebuild its button hit map: an info icon and the
    /// offer in `Body` on the band's own ground, Restore as the view's
    /// primary action and Discard as the destructive one, one hairline under
    /// the band.
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        self.buttons.clear();
        if !self.is_active() {
            return;
        }
        // The offer is the toolkit's information banner, with Discard and
        // Restore beside it at the band's trailing end.
        let sp = &theme.tokens.spacing;
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        let cy = bar.center().y;
        let button_h = ButtonFace::height(ControlSize::Regular, theme);
        let mut x = bar.x1 - s3;
        for (action, label, role) in [
            (
                RecoveryAction::Restore,
                t("recovery-restore"),
                ButtonRole::Primary,
            ),
            (
                RecoveryAction::Discard,
                t("recovery-discard"),
                ButtonRole::Destructive,
            ),
        ] {
            let width = ButtonFace::new(Rect::ZERO, &label)
                .role(role)
                .intrinsic_width(tr, theme);
            x -= width;
            let rect = Rect::new(x, cy - button_h / 2.0, x + width, cy + button_h / 2.0);
            let face = ButtonFace::new(rect, &label)
                .role(role)
                .hovered(self.hovered == Some(action));
            Button::paint(scene, transform, tr, theme, &face);
            self.buttons.push((action, rect));
            x -= s2;
        }
        let title = t("recovery-banner");
        let card = Rect::new(
            bar.x0 + s3,
            bar.y0 + s2,
            x.max(bar.x0 + s3 + 1.0),
            bar.y1 - s2,
        );
        Banner::paint(
            &mut PaintCtx::new(scene, transform, tr, theme),
            card,
            &BannerContent::new(BannerKind::Info, &title),
            Point::new(f64::NAN, f64::NAN),
        );
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

    /// The threshold persists and clear removes the recovery copy; click resolution consumes the offer and
    /// its file.
    #[test]
    fn the_recovery_banner_persists_its_threshold_and_consumes_its_offer() {
        // Phase: threshold_persists_and_clear_removes_the_recovery_copy.
        {
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

        // Phase: click_resolution_consumes_the_offer_and_its_file.
        {
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
}
