//! Attribution and licence information shown from the map overflow menu.
//!
//! The component owns its modal state and routes drawing, hit-testing, focus,
//! keyboard dismissal, and accessibility through the shared `ModalDialog`.

use std::cell::Cell;
use std::rc::Rc;

#[cfg(test)]
use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::tokens::TextRole;
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

    /// Paint the sheet in the window's top layer, over the whole window.
    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.current.is_none() {
            return;
        }
        self.dialog
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    /// Route one event through the shared modal. Returns true while the sheet
    /// is open, including for the event that dismisses it, so covered map
    /// controls never receive the same press or key.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
            "Map data and map tiles © OpenStreetMap contributors.",
        ),
        MapInfo::Licenses => (
            "Licenses",
            "The map data is © OpenStreetMap contributors, available under \
             the Open Database License (ODbL). The map tiles are \
             OpenStreetMap Standard tiles, used under the OpenStreetMap \
             Foundation's tile usage policy.",
        ),
    };
    let dismiss = Rc::clone(&close_requested);
    ModalDialog::new(title, Text::new(body).with_role(TextRole::Body))
        .primary_action("Done", move || close_requested.set(true))
        .on_dismiss(move || dismiss.set(true))
        .with_max_width(440.0)
        .open(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};

    fn escape() -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            Key::Named(NamedKey::Escape),
            None,
            ElementState::Pressed,
        ))
    }

    #[test]
    fn info_sheet_is_modal_only_while_open() {
        {
            let mut sheet = MapInfoSheet::default();
            sheet.open(MapInfo::Attribution);
            assert!(sheet.handle_event(
                &escape(),
                1.0,
                Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            ));
            assert!(!sheet.is_open());
        }

        {
            let mut sheet = MapInfoSheet::default();
            let press = ui::synthetic_input::mouse_press();
            assert!(!sheet.handle_event(
                &press,
                1.0,
                Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            ));
        }

        {
            let mut sheet = MapInfoSheet::default();
            sheet.open(MapInfo::Licenses);
            let presence = PresenceCtx::empty();
            presence
                .viewport
                .set(Some(Rect::new(0.0, 0.0, 1000.0, 700.0)));
            sheet.draw_overlay(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &presence,
            );

            assert!(sheet.modal_capture().is_some());
            let node = sheet.accessibility().expect("open dialog a11y node");
            assert_eq!(node.role, ui::a11y::Role::Dialog);
            assert!(node.state.modal);
        }
    }
}
