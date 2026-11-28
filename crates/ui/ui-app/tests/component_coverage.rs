//! Every described component settles its layout: laid out and left alone it asks for none, and a
//! change to its props lays it out once more and settles again; and the gallery shows each.
use std::collections::HashMap;
use std::path::Path;

use taffy::{NodeId, Style, TaffyTree};
use tilo_ui_app::component_catalogue::TOOLKIT;
use tilo_ui_app::component_testing::given;
use tilo_ui_app::component_testing::{given_with_views, sample};
use tilo_ui_app::headless::Driver;
use tilo_ui_base::component::Registry;
use tilo_ui_base::component::Value;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Element;
use tilo_ui_base::widget::TaffyTreeExt as _;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::counts::asking_for_layout;
use vello::kurbo::{Point, Rect};

/// The components that cannot settle their layout, each with the reason it cannot: a widget that
/// takes a size from something a frame moves after it was laid out.
const NEVER_SETTLES: &[(&str, &str)] = &[];

/// A window's own frame around the component under test, so its props are given to the instance
/// that stands there, as a window that keeps one gives them.
struct Mounted(Element);

impl Widget for Mounted {
    fn name(&self) -> &'static str {
        "Mounted"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let held = self.0.build_layout(taffy, theme, nodes);
        let style = Style {
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &[held], nodes, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if let Ok(held) = taffy.child_at_index(node, 0) {
            self.0.apply_layout(taffy, held, rect.origin());
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&*self.0]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&*self.0);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut *self.0]
    }

    fn get_rect(&self) -> Rect {
        self.0.get_rect()
    }
}

/// Every described component settles its layout: laid out and left alone, it asks for none, so a
/// frame that handles input lays the window out for nothing; and a change to its props lays it out
/// once more and settles again, so content that changed is measured again without asking forever.
///
/// A leaf that never says what its size rests on asks layout on every frame it is shown, and the
/// window is solved again each time; a leaf that measures its content and stops asking when it
/// changes shows the content clipped at its old size.
#[test]
fn every_described_component_settles_its_layout() {
    let registry = Registry::of(&[
        &TOOLKIT,
        &tilo_ui_charts::COMPONENTS,
        &tilo_ui_map::COMPONENTS,
    ]);
    let mut problems = Vec::new();
    for entry in registry.entries() {
        let description = (entry.describe)();
        let name = description.name.as_str();
        if let Some((_, why)) = NEVER_SETTLES.iter().find(|(known, _)| *known == name) {
            assert!(!why.trim().is_empty(), "{name} says why it cannot settle");
            continue;
        }
        let required: Vec<(&str, Value)> = description
            .props
            .iter()
            .filter(|prop| prop.required)
            .map(|prop| (prop.name.as_str(), sample(&prop.ty)))
            .collect();
        let mut source = given("k", &required);
        let element = match (entry.build)(&mut source) {
            Ok(element) => element,
            Err(error) => {
                problems.push(format!("{name} builds from its props: {error}"));
                continue;
            }
        };
        let mut host = Driver::for_root(
            tilo_ui_app::State::new(),
            Mounted(element),
            (900, 600),
            Theme::dark(),
        );
        for _ in 0..3 {
            host.relayout();
        }
        let asking = asking_for_layout(host.root_widget());
        if !asking.is_empty() {
            problems.push(format!("{name} asks layout once it is settled: {asking:?}"));
        }
        // A change to its content: every prop it takes, filled, and a view in the parts it asks
        // for. A widget built again by the change is mounted again, as production does.
        let every: Vec<(&str, Value)> = description
            .props
            .iter()
            .map(|prop| (prop.name.as_str(), sample(&prop.ty)))
            .collect();
        let mut next = given_with_views("k", &every, 1);
        let taken = {
            let mut root = host.root_mut::<Mounted>();
            (entry.update)(&mut root.0, &mut next)
        };
        match taken {
            Ok(true) => {}
            Ok(false) => {
                let mut rebuilt = given_with_views("k", &every, 1);
                let Ok(element) = (entry.build)(&mut rebuilt) else {
                    problems.push(format!("{name} builds again from every prop it takes"));
                    continue;
                };
                host.root_mut::<Mounted>().replace(Mounted(element));
            }
            Err(error) => {
                problems.push(format!(
                    "{name} takes every prop it takes where it stands: {error}"
                ));
                continue;
            }
        }
        for _ in 0..3 {
            host.relayout();
        }
        let asking = asking_for_layout(host.root_widget());
        if !asking.is_empty() {
            problems.push(format!(
                "{name} asks layout after its props changed: {asking:?}"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "these components do not settle their layout; a leaf asks until its box is given, and one \
         that measures its content asks again when that content changes:\n  {}",
        problems.join("\n  ")
    );
}

/// The gallery app (`apps/tl/gallery`) shows every described component of the toolkit and the charts as
/// `ui.<Name>`, so a widget described here is seen in one window the day it is written, and
/// tilolang's check lays it out there. A component a window cannot meaningfully show says so at
/// its declaration (`#[component(not_in_gallery = "…")]`).
#[test]
fn the_gallery_shows_every_described_component() {
    let sources = gallery_sources();
    let catalogue = tilo_ui_base::component::Registry::of(&[
        &TOOLKIT,
        &tilo_ui_charts::COMPONENTS,
        &tilo_ui_map::COMPONENTS,
    ])
    .catalogue();
    let coverage = tilo_ui_base::component::gallery::coverage(&catalogue, &sources);
    let problems = coverage.problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    for name in ["Button", "Section", "Text"] {
        assert!(
            tilo_ui_base::component::gallery::names(&sources, name),
            "the gallery's sources are read: `ui.{name}` is found in them"
        );
    }
}

fn gallery_sources() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/gallery");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("the gallery app reads at {}: {error}", dir.display()))
        .map(|entry| entry.expect("the gallery reads").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "tl"))
        .collect();
    entries.sort();
    let mut sources = String::new();
    for path in entries {
        sources.push_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{} reads: {error}", path.display())),
        );
    }
    sources
}
