//! [`KeyboardView`] — a picture of the keyboard layout the person picked.

use std::collections::HashMap;

use taffy::Size;
use tilo_keyboard_map::{Drawing, Layout, draw};
use tilo_ui_base::{widget::AppModel, widget::ModelPath, widget::Widget};
use ui::kurbo::Point;
use ui::{KeyboardMap, KeyboardMapKey};
use winit::keyboard::{NativeKeyCode, PhysicalKey};

pub struct KeyboardView {
    selection: tilo_ui_base::LocalState<String>,
    layout: Layout,
    changed: bool,
    map: KeyboardMap,
}

impl KeyboardView {
    pub fn new(selection: &tilo_ui_base::LocalState<String>) -> Self {
        let layout = Self::chosen(selection).unwrap_or_else(tilo_keyboard_map::default_layout);
        Self {
            selection: selection.clone(),
            map: map(&layout),
            layout,
            changed: false,
        }
    }

    fn chosen(selection: &tilo_ui_base::LocalState<String>) -> Option<Layout> {
        selection.with(|name| tilo_keyboard_map::by_display_name(name))
    }

    fn resolve(&mut self) {
        if let Some(layout) = Self::chosen(&self.selection)
            && layout.id != self.layout.id
        {
            self.map = map(&layout);
            self.layout = layout;
            self.changed = true;
        }
    }

    pub fn layout_id(&self) -> &str {
        &self.layout.id
    }
}

fn map(layout: &Layout) -> KeyboardMap {
    let drawn = draw(&layout.id, "pc105", "", false).unwrap_or_else(|_| Drawing {
        layout: layout.id.clone(),
        model: "pc105".to_owned(),
        width: 0.0,
        height: 0.0,
        level_count: 1,
        keys: Vec::new(),
    });
    let keys = drawn
        .keys
        .into_iter()
        .map(|key| KeyboardMapKey {
            name: key.name,
            physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            outline: key
                .outline
                .into_iter()
                .map(|point| Point::new(point.x, point.y))
                .collect(),
            levels: key.levels,
        })
        .collect();
    KeyboardMap::new(
        keys,
        Size {
            width: drawn.width,
            height: drawn.height,
        },
        crate::setup::strings::t_args(
            "installer-keyboard-map-name",
            &[(
                "layout",
                tilo_ui_base::i18n::ArgValue::Text(layout.label.clone()),
            )],
        ),
    )
    .with_configuration(layout.xkb_layout().to_string(), "pc105".to_string())
}

impl tilo_ui_base::widget::Decorator for KeyboardView {
    type Inner = KeyboardMap;

    fn inner(&self) -> &KeyboardMap {
        &self.map
    }

    fn inner_mut(&mut self) -> &mut KeyboardMap {
        &mut self.map
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        self.resolve();
        self.map.sync(state, root);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let changed = std::mem::take(&mut self.changed);
        self.map.take_structure_dirty() || changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.resolve();
        self.changed = false;
        self.map.build_layout(taffy, theme, node_map)
    }

    fn is_focusable(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "KeyboardView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("keyboard_view")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync(view: &mut KeyboardView) {
        let doc = automerge::AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(automerge::ReadDoc::length(&doc, automerge::ROOT), 0);
    }

    fn label(id: &str) -> String {
        tilo_keyboard_map::fallback()
            .into_iter()
            .find(|layout| layout.id == id)
            .map(|layout| layout.label)
            .expect("the installer offers this layout")
    }

    #[test]
    fn keyboard_view_follows_the_selected_layout_as_a_non_focusable_picture() {
        {
            let kv = KeyboardView::new(&tilo_ui_base::LocalState::new(String::new()));
            assert_eq!(kv.layout_id(), "us");
        }
        {
            let kv = KeyboardView::new(&tilo_ui_base::LocalState::new(label("de")));
            assert_eq!(kv.layout_id(), "de");
        }
        {
            let selection = tilo_ui_base::LocalState::new(label("fr"));
            let mut kv = KeyboardView::new(&selection);
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "fr");
            assert!(!kv.take_structure_dirty(), "nothing changed yet");

            selection.set(label("us:dvorak"));
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "us:dvorak");
            assert!(kv.take_structure_dirty(), "a new layout is laid out again");
        }
        {
            let selection = tilo_ui_base::LocalState::new("Nonsense".to_string());
            let mut kv = KeyboardView::new(&selection);
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "us");
        }
        {
            let kv = KeyboardView::new(&tilo_ui_base::LocalState::new(String::new()));
            assert!(!kv.is_focusable());
            assert!(kv.children().is_empty());
        }
    }
}
