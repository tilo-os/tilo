//! The keyboard the Keyboard page draws, watching the keys the person
//! presses ([`KeyboardWatch`]), and what it and the page's add-source picker
//! show, handed to the effects that answer an agent's keyboard inspection.
//!
//! A piece of Settings' own around its shell: the host hands the physical
//! keys and the foreground events to the window's root, never to a widget
//! deep in a page, so the drawn keyboard (`ui::KeyboardMap`) sees them only
//! through whoever holds the page; and what the drawn keyboard shows is read
//! by Settings' own effects ([`crate::settings_app::SettingsApp::keyboard_inspection`]),
//! which the toolkit knows nothing of.

use std::rc::Rc;

use ui::widget::{Element, Widget};

use crate::categories::Category;
use crate::settings_app::SettingsApp;

/// The window's content, its drawn keyboard watching the keys. See the
/// [module docs](self).
pub(crate) struct KeyboardWatch {
    app: Rc<SettingsApp>,
    content: Element,
}

impl KeyboardWatch {
    pub(crate) fn new(app: Rc<SettingsApp>, content: Element) -> Self {
        Self { app, content }
    }

    /// The keyboard drawn on the Keyboard page, read now; `None` while the
    /// page draws none.
    fn keyboard_now(&self) -> Option<Result<serde_json::Value, String>> {
        fn mounted_map(widget: &dyn Widget) -> Option<&ui::KeyboardMap> {
            if let Some(map) = widget
                .as_any()
                .and_then(|value| value.downcast_ref::<ui::KeyboardMap>())
            {
                return Some(map);
            }
            widget.children().into_iter().find_map(mounted_map)
        }
        if self.app.selected() != Category::Keyboard {
            return None;
        }
        mounted_map(&*self.content).map(ui::KeyboardMap::inspection_snapshot)
    }

    /// Hand the effects what the drawn keyboard and the add-source picker
    /// show now: whatever moved them (a key held, a layout, a key inspected,
    /// a filter typed) went through the window.
    fn share(&self) {
        self.app
            .keyboard_drawn(self.keyboard_now().and_then(Result::ok));
        let picker = (self.app.selected() == Category::Keyboard)
            .then(|| crate::panes::mouse_keyboard::input_picker_snapshot(&*self.content))
            .flatten();
        self.app.input_picker_drawn(picker);
    }
}

impl ui::widget::Decorator for KeyboardWatch {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn name(&self) -> &'static str {
        "SettingsKeyboardWatch"
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        self.content.take_structure_change()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.content.sync(state, path);
        self.share();
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.content.apply_layout(taffy, node, origin);
        self.share();
    }

    fn observe_physical_input(&mut self, input: ui::widget::PhysicalInput) {
        fn observe_map(widget: &mut dyn Widget, input: &ui::widget::PhysicalInput) -> bool {
            if let Some(map) = widget
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<ui::KeyboardMap>())
            {
                map.observe_physical_input(input.clone());
                return true;
            }
            let mut visible = false;
            for child in widget.children_mut() {
                visible |= observe_map(child, input);
            }
            visible
        }
        let visible =
            self.app.selected() == Category::Keyboard && observe_map(&mut *self.content, &input);
        if !visible {
            self.app.keyboard_map_input().observe_configuration(&input);
            self.app.keyboard_map_input().clear();
        }
        self.share();
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // Observe only the mounted foreground page, before its search or
        // picker can consume editing events. This never changes handling
        // or focus.
        fn observe_map(widget: &mut dyn Widget, event: &winit::event::WindowEvent) {
            if let Some(map) = widget
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<ui::KeyboardMap>())
            {
                map.observe_foreground_event(event);
                return;
            }
            for child in widget.children_mut() {
                observe_map(child, event);
            }
        }
        if self.app.selected() == Category::Keyboard {
            observe_map(&mut *self.content, event);
        }
        self.content.handle_event(event, ctx);
        self.share();
    }
}
