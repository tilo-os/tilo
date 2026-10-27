//! `FileOpenSurface`: how an app that opens files invites one, and opens a
//! file dropped anywhere on its window.
//!
//! Feed window events through [`FileOpenSurface::handle_event`] before the
//! app's own controls see them, and paint the app's empty state through
//! [`FileOpenSurface::paint`]. A drop opens wherever it lands: it does not
//! depend on the pointer position or on a preceding hover (Wayland may
//! deliver the path only when the transfer finishes). While a file hovers the
//! window, the empty state becomes a lit [`DropZoneFace`].
//!
//! Opening stays the app's: a dropped path goes to the same importer as its
//! Open action and file chooser, which reports a file it cannot read. The
//! surface reads nothing and grants no access.

use std::path::PathBuf;

use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::paint::PaintCtx;
use crate::widget::{DropZoneFace, EmptyState, EmptyStateContent};

/// A file event the surface took.
#[derive(Debug, PartialEq, Eq)]
pub enum FileOpenEvent {
    /// A file started or stopped hovering the window: repaint.
    HoverChanged,
    /// Open this file the way the app's Open action does.
    Open(PathBuf),
}

/// Window-wide file drops and the "open a file" empty state. See the module
/// docs.
#[derive(Debug, Default)]
pub struct FileOpenSurface {
    hovering: bool,
}

impl FileOpenSurface {
    /// Whether a file is being dragged over the window.
    #[must_use]
    pub fn is_hovering(&self) -> bool {
        self.hovering
    }

    /// Take the window's file events. Call before routing the event to the
    /// app's controls: a file event carries no position and is never input
    /// for a text field under the pointer.
    pub fn handle_event(&mut self, event: &WindowEvent) -> Option<FileOpenEvent> {
        match event {
            WindowEvent::HoveredFile(_) => {
                let changed = !self.hovering;
                self.hovering = true;
                changed.then_some(FileOpenEvent::HoverChanged)
            }
            WindowEvent::HoveredFileCancelled => {
                let changed = self.hovering;
                self.hovering = false;
                changed.then_some(FileOpenEvent::HoverChanged)
            }
            WindowEvent::DroppedFile(path) => {
                self.hovering = false;
                Some(FileOpenEvent::Open(path.clone()))
            }
            _ => None,
        }
    }

    /// Paint `content`, an empty state whose action opens a file, saying a
    /// file can also be dropped; while one hovers, the lit drop zone instead.
    /// Returns the action's rectangle, as [`EmptyState::paint_focused`]
    /// does. A state without an action (loading) is painted as it is.
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
            let release = crate::i18n!("file-open-release");
            DropZoneFace::new(&release)
                .icon(content.icon.unwrap_or(crate::icons::Icon::Download))
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
    fn a_drop_opens_without_a_hover_and_a_cancelled_hover_ends() {
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
        // Every hovered file of a multi-file drag reports; one repaint does.
        assert_eq!(
            surface.handle_event(&WindowEvent::HoveredFile(path.clone())),
            None
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

    /// A failed open says what went wrong on its one bounded line, and the
    /// drop hint takes a line of its own under it: the action moves down
    /// by that line instead of the hint being drawn over the button.
    #[test]
    fn an_error_and_the_drop_hint_each_keep_a_line_above_the_action() {
        let bounds = Rect::new(0.0, 0.0, 480.0, 640.0);
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let content = EmptyStateContent {
            message: Some("Check the file and try again."),
            action_label: Some("Open another file"),
            error: true,
            ..EmptyStateContent::new("Couldn't open this document")
        };
        let alone = EmptyState::paint_focused(&mut ctx, bounds, &content, Point::ZERO, false)
            .expect("the action");
        let hinted = FileOpenSurface::default()
            .paint(&mut ctx, bounds, &content, Point::ZERO, false)
            .expect("the action");
        // The stack is centred, so a line added above the action moves it
        // down by half a line.
        let moved = hinted.y0 - alone.y0;
        assert!(
            moved > 4.0,
            "the hint's line moves the action down: {alone:?} -> {hinted:?}"
        );
    }

    #[test]
    fn the_open_action_stays_reachable_and_returns_after_a_cancelled_drag() {
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
                .expect("the open action");
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
