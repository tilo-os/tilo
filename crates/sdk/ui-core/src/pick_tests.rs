use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use super::*;
use crate::Theme;
use crate::a11y::{AccessBounds, AccessNode, AccessState, Role};

/// A laid-out widget with fixed geometry and semantics.
struct Node {
    name: &'static str,
    rect: Rect,
    access: Option<AccessNode>,
    clip: Option<Rect>,
    kids: Vec<Node>,
}

fn node(name: &'static str, rect: Rect, access: Option<AccessNode>, kids: Vec<Node>) -> Node {
    Node {
        name,
        rect,
        access,
        clip: None,
        kids,
    }
}

impl Widget for Node {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn child_clip_rect(&self) -> Option<Rect> {
        self.clip
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.access.clone()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.kids.iter().map(|kid| kid as &dyn Widget).collect()
    }
}

fn text(name: &str, interface: bool) -> Option<AccessNode> {
    Some(
        AccessNode::new(Role::StaticText)
            .with_name(name)
            .with_interface_name(interface),
    )
}

/// A chat window: a heading, a list of two message rows (author, text,
/// a Reply button each) and a composer field.
fn window() -> Node {
    let row = |y: f64, author: &str, said: &str| {
        node(
            "Row",
            Rect::new(0.0, y, 400.0, y + 50.0),
            Some(AccessNode::new(Role::ListItem).with_semantic_key(format!("m{y}"))),
            vec![
                node(
                    "Text",
                    Rect::new(10.0, y, 100.0, y + 20.0),
                    text(author, false),
                    vec![],
                ),
                node(
                    "Text",
                    Rect::new(10.0, y + 20.0, 300.0, y + 45.0),
                    text(said, false),
                    vec![],
                ),
                node(
                    "Button",
                    Rect::new(320.0, y + 10.0, 390.0, y + 40.0),
                    Some(
                        AccessNode::new(Role::Button)
                            .with_name("Reply")
                            .with_interface_name(true),
                    ),
                    vec![],
                ),
            ],
        )
    };
    let mut list = node(
        "List",
        Rect::new(0.0, 40.0, 400.0, 160.0),
        Some(
            AccessNode::new(Role::List)
                .with_name("Messages")
                .with_interface_name(true),
        ),
        vec![
            row(50.0, "Ada Lovelace", "Thanks Ada for the fix"),
            row(100.0, "Grace Hopper", "Looks good"),
            // Scrolled out of the list's clip.
            row(170.0, "Hidden Person", "Not on screen"),
        ],
    );
    list.clip = Some(Rect::new(0.0, 40.0, 400.0, 160.0));
    node(
        "Window",
        Rect::new(0.0, 0.0, 400.0, 300.0),
        None,
        vec![
            node(
                "Text",
                Rect::new(0.0, 0.0, 200.0, 30.0),
                text("General", false),
                vec![],
            ),
            list,
            node(
                "Field",
                Rect::new(0.0, 250.0, 400.0, 290.0),
                Some(
                    AccessNode::new(Role::TextInput)
                        .with_name("Message #general")
                        .with_value("a draft with secrets"),
                ),
                vec![],
            ),
            node(
                "Password",
                Rect::new(0.0, 200.0, 200.0, 240.0),
                Some(
                    AccessNode::new(Role::TextInput)
                        .with_name("Password")
                        .with_state(AccessState {
                            protected: true,
                            ..AccessState::default()
                        }),
                ),
                vec![],
            ),
        ],
    )
}

#[test]
fn the_deepest_named_node_is_picked_with_its_chain_to_widen_along() {
    let window = window();
    let chain = targets_at(&window, Point::new(50.0, 80.0));
    let names: Vec<_> = chain.iter().map(|target| target.role_name()).collect();
    assert_eq!(names, ["List", "ListItem", "StaticText"]);
    let deepest = chain.last().unwrap();
    assert_eq!(deepest.name.as_deref(), Some("Thanks Ada for the fix"));
    assert!(!deepest.interface);
    assert_eq!(deepest.path, "Window[0]/List[0]/Row[0]/Text[1]");
    assert_eq!(deepest.rect, Rect::new(10.0, 70.0, 300.0, 95.0));
    let reply = targets_at(&window, Point::new(350.0, 120.0));
    let reply = reply.last().unwrap();
    assert_eq!(reply.role, Role::Button);
    assert!(reply.interface);
    assert_eq!(reply.path, "Window[0]/List[0]/Row[1]/Button[0]");
}

#[test]
fn clipped_children_and_empty_space_pick_nothing_below() {
    let window = window();
    let below = targets_at(&window, Point::new(50.0, 185.0));
    assert!(
        below
            .iter()
            .all(|target| target.name.as_deref() != Some("Not on screen")),
        "{below:?}"
    );
    assert!(targets_at(&window, Point::new(399.0, 299.0)).is_empty());
}

#[test]
fn a_dragged_rectangle_picks_the_outermost_named_nodes_inside_it() {
    let window = window();
    let rows = targets_within(&window, Rect::new(0.0, 45.0, 400.0, 155.0), 6);
    let roles: Vec<_> = rows.iter().map(|target| target.role).collect();
    assert_eq!(roles, [Role::ListItem, Role::ListItem]);
    let whole = targets_within(&window, Rect::new(-1.0, -1.0, 401.0, 301.0), 6);
    let roles: Vec<_> = whole.iter().map(|target| target.role_name()).collect();
    assert_eq!(roles, ["StaticText", "List", "TextInput", "TextInput"]);
    assert_eq!(
        targets_within(&window, Rect::new(-1.0, -1.0, 401.0, 301.0), 2).len(),
        2
    );
}

#[test]
fn capture_reads_texts_in_order_classified_and_never_a_protected_value() {
    let window = window();
    let row = capture(&window, "Window[0]/List[0]/Row[0]").unwrap();
    assert_eq!(row.label, None);
    let texts: Vec<_> = row
        .texts
        .iter()
        .map(|text| (text.text.as_str(), text.interface))
        .collect();
    assert_eq!(
        texts,
        [
            ("Ada Lovelace", false),
            ("Thanks Ada for the fix", false),
            ("Reply", true)
        ]
    );
    assert_eq!(row.texts[0].rect, Some(Rect::new(10.0, 50.0, 100.0, 70.0)));
    assert_eq!(row.role, Some(Role::ListItem));
    let field = capture(&window, "Window[0]/Field[0]").unwrap();
    assert_eq!(field.label.unwrap().text, "Message #general");
    assert_eq!(field.texts[0].text, "a draft with secrets");
    assert!(!field.texts[0].interface, "a field's contents are content");
    let password = capture(&window, "Window[0]/Password[0]").unwrap();
    assert!(password.texts.is_empty(), "{password:?}");
    assert!(capture(&window, "Window[0]/List[0]/Row[7]").is_none());
}

#[test]
fn synthetic_rows_are_picked_and_captured_by_their_index_path() {
    let mut row = AccessNode::new(Role::ListItem).with_name("Row two");
    row.bounds = Some(AccessBounds {
        x0: 0.0,
        y0: 20.0,
        x1: 100.0,
        y1: 40.0,
    });
    row.children.push({
        let mut cell = AccessNode::new(Role::StaticText).with_name("cell text");
        cell.bounds = Some(AccessBounds {
            x0: 0.0,
            y0: 20.0,
            x1: 50.0,
            y1: 40.0,
        });
        cell
    });
    let mut table = AccessNode::new(Role::List).with_name("Table");
    table.children.push(row);
    let window = node(
        "Table",
        Rect::new(0.0, 0.0, 100.0, 100.0),
        Some(table),
        vec![],
    );
    let chain = targets_at(&window, Point::new(10.0, 30.0));
    assert_eq!(chain.last().unwrap().path, "Table[0]#0/0");
    assert_eq!(chain[1].path, "Table[0]#0");
    let captured = capture(&window, "Table[0]#0").unwrap();
    assert_eq!(captured.label.unwrap().text, "Row two");
    assert_eq!(captured.texts[0].text, "cell text");
}

#[test]
fn text_from_the_catalogue_is_interface_copy_and_literal_or_bound_text_is_content() {
    crate::i18n::I18nManager::init("en-US");
    crate::i18n::I18nManager::add_resource("en-US", "pick-test-heading = Settings\n".into());
    let heading = crate::widget::Text::new("pick-test-heading");
    let node = Widget::accessibility(&heading).unwrap();
    assert_eq!(node.name.as_deref(), Some("Settings"));
    assert!(node.interface_name);
    let literal = crate::widget::Text::new("Ada Lovelace");
    assert!(!Widget::accessibility(&literal).unwrap().interface_name);
    let argued = crate::widget::Text::new("pick-test-heading").with_arg("name", "Ada");
    assert!(!Widget::accessibility(&argued).unwrap().interface_name);
    let button = crate::widget::Button::new("b", "pick-test-heading");
    assert!(Widget::accessibility(&button).unwrap().interface_name);
    let named = crate::widget::Button::new("b", "Ada's file");
    assert!(!Widget::accessibility(&named).unwrap().interface_name);
}

#[test]
fn a_picture_is_masked_over_content_images_and_secrets_not_interface_copy() {
    let window = window();
    let whole = Rect::new(0.0, 0.0, 400.0, 300.0);
    let row = sensitive_areas(&window, "Window[0]/List[0]/Row[0]", whole);
    assert_eq!(
        row,
        [
            Rect::new(10.0, 50.0, 100.0, 70.0),
            Rect::new(10.0, 70.0, 300.0, 95.0)
        ],
        "the author and message, not the Reply button"
    );
    let password = sensitive_areas(&window, "Window[0]/Password[0]", whole);
    assert_eq!(password, [Rect::new(0.0, 200.0, 200.0, 240.0)]);
    assert_eq!(
        sensitive_areas(&window, "Window[0]/Gone[0]", whole),
        [whole],
        "a part no longer there is masked whole"
    );
}

fn placed(role: Role, name: &str, (x0, y0, x1, y1): (f64, f64, f64, f64)) -> AccessNode {
    let mut node = AccessNode::new(role).with_name(name);
    node.bounds = Some(AccessBounds { x0, y0, x1, y1 });
    node
}

/// A message row in a synthetic list: its name, and parts that say where
/// the author, the avatar and each line of the text are drawn.
fn message_list(lines: &[&str]) -> Node {
    let mut row = placed(
        Role::ListItem,
        "Ada: Thanks Grace for the tip",
        (0.0, 0.0, 400.0, 60.0),
    );
    row.children
        .push(placed(Role::Image, "Ada", (8.0, 8.0, 32.0, 32.0)));
    row.children
        .push(placed(Role::StaticText, "Ada", (40.0, 8.0, 80.0, 24.0)));
    let mut top = 28.0;
    for line in lines {
        row.children.push(placed(
            Role::StaticText,
            line,
            (40.0, top, 200.0, top + 14.0),
        ));
        top += 16.0;
    }
    row.children
        .push(placed(Role::Button, "Reply", (360.0, 8.0, 392.0, 24.0)).with_interface_name(true));
    let mut list = AccessNode::new(Role::List).with_name("Messages");
    list.children.push(row);
    node(
        "Chat",
        Rect::new(0.0, 0.0, 400.0, 100.0),
        Some(list),
        vec![],
    )
}

#[test]
fn a_row_its_parts_spell_out_is_masked_part_by_part() {
    let window = message_list(&["Thanks Grace", "for the tip"]);
    let whole = Rect::new(0.0, 0.0, 400.0, 60.0);
    assert_eq!(
        sensitive_areas(&window, "Chat[0]#0", whole),
        [
            Rect::new(8.0, 8.0, 32.0, 32.0),
            Rect::new(40.0, 8.0, 80.0, 24.0),
            Rect::new(40.0, 28.0, 200.0, 42.0),
            Rect::new(40.0, 44.0, 200.0, 58.0),
        ],
        "the avatar, the author and each line; the row's ground and Reply stay"
    );
}

#[test]
fn a_row_whose_parts_leave_words_out_is_masked_whole() {
    // The second line is drawn by the row itself: nothing says where.
    let window = message_list(&["Thanks Grace"]);
    let whole = Rect::new(0.0, 0.0, 400.0, 60.0);
    let areas = sensitive_areas(&window, "Chat[0]#0", whole);
    assert_eq!(areas[0], Rect::new(0.0, 0.0, 400.0, 60.0), "{areas:?}");
}
