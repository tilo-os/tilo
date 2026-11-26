//! A reader's exported find fields edit the real find query and replacement.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::Style;
use taffy::prelude::{length, percent};
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

fn tr(key: &str) -> String {
    tilo_ui_base::i18n::widget_format(key, &[])
}

#[derive(Default)]
struct DocFixture {
    text: String,
    found: Vec<FindMatch>,
    current: Option<usize>,
    replaced: Vec<(usize, String)>,
}

impl Findable for DocFixture {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.found.clear();
        if query.is_empty() {
            return Vec::new();
        }
        let (hay, needle) = if options.match_case {
            (self.text.clone(), query.to_owned())
        } else {
            (self.text.to_lowercase(), query.to_lowercase())
        };
        let mut at = 0;
        while let Some(found) = hay.get(at..).and_then(|rest| rest.find(&needle)) {
            let start = at + found;
            let end = start + needle.len();
            let bounded = |i: Option<char>| i.is_none_or(|c| !c.is_alphanumeric());
            if !options.whole_word
                || (bounded(hay.get(..start).and_then(|s| s.chars().last()))
                    && bounded(hay.get(end..).and_then(|s| s.chars().next())))
            {
                self.found.push(FindMatch::new(start, end));
            }
            at = end;
        }
        self.found.clone()
    }
    fn reveal(&mut self, index: usize) {
        self.current = Some(index);
    }
    fn supports_replace(&self) -> bool {
        true
    }
    fn replace(&mut self, index: usize, with: &str) -> bool {
        self.replaced.push((index, with.to_owned()));
        true
    }
}

struct Page {
    doc: DocFixture,
    rect: Rect,
}

impl Findable for Page {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.doc.find(query, options)
    }
    fn reveal(&mut self, index: usize) {
        self.doc.reveal(index);
    }
    fn supports_replace(&self) -> bool {
        self.doc.supports_replace()
    }
    fn replace(&mut self, index: usize, with: &str) -> bool {
        self.doc.replace(index, with)
    }
}

impl Widget for Page {
    tilo_ui_base::provides!(dyn Findable);
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(200.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    tilo_ui_base::rect_geometry!();
    fn name(&self) -> &'static str {
        "page"
    }
}

#[test]
fn hosted_accessibility_edits_the_real_find_query_and_replacement() {
    let page = Page {
        doc: DocFixture {
            text: "one two one".into(),
            ..DocFixture::default()
        },
        rect: Rect::ZERO,
    };
    let mut scope = FindScope::new(Box::new(page)).with_bar(FindBar::new().with_replace());
    scope.open();
    let mut harness = Driver::for_root(tilo_ui::State::new(), scope, (900, 400), Theme::dark());
    let field = |harness: &tilo_ui::app::driver::Driver, name: &str| {
        harness
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.role() == accesskit::Role::TextInput && node.label() == Some(name)
            })
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("missing exported find field {name}"))
    };
    for name in [
        tr("find-bar-placeholder"),
        tr("find-bar-replace-placeholder"),
    ] {
        assert_eq!(
            harness
                .a11y_nodes()
                .into_iter()
                .filter(|(_, node)| {
                    node.role() == accesskit::Role::TextInput && node.label() == Some(name.as_str())
                })
                .count(),
            1,
            "each actual find field has one exported accessibility node"
        );
    }
    let query = field(&harness, &tr("find-bar-placeholder"));
    harness
        .perform_a11y_action(query, &tilo_ui_base::a11y::Action::SetValue("one".into()))
        .expect("the exported query enters the real find scope");
    assert_eq!(harness.root::<FindScope>().bar().query(), "one");
    assert_eq!(harness.root::<FindScope>().bar().position(), Some((1, 2)));
    let query = field(&harness, &tr("find-bar-placeholder"));
    harness
        .perform_a11y_action(query, &tilo_ui_base::a11y::Action::Focus)
        .expect("the exported query can take keyboard focus");
    let query = field(&harness, &tr("find-bar-placeholder"));
    harness
        .perform_a11y_action(
            query,
            &tilo_ui_base::a11y::Action::SetSelection {
                anchor: 0,
                focus: 3,
            },
        )
        .unwrap();
    harness.type_text("two");
    assert_eq!(harness.root::<FindScope>().bar().query(), "two");
    assert_eq!(harness.root::<FindScope>().bar().position(), Some((1, 1)));
    let replacement = field(&harness, &tr("find-bar-replace-placeholder"));
    harness
        .perform_a11y_action(
            replacement,
            &tilo_ui_base::a11y::Action::SetValue("three".into()),
        )
        .unwrap();
    assert_eq!(
        harness.root::<FindScope>().bar().replace_text_for_tests(),
        "three"
    );
    assert_eq!(harness.root::<FindScope>().bar().query(), "two");
    let query = field(&harness, &tr("find-bar-placeholder"));
    assert!(
        harness
            .perform_a11y_action(query, &tilo_ui_base::a11y::Action::ToggleCheck)
            .is_err()
    );
    assert_eq!(harness.root::<FindScope>().bar().query(), "two");
}
