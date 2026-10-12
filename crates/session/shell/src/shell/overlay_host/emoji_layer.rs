//! The emoji picker as a stacked layer.

use std::time::Instant;

use settings_store::hardware::ShortcutAction;
use tilo_desktop::emoji_picker::EmojiPicker;
use ui::a11y::AccessNode;
use vello::kurbo::Rect;
use winit::event::{ElementState, WindowEvent};

use super::layers::{LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Routed, ShellLayer};

/// Owns every key and pointer event while open; its own shortcut closes it
/// again.
impl ShellLayer for EmojiPicker {
    fn id(&self) -> LayerId {
        LayerId::EmojiPicker
    }

    fn is_open(&self) -> bool {
        Self::is_open(self)
    }

    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        let toggles = matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed
        ) && cx.shortcut == Some(ShortcutAction::EmojiPicker);
        if toggles {
            self.close();
        } else {
            Self::handle_event(
                self,
                event,
                cx.event.scale_factor,
                cx.event.modifiers.state(),
                cx.cursor,
                Instant::now(),
            );
        }
        cx.request(LayerIntent::Repaint);
        Routed::Consumed
    }

    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        let viewport = cx.geometry.display_for(self.anchor());
        Self::draw(
            self,
            cx.scene,
            cx.transform,
            cx.text_renderer,
            cx.theme,
            cx.presence,
            viewport,
        );
    }

    fn is_animating(&self, _now: Instant) -> bool {
        self.needs_animation_frame()
    }

    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<AccessNode> {
        Self::accessibility(self)
    }

    fn accessibility_rect(&self, _geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect> {
        self.accessibility_rect_for_path(path)
    }

    fn focus_accessibility(&mut self, _path: &[usize]) -> bool {
        false
    }

    fn dismiss(&mut self) -> bool {
        let open = Self::is_open(self);
        self.close();
        open
    }
}
