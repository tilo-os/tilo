//! What the chrome tells assistive technology: the overlays it paints —
//! the emoji card, the strip labels, the window switcher — are in the tree
//! its host publishes, and a reader lands on what the person sees.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use emoji::Recents;
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::emoji_picker::{EmojiLink, EmojiPlace, RecentsStore};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use ui::a11y::Action;
use ui::headless::FrameHarness;
use ui::kurbo::{Affine, Rect};
use ui::synthetic_input::{char_key, key_press, named_key};
use ui::widget::{TileHost, Widget};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

struct Rules {
    handle: DesktopHandle,
}

impl DesktopRules for Rules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

fn window(id: u64, title: &str, app: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// A desk of two strips: "Main" holds a window, "Reading" is empty.
fn desk_tree() -> DesktopTree {
    let tabs = vec![window(20, "Notes", "so.tilo.notes")];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![
        Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![group],
        },
        Strip {
            id: StripId(4),
            name: "Reading".into(),
            window_groups: Vec::new(),
        },
    ];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

#[derive(Clone)]
struct MemoryRecents(Rc<RefCell<Recents>>);

impl RecentsStore for MemoryRecents {
    fn load(&self) -> Recents {
        self.0.borrow().clone()
    }

    fn save(&self, recents: &Recents) -> Result<(), String> {
        *self.0.borrow_mut() = recents.clone();
        Ok(())
    }
}

/// The desk and the chrome a window shows over it, before either is mounted.
struct Desk {
    surface: DesktopSurface,
    handle: DesktopHandle,
    chrome: DesktopChrome,
    link: EmojiLink,
}

/// A desk of two strips and the chrome over it, and the link the host keeps
/// the picker through.
fn desk() -> Desk {
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let link = EmojiLink::new(Box::new(MemoryRecents(Rc::new(RefCell::new(
        Recents::default(),
    )))));
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(Rules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(&tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    )
    .with_emoji(link.clone());
    Desk {
        surface,
        handle,
        chrome,
        link,
    }
}

/// The chrome over a test desk: the frame that mounts it, the frame that
/// mounts the desk under it, and the link the host keeps the picker through.
fn chrome() -> (FrameHarness, FrameHarness, DesktopHandle, EmojiLink) {
    let Desk {
        surface,
        handle,
        chrome,
        link,
    } = desk();
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
    (frame, desk, handle, link)
}

fn hold(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

/// [`hold`], in the window the live app runs.
fn hold_in(app: &mut ui::App, screen: &Screen, mods: ModifiersState) {
    let modifiers = Modifiers::from(mods);
    app.set_surface_modifiers(modifiers);
    screen.send(app, &WindowEvent::ModifiersChanged(modifiers));
}

/// A chord with Super held.
fn super_chord(frame: &mut FrameHarness, key: &str) {
    hold(frame, ModifiersState::SUPER);
    frame.send_host_event(&key_press(Key::Character(key.into()), None));
    hold(frame, ModifiersState::empty());
}

fn type_text(frame: &mut FrameHarness, text: &str) {
    for character in text.chars() {
        frame.send_host_event(&char_key(&character.to_string()));
    }
}

/// Run `visit` on the chrome the frame mounts, as the session's chrome layer
/// holds it.
fn with_chrome<R>(frame: &mut FrameHarness, visit: impl FnOnce(&mut DesktopChrome) -> R) -> R {
    let mut chrome = frame.root_mut::<DesktopChrome>();
    visit(&mut chrome)
}

/// Draw one frame of the chrome, as the session's chrome layer does.
fn draw_chrome(frame: &mut FrameHarness) {
    let theme = ui::Theme::dark();
    let mut text = ui::TextRenderer::default();
    let presence = ui::presence::PresenceCtx::default();
    let mut scene = ui::vello::Scene::new();
    with_chrome(frame, |chrome| {
        chrome.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
    });
}

/// What the reader says unprompted this frame: the live regions the tree
/// carries, which a screen reader announces as they change.
fn announced(frame: &FrameHarness) -> Vec<String> {
    frame
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.live().is_some())
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

/// Whether `node` is `root` or stands below it in the published tree.
fn within(frame: &FrameHarness, root: ui::accesskit::NodeId, node: ui::accesskit::NodeId) -> bool {
    let nodes: std::collections::HashMap<_, _> = frame.a11y_nodes().into_iter().collect();
    let mut frontier = vec![root];
    while let Some(current) = frontier.pop() {
        if current == node {
            return true;
        }
        if let Some(exported) = nodes.get(&current) {
            frontier.extend(exported.children().iter().copied());
        }
    }
    false
}

/// The name of the emoji the person is on, as the card says it.
fn highlighted(frame: &mut FrameHarness) -> String {
    with_chrome(frame, |chrome| {
        let text = chrome
            .emoji()
            .selected()
            .expect("the card highlights an emoji")
            .to_owned();
        chrome.emoji().spoken_name(&text)
    })
}

/// The name of the emoji the person is on, as the card says it, in the
/// window the live app runs.
fn highlighted_in(app: &mut ui::App) -> String {
    let chrome = app
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<DesktopChrome>())
        .expect("the window mounts the desk's chrome");
    let text = chrome
        .emoji()
        .selected()
        .expect("the card highlights an emoji")
        .to_owned();
    chrome.emoji().spoken_name(&text)
}

/// What Tilo's own screen reader said, through the toolkit's recording
/// speaker.
#[derive(Clone, Default)]
struct Heard(Arc<Mutex<tts::NullSpeaker>>);

impl Heard {
    fn last(&self) -> Option<String> {
        self.0.lock().unwrap().last().map(str::to_owned)
    }

    fn forget(&self) {
        self.0.lock().unwrap().reset();
    }
}

impl tts::Speaker for Heard {
    fn speak_utterance(&mut self, utterance: tts::Utterance) {
        self.0.lock().unwrap().speak_utterance(utterance);
    }
    fn stop(&mut self) {
        self.0.lock().unwrap().stop();
    }
    fn set_rate(&mut self, rate: f32) {
        self.0.lock().unwrap().set_rate(rate);
    }
    fn set_voice(&mut self, voice: &str) {
        self.0.lock().unwrap().set_voice(voice);
    }
    fn is_speaking(&self) -> bool {
        self.0.lock().unwrap().is_speaking()
    }
    fn backend_name(&self) -> &'static str {
        "heard"
    }
}

/// The desk's window, as its host shows it: events and frames in, pixels
/// out.
struct Screen;

impl ui::app::AppSurface for Screen {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(1280, 800)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

impl Screen {
    fn send(&self, app: &mut ui::App, event: &WindowEvent) {
        app.surface_input(self, event.clone(), false);
    }

    /// One frame of the window, and then what it says of its live regions as
    /// the next one opens: what the person sees is what the reader reads.
    fn frame(&self, app: &mut ui::App) {
        let mut scene = ui::vello::Scene::new();
        for _ in 0..3 {
            scene.reset();
            app.surface_frame(self, &mut scene);
        }
        app.surface_accessibility();
    }
}

/// Super+. opens the card in a window of its own, with Tilo's own reader
/// listening: it hears the card's footer, the polite region the card paints
/// inside its own dialog, and the emoji it names follows the arrows.
#[test]
fn the_built_in_reader_hears_the_cards_footer() {
    let Desk {
        surface: _surface,
        handle: _handle,
        chrome,
        link,
    } = desk();
    let screen = Screen;
    let mut app = ui::App::new(ui::state::State::new(), Box::new(chrome));
    app.init_layout();
    let heard = Heard::default();
    app.set_screen_reader(Some(ui::screen_reader::ScreenReader::with_speaker(
        Box::new(heard.clone()),
    )));

    link.place_at(EmojiPlace {
        viewport: Some(Rect::new(0.0, 0.0, 1280.0, 800.0)),
        caret: Some(Rect::new(320.0, 180.0, 322.0, 200.0)),
    });
    hold_in(&mut app, &screen, ModifiersState::SUPER);
    screen.send(&mut app, &key_press(Key::Character(".".into()), None));
    hold_in(&mut app, &screen, ModifiersState::empty());
    // A search with more than one answer, so the arrows have somewhere to go.
    for character in "smile".chars() {
        screen.send(&mut app, &char_key(&character.to_string()));
    }

    // The reader says the emoji the person is on, from the footer the card
    // keeps inside its own subtree.
    screen.frame(&mut app);
    let first = highlighted_in(&mut app);
    assert_eq!(
        heard.last().as_deref(),
        Some(first.as_str()),
        "the reader says the emoji the person is on"
    );

    // An arrow moves the highlight, and the reader says the next emoji.
    heard.forget();
    screen.send(&mut app, &named_key(NamedKey::ArrowRight));
    screen.frame(&mut app);
    let next = highlighted_in(&mut app);
    assert_ne!(next, first, "the arrow moves the highlight");
    assert_eq!(
        heard.last().as_deref(),
        Some(next.as_str()),
        "and the reader says where the person now is"
    );
}

/// Super+. opens the card; a reader reaches it in the published tree and
/// hears the emoji the person is on, which follows the arrows.
#[test]
fn the_emoji_card_is_the_readers_and_it_says_the_emoji_the_person_is_on() {
    let (mut frame, _desk, _handle, link) = chrome();
    let caret = Rect::new(320.0, 180.0, 322.0, 200.0);
    let display = Rect::new(0.0, 0.0, 1280.0, 800.0);
    link.place_at(EmojiPlace {
        viewport: Some(display),
        caret: Some(caret),
    });
    super_chord(&mut frame, ".");
    // A search with more than one answer, so the arrows have somewhere to go.
    type_text(&mut frame, "smile");
    frame.relayout();

    // The card is in the tree, named as the person sees its title.
    let card = frame
        .a11y_node_named("Emoji")
        .expect("the card is reachable");
    // The reader moves into the card as into any dialog, and lands inside it.
    frame
        .perform_a11y_action(card, &Action::Focus)
        .expect("the reader reaches the card");
    frame.relayout();
    let (focus, _) = frame.a11y_focus().expect("the reader sits somewhere");
    assert!(
        within(&frame, card, focus),
        "the reader landed in the card, not back on the desk"
    );

    // And the card says the emoji the person is on.
    let first = highlighted(&mut frame);
    assert_eq!(
        announced(&frame),
        vec![first.clone()],
        "the reader says the emoji the person is on"
    );

    // An arrow moves the highlight, and the reader says the next emoji.
    frame.send_host_event(&named_key(NamedKey::ArrowRight));
    frame.relayout();
    let next = highlighted(&mut frame);
    assert_ne!(next, first, "the arrow moves the highlight");
    assert_eq!(
        announced(&frame),
        vec![next],
        "and the reader says where the person now is"
    );
}

/// Super+S opens the strip labels; a reader reaches each strip by name and
/// choosing one goes there, as pressing it does.
#[test]
fn the_strip_labels_are_the_readers_and_choosing_one_goes_there() {
    let (mut frame, mut desk, handle, _link) = chrome();
    super_chord(&mut frame, "s");
    frame.relayout();
    draw_chrome(&mut frame);
    frame.relayout();

    frame
        .a11y_node_named("Strips")
        .expect("the labels are in the tree");
    let reading = frame
        .a11y_node_named("Reading")
        .expect("each strip is reachable by its name");
    frame
        .perform_a11y_action(reading, &Action::Activate)
        .expect("the reader picks the strip");
    desk.relayout();

    assert_eq!(
        handle.read(|tree| tree.active_strip.get(&WorkspaceId(1)).copied()),
        Some(StripId(4)),
        "the desk moved to the strip the reader chose"
    );
}
