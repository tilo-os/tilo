//! `Terminal` through the interfaces it is drawn and driven by: a session behind it, window
//! events in front of it, and what a person sees and copies out.

use std::sync::Mutex;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

use crate::terminal::{GridPos, Line, Modes, Placement, Screen, Session, SessionEvent};
use crate::testing::Dispatch;
use crate::{widget::GridEvents, widget::Terminal, widget::TerminalProps};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{
    component::Described as _, component::Event, component::Intent, component::Value,
};
use tilo_ui_base::{
    key_input::Button, key_input::Held, key_input::Key, key_input::Keys, key_input::Named,
};

/// What a session was asked, and the grid it answers with.
#[derive(Default)]
struct Fake {
    state: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    screen: Screen,
    modes: Modes,
    revision: u64,
    events: Vec<SessionEvent>,
    keys: Vec<(Key, Option<String>, Held)>,
    texts: Vec<String>,
    pastes: Vec<String>,
    mice: Vec<(Button, bool, usize, usize)>,
    resizes: Vec<(u16, u16)>,
    focuses: Vec<bool>,
    placements: Vec<Placement>,
    image: Option<Vec<u8>>,
    ended: bool,
    clipboard: Vec<bool>,
}

impl Fake {
    fn with_lines(lines: &[&str]) -> Self {
        let screen = Screen {
            lines: lines
                .iter()
                .map(|text| Line {
                    text: (*text).to_owned(),
                    styles: Vec::new(),
                })
                .collect(),
            ..Screen::default()
        };
        let fake = Self::default();
        fake.state.lock().unwrap().screen = screen;
        fake.state.lock().unwrap().revision = 1;
        fake
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    fn bump(&self, event: SessionEvent) {
        let mut state = self.state();
        state.events.push(event);
        state.revision += 1;
    }
}

impl Keys for Fake {
    fn key(&self, key: Key, text: Option<&str>, held: Held) {
        self.state().keys.push((key, text.map(str::to_owned), held));
    }

    fn text(&self, text: &str) {
        self.state().texts.push(text.to_owned());
    }

    fn paste(&self, text: &str) {
        self.state().pastes.push(text.to_owned());
    }

    fn mouse(&self, button: Button, pressed: bool, _held: Held, column: usize, row: usize) -> bool {
        let mut state = self.state();
        if !state.modes.mouse_report {
            return false;
        }
        state.mice.push((button, pressed, column, row));
        true
    }

    fn focus(&self, focused: bool) {
        self.state().focuses.push(focused);
    }
}

impl Session for Fake {
    fn screen(&self) -> std::sync::Arc<Screen> {
        std::sync::Arc::new(self.state().screen.clone())
    }

    fn modes(&self) -> Modes {
        self.state().modes
    }

    fn revision(&self) -> u64 {
        self.state().revision
    }

    fn resize(&self, columns: u16, rows: u16) {
        self.state().resizes.push((columns, rows));
    }

    fn selection_text(&self, anchor: GridPos, cursor: GridPos) -> Option<String> {
        let screen = self.screen();
        let selection = crate::terminal::GridSelection { anchor, cursor };
        if selection.is_collapsed() {
            return None;
        }
        let mut out = String::new();
        for row in anchor.row.min(cursor.row)..=anchor.row.max(cursor.row) {
            if !out.is_empty() {
                out.push('\n');
            }
            let (from, to) = selection.span_on(row, 200)?;
            let text = screen.lines.get(row).map_or("", |line| line.text.as_str());
            let taken: String = text.chars().skip(from).take(to - from).collect();
            out.push_str(taken.trim_end());
        }
        Some(out)
    }

    fn placements(&self) -> Vec<Placement> {
        self.state().placements.clone()
    }

    fn image_bytes(&self, _hash: &str) -> Option<Vec<u8>> {
        self.state().image.clone()
    }

    fn public_link(&self, url: &str) -> bool {
        url.starts_with("https://") && !url.contains('@') && !url.contains('?')
    }

    fn events(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        let state = self.state();
        if since == 0 && !state.events.is_empty() {
            return (state.events.clone(), 1);
        }
        (Vec::new(), since)
    }

    fn ended(&self) -> bool {
        self.state().ended
    }

    fn allow_clipboard(&self, allow: bool) {
        self.state().clipboard.push(allow);
    }
}

fn placed(widget: &mut Terminal) -> (TaffyTree<()>, NodeId) {
    let mut tree = TaffyTree::<()>::new();
    let mut node_map = std::collections::HashMap::new();
    let node = widget.build_layout(&mut tree, &Theme::dark(), &mut node_map);
    let column = tree
        .new_with_children(
            taffy::Style {
                flex_direction: taffy::FlexDirection::Column,
                size: taffy::Size::from_lengths(400.0, 300.0),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    tree.compute_layout(
        column,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(400.0),
            height: taffy::AvailableSpace::Definite(300.0),
        },
    )
    .unwrap();
    widget.apply_layout(&tree, node, Point::ZERO);
    (tree, node)
}

/// Draw the grid once, so its cell geometry is measured as a frame measures it.
fn paint(widget: &mut Terminal) -> Scene {
    let mut scene = Scene::new();
    widget.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    ));
    scene
}

fn send(widget: &mut Terminal, tree: &TaffyTree<()>, node: NodeId, event: &WindowEvent) -> bool {
    Dispatch::new()
        .layout(tree, node)
        .send(widget, event)
        .handled
}

fn send_with(
    widget: &mut Terminal,
    tree: &TaffyTree<()>,
    node: NodeId,
    event: &WindowEvent,
    modifiers: ModifiersState,
) -> bool {
    Dispatch::new()
        .layout(tree, node)
        .modifiers(modifiers)
        .send(widget, event)
        .handled
}

fn key(key: WKey, state: ElementState) -> WindowEvent {
    tilo_ui_base::synthetic_input::key_input(&key, state)
}

fn moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: (x, y).into(),
    }
}

fn button(state: ElementState, button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button,
    }
}

fn wheel(lines: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, lines),
        phase: TouchPhase::Moved,
    }
}

tilo_test_env::scenario! {
    fn a_terminal_draws_and_drives_the_session_the_window_hands_in;

    phase a_grid_is_the_sessions_screen_and_is_laid_out_at_the_cells_it_has;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ echo hi", "hi"]));
        let mut widget = Terminal::new(fake.clone());
        let (_tree, _node) = placed(&mut widget);
        let scene = paint(&mut widget);
        assert!(!scene.encoding().is_empty(), "the grid painted its ground");
        assert_eq!(widget.screen().lines.len(), 2);
        let state = fake.state();
        assert!(
            state.resizes.last().is_some_and(|(columns, rows)| *columns > 0 && *rows > 0),
            "the pty was told the grid's size: {:?}",
            state.resizes
        );
    }

    phase keystrokes_go_to_the_session_and_never_to_a_document;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ "]));
        let mut widget = Terminal::new(fake.clone());
        widget.set_focus(true);
        let (tree, node) = placed(&mut widget);
        assert!(send(&mut widget, &tree, node, &tilo_ui_base::synthetic_input::char_key("a")));
        assert!(send(&mut widget, &tree, node, &key(WKey::Named(NamedKey::ArrowUp), ElementState::Pressed)));
        let state = fake.state();
        assert_eq!(state.keys[0].0, Key::Char('a'));
        assert_eq!(state.keys[0].1.as_deref(), Some("a"));
        assert_eq!(state.keys[1].0, Key::Named(Named::ArrowUp));
        assert!(state.texts.is_empty(), "a key is a key, not a paste");
        assert!(state.focuses.contains(&true), "the session heard it got the keyboard");
    }

    phase the_terminals_own_chords_copy_and_paste_without_reaching_the_shell;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["selected words"]));
        let mut widget = Terminal::new(fake.clone());
        widget.set_focus(true);
        let (tree, node) = placed(&mut widget);
        paint(&mut widget);
        // Select the first word with a drag, then copy it with the terminal's chord.
        send(&mut widget, &tree, node, &moved(1.0, 1.0));
        send(&mut widget, &tree, node, &button(ElementState::Pressed, MouseButton::Left));
        let fifth = widget.char_w * 5.0;
        send(&mut widget, &tree, node, &moved(fifth, 1.0));
        send(&mut widget, &tree, node, &button(ElementState::Released, MouseButton::Left));
        assert_eq!(
            widget.selected_text().as_deref(),
            Some("selec"),
            "a drag selects the cells it covers"
        );
        let before = fake.state().keys.len();
        send_with(
            &mut widget,
            &tree,
            node,
            &tilo_ui_base::synthetic_input::char_key("c"),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        assert_eq!(fake.state().keys.len(), before, "Ctrl+Shift+C is a copy, not Ctrl+C");
    }

    phase zoom_and_the_desktops_tab_chords_are_the_windows_and_ctrl_t_is_the_shells;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$"]));
        let mut widget = Terminal::new(fake.clone());
        widget.set_focus(true);
        let (tree, node) = placed(&mut widget);
        paint(&mut widget);
        let ctrl = ModifiersState::CONTROL;
        for zoom in ["+", "-", "0"] {
            let taken = send_with(
                &mut widget,
                &tree,
                node,
                &tilo_ui_base::synthetic_input::char_key(zoom),
                ctrl,
            );
            assert!(!taken, "Ctrl+{zoom} zooms the window");
        }
        let tab = key(WKey::Named(NamedKey::Tab), ElementState::Pressed);
        assert!(!send_with(&mut widget, &tree, node, &tab, ctrl), "Ctrl+Tab is the desktop's");
        let reopen = tilo_ui_base::synthetic_input::char_key("t");
        assert!(
            !send_with(&mut widget, &tree, node, &reopen, ctrl | ModifiersState::SHIFT),
            "Ctrl+Shift+T is the desktop's"
        );
        assert!(fake.state().keys.is_empty(), "none of them reached the shell");
        assert!(
            send_with(&mut widget, &tree, node, &reopen, ctrl),
            "Ctrl+T is the shell's"
        );
        assert_eq!(fake.state().keys.len(), 1);
    }

    phase a_drag_selects_in_reading_order_and_a_finished_selection_reports_its_text;
    {
        let intents = tilo_ui_base::Intents::default();
        let fake = std::sync::Arc::new(Fake::with_lines(&["hello world", "second line"]));
        let mut widget = Terminal::new(fake.clone());
        widget.update_events(GridEvents {
            selected: Event::View(Intent::new(&intents, "terminal", "selected")),
            ..GridEvents::default()
        });
        let (tree, node) = placed(&mut widget);
        paint(&mut widget);
        send(&mut widget, &tree, node, &moved(1.0, 1.0));
        send(&mut widget, &tree, node, &button(ElementState::Pressed, MouseButton::Left));
        let y = widget.char_h + 1.0;
        let sixth = widget.char_w * 6.0;
        send(&mut widget, &tree, node, &moved(sixth, y));
        send(&mut widget, &tree, node, &button(ElementState::Released, MouseButton::Left));
        let fired = intents.take();
        assert_eq!(fired.len(), 1, "the finished selection is one report");
        assert_eq!(
            fired[0].value,
            Some(Value::Text("hello world\nsecond".to_owned()))
        );
    }

    phase a_program_that_tracks_the_mouse_is_reported_to_and_otherwise_the_view_selects;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ "]));
        let mut widget = Terminal::new(fake.clone());
        let (tree, node) = placed(&mut widget);
        paint(&mut widget);
        send(&mut widget, &tree, node, &moved(3.0, 3.0));
        send(&mut widget, &tree, node, &button(ElementState::Pressed, MouseButton::Left));
        assert!(fake.state().mice.is_empty(), "with no request the view selects");
        send(&mut widget, &tree, node, &button(ElementState::Released, MouseButton::Left));

        fake.state().modes = Modes {
            mouse_report: true,
            ..Modes::default()
        };
        // A second widget over the same session, so the modes are read afresh.
        let mut tracking = Terminal::new(fake.clone());
        let (tree, node) = placed(&mut tracking);
        paint(&mut tracking);
        send(&mut tracking, &tree, node, &moved(3.0, 3.0));
        send(&mut tracking, &tree, node, &button(ElementState::Pressed, MouseButton::Left));
        let mice = fake.state().mice.clone();
        assert_eq!(
            mice,
            vec![(Button::Left, true, 1, 1)],
            "a tracking program hears a press at the one-based cell"
        );
    }

    phase the_wheel_scrolls_back_and_fresh_output_returns_to_the_live_end;
    {
        let lines: Vec<String> = (0..200).map(|n| format!("line {n}")).collect();
        let names: Vec<&str> = lines.iter().map(String::as_str).collect();
        let fake = std::sync::Arc::new(Fake::with_lines(&names));
        let mut widget = Terminal::new(fake.clone());
        let (tree, node) = placed(&mut widget);
        paint(&mut widget);
        let before = widget.first_row;
        send(&mut widget, &tree, node, &wheel(4.0));
        paint(&mut widget);
        assert!(widget.first_row < before, "the wheel showed older rows");
        fake.bump(SessionEvent::Title("new".to_owned()));
        let doc = automerge::AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        paint(&mut widget);
        assert_eq!(widget.scroll_rows, 0, "fresh output comes back to the live end");
        assert_eq!(
            widget.first_row,
            widget.screen().lines.len().saturating_sub(widget.viewport_rows),
            "a live grid shows its newest rows"
        );
    }

    phase the_grid_reports_the_title_the_marked_runs_the_shells_asks_and_its_end;
    {
        let intents = tilo_ui_base::Intents::default();
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ "]));
        let mut widget = Terminal::new(fake.clone());
        widget.update_events(GridEvents {
            title: Event::View(Intent::new(&intents, "terminal", "title")),
            finished: Event::View(Intent::new(&intents, "terminal", "finished")),
            exit: Event::View(Intent::new(&intents, "terminal", "exit")),
            lost: Event::View(Intent::new(&intents, "terminal", "lost")),
            history: Event::View(Intent::new(&intents, "terminal", "history")),
            held: Event::View(Intent::new(&intents, "terminal", "held")),
            ..GridEvents::default()
        });
        fake.bump(SessionEvent::Title("vim README".to_owned()));
        fake.bump(SessionEvent::Finished("{\"exit\":0}".to_owned()));
        fake.bump(SessionEvent::Exit(3));
        fake.bump(SessionEvent::Lost("the relay went away".to_owned()));
        fake.bump(SessionEvent::History("{\"skip\":0}".to_owned()));
        fake.bump(SessionEvent::Held(String::new()));
        let doc = automerge::AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        let fired = intents.take();
        let values: Vec<Option<Value>> = fired.iter().map(|f| f.value.clone()).collect();
        assert!(values.contains(&Some(Value::Text("vim README".to_owned()))));
        assert!(values.contains(&Some(Value::Text("{\"exit\":0}".to_owned()))));
        let said = |event: &str| {
            fired
                .iter()
                .filter(|f| f.event == event)
                .map(|f| f.value.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(said("exit"), vec![Some(Value::Text("exit 3".to_owned()))]);
        assert_eq!(
            said("lost"),
            vec![Some(Value::Text("the relay went away".to_owned()))],
            "a lost connection is not the shell exiting"
        );
        assert!(
            values.contains(&Some(Value::Text("{\"skip\":0}".to_owned()))),
            "the shell's ask for its history reaches the app"
        );
        assert!(
            values.contains(&Some(Value::Text(String::new()))),
            "a held command let go reaches the app"
        );
    }

    phase an_input_method_commits_text_to_the_shell_and_accessibility_names_the_grid;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ café"]));
        let mut widget = Terminal::new(fake.clone()).with_accessible_name("Terminal, café");
        widget.set_focus(true);
        let (tree, node) = placed(&mut widget);
        send(
            &mut widget,
            &tree,
            node,
            &WindowEvent::Ime(Ime::Commit("é".to_owned())),
        );
        assert_eq!(fake.state().texts, vec!["é".to_owned()]);
        let node = widget.accessibility().expect("a terminal is announced");
        assert_eq!(node.role, tilo_ui_base::a11y::Role::Terminal);
        assert_eq!(node.name.as_deref(), Some("Terminal, café"));
        assert!(widget.ime_content().is_some(), "the input method writes into the shell");
    }

    phase a_grid_the_window_holds_no_session_for_draws_nothing_and_takes_nothing;
    {
        let mut widget = Terminal::from_props(
            "terminal",
            TerminalProps {
                session: "absent".to_owned(),
                ..Default::default()
            },
        );
        let (_tree, _node) = placed(&mut widget);
        paint(&mut widget);
        assert!(widget.screen().lines.is_empty());
        assert_eq!(widget.accessibility().map(|node| node.role), Some(tilo_ui_base::a11y::Role::Terminal));
    }


    phase the_grid_takes_its_colours_from_the_theme_it_is_drawn_in;
    {
        let dark = crate::terminal::Palette::from_theme(&Theme::dark());
        let light = crate::terminal::Palette::from_theme(&Theme::light());
        assert_ne!(
            dark.background, light.background,
            "a grid's ground is the theme's, not the terminal's own"
        );
        assert_ne!(dark.ansi[1], light.ansi[1], "the ramp follows the theme too");
        let fake = std::sync::Arc::new(Fake::with_lines(&["\u{1b}[31mred"]));
        let mut widget = Terminal::new(fake);
        let (_tree, _node) = placed(&mut widget);
        let mut scene = Scene::new();
        widget.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::light(),
            tilo_ui_base::presence::PresenceCtx::default().draw_env(),
        ));
        assert!(!scene.encoding().is_empty(), "the light theme paints the grid");
    }

    phase a_program_sets_the_clipboard_only_while_the_person_allows_it;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ "]));
        let mut widget = Terminal::new(fake.clone()).with_allow_clipboard(true);
        widget.set_allow_clipboard(false);
        assert_eq!(fake.state().clipboard, vec![true, false]);
    }

    phase find_in_the_scrollback_finds_every_row_and_reveals_the_one_chosen;
    {
        let lines: Vec<String> = (0..120)
            .map(|n| {
                if n == 10 || n == 70 {
                    format!("line {n} with a needle in it")
                } else {
                    format!("line {n}")
                }
            })
            .collect();
        let names: Vec<&str> = lines.iter().map(String::as_str).collect();
        let fake = std::sync::Arc::new(Fake::with_lines(&names));
        let mut widget = Terminal::new(fake);
        let (_tree, _node) = placed(&mut widget);
        paint(&mut widget);
        let found = crate::widget::Findable::find(
            &mut widget,
            "needle",
            crate::widget::FindOptions::default(),
        );
        assert_eq!(found.len(), 2, "each row the words stand on is a match");
        crate::widget::Findable::reveal(&mut widget, 1);
        paint(&mut widget);
        assert!(
            widget.first_row <= 70 && 70 < widget.first_row + widget.viewport_rows,
            "revealing the second match scrolls it into view"
        );
        let any_case = crate::widget::Findable::find(
            &mut widget,
            "NEEDLE",
            crate::widget::FindOptions::default(),
        );
        assert_eq!(any_case.len(), 2, "find ignores case unless it is asked not to");
        let asked_case = crate::widget::Findable::find(
            &mut widget,
            "NEEDLE",
            crate::widget::FindOptions {
                match_case: true,
                ..Default::default()
            },
        );
        assert!(asked_case.is_empty(), "no row spells it in capitals");
        let spelled = crate::widget::Findable::find(
            &mut widget,
            "needle",
            crate::widget::FindOptions {
                match_case: true,
                ..Default::default()
            },
        );
        assert_eq!(spelled.len(), 2, "the same case matches");
    }

    phase images_a_program_placed_are_drawn_on_the_cells_they_occupy;
    {
        let fake = std::sync::Arc::new(Fake::with_lines(&["$ "]));
        fake.state().placements = vec![Placement {
            hash: "0123456789abcdef".to_owned(),
            mime: "image/png".to_owned(),
            row: 0,
            column: 0,
            columns: 2,
            rows: 1,
            alternate: false,
        }];
        let mut widget = Terminal::new(fake);
        let (_tree, _node) = placed(&mut widget);
        let scene = paint(&mut widget);
        assert!(!scene.encoding().is_empty(), "a placement without bytes is still framed");
        assert_eq!(widget.screen().lines.len(), 1);
    }

}
