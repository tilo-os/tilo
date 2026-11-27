//! Every shared control that runs on a completed click completes a press across a rebuild that put an
//! equivalent control in its place, and only there; and the headless host replays a pointer a relayout
//! slid a control under.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui_base::i18n::widget_text;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};

use tilo_ui::headless::Driver;
use tilo_ui_base::icons::Icon;

use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::EventCtx, widget::TaffyTreeExt as _, widget::Widget};

type Clicks = Rc<RefCell<Vec<&'static str>>>;
type Staged = Rc<RefCell<Option<Box<dyn Widget>>>>;
type Build = fn(&Clicks, &'static str) -> Box<dyn Widget>;

const VIEWPORT: (u32, u32) = (480, 320);

fn moved(point: Point) -> WindowEvent {
    tilo_ui_base::synthetic_input::cursor_moved(point.x, point.y)
}

fn button(state: ElementState) -> WindowEvent {
    tilo_ui_base::synthetic_input::mouse_input(state, MouseButton::Left)
}

fn log(clicks: &Clicks, generation: &'static str) -> impl FnMut() + 'static {
    let clicks = Rc::clone(clicks);
    move || clicks.borrow_mut().push(generation)
}

/// Shows one control `offset` down and swaps in the staged replacement on its next structural poll, as
/// an app rebuilding its content from a worker result does.
struct Shows {
    child: Box<dyn Widget>,
    staged: Staged,
    offset: Rc<Cell<f64>>,
    replaced: bool,
    rect: Rect,
}

impl Widget for Shows {
    fn take_structure_dirty(&mut self) -> bool {
        if let Some(child) = self.staged.borrow_mut().take() {
            self.child = child;
            self.replaced = true;
        }
        std::mem::take(&mut self.replaced) | self.child.take_structure_dirty()
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::length(360.0),
                height: taffy::Dimension::auto(),
            },
            ..taffy::Style::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)));
        if let Some(child) = taffy
            .children(node)
            .ok()
            .and_then(|children| children.first().copied())
        {
            let at = origin + Vec2::new(20.0, 20.0 + self.offset.get());
            self.child.apply_layout(taffy, child, at);
        }
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.child.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.child.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
    fn name(&self) -> &'static str {
        "Shows"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

struct Stage {
    harness: Driver,
    staged: Staged,
    offset: Rc<Cell<f64>>,
    clicks: Clicks,
    build: Build,
}

impl Stage {
    fn new(build: Build) -> Self {
        let clicks = Clicks::default();
        let staged = Staged::default();
        let offset = Rc::new(Cell::new(0.0));
        let root = Shows {
            child: build(&clicks, "first"),
            staged: Rc::clone(&staged),
            offset: Rc::clone(&offset),
            replaced: false,
            rect: Rect::ZERO,
        };
        let harness = Driver::new(tilo_ui::State::new(), Box::new(root), VIEWPORT);
        Self {
            harness,
            staged,
            offset,
            clicks,
            build,
        }
    }

    fn point_at(&mut self, point: Point) {
        self.harness.send(&moved(point));
        self.harness.relayout();
    }

    /// Swap in a fresh, equivalent control `offset` below the first and run the frame that shows it, as
    /// a worker result landing does.
    fn rebuild(&mut self, offset: f64) {
        *self.staged.borrow_mut() = Some((self.build)(&self.clicks, "rebuilt"));
        self.offset.set(offset);
        self.harness.relayout();
        assert!(self.staged.borrow().is_none(), "the frame swapped it in");
    }

    fn press_rebuild_release(&mut self, offset: f64) -> Vec<&'static str> {
        self.harness.send(&button(ElementState::Pressed));
        assert!(
            self.clicks.borrow().is_empty(),
            "nothing runs before the release"
        );
        self.rebuild(offset);
        self.harness.send(&button(ElementState::Released));
        self.clicks.borrow().clone()
    }
}

/// Whether `widget` or a widget inside it shows the pointer hand at `point`: where a person sees
/// something to press.
fn pressable_at(widget: &dyn Widget, point: Point) -> bool {
    let mut found = widget.cursor_shape(point) == Some(winit::window::CursorIcon::Pointer);
    widget.for_each_child(&mut |child| found |= pressable_at(child, point));
    found
}

fn pressable_centre(harness: &Driver) -> Point {
    let mut hits = Vec::new();
    for y in (0..VIEWPORT.1).step_by(2) {
        for x in (0..VIEWPORT.0).step_by(2) {
            let point = Point::new(f64::from(x), f64::from(y));
            if pressable_at(harness.root_widget(), point) {
                hits.push(point);
            }
        }
    }
    let first = hits
        .first()
        .copied()
        .expect("the scene shows a pressable target");
    let bounds = hits
        .iter()
        .fold(Rect::from_points(first, first), |r, p| r.union_pt(*p));
    bounds.center()
}

fn centre_of(harness: &Driver, id: &str) -> Point {
    harness
        .targets()
        .get(id)
        .unwrap_or_else(|| panic!("no target {id}"))
        .center()
}

/// The control rebuilt at the same place under the held button completes the click; rebuilt a few
/// pixels away, still under the pointer, it does not.
fn assert_press_carries(build: Build, locate: impl Fn(&mut Driver) -> Point) {
    let mut scene = Stage::new(build);
    let point = locate(&mut scene.harness);
    scene.point_at(point);
    assert_eq!(
        scene.press_rebuild_release(0.0),
        vec!["rebuilt"],
        "a rebuild between press and release completes the click"
    );

    let mut scene = Stage::new(build);
    let point = locate(&mut scene.harness);
    scene.point_at(point);
    assert!(
        scene.press_rebuild_release(3.0).is_empty(),
        "a control that moved does not complete its predecessor's press"
    );
}

#[test]
fn a_button_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(tilo_ui::widget::Button::new("save", "Save").on_click(log(clicks, generation)))
        },
        |h| centre_of(h, "save"),
    );
}

#[test]
fn a_list_row_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                tilo_ui::widget::ListRow::new("inbox", "Inbox").on_open(log(clicks, generation)),
            )
        },
        |h| centre_of(h, "inbox"),
    );
}

#[test]
fn a_list_row_action_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(tilo_ui::widget::ListRow::new("inbox", "Inbox").action(
                Icon::Trash,
                "Delete",
                log(clicks, generation),
            ))
        },
        |h| {
            let row = centre_of(h, "inbox");
            h.send(&moved(row));
            h.relayout();
            pressable_centre(h)
        },
    );
}

#[test]
fn a_list_row_checkbox_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            let mut ran = log(clicks, generation);
            Box::new(
                tilo_ui::widget::ListRow::new("inbox", "Inbox").checkbox(false, move |_| ran()),
            )
        },
        |h| pressable_centre(h),
    );
}

#[test]
fn a_chip_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                tilo_ui::widget::Chip::new("Urgent")
                    .with_id("urgent")
                    .on_open(log(clicks, generation)),
            )
        },
        |h| centre_of(h, "urgent"),
    );
}

#[test]
fn a_card_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                tilo_ui::widget::Card::new(Box::new(tilo_ui::widget::Text::new("Trip")))
                    .on_activate("trip", log(clicks, generation)),
            )
        },
        |h| centre_of(h, "trip"),
    );
}

#[test]
fn an_interactive_container_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                tilo_ui::widget::Container::new()
                    .with_size(200.0, 60.0)
                    .interactive("channel", log(clicks, generation)),
            )
        },
        |h| centre_of(h, "channel"),
    );
}

#[test]
fn a_stat_tile_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                tilo_ui::widget::StatTile::new("Revenue", "1,204")
                    .on_open("revenue", log(clicks, generation)),
            )
        },
        |h| centre_of(h, "revenue"),
    );
}

#[test]
fn a_media_card_and_its_hover_action_carry_their_press_across_a_rebuild() {
    use tilo_ui::widget::{MediaCard, MediaSource};
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                MediaCard::new("album", MediaSource::Kind(Icon::Music))
                    .on_open(log(clicks, generation)),
            )
        },
        |h| centre_of(h, "album"),
    );
    assert_press_carries(
        |clicks, generation| {
            Box::new(
                MediaCard::new("album", MediaSource::Kind(Icon::Music)).on_hover_action(
                    Icon::Play,
                    "Play",
                    log(clicks, generation),
                ),
            )
        },
        |h| {
            let card = h.targets()["album"];
            Point::new(card.center().x, card.y0 + card.width() / 2.0)
        },
    );
}

#[test]
fn a_collapsible_section_header_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            let mut ran = log(clicks, generation);
            Box::new(
                tilo_ui::widget::Section::titled(
                    "Details",
                    vec![Box::new(tilo_ui::widget::Text::new("Body"))],
                )
                .on_expand(move |_| ran()),
            )
        },
        |h| pressable_centre(h),
    );
}

#[test]
fn a_property_list_action_carries_its_press_across_a_rebuild() {
    use tilo_ui::widget::{Property, PropertyList};
    assert_press_carries(
        |clicks, generation| {
            Box::new(PropertyList::new(vec![
                Property::new("Website", "tilo.so").action(
                    Icon::ExternalLink,
                    "Open",
                    log(clicks, generation),
                ),
            ]))
        },
        |h| pressable_centre(h),
    );
}

#[test]
fn a_property_value_copy_carries_its_press_across_a_rebuild() {
    use tilo_ui::widget::PropertyValue;
    let services = tilo_ui::process::ProcessServices::null();
    let _host = tilo_ui::process::enter(&services);
    // Each generation shows its own value; the copied one says which ran.
    let build: Build = |_, generation| {
        Box::new(
            PropertyValue::new(format!("key-{generation}"))
                .with_accessible_name("Account key")
                .copyable(),
        )
    };
    let copied = |mut scene: Stage, offset| {
        tilo_ui_base::text_selection::clipboard_copy("");
        scene.press_rebuild_release(offset);
        tilo_ui_base::text_selection::clipboard_paste().unwrap_or_default()
    };
    let mut scene = Stage::new(build);
    let point = pressable_centre(&scene.harness);
    scene.point_at(point);
    assert_eq!(copied(scene, 0.0), "key-rebuilt");

    let mut scene = Stage::new(build);
    scene.point_at(point);
    assert_eq!(copied(scene, 3.0), "", "a moved copy action copies nothing");
}

#[test]
fn a_secret_fields_reveal_toggle_carries_its_press_across_a_rebuild() {
    let build: Build = |_, _| {
        Box::new(
            tilo_ui::widget::TextInput::local()
                .with_secret(true)
                .with_id("passphrase"),
        )
    };
    let hide = widget_text("text-input-hide-password");
    let revealed = |mut scene: Stage, offset| {
        scene.press_rebuild_release(offset);
        scene.harness.relayout();
        scene.harness.a11y_node_named(&hide).is_some()
    };
    let mut scene = Stage::new(build);
    let point = centre_of(&scene.harness, "passphrase_reveal");
    scene.point_at(point);
    assert!(scene.harness.a11y_node_named(&hide).is_none());
    assert!(revealed(scene, 0.0), "the rebuilt toggle reveals the field");

    let mut scene = Stage::new(build);
    scene.point_at(point);
    assert!(!revealed(scene, 3.0), "a moved toggle does not run");
}

#[test]
fn a_control_a_relayout_slides_under_a_still_pointer_takes_the_click() {
    let mut scene = Stage::new(|clicks, generation| {
        Box::new(tilo_ui::widget::Button::new("save", "Save").on_click(log(clicks, generation)))
    });
    let button_at = scene.harness.targets()["save"];
    let below = button_at.center() + Vec2::new(0.0, 120.0);
    scene.point_at(below);
    scene.offset.set(120.0);
    scene.harness.relayout();
    assert_eq!(
        scene.harness.targets()["save"],
        button_at + Vec2::new(0.0, 120.0),
        "the relayout moved the button, no rebuild"
    );
    scene.harness.send(&button(ElementState::Pressed));
    scene.harness.send(&button(ElementState::Released));
    assert_eq!(*scene.clicks.borrow(), vec!["first"]);
}

/// The centre of the place named `name` in a navigation list the harness shows.
fn place_centre(harness: &mut Driver, name: &str) -> Point {
    fn find(node: &tilo_ui_base::a11y::AccessNode, name: &str) -> Option<Point> {
        if node.role == tilo_ui_base::a11y::Role::TreeItem && node.name.as_deref() == Some(name) {
            let b = node.bounds?;
            return Some(Point::new(
                f64::midpoint(b.x0, b.x1),
                f64::midpoint(b.y0, b.y1),
            ));
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    harness.relayout();
    let snapshot = harness.inspection();
    snapshot
        .root
        .nodes()
        .iter()
        .filter_map(|node| node.semantics.as_ref())
        .find_map(|access| find(access, name))
        .unwrap_or_else(|| panic!("no place {name}"))
}

/// A place in a sidebar is chosen by one press, also when the list was built again while the
/// button was down: a page arriving beside it does not swallow the choice.
#[test]
fn a_navigation_place_carries_its_press_across_a_rebuild() {
    assert_press_carries(
        |clicks, generation| {
            let mut ran = log(clicks, generation);
            let places = vec![
                tilo_ui::widget::NavItem::new("inbox", "Inbox"),
                tilo_ui::widget::NavItem::new("drafts", "Drafts"),
            ];
            Box::new(
                tilo_ui::widget::NavList::new(vec![tilo_ui::widget::NavSection::untitled(
                    "places", places,
                )])
                .selected("drafts")
                .on_choose("places", move |_| ran()),
            )
        },
        |h| place_centre(h, "Inbox"),
    );
}
