//! [`KeyboardView`] — a picture of the keyboard layout the person picked.

use std::collections::HashMap;

use taffy::Size;
use ui::kurbo::{Point, Rect};
use ui::widget::{AppModel, ModelPath, Widget};
use ui::{KeyboardMap, KeyboardMapKey};
use winit::keyboard::{NativeKeyCode, PhysicalKey};

use imager_backend::destination::layouts::{self, Layout};

const STAGGER: [f64; 4] = [0.0, 0.5, 0.75, 1.25];
const SPACE_UNDER: std::ops::Range<usize> = 2..8;
const KEY_GAP: f64 = 0.12;

pub struct KeyboardView {
    selection: ui::LocalState<String>,
    layout: &'static Layout,
    changed: bool,
    map: KeyboardMap,
}

impl KeyboardView {
    pub fn new(selection: &ui::LocalState<String>) -> Self {
        let layout = Self::chosen(selection).unwrap_or_else(layouts::default_layout);
        Self {
            selection: selection.clone(),
            layout,
            changed: false,
            map: map(layout),
        }
    }

    fn chosen(selection: &ui::LocalState<String>) -> Option<&'static Layout> {
        selection.with(|name| layouts::by_display_name(name))
    }

    fn resolve(&mut self) {
        if let Some(layout) = Self::chosen(&self.selection)
            && !std::ptr::eq(layout, self.layout)
        {
            self.layout = layout;
            self.map = map(layout);
            self.changed = true;
        }
    }

    pub fn layout_id(&self) -> &str {
        self.layout.id
    }
}

fn key(name: String, x: f64, row: usize, width: f64, levels: Vec<String>) -> KeyboardMapKey {
    let top = ui::num::f64_of_usize(row);
    let cap = Rect::new(
        x + KEY_GAP / 2.0,
        top + KEY_GAP / 2.0,
        x + width - KEY_GAP / 2.0,
        top + 1.0 - KEY_GAP / 2.0,
    );
    KeyboardMapKey {
        name,
        physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        outline: vec![
            Point::new(cap.x0, cap.y0),
            Point::new(cap.x1, cap.y0),
            Point::new(cap.x1, cap.y1),
            Point::new(cap.x0, cap.y1),
        ],
        levels,
    }
}

fn map(layout: &'static Layout) -> KeyboardMap {
    let mut keys = Vec::new();
    let mut width = 0.0_f64;
    for (row, (glyphs, &start)) in layout.rows.iter().zip(STAGGER.iter()).enumerate() {
        for (index, glyph) in glyphs.iter().enumerate() {
            let x = start + ui::num::f64_of_usize(index);
            let mut levels = vec![(*glyph).to_string()];
            if row == 0
                && let Some(shift) = layout.number_shift.get(index)
                && !shift.is_empty()
                && shift != glyph
            {
                levels.push((*shift).to_string());
            }
            keys.push(key(format!("{row}:{index}"), x, row, 1.0, levels));
            width = width.max(x + 1.0);
        }
    }
    let space_row = layout.rows.len();
    let bottom = STAGGER.last().copied().unwrap_or_default();
    keys.push(key(
        "space".to_string(),
        bottom + ui::num::f64_of_usize(SPACE_UNDER.start),
        space_row,
        ui::num::f64_of_usize(SPACE_UNDER.len()),
        vec![String::new()],
    ));
    KeyboardMap::new(
        keys,
        Size {
            width,
            height: ui::num::f64_of_usize(space_row + 1),
        },
        crate::setup::strings::t_args(
            "installer-keyboard-map-name",
            &[(
                "layout",
                ui::i18n::ArgValue::Text(layout.display_name.to_string()),
            )],
        ),
    )
    .with_configuration(layout.xkb_layout.to_string(), "pc105".to_string())
}

impl ui::widget::Decorator for KeyboardView {
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
        theme: &ui::theme::Theme,
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

    #[test]
    fn keyboard_view_follows_the_selected_layout_as_a_non_focusable_picture() {
        {
            let kv = KeyboardView::new(&ui::LocalState::new(String::new()));
            assert_eq!(kv.layout_id(), "us");
        }
        {
            let kv = KeyboardView::new(&ui::LocalState::new("German (QWERTZ)".into()));
            assert_eq!(kv.layout_id(), "de");
        }
        {
            let selection = ui::LocalState::new("French (AZERTY)".to_string());
            let mut kv = KeyboardView::new(&selection);
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "fr");
            assert!(!kv.take_structure_dirty(), "nothing changed yet");

            selection.set("Dvorak".to_string());
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "dvorak");
            assert!(kv.take_structure_dirty(), "a new layout is laid out again");
        }
        {
            let selection = ui::LocalState::new("Nonsense".to_string());
            let mut kv = KeyboardView::new(&selection);
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "us");
        }
        {
            let kv = KeyboardView::new(&ui::LocalState::new(String::new()));
            assert!(!kv.is_focusable());
            assert!(kv.children().is_empty());
        }
    }
}
