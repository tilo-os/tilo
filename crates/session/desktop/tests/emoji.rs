//! Super+. on the real chrome: the emoji card opens at the caret of the
//! field the person types into, and the emoji they pick goes to the host,
//! which writes it into that field and remembers it.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::cell::RefCell;
use std::rc::Rc;

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
use ui::headless::FrameHarness;
use ui::kurbo::{Affine, Rect};
use ui::synthetic_input::{char_key, key_press, named_key};
use ui::widget::{TileHost, Widget};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// The windows of the test desk, with their content as plain notices.
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
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The recents where the session keeps them: in memory, as the test's own
/// state file.
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

/// The chrome over a desk with one window, keeping the emoji picker's recents
/// in `recents` and opening the card where the host says.
fn chrome_with_picker(recents: Rc<RefCell<Recents>>) -> (FrameHarness, DesktopHandle, EmojiLink) {
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let link = EmojiLink::new(Box::new(MemoryRecents(recents)));
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
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
    (frame, handle, link)
}

fn hold(frame: &mut FrameHarness, mods: ModifiersState) {
    frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(mods)));
}

/// Super+., the emoji picker's chord.
fn press_emoji_shortcut(frame: &mut FrameHarness) {
    hold(frame, ModifiersState::SUPER);
    frame.send_host_event(&key_press(Key::Character(".".into()), None));
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

/// Super+. opens the card at the caret of the field the person types into;
/// the emoji picked there is handed to the host, which writes it into that
/// field and remembers it for the next time the card opens.
#[test]
fn the_picker_opens_at_the_cursor_and_inserts_where_it_closed() {
    let recents = Rc::new(RefCell::new(Recents::default()));
    let (mut frame, _handle, link) = chrome_with_picker(Rc::clone(&recents));

    // The caret of the field the person is typing into, as the session reads
    // it from the compositor: a text caret on the desk, on the whole display.
    let caret = Rect::new(320.0, 180.0, 322.0, 200.0);
    let display = Rect::new(0.0, 0.0, 1280.0, 800.0);
    link.place_at(EmojiPlace {
        viewport: Some(display),
        caret: Some(caret),
    });

    press_emoji_shortcut(&mut frame);
    with_chrome(&mut frame, |chrome| {
        assert!(chrome.emoji().is_open(), "Super+. opens the picker");
        assert_eq!(
            chrome.emoji().anchor(),
            Some(caret),
            "the card opens against the field's caret"
        );
        assert!(
            chrome.wants_keyboard(),
            "the keys are the card's while it is up"
        );
    });

    frame.relayout();
    draw_chrome(&mut frame);
    let card = with_chrome(&mut frame, |chrome| chrome.emoji().card_rect());
    assert!(
        card.y0 >= caret.y1,
        "the card stands below the caret: {card:?} against {caret:?}"
    );
    let caret_under_card = card.x0 <= caret.x0 && caret.x1 <= card.x1;
    assert!(
        caret_under_card,
        "its left edge at the caret: {card:?} against {caret:?}"
    );
    assert!(
        display.contains(card.center()),
        "and on the display it was opened on: {card:?}"
    );

    // The person searches for an emoji and takes it.
    type_text(&mut frame, "tada");
    let chosen = with_chrome(&mut frame, |chrome| {
        chrome.emoji().selected().map(str::to_owned)
    });
    assert_eq!(chosen.as_deref(), Some("🎉"), "the search finds the emoji");
    frame.send_host_event(&named_key(NamedKey::Enter));

    assert_eq!(
        link.take_picks(),
        ["🎉".to_string()],
        "the pick is handed to the host once"
    );
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.emoji().is_open(), "and the card closes");
        assert!(
            !chrome.wants_keyboard(),
            "the field it came from has the keys back"
        );
    });

    // The host writes it into the field it was picked from and reports what
    // arrived, which is when the emoji joins the recents.
    link.note_delivered("🎉".into(), false);
    frame.relayout();
    draw_chrome(&mut frame);
    assert_eq!(
        recents.borrow().items(),
        ["🎉".to_string()],
        "the emoji picked is remembered"
    );

    // Opened again, the card lists it first.
    press_emoji_shortcut(&mut frame);
    frame.relayout();
    draw_chrome(&mut frame);
    with_chrome(&mut frame, |chrome| {
        assert_eq!(chrome.emoji().recents(), ["🎉".to_string()]);
        assert_eq!(
            chrome.emoji().items().first().map(String::as_str),
            Some("🎉")
        );
    });
}

/// A field whose content is secret keeps its emoji out of the recents: what
/// was picked into a password is never remembered.
#[test]
fn a_pick_into_a_secret_field_is_never_remembered() {
    let recents = Rc::new(RefCell::new(Recents::default()));
    let (mut frame, _handle, link) = chrome_with_picker(Rc::clone(&recents));

    link.note_delivered("👍".into(), true);
    frame.relayout();
    draw_chrome(&mut frame);

    assert!(recents.borrow().items().is_empty(), "nothing is remembered");
    press_emoji_shortcut(&mut frame);
    with_chrome(&mut frame, |chrome| {
        assert!(chrome.emoji().is_open());
        assert!(chrome.emoji().recents().is_empty());
    });
}

/// A field that reported no caret opens the card centred over the desk, and
/// the picker's own chord closes it again.
#[test]
fn the_picker_opens_centred_without_a_caret_and_its_shortcut_closes_it() {
    let recents = Rc::new(RefCell::new(Recents::default()));
    let (mut frame, _handle, link) = chrome_with_picker(recents);
    link.place_at(EmojiPlace::default());

    press_emoji_shortcut(&mut frame);
    frame.relayout();
    draw_chrome(&mut frame);
    let card = with_chrome(&mut frame, |chrome| chrome.emoji().card_rect());
    assert!(
        card.x0 > 0.0 && card.x1 < 1280.0 && card.y0 > 0.0 && card.y1 < 800.0,
        "the card is on the desk: {card:?}"
    );
    assert!(
        (card.center().x - 640.0).abs() < 1.0,
        "centred over the desk: {card:?}"
    );

    press_emoji_shortcut(&mut frame);
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.emoji().is_open(), "its own chord closes it");
        assert!(!chrome.wants_keyboard());
    });
    assert!(
        link.take_picks().is_empty(),
        "closing the card picks nothing"
    );
}
