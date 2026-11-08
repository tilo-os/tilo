use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use winit::keyboard::ModifiersState;

use super::*;
use crate::emoji_catalogue::{EmojiCatalogue, EmojiInfo};
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release, named_key};
use crate::workspace_emoji::{Adding, CustomEmoji, EmojiSource, WorkspaceEmoji};

/// A catalogue of two small groups. It names emoji as the real catalogue
/// does, so another module's tooltip reads the same whichever of the two is
/// installed (their catalogues share one process).
struct Fake;

const SMILEYS: &[(&str, &str)] = &[
    ("😀", "grinning face"),
    ("😂", "face with tears of joy"),
    ("👍", "thumbs up"),
    // Named as the real catalogue names them too, so a tooltip that reads a
    // name here reads the same whether this fake or the toolkit's own
    // catalogue is in (the two share the process).
    ("🎉", "party popper"),
    ("🙌", "raising hands"),
    ("🔥", "fire"),
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
    added: Rc<RefCell<(u64, Option<Adding>)>>,
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
    fn last_added(&self) -> (u64, Option<Adding>) {
        self.added.borrow().clone()
    }
}

/// A window whose workspace has `:party_parrot:` and `:pizza_cat:`.
struct Window {
    scope: crate::wake::WakeScope,
    chosen: Rc<Cell<usize>>,
    added: Rc<RefCell<(u64, Option<Adding>)>>,
}

impl Window {
    /// The picture chosen last settled as `adding`.
    fn settle(&self, adding: Adding) {
        let mut added = self.added.borrow_mut();
        *added = (added.0 + 1, Some(adding));
    }
}

fn window(manage: bool) -> Window {
    emoji_catalogue::CATALOGUE.install(fake);
    let scope = crate::wake::WakeScope::new();
    let chosen = Rc::new(Cell::new(0));
    let added = Rc::default();
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
            added: Rc::clone(&added),
        }),
    );
    Window {
        scope,
        chosen,
        added,
    }
}

fn opened(theme: &Theme) -> EmojiPicker {
    let mut picker = EmojiPicker::framed();
    picker.open_at(
        Some(Rect::new(100.0, 40.0, 130.0, 70.0)),
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
            Some("🎉"),
            Some("🙌"),
            Some("🔥"),
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

fn redraw(picker: &mut EmojiPicker) {
    picker.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::default(),
    );
}

/// What the picker's last line says.
fn footer(picker: &EmojiPicker) -> String {
    let dialog = picker.accessibility().expect("open");
    let last = dialog.children.last().expect("a footer");
    assert_eq!(last.role, Role::StaticText);
    last.name.clone().unwrap_or_default()
}

#[test]
fn a_picture_not_added_says_why_and_one_added_is_selected() {
    let window = window(true);
    let _in = window.scope.enter();
    window.settle(Adding::Added("old".into()));
    let mut picker = opened(&Theme::dark());
    assert_eq!(
        footer(&picker),
        ":party_parrot:",
        "what was added before it opened is not news"
    );
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    press(&mut picker, &named_key(NamedKey::ArrowRight));
    press(&mut picker, &named_key(NamedKey::Enter));
    window.settle(Adding::Failed("it isn't a PNG, JPEG or WebP".into()));
    assert!(picker.needs_redraw());
    redraw(&mut picker);
    assert_eq!(
        footer(&picker),
        "Couldn't add that picture: it isn't a PNG, JPEG or WebP"
    );
    assert_eq!(picker.selected(), None, "the add tile stays highlighted");
    window.settle(Adding::Added("pizza_cat".into()));
    redraw(&mut picker);
    assert_eq!(
        picker.selected(),
        Some(":pizza_cat:"),
        "the new emoji is selected"
    );
    assert_eq!(footer(&picker), ":pizza_cat:");
    window.settle(Adding::Failed("no".into()));
    redraw(&mut picker);
    press(&mut picker, &named_key(NamedKey::ArrowLeft));
    assert_eq!(footer(&picker), ":pizza_cat:", "a key clears the line");
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
        Some(Rect::new(500.0, 300.0, 530.0, 330.0)),
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

fn paint(picker: &mut EmojiPicker, theme: &Theme) {
    picker.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &PresenceCtx::default(),
    );
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 && inner.y0 >= outer.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1
}

/// The skin tones of the emoji the search found, offered.
fn tones_opened(viewport: Rect, theme: &Theme) -> EmojiPicker {
    let mut picker = EmojiPicker::sheet();
    picker.open_at(None, viewport, theme);
    picker.set_query("thumbs");
    paint(&mut picker, theme);
    shift(&mut picker, NamedKey::Enter);
    picker
}

#[test]
fn a_short_display_keeps_the_card_the_grid_and_the_chosen_emoji_inside_it() {
    for theme in [
        Theme::light(),
        Theme::dark().with_text_scale(2.0),
        Theme::light().with_ui_scale(1.5).with_text_scale(1.3),
    ] {
        let window = window(false);
        let _in = window.scope.enter();
        let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);
        let mut picker = EmojiPicker::sheet();
        picker.open_at(None, viewport, &theme);
        picker.set_query("thumbs");
        paint(&mut picker, &theme);
        assert!(
            contains(viewport, picker.rect()),
            "the card stays on the short display: {:?}",
            picker.rect()
        );
        assert!(
            picker.geometry.grid.height() >= picker.geometry.cell,
            "the chrome leaves the emoji a row: {:?}",
            picker.geometry
        );
        let selected = picker
            .item_rect(picker.selected)
            .expect("the found emoji is in view");
        assert!(selected.height() > 0.0);
        assert!(
            contains(picker.rect(), selected),
            "the emoji found is in the card: {selected:?} in {:?}",
            picker.rect()
        );
    }
}

#[test]
fn narrow_tones_wrap_inside_the_card_at_the_emoji_cell_size() {
    let window = window(false);
    let _in = window.scope.enter();
    let theme = Theme::light();
    let picker = tones_opened(Rect::new(0.0, 0.0, 240.0, 360.0), &theme);
    let tones = picker.tones.as_ref().expect("the skin tones");
    assert!(
        contains(picker.rect(), tones.rect),
        "the tones wrap rather than leave the card: {:?} in {:?}",
        tones.rect,
        picker.rect()
    );
    assert_eq!(tones.cells.len(), 6);
    for cell in &tones.cells {
        assert!(
            contains(tones.rect, *cell),
            "{cell:?} is in {:?}",
            tones.rect
        );
        assert_eq!(cell.width(), picker.geometry.cell);
        assert_eq!(cell.height(), picker.geometry.cell);
    }
}

#[test]
fn wrapped_tones_walk_row_by_row_and_the_last_one_picks() {
    let window = window(false);
    let _in = window.scope.enter();
    let theme = Theme::dark();
    let card = Rect::new(0.0, 0.0, 240.0, 360.0);
    let mut picker = tones_opened(card, &theme);
    let columns = usize_of_f64(
        ((picker.rect().width() - 2.0 * f64::from(theme.tokens.spacing.s1)) / picker.geometry.cell)
            .floor(),
    )
    .max(1);
    assert!(columns < 6, "the tones wrap at this width: {columns}");
    press(&mut picker, &named_key(NamedKey::ArrowDown));
    assert_eq!(
        picker.tones.as_ref().expect("tones").selected,
        columns,
        "down reaches the next painted row"
    );
    press(&mut picker, &named_key(NamedKey::ArrowUp));
    assert_eq!(picker.tones.as_ref().expect("tones").selected, 0);
    press(&mut picker, &named_key(NamedKey::End));
    let (target, expected) = {
        let tones = picker.tones.as_ref().expect("tones");
        (
            tones.cells.last().expect("six tones").center(),
            tones.forms.last().expect("six tones").clone(),
        )
    };
    click(&mut picker, target);
    assert_eq!(picker.take_events(), [PickerEvent::Picked(expected)]);
}

#[test]
fn a_narrow_card_stacks_the_group_tabs_on_their_own_rows() {
    let window = window(false);
    let _in = window.scope.enter();
    let theme = Theme::dark();
    let viewport = Rect::new(0.0, 0.0, 120.0, 480.0);
    let mut picker = EmojiPicker::sheet();
    picker.open_at(None, viewport, &theme);
    paint(&mut picker, &theme);
    assert_eq!(picker.geometry.tab_columns, 1, "one tab to a row");
    let first = picker.tab_rect(0).expect("the first group");
    let second = picker.tab_rect(1).expect("the second group");
    assert!(first.y1 <= second.y0, "the tabs stack rather than shrink");
    let card = picker.rect();
    assert!(contains(card, first) && contains(card, second));
    click(&mut picker, second.center());
    assert_eq!(picker.group(), 1, "a tab on its own row chooses its group");
}

#[test]
fn a_touch_presses_and_picks_where_a_mouse_does() {
    let window = window(false);
    let _in = window.scope.enter();
    let mut picker = opened(&Theme::dark());
    let target = picker
        .item_rect(1)
        .expect("the second emoji is in view")
        .center();
    let touch = |phase| {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(target.x, target.y),
            force: None,
            id: 7,
        })
    };
    press(&mut picker, &touch(TouchPhase::Started));
    assert!(
        picker.take_events().is_empty(),
        "a finger down picks nothing by itself"
    );
    press(&mut picker, &touch(TouchPhase::Ended));
    assert_eq!(
        picker.take_events(),
        [PickerEvent::Picked(":pizza_cat:".into())],
        "lifting the finger picks what it pressed"
    );
    // A touch the system took back presses nothing.
    let mut picker = opened(&Theme::dark());
    press(&mut picker, &touch(TouchPhase::Started));
    press(&mut picker, &touch(TouchPhase::Cancelled));
    press(&mut picker, &touch(TouchPhase::Ended));
    assert!(picker.take_events().is_empty());
}

#[test]
fn a_framed_card_without_a_caret_centres_over_the_display_it_was_given() {
    let window = window(false);
    let _in = window.scope.enter();
    let theme = Theme::light();
    let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
    let mut picker = EmojiPicker::sheet();
    picker.open_at(None, viewport, &theme);
    paint(&mut picker, &theme);
    let card = picker.rect();
    assert!(contains(viewport, card));
    assert!(
        (card.center().x - viewport.center().x).abs() < 0.5,
        "centred across the display: {card:?}"
    );
    assert!(
        card.y0 > viewport.y0 && card.y0 < viewport.center().y,
        "and high up rather than off the top: {card:?}"
    );
}
