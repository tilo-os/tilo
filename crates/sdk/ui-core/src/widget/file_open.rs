//! The common entry surface for file-based apps.
//!
//! Paint the chooser affordance with `paint`, route its returned hit rectangle
//! to the app's existing Open action, and feed file events through `handle_event`
//! before editors and modal controls. A drop opens anywhere in the window; it
//! does not depend on a stale pointer or a preceding hover event (Wayland may
//! deliver the paths only when the transfer finishes).
//!
//! Both routes use the app's normal importer. The surface neither reads files
//! nor grants access: the host's `PickRequest`/framework `choose_file` remains
//! the chooser, and the parser reports unsupported or unreadable files.

use std::path::PathBuf;
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::paint::PaintCtx;
use crate::widget::{DropZoneFace, EmptyState, EmptyStateContent};

/// A file event owned by the opening surface.
#[derive(Debug, PartialEq, Eq)]
pub enum FileOpenEvent {
    /// Redraw the invitation after the hover entered or left.
    HoverChanged,
    /// Open with the same importer used by the chooser and Open action.
    Open(PathBuf),
}

/// Shared file-opening presentation and window-wide drop routing.
#[derive(Default)]
pub struct FileOpenSurface {
    hovering: bool,
}

impl FileOpenSurface {
    #[must_use]
    pub fn is_hovering(&self) -> bool {
        self.hovering
    }

    /// Call before routing input to controls. File events carry no coordinates
    /// and must never be interpreted as text-editor input.
    pub fn handle_event(&mut self, event: &WindowEvent) -> Option<FileOpenEvent> {
        match event {
            WindowEvent::HoveredFile(_) => {
                self.hovering = true;
                Some(FileOpenEvent::HoverChanged)
            }
            WindowEvent::HoveredFileCancelled => {
                self.hovering = false;
                Some(FileOpenEvent::HoverChanged)
            }
            WindowEvent::DroppedFile(path) => {
                self.hovering = false;
                Some(FileOpenEvent::Open(path.clone()))
            }
            _ => None,
        }
    }

    /// The existing Open action owns activation, keyboard focus and accessibility.
    /// Its hit rectangle is measured by the same shared empty-state component.
    /// A state without an action (for example Loading) has no drop invitation.
    pub fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        bounds: Rect,
        content: &EmptyStateContent<'_>,
        cursor: Point,
        focused: bool,
    ) -> Option<Rect> {
        if content.action_label.is_none() {
            return EmptyState::paint_focused(ctx, bounds, content, cursor, focused);
        }
        let hint = crate::i18n!("file-open-drop-hint");
        if self.hovering {
            let release = crate::i18n!("file-open-release-hint");
            DropZoneFace::new(&release)
                .message(&hint)
                .drag_over(true)
                .paint(ctx, bounds, 0.0);
            return None;
        }
        let message = content
            .message
            .map_or_else(|| hint.clone(), |message| format!("{message}\n{hint}"));
        EmptyState::paint_focused(
            ctx,
            bounds,
            &EmptyStateContent {
                message: Some(&message),
                ..*content
            },
            cursor,
            focused,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use vello::Scene;
    use vello::kurbo::Affine;

    #[test]
    fn drops_do_not_require_hover_or_pointer_coordinates_and_hover_cancels() {
        let mut surface = FileOpenSurface::default();
        let path = PathBuf::from("/workspace/book.epub");
        assert_eq!(
            surface.handle_event(&WindowEvent::DroppedFile(path.clone())),
            Some(FileOpenEvent::Open(path.clone()))
        );
        assert!(!surface.is_hovering());
        assert_eq!(
            surface.handle_event(&WindowEvent::HoveredFile(path.clone())),
            Some(FileOpenEvent::HoverChanged)
        );
        assert!(surface.is_hovering());
        assert_eq!(
            surface.handle_event(&WindowEvent::HoveredFileCancelled),
            Some(FileOpenEvent::HoverChanged)
        );
        assert!(!surface.is_hovering());
        surface.handle_event(&WindowEvent::HoveredFile(path.clone()));
        assert_eq!(
            surface.handle_event(&WindowEvent::DroppedFile(path.clone())),
            Some(FileOpenEvent::Open(path))
        );
        assert!(!surface.is_hovering());
        assert_eq!(surface.handle_event(&WindowEvent::RedrawRequested), None);
    }

    #[test]
    fn chooser_stays_reachable_in_compact_windows_and_returns_after_drag_cancellation() {
        let bounds = Rect::new(0.0, 0.0, 320.0, 240.0);
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
            let mut surface = FileOpenSurface::default();
            let content = EmptyStateContent {
                action_label: Some("Open file"),
                ..EmptyStateContent::new("No file open")
            };
            let button = surface
                .paint(&mut ctx, bounds, &content, Point::ZERO, true)
                .unwrap();
            assert_eq!(button.intersect(bounds), button);
            assert!(button.width() > 0.0 && button.height() > 0.0);
            surface.handle_event(&WindowEvent::HoveredFile(PathBuf::from("/workspace/file")));
            assert!(
                surface
                    .paint(&mut ctx, bounds, &content, Point::ZERO, false)
                    .is_none()
            );
            surface.handle_event(&WindowEvent::HoveredFileCancelled);
            assert_eq!(
                surface.paint(&mut ctx, bounds, &content, Point::ZERO, false),
                Some(button)
            );
        }
    }
}
