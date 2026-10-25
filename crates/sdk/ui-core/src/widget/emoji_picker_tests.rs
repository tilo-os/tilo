use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use winit::keyboard::ModifiersState;

use super::*;
use crate::emoji_catalogue::{EmojiCatalogue, EmojiInfo};
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release, named_key};
use crate::workspace_emoji::{CustomEmoji, EmojiSource, WorkspaceEmoji};

/// A catalogue of two small groups. It names no emoji the reaction strip's
/// tests name, so their tooltips read the same with or without it.
struct Fake;

const SMILEYS: &[(&str, &str)] = &[
    ("😀", "grinning face"),
    ("😂", "face with tears of joy"),
    ("👍", "thumbs up"),
];
const FOOD: &[(&str, &str)] = &[("🍕", "pizza"), ("🍔", "hamburger")];
const TONED: [&str; 6] = ["👍", "👍🏻", "👍🏼", "👍🏽", "👍🏾", "👍🏿"];

impl EmojiCatalogue for Fake {
    fn groups(&self) -> Vec<CatalogueGroup> {
        vec![
            CatalogueGroup {
                id: "smileys-emotion",
                label: "Smileys",
                icon: "😀",
            },
            CatalogueGroup {
                id: "food-drink",
                label: "Food",
                icon: "🍔",
            },
        ]
    }

    fn group_emoji(&self, index: usize) -> Vec<&'static str> {
        [SMILEYS, FOOD]
            .get(index)
            .map(|group| group.iter().map(|(emoji, _)| *emoji).collect())
            .unwrap_or_default()
    }

    fn search(&self, query: &str, limit: usize) -> Vec<&'static str> {
        SMILEYS
            .iter()
            .chain(FOOD)
            .filter(|(_, name)| name.contains(query))
            .map(|(emoji, _)| *emoji)
            .take(limit)
            .collect()
    }

    fn info(&self, emoji: &str) -> Option<EmojiInfo> {
        if let Some(tone) = TONED.iter().position(|form| *form == emoji) {
            return Some(EmojiInfo {
                name: "thumbs up",
                tone: tone.checked_sub(1).and_then(|t| u8::try_from(t).ok()),
                takes_tone: true,
            });
        }
        SMILEYS
            .iter()
            .chain(FOOD)
            .find(|(text, _)| *text == emoji)
            .map(|(_, name)| EmojiInfo {
                name,
                tone: None,
                takes_tone: false,
            })
    }

    fn tones(&self, emoji: &str) -> Vec<&'static str> {
        if TONED.contains(&emoji) {
            TONED.to_vec()
        } else {
            Vec::new()
        }
    }
}

fn fake(_: &str) -> &'static dyn EmojiCatalogue {
    &Fake
}

struct Source {
    emoji: Rc<WorkspaceEmoji>,
    manage: bool,
    chosen: Rc<Cell<usize>>,
}

impl EmojiSource for Source {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        Rc::clone(&self.emoji)
    }
    fn revision(&self) -> u64 {
        1
    }
    fn can_manage(&self) -> bool {
        self.manage
    }
    fn add(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn choose_and_add(&self) {
        self.chosen.set(self.chosen.get() + 1);
    }
}

/// A window whose workspace has `:party_parrot:` and `:pizza_cat:`.
struct Window {
    scope: crate::wake::WakeScope,
    chosen: Rc<Cell<usize>>,
}

fn window(manage: bool) -> Window {
    emoji_catalogue::CATALOGUE.install(fake);
    let scope = crate::wake::WakeScope::new();
    let chosen = Rc::new(Cell::new(0));
    let emoji = WorkspaceEmoji::new(
        ["party_parrot", "pizza_cat"]
            .into_iter()
            .map(|name| CustomEmoji {
                name: name.to_owned(),
                image: Arc::from(&b"not a picture"[..]),
            })
            .collect(),
    );
    crate::workspace_emoji::register(
        &scope.handle(),
        Rc::new(Source {
            emoji: Rc::new(emoji),
            manage,
            chosen: Rc::clone(&chosen),
        }),
    );
    Window { scope, chosen }
}

fn opened(theme: &Theme) -> EmojiPicker {
    let mut picker = EmojiPicker::framed();
    picker.open_at(
        Rect::new(100.0, 40.0, 130.0, 70.0),
        Rect::new(0.0, 0.0, 1280.0, 800.0),
        theme,
    );
    let mut scene = Scene::new();
    picker.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &PresenceCtx::default(),
    );
    picker
}

fn press(picker: &mut EmojiPicker, event: &WindowEvent) {
    crate::testing::dispatch(picker, event);
}

fn shift(picker: &mut EmojiPicker, key: NamedKey) {
    crate::testing::Dispatch::new()
        .modifiers(ModifiersState::SHIFT)
        .send(picker, &named_key(key));
}

fn click(picker: &mut EmojiPicker, at: Point) {
    press(picker, &cursor_moved(at.x, at.y));
    press(picker, &mouse_press());
    press(picker, &mouse_release());
}

#[test]
fn the_workspace_comes_first_then_the_recents_then_the_group() {
    let window = window(true);
    let _in = window.scope.enter();
    emoji_catalogue::record_recent("😂");
    emoji_catalogue::record_recent(":party_parrot:");
    let mut picker = opened(&Theme::dark());
    assert_eq!(
        picker.items(),
        [
            Some(":party_parrot:"),
            Some(":pizza_cat:"),
            None,
            Some(":party_parrot:"),
            Some("😂"),
            Some("😀"),
            Some("😂"),
            Some("👍"),
        ],
        "workspace emoji with the add tile, the recents (custom ones too), the group"
    );
    // The add tile asks the workspace for a picture to add.
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    assert_eq!(picker.selected(), None, "the add tile is highlighted");
    press(&mut picker, &named_key(NamedKey::Enter));
    assert_eq!(window.chosen.get(), 1);
    assert!(picker.is_open(), "adding an emoji is no pick");
    assert!(picker.take_events().is_empty());
}

#[test]
fn only_who_may_manage_the_workspace_emoji_gets_the_add_tile() {
    let window = window(false);
    let _in = window.scope.enter();
    let picker = opened(&Theme::light());
    assert!(picker.items().iter().all(Option::is_some));
    assert_eq!(
        picker.items()[..2],
        [Some(":party_parrot:"), Some(":pizza_cat:")]
    );
}

#[test]
fn typing_searches_the_workspace_first_and_enter_picks() {
    let window = window(false);
    let _in = window.scope.enter();
    let mut picker = opened(&Theme::dark());
    press(&mut picker, &char_key("p"));
    press(&mut picker, &char_key("i"));
    assert_eq!(picker.query(), "pi");
    assert_eq!(picker.items(), [Some(":pizza_cat:"), Some("🍕")]);
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    press(&mut picker, &named_key(NamedKey::Enter));
    assert_eq!(picker.take_events(), [PickerEvent::Picked("🍕".into())]);
    assert!(!picker.is_open(), "a pick closes a framed picker");
    assert_eq!(
        emoji_catalogue::recents().first().map(String::as_str),
        Some("🍕")
    );
    let mut picker = opened(&Theme::dark());
    picker.set_query("nothing like it");
    assert!(picker.items().is_empty());
    let dialog = picker.accessibility().expect("open");
    assert!(
        dialog
            .children
            .iter()
            .any(|node| node.name.as_deref() == Some("No emoji found")),
        "nothing found says so"
    );
}

#[test]
fn tab_changes_the_group_and_escape_closes_without_a_pick() {
    let window = window(false);
    let _in = window.scope.enter();
    let mut picker = opened(&Theme::dark());
    press(&mut picker, &named_key(NamedKey::Tab));
    assert_eq!(picker.selected(), Some("🍕"), "the group's first emoji");
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    assert_eq!(picker.selected(), Some("🍔"));
    press(&mut picker, &named_key(NamedKey::ArrowUp));
    assert_eq!(
        picker.selected(),
        Some(":pizza_cat:"),
        "up goes to the nearest in the row above"
    );
    shift(&mut picker, NamedKey::Tab);
    assert_eq!(picker.selected(), Some("😀"), "back to the first group");
    press(&mut picker, &named_key(NamedKey::Escape));
    assert_eq!(picker.take_events(), [PickerEvent::Dismissed]);
    assert!(!picker.is_open());
}

#[test]
fn shift_enter_offers_the_skin_tones_and_enter_picks_one() {
    let window = window(false);
    let _in = window.scope.enter();
    let mut picker = opened(&Theme::dark());
    picker.set_query("thumbs");
    assert_eq!(picker.selected(), Some("👍"));
    shift(&mut picker, NamedKey::Enter);
    assert_eq!(picker.tone_forms().map(<[String]>::len), Some(6));
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    press(&mut picker, &named_key(NamedKey::Enter));
    assert_eq!(picker.take_events(), [PickerEvent::Picked("👍🏼".into())]);
}

#[test]
fn a_click_picks_and_a_click_outside_dismisses() {
    let window = window(false);
    let _in = window.scope.enter();
    let theme = Theme::dark();
    let mut picker = opened(&theme);
    let cell = picker.item_rect(1).expect("the second emoji is in view");
    click(&mut picker, cell.center());
    assert_eq!(
        picker.take_events(),
        [PickerEvent::Picked(":pizza_cat:".into())]
    );
    let mut picker = opened(&theme);
    let tab = picker.tab_rect(1).expect("two tabs");
    click(&mut picker, tab.center());
    assert_eq!(picker.selected(), Some("🍕"), "a tab chooses its group");
    let outside = picker.rect().origin() - Vec2::new(10.0, 10.0);
    click(&mut picker, outside);
    assert_eq!(picker.take_events(), [PickerEvent::Dismissed]);
}

#[test]
fn moving_an_open_picker_moves_its_grid_and_field_with_it() {
    let window = window(false);
    let _in = window.scope.enter();
    let theme = Theme::dark();
    let mut picker = opened(&theme);
    let before = picker.item_rect(0).expect("the first emoji is in view");
    let from = picker.rect().origin();
    // A host placing the open picker on every frame, as its window moves.
    picker.place_at(
        Rect::new(500.0, 300.0, 530.0, 330.0),
        Rect::new(0.0, 0.0, 1280.0, 800.0),
        &theme,
    );
    let moved = picker.rect().origin() - from;
    assert!(moved.hypot() > 100.0, "the picker moved: {moved:?}");
    let after = picker
        .item_rect(0)
        .expect("the first emoji is still in view");
    assert_eq!(
        after.origin() - before.origin(),
        moved,
        "the grid moved with the panel"
    );
    click(&mut picker, after.center());
    assert_eq!(
        picker.take_events(),
        [PickerEvent::Picked(":party_parrot:".into())],
        "a click where the first emoji now is picks it"
    );
}

#[test]
fn the_picker_is_a_dialog_with_its_field_tabs_and_named_emoji() {
    let window = window(true);
    let _in = window.scope.enter();
    let picker = opened(&Theme::dark());
    let dialog = picker.accessibility().expect("open");
    assert_eq!(dialog.role, Role::Dialog);
    assert_eq!(dialog.name.as_deref(), Some("Emoji"));
    assert_eq!(dialog.children[0].role, Role::TextInput);
    assert_eq!(dialog.children[1].role, Role::TabBar);
    assert_eq!(dialog.children[1].children.len(), 2);
    let workspace = &dialog.children[2];
    assert_eq!(workspace.name.as_deref(), Some("Workspace"));
    let names: Vec<_> = workspace
        .children
        .iter()
        .filter_map(|node| node.name.as_deref())
        .collect();
    assert_eq!(names, [":party_parrot:", ":pizza_cat:", "Add emoji"]);
    let smileys = dialog
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Smileys"))
        .expect("the group is a section");
    assert_eq!(
        smileys.children[2].name.as_deref(),
        Some("thumbs up"),
        "an emoji is named in the catalogue's language"
    );
    assert_eq!(
        emoji_catalogue::emoji_name("👍🏽"),
        "thumbs up, medium skin tone"
    );
}
