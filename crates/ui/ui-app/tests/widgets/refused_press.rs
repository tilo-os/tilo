//! A Viewer's press on a control whose action their grants refuse: every kind of control bound to
//! an action sends the press, the workspace refuses it, and the window says why. None runs it,
//! none drops it unseen, and a value control keeps the value it held. A call made by anyone but
//! the person at the window is refused where it is made, and never asks the workspace.

#![allow(clippy::unwrap_used, reason = "test: a failure is the assertion")]

/// Where a press lands on the control a person finds by its name.
use ::collaboration::permissions::{MemberId, PermissionContext, Role};
use std::cell::Cell;
use std::rc::Rc;
use tilo_actions::ActionBinding;
use tilo_ui::app::driver::Driver;
use tilo_ui_base::widget::Element;
use vello::kurbo::{Point, Rect};
type Aim = fn(Rect) -> Point;

fn centre(bounds: Rect) -> Point {
    bounds.center()
}

/// A window whose control `control` makes over the send action stands before a Viewer.
fn before_a_viewer<I: 'static>(
    control: impl FnOnce(ActionBinding<I>) -> Element,
) -> (Driver, Rc<Cell<usize>>) {
    let ran = Rc::new(Cell::new(0));
    let counted = Rc::clone(&ran);
    let root = tilo_ui::widget::effect_fixture::sends_through(
        move || counted.set(counted.get() + 1),
        control,
    );
    let mut driver = Driver::new_with_theme(
        tilo_ui::State::new(),
        root,
        (600, 400),
        tilo_ui_base::theme::Theme::dark(),
    );
    driver.connect_peer();
    driver.set_permission_ctx(PermissionContext::member(
        &MemberId::person("ada"),
        Role::Viewer,
    ));
    driver.relayout();
    (driver, ran)
}

fn press(driver: &mut Driver, name: &str, aim: Aim) {
    let bounds = driver
        .semantic_tree()
        .find_by_name(name)
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("the window shows {name:?}"));
    let at = aim(bounds);
    driver.click_at(at.x, at.y);
    driver.relayout();
}

/// What the control named `name` shows as its value, or its check.
fn shown(driver: &mut Driver, name: &str) -> (Option<String>, Option<bool>) {
    driver
        .semantic_tree()
        .find_by_name(name)
        .map(|node| (node.value.clone(), node.state.checked))
        .unwrap_or_else(|| panic!("the window shows {name:?}"))
}

/// The Viewer presses the controls named `presses` in turn: the send never runs, the window says
/// why, and the control the first press names shows what it showed before.
fn refused_out_loud<I: 'static>(
    control: impl FnOnce(ActionBinding<I>) -> Element,
    presses: &[(&str, Aim)],
) -> Driver {
    let (mut driver, ran) = before_a_viewer(control);
    let held = shown(&mut driver, presses[0].0);
    for (name, aim) in presses {
        press(&mut driver, name, *aim);
    }
    assert_eq!(
        shown(&mut driver, presses[0].0),
        held,
        "the control keeps what it held"
    );
    assert_eq!(
        ran.get(),
        0,
        "a Viewer's press on {presses:?} sends nothing"
    );
    assert!(
        driver.refusal_said().is_some(),
        "the press on {:?} is refused out loud: {:?}",
        presses.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        driver.control_failures()
    );
    driver
}

#[test]
fn a_refused_checkbox_says_why_and_keeps_its_mark() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::Checkbox::given(false)
                    .with_label("Send")
                    .on_action(binding),
            )
        },
        &[("Send", centre)],
    );
}

#[test]
fn a_refused_switch_says_why() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::Toggle::given("send", false)
                    .with_label("Send")
                    .on_action(binding),
            )
        },
        &[("Send", centre)],
    );
}

#[test]
fn a_refused_menu_item_says_why() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::RowMenu::new("more", "More for Outbox").action("Send", binding),
            )
        },
        &[("More for Outbox", centre), ("Send", centre)],
    );
}

#[test]
fn a_refused_segment_says_why() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::ToggleGroup::new(
                    vec![
                        tilo_ui::widget::Segment::new("Hold"),
                        tilo_ui::widget::Segment::new("Send"),
                    ],
                    0,
                )
                .on_action(binding),
            )
        },
        &[("Send", centre)],
    );
}

#[test]
fn a_refused_link_says_why() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::Link::new("https://example.org")
                    .with_label("Send")
                    .on_action(binding),
            )
        },
        &[("Send", centre)],
    );
}

#[test]
fn a_refused_chip_says_why() {
    refused_out_loud(
        |binding| Box::new(tilo_ui::widget::Chip::new("Send").on_action(binding)),
        &[("Send", centre)],
    );
}

#[test]
fn a_refused_toolbar_command_says_why() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::Toolbar::new(tilo_ui::widget::toolbar::Orientation::Horizontal)
                    .push_button("Send", || {})
                    .with_action(binding),
            )
        },
        &[("Send", centre)],
    );
}

#[test]
fn a_refused_colour_swatch_says_why() {
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::ColorSwatchPicker::new(
                    [
                        tilo_ui::widget::ColorSwatch::named(
                            vello::peniko::Color::from_rgb8(0x20, 0x60, 0xd0),
                            "Blue",
                        ),
                        tilo_ui::widget::ColorSwatch::named(
                            vello::peniko::Color::from_rgb8(0xd0, 0x30, 0x30),
                            "Send",
                        ),
                    ],
                    2,
                )
                .on_action(binding),
            )
        },
        &[("Send", centre)],
    );
}

/// The value a press tries to set is the one sent, and the slider stays where it was.
#[test]
fn a_refused_slider_sends_the_value_pressed_and_stays() {
    /// A quarter along, away from the half the slider starts at.
    fn a_quarter_along(bounds: Rect) -> Point {
        Point::new(bounds.x0 + bounds.width() / 4.0, bounds.center().y)
    }
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::Slider::new("send", "")
                    .with_accessible_name("Send")
                    .on_action(binding),
            )
        },
        &[("Send", a_quarter_along)],
    );
}

/// The person opens the chooser and picks the other choice: it is sent, refused out loud, and the
/// chooser keeps the one it held.
#[test]
fn a_refused_dropdown_sends_the_choice_and_keeps_its_own() {
    let (mut driver, ran) = before_a_viewer(|binding| {
        Box::new(
            tilo_ui::widget::Dropdown::new(vec!["Hold".to_owned(), "Send".to_owned()], "")
                .with_selected("Hold")
                .with_accessible_name("Outbox")
                .on_action(binding),
        )
    });
    let held = shown(&mut driver, "Outbox");
    press(&mut driver, "Outbox", centre);
    assert!(
        driver
            .with_widget_mut::<tilo_ui::widget::Dropdown, _>(|dropdown| dropdown.is_open())
            .unwrap(),
        "the chooser opens for the person's choice"
    );
    press(&mut driver, "Send", centre);
    assert_eq!(ran.get(), 0, "the choice sends nothing");
    assert!(
        driver.refusal_said().is_some(),
        "the choice is refused out loud: {:?}",
        driver.control_failures()
    );
    assert_eq!(
        shown(&mut driver, "Outbox"),
        held,
        "the chooser keeps what it held"
    );
}

#[test]
fn a_refused_number_step_says_why_and_keeps_the_number() {
    fn increase(bounds: Rect) -> Point {
        Point::new(bounds.x1 - 4.0, bounds.center().y)
    }
    refused_out_loud(
        |binding| {
            Box::new(
                tilo_ui::widget::NumberInput::new(1.0)
                    .with_accessible_name("Copies")
                    .on_action(binding),
            )
        },
        &[("Copies", increase)],
    );
}

#[test]
fn a_refused_text_edit_says_why_and_keeps_the_text() {
    let (mut driver, ran) = before_a_viewer(|binding| {
        Box::new(
            tilo_ui::widget::TextInput::new(Vec::new())
                .with_accessible_name("Note")
                .on_action(binding),
        )
    });
    let held = shown(&mut driver, "Note");
    press(&mut driver, "Note", centre);
    driver.type_text("x");
    driver.relayout();
    assert_eq!(ran.get(), 0, "the edit sends nothing");
    assert!(
        driver.refusal_said().is_some(),
        "the edit is refused out loud: {:?}",
        driver.control_failures()
    );
    assert_eq!(
        shown(&mut driver, "Note"),
        held,
        "the field keeps what it held"
    );
}

/// Relays an agent's call to the send action: its `relay` handler, which changes only the
/// document, queues the send on behalf of the call it runs, as a handler continues the call it
/// serves. The send reaches outside, which an Editor's grants only ask about.
struct Relays {
    resource: tilo_actions::ResourceSpec,
    queued: Option<tilo_ui_base::widget::controls::AttributedControlInvocation>,
    sent: Rc<Cell<usize>>,
    child: Element,
}

const SEND: &str = "fixture.send";
const RELAY: &str = "fixture.relay";

fn relays(sent: Rc<Cell<usize>>) -> Element {
    use tilo_actions::{ActionSpec, EncodedAction, OperationKind, ResourceSpec};
    let action = |name: &'static str, operation: &'static str| {
        ActionSpec::typed_operation(
            name,
            name,
            name,
            OperationKind::Effect,
            operation,
            vec![],
            move |_| EncodedAction::encode_operation(operation, &()).unwrap(),
        )
    };
    let resource = ResourceSpec::new("fixture:outbox", "fixture", "Outbox")
        .with_action(action("send", SEND))
        .with_action(action("relay", RELAY).of_kind(tilo_actions::ActionKind::Undoable));
    // The window shows the relay, as an agent finds it there.
    let relay = ActionBinding::new(&resource, &resource.actions[1], |(): ()| {
        serde_json::Map::new()
    })
    .unwrap();
    Box::new(Relays {
        resource,
        queued: None,
        sent,
        child: Box::new(
            tilo_ui::widget::Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    tilo_ui::widget::Button::new("relay", "Relay").on_action(relay),
                )),
        ),
    })
}

impl tilo_ui_base::widget::Widget for Relays {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn tilo_ui::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );
    fn sync(
        &mut self,
        doc: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.child.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn tilo_ui_base::widget::Widget>,
    ) -> taffy::NodeId {
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        self.child.draw(ctx);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        self.child.handle_event(event, ctx);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.child.get_rect()
    }
    fn name(&self) -> &str {
        "Relay fixture"
    }
    fn children(&self) -> Vec<&dyn tilo_ui_base::widget::Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn tilo_ui_base::widget::Widget + '_)> {
        vec![self.child.as_mut()]
    }
}

impl tilo_ui_base::widget::ResourceOperations for Relays {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [SEND.to_owned(), RELAY.to_owned()].into_iter().collect()
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        match op.request().type_id {
            SEND => self.sent.set(self.sent.get() + 1),
            RELAY => {
                let send =
                    ActionBinding::new(&self.resource, &self.resource.actions[0], |(): ()| {
                        serde_json::Map::new()
                    })
                    .unwrap();
                self.queued = Some((send.invoke(()).unwrap(), Some(op.invocation())));
            }
            _ => return Ok(None),
        }
        Ok(Some(
            tilo_ui_base::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}

impl tilo_ui::widget::resources::PublishesResources for Relays {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![self.resource.clone()]
    }
}

impl tilo_ui_base::widget::controls::ActionControls for Relays {
    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<tilo_ui_base::widget::controls::AttributedControlInvocation> {
        self.queued.take()
    }
}

impl tilo_ui::widget::AppWidget for Relays {
    fn app_kind(&self) -> tilo_ui::widget::AppKind {
        tilo_ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Relay fixture"
    }
    fn app_icon(&self) -> tilo_ui::widget::IconDescriptor {
        tilo_ui::widget::IconDescriptor::Glyph("T")
    }
    fn permission_hint(&self) -> Option<tilo_ui::widget::AppKind> {
        None
    }
}

/// An agent's call that goes on to press an action the person would be asked about is refused for
/// the agent: the press is the agent's, not the person's, so the person is never asked about it,
/// the workspace never hears it as the window's own press, and nothing is said, as the person
/// pressed nothing.
#[test]
fn an_agent_s_call_to_a_refused_action_never_asks_the_workspace() {
    let sent = Rc::new(Cell::new(0));
    let mut driver = Driver::new_with_theme(
        tilo_ui::State::new(),
        relays(Rc::clone(&sent)),
        (600, 400),
        tilo_ui_base::theme::Theme::dark(),
    );
    driver.connect_peer();
    driver.set_permission_ctx(PermissionContext::member(
        &MemberId::person("ada"),
        Role::Editor,
    ));
    driver.relayout();
    let send = tilo_actions::ActionAddress::new("fixture:outbox", "send");
    let agent = tilo_actions::Actor::Member(MemberId::agent("scribe", "ada"));
    let relayed = driver.invoke_action_as(
        &agent,
        &tilo_actions::ActionAddress::new("fixture:outbox", "relay"),
        &serde_json::Map::new(),
        None,
    );
    assert!(
        relayed.is_ok(),
        "the agent's own call is delivered: {relayed:?}"
    );
    driver.relayout();
    driver.relayout();
    assert_eq!(
        sent.get(),
        0,
        "the send the agent's call pressed does not run"
    );
    assert_eq!(driver.asking(), None, "nobody is asked about it");
    assert_eq!(driver.refusal_said(), None, "the person pressed nothing");
    let asked_the_workspace = driver.peer_requests().into_iter().any(|request| {
        matches!(
            request,
            tilo_ui_base::collaboration::CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(ref invoke),
                ..
            } if invoke.target.provider == tilo_actions::remote::OWN_PROVIDER
                && invoke.target.address == send
                && invoke.on_behalf_of.is_none()
        )
    });
    assert!(
        !asked_the_workspace,
        "the window never hands the agent's press to the workspace as the person's own"
    );
}
