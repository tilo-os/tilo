//! The window's search on an app embedded in the desktop's window.

use super::theme_repaint_tests::{layout, paint};
use super::*;
use crate::widget::find_bar::{FindMatch, FindOptions, Findable};
use crate::widget::{InPlace, SearchDriver, TaffyTreeExt};
use std::cell::Cell;
use std::rc::Rc;
use taffy::Style;
use vello::kurbo::Rect;

/// A document that replaces as its own editors do: the replacement waits
/// for the frame's settle, where it is written.
struct Document {
    asked: bool,
    written: Rc<Cell<usize>>,
    rect: Rect,
}

impl Findable for Document {
    fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        vec![FindMatch::new(0, query.len())]
    }
    fn reveal(&mut self, _index: usize) {}
    fn clear_find(&mut self) {}
    fn supports_replace(&self) -> bool {
        true
    }
    fn replace(&mut self, _index: usize, _with: &str) -> bool {
        self.asked = true;
        true
    }
    fn replaces_later(&self) -> bool {
        true
    }
}

impl Widget for Document {
    crate::provides!(dyn Findable);
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn settle(&mut self, _edits: &mut crate::widget::Edits<'_>, _path: &automerge::ObjId) {
        if std::mem::take(&mut self.asked) {
            self.written.set(self.written.get() + 1);
        }
    }
    fn name(&self) -> &'static str {
        "Document"
    }
}

#[test]
fn a_replacement_from_the_window_search_is_written_on_the_apps_next_frame() {
    let written = Rc::new(Cell::new(0));
    let app = App::new_with_controlled_inputs(
        crate::state::State::new(),
        Box::new(Document {
            asked: false,
            written: Rc::clone(&written),
            rect: Rect::ZERO,
        }),
    );
    let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
    let theme = Theme::light();
    for _ in 0..4 {
        layout(&mut tile, &theme);
        paint(&mut tile, &theme);
    }
    let mut search = SearchDriver::default();
    search.open(InPlace::Document);
    let replaced = crate::widget::SearchableWindow::visit_search_root(&mut tile, &mut |root| {
        search.drive(root, "draft", FindOptions::default(), None, false);
        search.replace(root, "draft", FindOptions::default(), "final", false)
    });
    assert!(replaced, "the document took the replacement");
    assert_eq!(written.get(), 0, "it waits for the app's frame");
    layout(&mut tile, &theme);
    paint(&mut tile, &theme);
    assert_eq!(written.get(), 1, "the app's next frame writes it");
}
