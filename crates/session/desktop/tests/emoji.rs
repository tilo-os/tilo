//! Super+. on the real chrome: the emoji card opens at the caret of the
//! field the person types into, and the emoji they pick goes to the session,
//! which writes it into that field.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::emoji::{EmojiLink, EmojiPlace};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use ui::emoji_catalogue::{self, RecentsStore};
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
        view: String::new(),
        pinned: false,
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
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The emoji recents where the session keeps them: in memory, as the test's
/// own list.
#[derive(Clone)]
struct MemoryRecents(Rc<RefCell<Vec<String>>>);

impl RecentsStore for MemoryRecents {
    fn load(&self) -> Vec<String> {
        self.0.borrow().clone()
    }

    fn save(&self, items: &[String]) {
        *self.0.borrow_mut() = items.to_vec();
    }
}

/// The chrome over a desk with one window, keeping the emoji card's recents
/// in `recents` and opening the card where the session says.
fn chrome_with_picker(
    recents: Rc<RefCell<Vec<String>>>,
) -> (FrameHarness, DesktopHandle, EmojiLink) {
    emoji_catalogue::keep_recents_in(Box::new(MemoryRecents(recents)));
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk = FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    desk.relayout();
    let link = EmojiLink::default();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(Rules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(&tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_emoji(link.clone());
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::Theme::dark(),
    );
    frame.relayout();
    draw_chrome(&mut frame);
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
/// the emoji picked there is handed to the session, which writes it into that
/// field, and the card remembers it for the next time it opens.
#[test]
fn the_picker_opens_at_the_cursor_and_inserts_where_it_closed() {
    let recents = Rc::new(RefCell::new(Vec::new()));
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
        assert_eq!(
            chrome.overlays().target().slot(),
            Some(tilo_desktop::overlays::Slot::Emoji),
            "the keys are the card's while it is up"
        );
    });

    frame.relayout();
    draw_chrome(&mut frame);
    let card = with_chrome(&mut frame, |chrome| chrome.emoji().rect());
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
        "the pick is handed to the session once"
    );
    with_chrome(&mut frame, |chrome| {
        assert!(!chrome.emoji().is_open(), "and the card closes");
        assert!(
            chrome.overlays().target().slot().is_none(),
            "the field it came from has the keys back"
        );
    });

    // Picking it is what remembers it.
    assert_eq!(
        recents.borrow().as_slice(),
        ["🎉".to_string()],
        "the emoji picked is remembered"
    );

    // Opened again, the card lists it first.
    press_emoji_shortcut(&mut frame);
    frame.relayout();
    draw_chrome(&mut frame);
    with_chrome(&mut frame, |chrome| {
        assert_eq!(
            chrome.emoji().items().first().copied().flatten(),
            Some("🎉"),
            "the recents come first"
        );
    });
}

/// A field that reported no caret opens the card centred over the desk, and
/// the picker's own chord closes it again.
#[test]
fn the_picker_opens_centred_without_a_caret_and_its_shortcut_closes_it() {
    let recents = Rc::new(RefCell::new(Vec::new()));
    let (mut frame, _handle, link) = chrome_with_picker(recents);
    link.place_at(EmojiPlace::default());

    press_emoji_shortcut(&mut frame);
    frame.relayout();
    draw_chrome(&mut frame);
    let card = with_chrome(&mut frame, |chrome| chrome.emoji().rect());
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
        assert!(chrome.overlays().target().slot().is_none());
    });
    assert!(
        link.take_picks().is_empty(),
        "closing the card picks nothing"
    );
}
