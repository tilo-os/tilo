//! Keys reach the window with the keyboard, never a window beside it: each app reads every
//! key it is handed as its own, as a window of its own does.

use std::cell::Cell;
use std::cell::*;
use std::collections::HashMap;
use std::collections::*;
use std::path::*;
use std::rc::Rc;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::style::Style;
use taffy::{NodeId, TaffyTree};
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_app::widget::{GroupRow, TabDescriptor, TileContent, WindowGroup, WindowWidth};
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use tilo_ui_base::widget::{TaffyTreeExt, Widget};
use winit::event::*;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::*;

struct EveryKey {
    keys: Rc<Cell<u32>>,
    focused: bool,
}

impl Widget for EveryKey {
    fn name(&self) -> &str {
        "EveryKey"
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            self.keys.set(self.keys.get() + 1);
            *ctx.handled = true;
        }
        // A press anywhere in it takes the keyboard, as Chat's composer does.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
        {
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
        }
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        tree.widget_leaf(Style::default(), nodes, self)
    }
}

impl tilo_ui_app::widget::AppWidget for EveryKey {
    fn app_kind(&self) -> tilo_ui_app::widget::AppKind {
        tilo_ui_app::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Every key"
    }
    fn app_icon(&self) -> tilo_ui_app::widget::IconDescriptor {
        tilo_ui_app::widget::IconDescriptor::Glyph("K")
    }
}

fn two_windows(holds: [bool; 2]) -> (Driver, [Rc<Cell<u32>>; 2]) {
    let keys = [Rc::new(Cell::new(0)), Rc::new(Cell::new(0))];
    let groups = (0..2u64)
        .map(|i| {
            let tab = i + 1;
            let mut group = WindowGroup::new(
                (i + 1) * 100,
                vec![(
                    TabDescriptor::new(tab, format!("T{tab}")),
                    TileContent::App(Box::new(EveryKey {
                        keys: Rc::clone(&keys[usize::from(i == 1)]),
                        focused: holds[usize::from(i == 1)],
                    })),
                )],
                tab,
                WindowWidth::Normal,
            );
            let _ = group.set_focused(i == 1);
            group
        })
        .collect();
    let mut harness = Driver::for_root(
        tilo_ui_app::State::new(),
        GroupRow::new(groups),
        (1200, 800),
        Theme::dark(),
    );
    harness.relayout();
    (harness, keys)
}

#[test]
fn a_letter_reaches_the_focused_window_not_the_one_before_it() {
    let (mut harness, [first, second]) = two_windows([false, false]);
    harness.key("r");
    assert_eq!(
        first.get(),
        0,
        "the window beside the focused one is not handed the key"
    );
    assert_eq!(second.get(), 1, "the focused window's app takes it");
}

#[test]
fn a_letter_follows_the_focused_control_into_its_window() {
    let (mut harness, [first, second]) = two_windows([true, false]);
    harness.key("r");
    assert_eq!(first.get(), 1);
    assert_eq!(second.get(), 0);
}

/// A window whose app is one text field, as Tasks' "What needs doing" row is.
struct Field {
    input: tilo_ui_app::widget::TextInput,
}

impl tilo_ui_base::widget::Decorator for Field {
    type Inner = tilo_ui_app::widget::TextInput;
    fn inner(&self) -> &tilo_ui_app::widget::TextInput {
        &self.input
    }
    fn inner_mut(&mut self) -> &mut tilo_ui_app::widget::TextInput {
        &mut self.input
    }
}

impl tilo_ui_app::widget::AppWidget for Field {
    fn app_kind(&self) -> tilo_ui_app::widget::AppKind {
        tilo_ui_app::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Field"
    }
    fn app_icon(&self) -> tilo_ui_app::widget::IconDescriptor {
        tilo_ui_app::widget::IconDescriptor::Glyph("F")
    }
}

/// A window whose app reads every key it is handed (Chat), and beside it a window with a text
/// field (Tasks), in strip order `chat_first` or the other way round. Chat has the keyboard.
fn chat_and_a_field(chat_first: bool) -> (Driver, Rc<Cell<u32>>) {
    let keys = Rc::new(Cell::new(0));
    let mut chat = WindowGroup::new(
        100,
        vec![(
            TabDescriptor::new(1, "Chat"),
            TileContent::App(Box::new(EveryKey {
                keys: Rc::clone(&keys),
                focused: false,
            })),
        )],
        1,
        WindowWidth::Normal,
    );
    let _ = chat.set_focused(true);
    let field = WindowGroup::new(
        200,
        vec![(
            TabDescriptor::new(2, "Tasks"),
            TileContent::App(Box::new(Field {
                input: tilo_ui_app::widget::TextInput::new(vec![
                    tilo_ui_base::presence::PathSegment::Field("title".into()),
                ])
                .with_id("title"),
            })),
        )],
        2,
        WindowWidth::Normal,
    );
    let groups = if chat_first {
        vec![chat, field]
    } else {
        vec![field, chat]
    };
    let mut harness = Driver::for_root(
        tilo_ui_app::State::new(),
        GroupRow::new(groups),
        (1600, 800),
        Theme::dark(),
    );
    harness.relayout();
    (harness, keys)
}

/// The words in the control with the keyboard, as assistive technology reads them.
fn focused_words(harness: &Driver) -> Option<String> {
    harness.focused_access_node().and_then(|node| node.value)
}

/// What the person typed into the field they clicked, Enter included, stays the field's: none of
/// it reaches the window that had the keyboard before, whichever side of the field it stands.
fn typing_follows_the_clicked_field(chat_first: bool) {
    let (mut harness, chat_keys) = chat_and_a_field(chat_first);
    // Typed into Chat first: Chat has the keyboard and reads it.
    let chat_at = if chat_first { 300.0 } else { 1300.0 };
    harness.click_at(chat_at, 400.0);
    harness.relayout();
    harness.type_text("hi");
    assert_eq!(
        chat_keys.get(),
        2,
        "Chat reads what is typed while it has the keyboard"
    );
    chat_keys.set(0);

    harness.click_named("title").unwrap();
    harness.relayout();
    harness.type_text("buy milk");
    harness.key("Enter");
    assert_eq!(
        chat_keys.get(),
        0,
        "no key typed into the clicked field, Enter included, reaches the window beside it"
    );
    assert_eq!(focused_words(&harness).as_deref(), Some("buy milk"));
}

#[test]
fn keys_typed_into_a_field_right_of_chat_stay_in_the_field() {
    typing_follows_the_clicked_field(true);
}

/// Chat has the keyboard and has been typed into; then the shell gives the keyboard to the window
/// beside it with no click there (a search chose it, it just opened, or the window in front
/// closed). What the person types next, Enter included, never reaches Chat, though the window
/// they went to has no control holding the keyboard yet (its app is still on its way).
fn typing_after_the_keyboard_moves_away_from_chat(chat_first: bool) {
    let (mut harness, chat_keys) = chat_and_a_field(chat_first);
    let (chat, field) = if chat_first { (0, 1) } else { (1, 0) };
    let chat_at = if chat_first { 300.0 } else { 1300.0 };
    harness.click_at(chat_at, 400.0);
    harness.relayout();
    harness.type_text("hi");
    assert_eq!(
        chat_keys.get(),
        2,
        "Chat reads what is typed while it has the keyboard"
    );
    chat_keys.set(0);

    {
        let mut row = harness.root_mut::<GroupRow>();
        let _ = row.groups[chat].set_focused(false);
        let _ = row.groups[field].set_focused(true);
    }
    harness.relayout();
    harness.type_text("Files");
    harness.key("Enter");
    assert_eq!(
        chat_keys.get(),
        0,
        "no key typed after the keyboard moved, Enter included, reaches the window it left"
    );
    assert_eq!(
        harness.inspection().keyboard_focus_owner,
        None,
        "no control of the window left keeps the keyboard"
    );
}

#[test]
fn keys_after_the_keyboard_moves_right_of_chat_stay_out_of_chat() {
    typing_after_the_keyboard_moves_away_from_chat(true);
}
