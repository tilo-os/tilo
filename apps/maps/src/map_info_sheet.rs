//! Attribution and licence information shown from the map overflow menu.
//!
//! The component owns its modal state and routes drawing, hit-testing, focus,
//! keyboard dismissal, and accessibility through the shared `ModalDialog`.

use std::cell::Cell;
use std::rc::Rc;

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{Modifiers, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ModalDialog, Text, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapInfo {
    Attribution,
    Licenses,
}

pub(crate) struct MapInfoSheet {
    dialog: ModalDialog<Text>,
    close_requested: Rc<Cell<bool>>,
    current: Option<MapInfo>,
}

impl Default for MapInfoSheet {
    fn default() -> Self {
        let close_requested = Rc::new(Cell::new(false));
        let mut dialog = dialog_for(MapInfo::Attribution, Rc::clone(&close_requested));
        dialog.set_open(false);
        Self {
            dialog,
            close_requested,
            current: None,
        }
    }
}

impl MapInfoSheet {
    pub(crate) fn open(&mut self, info: MapInfo) {
        self.close_requested = Rc::new(Cell::new(false));
        self.dialog = dialog_for(info, Rc::clone(&self.close_requested));
        self.current = Some(info);
    }

    pub(crate) fn close(&mut self) -> bool {
        let was_open = self.current.take().is_some();
        self.dialog.set_open(false);
        self.close_requested.set(false);
        was_open
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        if self.current.is_none() {
            return;
        }
        self.dialog.layout_in(viewport, theme, text_renderer);
        self.dialog.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &PresenceCtx::empty(),
        );
    }

    /// Route one event through the shared modal. Returns true while the sheet
    /// is open, including for the event that dismisses it, so covered map
    /// controls never receive the same press or key.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
    ) -> bool {
        if self.current.is_none() {
            return false;
        }
        self.dialog
            .handle_detached(event, scale_factor, modifiers, state, &automerge::ROOT);
        if self.close_requested.get() {
            self.close();
        }
        true
    }

    pub(crate) fn modal_capture(&self) -> Option<Rect> {
        self.dialog.modal_capture()
    }

    pub(crate) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.current
            .is_some()
            .then(|| self.dialog.accessibility())
            .flatten()
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<MapInfo> {
        self.current
    }

    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.current.is_some()
    }
}

fn dialog_for(info: MapInfo, close_requested: Rc<Cell<bool>>) -> ModalDialog<Text> {
    let (title, body) = match info {
        MapInfo::Attribution => (
            "Attribution",
            "Map data and tiles © OpenStreetMap contributors",
        ),
        MapInfo::Licenses => (
            "Licenses",
            "OpenStreetMap data — Open Database License (ODbL)\n\
             OpenStreetMap Standard tiles — OSMF Tile Usage Policy\n\
             © OpenStreetMap contributors",
        ),
    };
    let dismiss = Rc::clone(&close_requested);
    ModalDialog::new(title, Text::new(body).with_size(13.0))
        .primary_action("Close", move || close_requested.set(true))
        .on_dismiss(move || dismiss.set(true))
        .with_max_size(440.0, 220.0)
        .open(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{DeviceId, ElementState, MouseButton};
    use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

    fn escape() -> WindowEvent {
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.logical_key = Key::Named(NamedKey::Escape);
        event.text = None;
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn info_sheet_is_modal_only_while_open() {
        // Phase `escape_dismisses_and_consumes`.
        {
            let mut sheet = MapInfoSheet::default();
            sheet.open(MapInfo::Attribution);
            assert!(sheet.handle_event(
                &escape(),
                1.0,
                Modifiers::default(),
                &mut AutoCommit::new(),
            ));
            assert!(!sheet.is_open());
        }

        // Phase `closed_sheet_does_not_capture_input`.
        {
            let mut sheet = MapInfoSheet::default();
            let press = WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            assert!(
                !sheet.handle_event(&press, 1.0, Modifiers::default(), &mut AutoCommit::new(),)
            );
        }

        // Phase `open_sheet_exposes_shared_modal_semantics`.
        {
            let mut sheet = MapInfoSheet::default();
            sheet.open(MapInfo::Licenses);
            sheet.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                Rect::new(0.0, 0.0, 1000.0, 700.0),
            );

            assert!(sheet.modal_capture().is_some());
            let node = sheet.accessibility().expect("open dialog a11y node");
            assert_eq!(node.role, ui::a11y::Role::Dialog);
            assert!(node.state.modal);
        }
    }
}
