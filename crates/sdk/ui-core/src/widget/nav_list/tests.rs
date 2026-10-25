use super::*;
use crate::chrome::{NavRowFace, NavSectionFace, SelectionState, paint_nav_row, paint_nav_section};
use crate::headless::FrameHarness;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::tree_view::NavRowSpec;
use std::cell::RefCell as Cell2;
use vello::kurbo::{Affine, Rect};
use winit::event::ElementState;
use winit::keyboard::{Key, NamedKey};

fn mailboxes() -> Vec<NavSection> {
    vec![
        NavSection::untitled(
            "top",
            vec![
                NavItem::new("inbox", "Inbox")
                    .icon(Icon::Inbox)
                    .live_count(3),
                NavItem::new("sent", "Sent").icon(Icon::Send),
            ],
        ),
        NavSection::new(
            "folders",
            "Folders",
            vec![
                NavItem::new("work", "Work")
                    .icon(Icon::Folder)
                    .count(12)
                    .children(vec![NavItem::new("design", "Design").icon(Icon::Folder)])
                    .expanded(),
                NavItem::new("travel", "Travel").icon(Icon::Folder),
            ],
        )
        .collapsible()
        .trailing_action(NavAction::new("new-folder", Icon::Plus, "New folder")),
    ]
}

#[test]
fn sdk_boxing_preserves_navigation_identity_and_selection() {
    let mut widget = NavList::new(mailboxes()).selected("work").into_widget();
    let list = widget
        .as_any_mut()
        .unwrap()
        .downcast_mut::<NavList>()
        .unwrap();
    assert_eq!(list.selected_id().as_deref(), Some("work"));
    assert_eq!(widget.name(), "NavList");
}

fn harness(list: NavList, theme: Theme) -> FrameHarness {
    let mut h = FrameHarness::for_root(State::new(), list, (280, 360), theme);
    h.relayout_at((280, 360)).expect("layout");
    h
}

fn key(h: &mut FrameHarness, key: Key) -> bool {
    h.dispatch_to_root(&crate::synthetic_input::key_input(
        &key,
        ElementState::Pressed,
    ))
}

fn focus(h: &mut FrameHarness) {
    h.root_mut::<NavList>().inner_focus();
    h.relayout_at((280, 360)).expect("layout");
}

impl NavList {
    fn inner_focus(&mut self) {
        crate::widget::Widget::set_focus(&mut self.tree, true);
    }
}

#[test]
fn rows_follow_the_sections_and_headers_are_not_destinations() {
    let h = harness(NavList::new(mailboxes()), Theme::dark());
    let list = h.root::<NavList>();
    let rows: Vec<String> = list
        .tree()
        .visible_rows()
        .iter()
        .map(|(_, label, _)| (*label).to_owned())
        .collect();
    assert_eq!(
        rows,
        ["Inbox", "Sent", "Folders", "Work", "Design", "Travel"],
        "untitled items stand at the top level; a titled section heads its items"
    );
    let theme = Theme::dark();
    let row = crate::chrome::nav_row_height(&theme);
    let inbox = list.row_rect("inbox").expect("inbox shown");
    assert!((inbox.height() - row).abs() < 0.5);
    let work = list.row_rect("work").expect("work shown");
    let section = work.y0 - list.row_rect("sent").expect("sent").y1;
    assert!(
        (section - crate::chrome::nav_section_height(&theme, false)).abs() < 0.5,
        "a section after the first leads with s3 of space ({section})"
    );
}

#[test]
fn the_keyboard_moves_the_selection_and_skips_headers() {
    let chosen = Rc::new(Cell2::new(Vec::<String>::new()));
    let seen = Rc::clone(&chosen);
    let list = NavList::new(mailboxes())
        .selected("sent")
        .on_select(move |id| seen.borrow_mut().push(id));
    let mut h = harness(list, Theme::dark());
    focus(&mut h);
    assert!(key(&mut h, Key::Named(NamedKey::ArrowDown)));
    // The header takes focus but is never chosen.
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("sent"));
    assert!(key(&mut h, Key::Named(NamedKey::ArrowDown)));
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("work"));
    assert_eq!(chosen.borrow().as_slice(), ["work"]);
    // Left on a parent collapses it; the child row goes away.
    assert!(key(&mut h, Key::Named(NamedKey::ArrowLeft)));
    h.relayout_at((280, 360)).expect("layout");
    assert!(h.root::<NavList>().row_rect("design").is_none());
}

#[test]
fn a_section_that_does_not_fold_stays_open() {
    let list = NavList::new(vec![NavSection::new(
        "library",
        "Library",
        vec![NavItem::new("songs", "Songs")],
    )]);
    let mut h = harness(list, Theme::dark());
    focus(&mut h);
    key(&mut h, Key::Named(NamedKey::Home));
    key(&mut h, Key::Named(NamedKey::ArrowLeft));
    key(&mut h, Key::Named(NamedKey::Enter));
    h.relayout_at((280, 360)).expect("layout");
    assert!(h.root::<NavList>().row_rect("songs").is_some());
}

#[test]
fn the_list_publishes_a_tree_with_selection_state() {
    let h = harness(NavList::new(mailboxes()).selected("inbox"), Theme::dark());
    let node = h
        .root::<NavList>()
        .tree()
        .accessibility()
        .expect("semantics");
    assert_eq!(node.role, crate::a11y::Role::Tree);
    let inbox = node
        .children
        .iter()
        .find(|child| child.name.as_deref() == Some("Inbox"))
        .expect("inbox row");
    assert!(inbox.state.selected);
    let folders = node
        .children
        .iter()
        .find(|child| child.name.as_deref() == Some("Folders"))
        .expect("section row");
    assert!(!folders.state.selected);
    assert_eq!(folders.state.expanded, Some(true));
}

#[test]
fn a_bound_selection_flows_both_ways() {
    let state = crate::LocalState::new(Some("sent".to_owned()));
    let mut h = harness(
        NavList::new(mailboxes()).bind_selection(&state),
        Theme::dark(),
    );
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("sent"));
    state.set(Some("travel".to_owned()));
    h.relayout_at((280, 360)).expect("layout");
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("travel"));
    focus(&mut h);
    key(&mut h, Key::Named(NamedKey::ArrowUp));
    assert_eq!(state.with(Clone::clone).as_deref(), Some("design"));
}

#[test]
fn a_followed_list_shows_the_state_it_follows() {
    let state = std::rc::Rc::new(Cell2::new((mailboxes(), Some("sent".to_owned()))));
    let source = std::rc::Rc::clone(&state);
    let mut h = harness(
        NavList::new(Vec::new()).follow(move || source.try_borrow().ok().map(|s| s.clone())),
        Theme::dark(),
    );
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("sent"));
    assert!(h.root::<NavList>().row_rect("travel").is_some());

    {
        let mut next = state.borrow_mut();
        next.0[0].items.push(NavItem::new("drafts", "Drafts"));
        next.1 = Some("drafts".to_owned());
    }
    h.relayout_at((280, 360)).expect("layout");
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("drafts"));

    // A state that cannot be read leaves the list as it was.
    let held = state.borrow_mut();
    h.relayout_at((280, 360)).expect("layout");
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("drafts"));
    drop(held);
}

#[test]
fn new_sections_keep_the_selection_and_what_was_folded() {
    let mut h = harness(NavList::new(mailboxes()).selected("travel"), Theme::dark());
    focus(&mut h);
    // Fold Work.
    h.root_mut::<NavList>().set_selected(Some("work"));
    h.relayout_at((280, 360)).expect("layout");
    key(&mut h, Key::Named(NamedKey::ArrowLeft));
    let mut next = mailboxes();
    if let Some(section) = next.get_mut(0) {
        section.items.push(NavItem::new("drafts", "Drafts"));
    }
    h.root_mut::<NavList>().set_sections(next);
    h.relayout_at((280, 360)).expect("layout");
    let list = h.root::<NavList>();
    assert_eq!(list.selected_id().as_deref(), Some("work"));
    assert!(list.row_rect("drafts").is_some());
    assert!(list.row_rect("design").is_none(), "Work stays folded");
}

/// A `NavList` and a painted app calling `paint_nav_row` draw the same
/// pixels.
#[test]
fn paint_nav_row_paints_what_the_list_paints() {
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let sections = vec![NavSection::new(
            "places",
            "Places",
            vec![
                NavItem::new("home", "Home").icon(Icon::Home).count(4),
                NavItem::new("music", "Music")
                    .icon(Icon::Music)
                    .live_count(2),
            ],
        )];
        let h = harness(NavList::new(sections).selected("music"), theme.clone());
        let list = h.root::<NavList>();
        let home = list.row_rect("home").expect("home");
        let music = list.row_rect("music").expect("music");
        let header = Rect::new(home.x0, 0.0, home.x1, home.y0);

        let mut scene = vello::Scene::new();
        let mut text = crate::text_render::TextRenderer::new();
        paint_nav_section(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &NavSectionFace::new(header, "Places"),
        );
        paint_nav_row(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &NavRowFace::new(home, "Home")
                .under_section()
                .icon(Icon::Home)
                .count(NavCount::Neutral(4)),
        );
        paint_nav_row(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &NavRowFace::new(music, "Music")
                .under_section()
                .icon(Icon::Music)
                .count(NavCount::Live(2))
                .state(SelectionState::active(true)),
        );
        let ground = theme.tokens.colors.surfaces.content;
        let painted = crate::offscreen::OffscreenRenderer::new(280, 360).render(&scene, ground);
        let listed = h.render_rgba();
        assert_eq!(painted.len(), listed.len());
        let differing: Vec<(usize, [u8; 4], [u8; 4])> = painted
            .chunks_exact(4)
            .zip(listed.chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
            .map(|(i, (a, b))| (i, [a[0], a[1], a[2], a[3]], [b[0], b[1], b[2], b[3]]))
            .collect();
        let bbox = differing
            .iter()
            .fold((usize::MAX, usize::MAX, 0, 0), |acc, (i, _, _)| {
                let (x, y) = (i % 280, i / 280);
                (acc.0.min(x), acc.1.min(y), acc.2.max(x), acc.3.max(y))
            });
        assert!(
            differing.is_empty(),
            "{}: {} pixels differ in {bbox:?}; rows home {home:?} music {music:?}; first {:?}",
            theme.name,
            differing.len(),
            differing.iter().take(6).collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_selected_row_lifts_visibly_on_every_ground() {
    use crate::chrome::{SELECTION_MIN_STEP, lightness_step, selection_tier_on};
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let colors = &theme.tokens.colors;
        for (name, ground) in [
            ("content", colors.surfaces.content),
            ("sidebar", colors.surfaces.sidebar),
            ("raised", colors.surfaces.raised),
        ] {
            let local = theme.on_ground(ground);
            let lift = selection_tier_on(&local, SelectionState::active(true), ground).fill;
            assert!(
                lightness_step(lift, ground) >= SELECTION_MIN_STEP,
                "{} {name}",
                theme.name
            );
        }
    }
}

#[test]
fn row_actions_and_section_actions_report_their_ids() {
    let actions = Rc::new(Cell2::new(Vec::<(String, String)>::new()));
    let seen = Rc::clone(&actions);
    let sections_seen = Rc::new(Cell2::new(Vec::<(String, String)>::new()));
    let section_log = Rc::clone(&sections_seen);
    let list = NavList::new(mailboxes())
        .item_actions(
            vec![NavAction::new("more", Icon::MoreHorizontal, "More")],
            move |item, action| seen.borrow_mut().push((item.to_owned(), action.to_owned())),
        )
        .on_section_action(move |section, action| {
            section_log
                .borrow_mut()
                .push((section.to_owned(), action.to_owned()));
        });
    let mut h = harness(list, Theme::dark());
    let work = h.root::<NavList>().row_rect("work").expect("work");
    let device = winit::event::DeviceId::dummy();
    let click = |h: &mut FrameHarness, x: f64, y: f64| {
        h.dispatch_to_root(&winit::event::WindowEvent::CursorMoved {
            device_id: device,
            position: winit::dpi::PhysicalPosition::new(x, y),
        });
        h.relayout_at((280, 360)).expect("layout");
        h.dispatch_to_root(&winit::event::WindowEvent::MouseInput {
            device_id: device,
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        });
        h.dispatch_to_root(&winit::event::WindowEvent::MouseInput {
            device_id: device,
            state: ElementState::Released,
            button: winit::event::MouseButton::Left,
        });
    };
    // The action shows at the trailing edge of the hovered row.
    let s2 = f64::from(Theme::dark().tokens.spacing.s2);
    click(&mut h, work.x1 - s2 - 10.0, work.center().y);
    assert_eq!(
        actions.borrow().as_slice(),
        [("work".to_owned(), "more".to_owned())]
    );
    let header_y = work.y0 - crate::chrome::nav_row_height(&Theme::dark()) / 2.0;
    click(&mut h, work.x1 - s2 - 10.0, header_y);
    assert_eq!(
        sections_seen.borrow().as_slice(),
        [("folders".to_owned(), "new-folder".to_owned())]
    );
}

#[test]
fn a_rail_stacks_icons_over_labels() {
    let list = NavList::new(mailboxes()).rail();
    let h = harness(list, Theme::dark());
    let inbox = h.root::<NavList>().row_rect("inbox").expect("inbox");
    assert!(
        (inbox.height() - crate::chrome::nav_rail_row_height(&Theme::dark())).abs() < 0.5,
        "rail tiles hold an icon over a caption"
    );
}

fn device() -> winit::event::DeviceId {
    winit::event::DeviceId::dummy()
}

fn point(h: &mut FrameHarness, x: f64, y: f64) {
    h.dispatch_to_root(&winit::event::WindowEvent::CursorMoved {
        device_id: device(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    });
}

fn press(h: &mut FrameHarness, button: winit::event::MouseButton) {
    for state in [ElementState::Pressed, ElementState::Released] {
        h.dispatch_to_root(&winit::event::WindowEvent::MouseInput {
            device_id: device(),
            state,
            button,
        });
    }
}

fn click_at(h: &mut FrameHarness, x: f64, y: f64) {
    point(h, x, y);
    h.relayout_at((280, 360)).expect("layout");
    press(h, winit::event::MouseButton::Left);
}

/// The face item `id` paints with, laid out where it paints.
fn layout_of(list: &NavList, id: &str) -> crate::chrome::NavRowLayout {
    let node = list.tree().model().item_id(id).expect("item");
    let row = list.row_rect(id).expect("row in view");
    let Some(NavRowSpec::Item(item)) = list.tree().nav_spec(node) else {
        panic!("{id} is an item");
    };
    let theme = Theme::dark();
    let face = item.face(row, id, &theme);
    let mut text = crate::text_render::TextRenderer::new();
    crate::chrome::nav_row_layout(&theme, &mut text, &face)
}

#[test]
fn a_secondary_line_and_a_level_grow_the_row() {
    let list = NavList::new(vec![NavSection::new(
        "connections",
        "Connections",
        vec![
            NavItem::new("plain", "Plain"),
            NavItem::new("db", "Orders").secondary("db.example.net:5432"),
            NavItem::new("disk", "Backup disk")
                .secondary("Every night")
                .level(0.6)
                .meta("1.2 TB"),
        ],
    )]);
    let h = harness(list, Theme::dark());
    let theme = Theme::dark();
    let list = h.root::<NavList>();
    let plain = list.row_rect("plain").expect("plain");
    let db = list.row_rect("db").expect("db");
    let disk = list.row_rect("disk").expect("disk");
    assert!((plain.height() - crate::chrome::nav_row_height(&theme)).abs() < 0.5);
    assert!(
        (db.height() - crate::chrome::nav_item_height(&theme, true, false)).abs() < 0.5,
        "a secondary line adds a caption line ({})",
        db.height()
    );
    assert!(db.height() > plain.height());
    assert!(
        disk.height() > db.height(),
        "a level bar sits under the text"
    );
    let layout = layout_of(list, "disk");
    let top = layout.secondary_top.expect("secondary line");
    assert!(
        top > layout.label_top,
        "the secondary line is under the label"
    );
    let bar = layout.level.expect("level bar");
    assert!(bar.y0 > top, "the bar is under the text");
    assert!((bar.height() - f64::from(theme.tokens.spacing.s1)).abs() < 1e-9);
    let meta = layout.meta.expect("meta");
    assert!(
        layout.label_x1 < meta.x0,
        "the label elides before the meta"
    );
}

#[test]
fn trailing_parts_line_up_and_give_way_to_actions() {
    let theme = Theme::dark();
    let mut text = crate::text_render::TextRenderer::new();
    let people = ["alice".to_owned(), "bob".to_owned()];
    let face = NavRowFace::new(Rect::new(0.0, 0.0, 280.0, 40.0), "Firewall zone")
        .status(RowStatus::Warning)
        .status_label("Active", Tone::Success)
        .meta("12")
        .count(NavCount::Neutral(3))
        .presence(&people);
    let layout = crate::chrome::nav_row_layout(&theme, &mut text, &face);
    let status = layout.status.expect("status dot");
    let count = layout.count.expect("count");
    let meta = layout.meta.expect("meta");
    let label = layout.status_label.expect("status label");
    assert_eq!(layout.presence.len(), 2);
    let marks = layout.presence[1];
    // Trailing to leading: status, count, meta, status label, people.
    assert!(count.x1 <= status.x0);
    assert!(meta.x1 <= count.x0);
    assert!(label.x1 <= meta.x0);
    assert!(marks.x1 <= label.x0);
    assert!(layout.presence[0].x0 < marks.x0, "marks overlap in order");
    assert!(layout.label_x1 <= layout.presence[0].x0);
    // Hover or focus swaps them for the row's actions.
    let actions = [Icon::MoreHorizontal];
    let hovered =
        crate::chrome::nav_row_layout(&theme, &mut text, &face.clone().actions(&actions, None));
    assert!(hovered.status.is_none() && hovered.meta.is_none());
    assert!(hovered.status_label.is_none() && hovered.count.is_none());
    assert!(hovered.presence.is_empty());
    assert_eq!(hovered.actions.len(), 1);
    // The parts are announced.
    let spoken = crate::chrome::nav_row_description(&face).expect("description");
    assert!(
        spoken.contains("Active") && spoken.contains("Needs attention"),
        "{spoken}"
    );
}

#[test]
fn badges_share_a_column_so_labels_line_up() {
    let list = NavList::new(vec![NavSection::new(
        "requests",
        "Requests",
        vec![
            NavItem::new("get", "List users").badge("GET"),
            NavItem::new("delete", "Remove user").badge("DELETE"),
        ],
    )]);
    let h = harness(list, Theme::dark());
    let list = h.root::<NavList>();
    let get = layout_of(list, "get");
    let delete = layout_of(list, "delete");
    assert!((get.label_x0 - delete.label_x0).abs() < 1e-9);
    let column = get.badge.expect("badge column");
    assert!((column.width() - delete.badge.expect("badge").width()).abs() < 1e-9);
    assert!(get.label_x0 > column.x1);
}

#[test]
fn a_swatch_paints_the_content_colour_in_the_icon_slot() {
    let colour = vello::peniko::Color::from_rgb8(0x3a, 0x86, 0xd8);
    let list = NavList::new(vec![NavSection::untitled(
        "calendars",
        vec![NavItem::new("work", "Work").swatch(colour)],
    )]);
    let h = harness(list, Theme::dark());
    let layout = layout_of(h.root::<NavList>(), "work");
    let slot = layout.icon.expect("swatch slot");
    let rgba = h.render_rgba();
    let centre = slot.center();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a pixel coordinate inside the 280 px frame"
    )]
    let index = ((centre.y as usize) * 280 + centre.x as usize) * 4;
    let [r, g, b, _] = colour.to_rgba8().to_u8_array();
    let pixel = &rgba[index..index + 3];
    assert!(
        pixel[0].abs_diff(r) <= 2 && pixel[1].abs_diff(g) <= 2 && pixel[2].abs_diff(b) <= 2,
        "the swatch centre is the content colour: {pixel:?}"
    );
}

#[test]
fn a_checked_row_toggles_as_a_whole_and_is_never_chosen() {
    let toggled = Rc::new(Cell2::new(Vec::<(String, bool)>::new()));
    let seen = Rc::clone(&toggled);
    let list = NavList::new(vec![NavSection::new(
        "calendars",
        "Calendars",
        vec![
            NavItem::new("home", "Home").checked(true),
            NavItem::new("work", "Work").checked(false),
        ],
    )])
    .on_check(move |id, on| seen.borrow_mut().push((id.to_owned(), on)));
    let mut h = harness(list, Theme::dark());
    let work = h.root::<NavList>().row_rect("work").expect("work");
    // The label, not only the box, is the target.
    click_at(&mut h, work.center().x, work.center().y);
    assert_eq!(toggled.borrow().as_slice(), [("work".to_owned(), true)]);
    assert_eq!(h.root::<NavList>().selected_id(), None);
    // Space on the focused row toggles it back.
    focus(&mut h);
    key(&mut h, Key::Character(" ".into()));
    assert_eq!(toggled.borrow().last(), Some(&("work".to_owned(), false)));
    let node = h
        .root::<NavList>()
        .tree()
        .accessibility()
        .expect("semantics");
    let home = node
        .children
        .iter()
        .find(|child| child.name.as_deref() == Some("Home"))
        .expect("home row");
    assert_eq!(home.state.checked, Some(true));
}

#[test]
fn sections_start_folded_say_when_empty_and_name_elided_titles() {
    let list = NavList::new(vec![
        NavSection::new("drafts", "Drafts", Vec::new()).empty_caption("No drafts"),
        NavSection::new("archive", "Archive", vec![NavItem::new("old", "Old mail")]).collapsed(),
        NavSection::new(
            "long",
            "A section title far too long for any sidebar to show whole",
            vec![NavItem::new("x", "X")],
        ),
    ]);
    let h = harness(list, Theme::dark());
    let list = h.root::<NavList>();
    let rows: Vec<String> = list
        .tree()
        .visible_rows()
        .iter()
        .map(|(_, label, _)| (*label).to_owned())
        .collect();
    assert!(rows.contains(&"No drafts".to_owned()), "{rows:?}");
    assert!(list.row_rect("old").is_none(), "Archive starts folded");
    assert!(!list.is_expanded("archive"));
    let node = list.tree().model().section_id("long").expect("section");
    let row = list.tree().row_rect(node).expect("header");
    let (_, tip) = list
        .tree()
        .nav_tooltip(node, row)
        .expect("elided title tooltip");
    assert_eq!(
        tip,
        "A section title far too long for any sidebar to show whole"
    );
}

#[test]
fn the_filter_narrows_only_the_sections_that_opt_in() {
    let mut list = NavList::new(vec![
        NavSection::new(
            "connections",
            "Connections",
            vec![
                NavItem::new("prod", "Production"),
                NavItem::new("dev", "Development"),
            ],
        ),
        NavSection::new(
            "tables",
            "Tables",
            vec![
                NavItem::new("users", "users"),
                NavItem::new("orders", "orders"),
                NavItem::new("schema", "public")
                    .children(vec![NavItem::new("user_roles", "user_roles")]),
            ],
        )
        .filterable(),
    ]);
    list.set_filter("user");
    let mut h = harness(list, Theme::dark());
    {
        let list = h.root::<NavList>();
        assert!(list.row_rect("prod").is_some() && list.row_rect("dev").is_some());
        assert!(list.row_rect("users").is_some());
        assert!(list.row_rect("orders").is_none());
        assert!(
            list.row_rect("user_roles").is_some(),
            "a match inside a folded parent shows"
        );
        assert_eq!(
            Filterable::filter_count(list),
            Some(FilterCount::new(3, 4)),
            "users, public and user_roles of four"
        );
    }
    h.root_mut::<NavList>().set_filter("");
    h.relayout_at((280, 360)).expect("layout");
    let list = h.root::<NavList>();
    assert!(list.row_rect("orders").is_some());
    assert!(
        list.row_rect("user_roles").is_none(),
        "the parent folds again"
    );
}

#[test]
fn expansion_is_set_reported_and_ensured() {
    let reported = Rc::new(Cell2::new(Vec::<(String, bool)>::new()));
    let seen = Rc::clone(&reported);
    let mut list = NavList::new(mailboxes()).on_expand(move |id, open| {
        seen.borrow_mut().push((id.to_owned(), open));
    });
    list.set_expanded("work", false);
    let mut h = harness(list, Theme::dark());
    assert!(h.root::<NavList>().row_rect("design").is_none());
    assert!(
        reported.borrow().is_empty(),
        "the app's own change is not reported"
    );
    // The person opens it: reported, so the app can keep it.
    h.root_mut::<NavList>().set_selected(Some("work"));
    focus(&mut h);
    key(&mut h, Key::Named(NamedKey::ArrowRight));
    assert_eq!(reported.borrow().as_slice(), [("work".to_owned(), true)]);
    key(&mut h, Key::Named(NamedKey::ArrowLeft));
    h.relayout_at((280, 360)).expect("layout");
    assert!(h.root::<NavList>().row_rect("design").is_none());
    // Folded by the person, then ensured by the app: the app wins.
    let mut next = mailboxes();
    next[1].items[0] = next[1].items[0].clone().ensure_expanded().count(13);
    h.root_mut::<NavList>().set_sections(next);
    h.relayout_at((280, 360)).expect("layout");
    assert!(h.root::<NavList>().row_rect("design").is_some());
}

#[test]
fn a_row_menu_is_asked_for_with_its_lift() {
    let asked = Rc::new(Cell2::new(Vec::<(String, Rect, vello::kurbo::Point)>::new()));
    let seen = Rc::clone(&asked);
    let list = NavList::new(mailboxes())
        .item_actions(
            vec![NavAction::new("more", Icon::MoreHorizontal, "More").opens_context_menu()],
            |_, _| panic!("a menu action is not reported as an action"),
        )
        .on_context_menu(move |id, lift, at| seen.borrow_mut().push((id.to_owned(), lift, at)));
    let mut h = harness(list, Theme::dark());
    let travel = h.root::<NavList>().row_rect("travel").expect("travel");
    let lift = h.root::<NavList>().row_lift_rect("travel").expect("lift");
    assert!(travel.contains(lift.center()) && lift.width() < travel.width());
    // A right click (and a long press, which arrives as one).
    point(&mut h, travel.center().x, travel.center().y);
    press(&mut h, winit::event::MouseButton::Right);
    assert_eq!(asked.borrow().len(), 1);
    assert_eq!(asked.borrow()[0].0, "travel");
    assert_eq!(asked.borrow()[0].1, lift);
    assert!((asked.borrow()[0].2.y - travel.center().y).abs() < 1e-9);
    assert_ne!(
        h.root::<NavList>().selected_id().as_deref(),
        Some("travel"),
        "asking for a menu does not navigate"
    );
    // The Menu key on the focused row.
    focus(&mut h);
    key(&mut h, Key::Named(NamedKey::ContextMenu));
    assert_eq!(asked.borrow().len(), 2);
    // The More action, anchored under itself.
    let more = h
        .root::<NavList>()
        .item_action_rect("travel", "more")
        .expect("more rect");
    click_at(&mut h, more.center().x, more.center().y);
    let last = asked.borrow().last().cloned().expect("menu from More");
    assert_eq!(last.0, "travel");
    assert!((last.2.x - more.x0).abs() < 1e-9 && (last.2.y - more.y1).abs() < 1e-9);
}

#[test]
fn choosing_a_row_brings_it_into_view() {
    let items: Vec<NavItem> = (0..40)
        .map(|i| NavItem::new(format!("p{i}"), format!("Page {i}")))
        .collect();
    let list = NavList::new(vec![NavSection::untitled("pages", items)]).selected("p39");
    let h = harness(list, Theme::dark());
    let row = h
        .root::<NavList>()
        .row_rect("p39")
        .expect("the chosen row is in view");
    assert!(row.y1 <= 360.0 + 0.5);
}

#[test]
fn delete_and_semantic_keys_address_items() {
    let deleted = Rc::new(Cell2::new(Vec::<String>::new()));
    let seen = Rc::clone(&deleted);
    let list = NavList::new(vec![NavSection::untitled(
        "pages",
        vec![
            NavItem::new("a", "Alpha").semantic_key("page:alpha"),
            NavItem::new("b", "Beta"),
        ],
    )])
    .selected("a")
    .on_delete(move |id| seen.borrow_mut().push(id));
    let mut h = harness(list, Theme::dark());
    focus(&mut h);
    assert!(key(&mut h, Key::Named(NamedKey::Delete)));
    assert_eq!(deleted.borrow().as_slice(), ["a"]);
    let node = h
        .root::<NavList>()
        .tree()
        .accessibility()
        .expect("semantics");
    let keys: Vec<Option<String>> = node
        .children
        .iter()
        .map(|child| child.semantic_key.clone())
        .collect();
    assert!(keys.contains(&Some("page:alpha".to_owned())), "{keys:?}");
    assert!(keys.contains(&Some("nav:item:b".to_owned())), "{keys:?}");
}

#[test]
fn a_staged_drag_lifts_the_row_over_its_drop_line() {
    let mut list = NavList::new(mailboxes()).on_move(|_, _| {});
    // Staged before the first layout, it waits for the rows.
    list.stage_drag("travel", "work");
    let h = harness(list, Theme::dark());
    let list = h.root::<NavList>();
    assert!(list.tree().is_dragging());
    let line = list.tree().reorder_preview().expect("drop line");
    let work = list.row_rect("work").expect("work");
    assert!(
        (line.center().y - work.y0).abs() < 2.0,
        "the line is the seam above Work"
    );
}

// From the coordinator (2026-10-19): shared rails never hyphenate a
// single word ("Connec-tions"); a caption wraps only at spaces. The rail
// grows with the text, up to a cap, to fit its longest single word, and only
// past that cap does a word elide; at stock text the cap is the stock width.
#[test]
fn rail_captions_wrap_at_spaces_and_never_break_a_word() {
    let list = NavList::new(vec![NavSection::untitled(
        "places",
        vec![
            NavItem::new("home", "Home").icon(Icon::Home),
            NavItem::new("subs", "Subscriptions").icon(Icon::Rss),
        ],
    )])
    .rail();
    let theme = Theme::dark();
    let mut h = FrameHarness::for_root(State::new(), list, (76, 360), theme.clone());
    h.relayout_at((76, 360)).expect("layout");
    let list = h.root::<NavList>();
    let home = list.row_rect("home").expect("home");
    let subs = list.row_rect("subs").expect("subs");
    assert!((home.height() - crate::chrome::nav_rail_row_height(&theme)).abs() < 0.5);
    assert!(
        (subs.height() - home.height()).abs() < 0.5,
        "one word too wide for the rail stays one line"
    );
    let mut text = crate::text_render::TextRenderer::new();
    for (label, width) in [
        ("Subscriptions", 60.0),
        ("Connections", 52.0),
        ("Connections", 60.0),
        ("Connections", 68.0),
    ] {
        let lines = crate::chrome::rail_caption_lines(&theme, &mut text, label, width);
        assert_eq!(lines.len(), 1, "{label} at {width}: {lines:?}");
        assert!(
            !lines[0].contains('-'),
            "{label} at {width} is never hyphenated: {lines:?}"
        );
        assert!(
            lines[0] == label || lines[0].ends_with('\u{2026}'),
            "{label} at {width} shows whole or elided: {lines:?}"
        );
    }
    let lines = crate::chrome::rail_caption_lines(&theme, &mut text, "Liked songs", 40.0);
    assert_eq!(lines, ["Liked", "songs"], "two words wrap at the space");

    // At double text the list asks for a rail wide enough for its longest
    // word, so "Channels" shows whole rather than as a fragment.
    use super::RailExtent as _;
    let big = Theme::dark().with_text_scale(2.0);
    let list = NavList::new(vec![NavSection::untitled(
        "places",
        vec![
            NavItem::new("home", "Home").icon(Icon::Home),
            NavItem::new("channels", "Channels").icon(Icon::Rss),
        ],
    )])
    .rail();
    let width = list.fitted_rail_width(&big).expect("a rail");
    assert!(width > f64::from(big.tokens.metrics.rail_width), "{width}");
    assert!(
        width <= f64::from(big.tokens.metrics.rail_max_width),
        "{width}"
    );
    let mut text = crate::text_render::TextRenderer::new();
    text.set_text_scale(big.text_scale);
    let caption = width - 4.0 * f64::from(big.tokens.spacing.s1);
    assert_eq!(
        crate::chrome::rail_caption_lines(&big, &mut text, "Channels", caption),
        ["Channels"]
    );
    // A list that is no rail asks for nothing.
    let plain = NavList::new(vec![NavSection::untitled(
        "p",
        vec![NavItem::new("a", "A")],
    )]);
    assert_eq!(plain.fitted_rail_width(&big), None);
}

/// Sections handed over before the first layout stand at the navigation
/// metrics once laid out, with nothing animating from the stock row
/// height.
#[test]
fn sections_set_before_layout_stand_at_the_nav_metrics() {
    let mut list = NavList::new(vec![NavSection::untitled(
        "top",
        vec![NavItem::new("inbox", "Inbox")],
    )]);
    list.set_sections(mailboxes());
    assert!(
        !list.tree().transitions_active(),
        "nothing moves before layout"
    );
    let h = harness(list, Theme::dark());
    let theme = Theme::dark();
    let list = h.root::<NavList>();
    assert!(!list.tree().transitions_active());
    let row = crate::chrome::nav_row_height(&theme);
    for id in ["inbox", "sent", "work", "design", "travel"] {
        let rect = list.row_rect(id).expect("row");
        assert!((rect.height() - row).abs() < 0.5, "{id}: {}", rect.height());
    }
    let work = list.row_rect("work").expect("work");
    let gap = work.y0 - list.row_rect("sent").expect("sent").y1;
    assert!((gap - crate::chrome::nav_section_height(&theme, false)).abs() < 0.5);
}

#[test]
fn sections_changed_after_layout_immediately_share_paint_and_hit_geometry() {
    let mut h = harness(NavList::new(mailboxes()), Theme::dark());
    let mut next = mailboxes();
    next[0].items.insert(1, NavItem::new("drafts", "Drafts"));
    h.root_mut::<NavList>().set_sections(next);
    assert!(!h.root::<NavList>().tree().transitions_active());
    h.relayout_at((280, 360)).expect("layout");
    let theme = Theme::dark();
    let row = crate::chrome::nav_row_height(&theme);
    let drafts = h.root::<NavList>().row_rect("drafts").expect("drafts");
    assert!((drafts.height() - row).abs() < 0.5);
}

#[test]
fn the_arrow_keys_pass_over_an_empty_sections_caption() {
    let list = NavList::new(vec![
        NavSection::untitled("top", vec![NavItem::new("overview", "Overview")]),
        NavSection::new("drafts", "Drafts", Vec::new()).empty_caption("No drafts"),
        NavSection::new("saved", "Saved", vec![NavItem::new("note", "Quick note")]),
    ])
    .selected("overview");
    let mut h = harness(list, Theme::dark());
    focus(&mut h);
    // Overview, the Drafts header, (its caption passed over), the Saved
    // header, the note.
    for _ in 0..3 {
        key(&mut h, Key::Named(NamedKey::ArrowDown));
    }
    assert_eq!(h.root::<NavList>().selected_id().as_deref(), Some("note"));
}

#[test]
fn exported_section_and_row_buttons_keep_their_keyboard_identity() {
    use crate::a11y::{AccessNode, Action};
    fn focused(node: &AccessNode, name: &str) -> bool {
        (node.name.as_deref() == Some(name) && node.state.focused)
            || node.children.iter().any(|child| focused(child, name))
    }
    let sections = vec![
        NavSection::new("saved", "Saved", vec![NavItem::new("one", "One")])
            .trailing_action(NavAction::new("rename", Icon::Pencil, "Rename section")),
    ];
    let sections_called = Rc::new(Cell2::new(Vec::new()));
    let rows_called = Rc::new(Cell2::new(Vec::new()));
    let section_log = Rc::clone(&sections_called);
    let row_log = Rc::clone(&rows_called);
    let list = NavList::new(sections)
        .with_id("navigation")
        .on_section_action(move |section, action| {
            section_log
                .borrow_mut()
                .push((section.to_owned(), action.to_owned()))
        })
        .item_actions(
            vec![NavAction::new("rename", Icon::Pencil, "Rename row")],
            move |row, action| {
                row_log
                    .borrow_mut()
                    .push((row.to_owned(), action.to_owned()));
            },
        );
    let mut h = harness(list, Theme::dark());
    let section = h
        .a11y_node_named("Rename section Saved")
        .expect("exported section button");
    h.perform_a11y_action(section, &Action::Focus)
        .expect("focus section action");
    assert!(focused(
        &h.root::<NavList>()
            .accessibility()
            .expect("published navigation"),
        "Rename section Saved"
    ));
    h.key("Enter");
    assert_eq!(
        *sections_called.borrow(),
        [("saved".to_owned(), "rename".to_owned())]
    );
    assert!(rows_called.borrow().is_empty());
    let row = h
        .a11y_node_named("Rename row One")
        .expect("exported row button");
    h.perform_a11y_action(row, &Action::Focus)
        .expect("focus row action");
    assert!(focused(
        &h.root::<NavList>()
            .accessibility()
            .expect("published navigation"),
        "Rename row One"
    ));
    h.key("Space");
    assert_eq!(
        *rows_called.borrow(),
        [("one".to_owned(), "rename".to_owned())]
    );
    h.root_mut::<NavList>().set_sections(vec![NavSection::new(
        "saved",
        "Saved",
        vec![NavItem::new("replacement", "Replacement")],
    )]);
    h.relayout();
    h.key("Enter");
    assert_eq!(
        rows_called.borrow().len(),
        1,
        "removed focused action never runs a stale callback"
    );
    assert_eq!(sections_called.borrow().len(), 1);
    assert!(h.a11y_node_named("Rename row One").is_none());
}

#[test]
fn bound_navigation_inventory_follows_the_visible_semantic_window() {
    fn targets(node: &crate::a11y::AccessNode, result: &mut HashSet<String>) {
        if let Some(target) = &node.action_target {
            result.insert(target.clone());
        }
        for child in &node.children {
            targets(child, result);
        }
    }
    let resource = tilo_actions::Resource::new("navigation", "test", "Navigation").with_action(
        tilo_actions::ActionSpec::operation(
            "open",
            "Open destination",
            tilo_actions::OperationKind::View,
            "test.open",
            &(),
        )
        .unwrap(),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "open").unwrap();
    let items = (0..40)
        .map(|index| NavItem::new(format!("item-{index}"), format!("Item {index}")))
        .collect();
    let list = NavList::new(vec![NavSection::new("items", "", items).filterable()])
        .selection_actions((0..40).map(|index| (format!("item-{index}"), binding.clone())));
    let mut frame = harness(list, Theme::dark());
    for filtered in [false, true] {
        if filtered {
            frame.root_mut::<NavList>().set_filter("Item 39");
            frame.relayout_at((280, 360)).unwrap();
        }
        let list = frame.root::<NavList>();
        let mut exported = HashSet::new();
        targets(&Widget::accessibility(list).unwrap(), &mut exported);
        let bindings = crate::widget::controls::control_action_bindings(list);
        assert!(!bindings.is_empty());
        assert!(
            bindings.len() < 40,
            "the inventory must follow viewport/filter bounds"
        );
        for binding in &bindings {
            assert!(
                exported.contains(&binding.control_id),
                "orphan: {binding:?}"
            );
        }
        if filtered {
            assert_eq!(
                bindings.len(),
                1,
                "the filtered destination alone is mounted"
            );
        }
    }
}

fn api_sections(populated: bool) -> Vec<NavSection> {
    ["Auth", "Webhooks", "History"]
        .into_iter()
        .map(|name| {
            let items = if populated {
                vec![
                    NavItem::new(format!("{name}:first"), format!("{name} first request")),
                    NavItem::new(format!("{name}:second"), format!("{name} second request")),
                ]
            } else {
                Vec::new()
            };
            NavSection::new(name, name, items).empty_caption("No requests yet")
        })
        .collect()
}

fn assert_navigation_matches_its_unanimated_paint(
    frame: &FrameHarness,
    sections: Vec<NavSection>,
    theme: &Theme,
) {
    let reference = FrameHarness::for_root(
        State::new(),
        NavList::new(sections).selected("Auth:first"),
        (320, 1200),
        theme.clone(),
    );
    let actual = frame.render_rgba();
    let expected = reference.render_rgba();
    assert_eq!(actual.len(), expected.len());
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.abs_diff(*b) <= 2),
        "immediate live navigation paint must equal the non-overlapping final layout"
    );
    assert!(!frame.root::<NavList>().tree().transitions_active());
    let tree = frame.root::<NavList>().tree();
    let mut previous = None;
    for index in 0..tree.visible_len() {
        let (id, label, _) = tree.visible_row(index).expect("visible navigation row");
        let rect = tree.row_rect(id).expect("actual row target");
        assert!(rect.x0 >= 0.0 && rect.x1 <= 320.0 && rect.y1 <= 1200.0);
        if let Some(bottom) = previous {
            assert!(
                rect.y0 >= bottom,
                "{label} overlaps the preceding navigation row"
            );
        }
        previous = Some(rect.y1);
    }
}

#[test]
fn populated_inserted_removed_and_reordered_navigation_paints_without_live_overlap() {
    for scale in [1.0, 2.0] {
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_text_scale(scale);
            let mut frame = FrameHarness::for_root(
                State::new(),
                NavList::new(api_sections(false)),
                (320, 1200),
                theme.clone(),
            );
            let mut sections = api_sections(true);
            frame.root_mut::<NavList>().set_sections(sections.clone());
            frame.root_mut::<NavList>().set_selected(Some("Auth:first"));
            frame
                .relayout_at((320, 1200))
                .expect("populate real navigation");
            assert_navigation_matches_its_unanimated_paint(&frame, sections.clone(), &theme);
            sections[0]
                .items
                .insert(1, NavItem::new("Auth:new", "New request"));
            frame.root_mut::<NavList>().set_sections(sections.clone());
            frame
                .relayout_at((320, 1200))
                .expect("insert real navigation row");
            assert_navigation_matches_its_unanimated_paint(&frame, sections.clone(), &theme);
            sections[0].items.remove(1);
            frame.root_mut::<NavList>().set_sections(sections.clone());
            frame
                .relayout_at((320, 1200))
                .expect("remove real navigation row");
            assert_navigation_matches_its_unanimated_paint(&frame, sections.clone(), &theme);
            sections.swap(1, 2);
            frame.root_mut::<NavList>().set_sections(sections.clone());
            frame
                .relayout_at((320, 1200))
                .expect("reorder real navigation sections");
            assert_navigation_matches_its_unanimated_paint(&frame, sections, &theme);
            assert_eq!(
                frame.root::<NavList>().selected_id().as_deref(),
                Some("Auth:first")
            );
        }
    }
}

#[test]
fn focused_navigation_target_stays_at_its_painted_row_after_population() {
    fn is_focused(node: &crate::a11y::AccessNode, label: &str) -> bool {
        (node.name.as_deref() == Some(label) && node.state.focused)
            || node.children.iter().any(|child| is_focused(child, label))
    }
    let mut frame = FrameHarness::for_root(
        State::new(),
        NavList::new(api_sections(true)),
        (320, 1200),
        Theme::dark().with_text_scale(2.0),
    );
    let target = frame
        .a11y_node_named("Auth first request")
        .expect("published row");
    frame
        .perform_a11y_action(target, &crate::a11y::Action::Focus)
        .expect("focus real row");
    let mut sections = api_sections(true);
    sections[0]
        .items
        .insert(1, NavItem::new("Auth:new", "New request"));
    frame.root_mut::<NavList>().set_sections(sections);
    frame
        .relayout_at((320, 1200))
        .expect("update live navigation");
    let list = frame.root::<NavList>();
    assert!(is_focused(
        &list.accessibility().expect("navigation tree"),
        "Auth first request"
    ));
    assert!(
        !list.tree().transitions_active(),
        "focused semantics and paint use one geometry"
    );
    let rect = list.row_rect("Auth:first").expect("focused row geometry");
    frame.click_at(rect.center().x, rect.center().y);
    assert_eq!(
        frame.root::<NavList>().selected_id().as_deref(),
        Some("Auth:first")
    );
}

#[test]
fn labelled_section_insets_items_without_inventing_tree_depth() {
    let h = harness(NavList::new(mailboxes()), Theme::dark());
    let list = h.root::<NavList>();
    for (id, grouped, depth) in [("inbox", false, 0), ("work", true, 0), ("design", true, 1)] {
        let node = list.tree().model().item_id(id).unwrap();
        let Some(NavRowSpec::Item(spec)) = list.tree().nav_spec(node) else {
            panic!("real navigation item");
        };
        assert_eq!(spec.under_section, grouped, "{id}");
        assert_eq!(spec.depth, depth, "real tree depth for {id}");
        let rect = list.row_rect(id).unwrap();
        assert_eq!(rect.x0, 0.0);
        assert_eq!(
            rect.width(),
            280.0,
            "the full row remains the pointer target"
        );
    }
}

#[test]
fn a_row_is_found_by_its_search_terms_and_says_which() {
    let items = vec![
        NavItem::new("pointing", "Mouse & touchpad").search_terms([
            NavSearchTerm::named("Tap to click"),
            NavSearchTerm::named("Natural scrolling").also(["reverse scrolling"]),
            NavSearchTerm::words(["trackpad"]),
        ]),
        NavItem::new("network", "Network")
            .search_terms([NavSearchTerm::named("Airplane mode").also(["flight mode"])]),
        NavItem::new("displays", "Displays").search_terms([NavSearchTerm::named("Night light")]),
    ];
    let tap = super::narrow(&items, "tap");
    assert_eq!(tap.len(), 1, "only the page holding it");
    assert_eq!(tap[0].secondary.as_deref(), Some("Tap to click"));
    let reverse = super::narrow(&items, "reverse");
    assert_eq!(
        reverse[0].secondary.as_deref(),
        Some("Natural scrolling"),
        "a word finds the thing and the row names it"
    );
    let trackpad = super::narrow(&items, "trackpad");
    assert_eq!(trackpad[0].id, "pointing");
    assert_eq!(
        trackpad[0].secondary, None,
        "words for the page itself add nothing"
    );
    let light = super::narrow(&items, "light");
    assert_eq!(
        light
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["displays"],
        "a word start, not the middle of \"flight\""
    );
    assert!(super::narrow(&items, "zzz").is_empty());
}

/// Which form a navigation sidebar starts in follows its places: a short,
/// flat set of places, each with an icon and a label that stands whole in a
/// tile, starts as a rail; headings, a "New" action, nesting, second lines,
/// many places or a label too long for a tile start it as a list. Switching
/// the form keeps the selection.
#[test]
fn a_short_flat_set_of_places_starts_as_a_rail_and_anything_more_as_a_list() {
    use super::AdaptiveNav as _;
    let theme = Theme::dark();
    let place = |id: &str, label: &str| NavItem::new(id, label).icon(Icon::Home);
    let flat = |items: Vec<NavItem>| NavList::new(vec![NavSection::untitled("p", items)]);
    let six: Vec<NavItem> = [
        "Home",
        "Trending",
        "Subscriptions",
        "Queue",
        "History",
        "Settings",
    ]
    .iter()
    .map(|label| place(&label.to_lowercase(), label))
    .collect();
    assert!(
        flat(six.clone()).rail_by_default(&theme),
        "Tube's six places"
    );

    let many: Vec<NavItem> = (0..8).map(|i| place(&format!("p{i}"), "Place")).collect();
    assert!(
        !flat(many).rail_by_default(&theme),
        "more than seven places"
    );
    let headed = NavList::new(vec![NavSection::new("p", "Library", six.clone())]);
    assert!(!headed.rail_by_default(&theme), "a heading");
    assert!(
        !flat(six.clone()).with_headings().rail_by_default(&theme),
        "a sidebar that can hold headings, before any has loaded"
    );
    let mut iconless = six.clone();
    iconless[0] = NavItem::new("home", "Home");
    assert!(
        !flat(iconless).rail_by_default(&theme),
        "a place without an icon"
    );
    let mut nested = six.clone();
    nested[0] = place("home", "Home").children(vec![place("a", "A")]);
    assert!(!flat(nested).rail_by_default(&theme), "nesting");
    let mut long = six.clone();
    long[0] = place("long", "Keyboard shortcuts and input methods");
    assert!(
        !flat(long).rail_by_default(&theme),
        "a label too long for a tile"
    );
    assert!(
        !NavList::new(mailboxes()).rail_by_default(&theme),
        "mailboxes"
    );

    // A list is wider than its rail, and the switch keeps what is chosen.
    let mut list = flat(six).selected("queue");
    assert!(list.list_fit(&theme) > list.rail_fit(&theme));
    assert!(!list.tree().is_rail());
    assert!(list.set_rail(true));
    assert!(list.tree().is_rail() && !list.set_rail(true));
    assert_eq!(list.selected_id().as_deref(), Some("queue"));
    assert!(list.set_rail(false));
    assert_eq!(list.selected_id().as_deref(), Some("queue"));
}

/// A row that starts expanded but has nothing under it yet (Photos'
/// Albums before the library loads) opens as asked when its first children
/// arrive: the person never had a chance to fold it.
#[test]
fn a_row_asked_open_opens_when_its_first_children_arrive() {
    let parent = |children: Vec<NavItem>| {
        vec![NavSection::untitled(
            "places",
            vec![
                NavItem::new("albums", "Albums")
                    .icon(Icon::Folder)
                    .children(children)
                    .expanded(),
            ],
        )]
    };
    let mut list = NavList::new(parent(Vec::new()));
    assert!(!list.is_expanded("albums"), "nothing to open yet");
    list.set_sections(parent(vec![NavItem::new("trip", "Trip")]));
    assert!(list.is_expanded("albums"), "the albums show as they arrive");
    list.set_expanded("albums", false);
    list.set_sections(parent(vec![
        NavItem::new("trip", "Trip"),
        NavItem::new("home", "Home"),
    ]));
    assert!(
        !list.is_expanded("albums"),
        "what the person folded stays folded"
    );
}

#[test]
fn a_pending_section_holds_its_rows_place_until_the_items_arrive() {
    let list = NavList::new(vec![
        NavSection::untitled("drives", Vec::new())
            .pending(1)
            .empty_caption("No drives"),
    ]);
    let h = harness(list, Theme::dark());
    let list = h.root::<NavList>();
    let rows = list.tree().visible_rows();
    assert_eq!(rows.len(), 1, "one row stands in, and no caption");
    let (pending, label, _) = rows[0];
    assert_eq!(label, "Loading", "assistive technology hears it is loading");
    assert!(
        !list.tree().model().is_selectable(pending),
        "a pending row is not a destination"
    );
    let stand_in = list.tree().row_rect(pending).expect("the pending row");

    let arrived = NavList::new(vec![
        NavSection::untitled("drives", vec![NavItem::new("disk", "Disk").meta("1 TB")]).pending(1),
    ]);
    let h = harness(arrived, Theme::dark());
    let row = h.root::<NavList>().row_rect("disk").expect("the item");
    assert_eq!(row, stand_in, "the item takes the pending row's place");
}

/// An unread conversation's row stands out, and a person's row carries
/// their avatar in their colour, ringed while they speak.
#[test]
fn an_unread_row_stands_out_and_a_persons_row_shows_them() {
    let colour = Color::from_rgb8(0x40, 0x80, 0xc0);
    let list = NavList::new(vec![NavSection::untitled(
        "rows",
        vec![
            NavItem::new("talk", "talk").unread(true),
            NavItem::new("dana", "Dana").person("u_dana", colour, true),
        ],
    )]);
    let h = harness(list, Theme::dark());
    let list = h.root::<NavList>();
    let face_of = |id: &str| {
        let node = list.tree().model().item_id(id).expect("item");
        let Some(NavRowSpec::Item(item)) = list.tree().nav_spec(node) else {
            panic!("{id} is an item");
        };
        item
    };
    let theme = Theme::dark();
    let talk = face_of("talk");
    let face = talk.face(Rect::new(0.0, 0.0, 200.0, 32.0), "talk", &theme);
    assert!(face.unread);
    assert!(face.avatar.is_none());
    let dana = face_of("dana");
    let face = dana.face(Rect::new(0.0, 0.0, 200.0, 32.0), "Dana", &theme);
    assert!(!face.unread);
    assert!(face.avatar.is_some(), "a person's row shows them");
    assert_eq!(
        dana.person
            .as_ref()
            .map(|person| (person.colour, person.speaking)),
        Some((colour, true))
    );
}

/// A person whose row says more than their name ("Dana (via Discord)")
/// shows their own initials, or their picture.
#[test]
fn a_persons_row_takes_their_face_from_their_name() {
    let colour = Color::from_rgb8(0x40, 0x80, 0xc0);
    let list = NavList::new(vec![NavSection::untitled(
        "rows",
        vec![
            NavItem::new("dana", "Dana Reyes (via Discord)")
                .person("discord:7", colour, false)
                .person_face("Dana Reyes", None),
        ],
    )]);
    let h = harness(list, Theme::dark());
    let list = h.root::<NavList>();
    let node = list.tree().model().item_id("dana").expect("item");
    let Some(NavRowSpec::Item(item)) = list.tree().nav_spec(node) else {
        panic!("dana is an item");
    };
    let theme = Theme::dark();
    let face = item.face(
        Rect::new(0.0, 0.0, 200.0, 32.0),
        "Dana Reyes (via Discord)",
        &theme,
    );
    assert_eq!(
        face.avatar.map(|avatar| avatar.initials()),
        Some("DR".into())
    );
}

/// `count` destinations as a mail sidebar lists them: icons, counts,
/// account badges and secondary lines, in two sections, the second with a
/// "New folder" action, and a calendar section of checkable rows.
fn destinations(count: usize) -> Vec<NavSection> {
    let items = |range: std::ops::Range<usize>| -> Vec<NavItem> {
        range
            .map(|index| {
                let item = NavItem::new(format!("item-{index}"), format!("Folder {index}"))
                    .icon(Icon::Folder);
                match index % 4 {
                    0 => item.count(u32::try_from(index).unwrap_or(u32::MAX)),
                    1 => item.badge(if index % 8 == 1 { "W" } else { "HOME" }),
                    2 => item.secondary(format!("{index} unread")),
                    _ => item,
                }
            })
            .collect()
    };
    let half = count / 2;
    vec![
        NavSection::untitled("top", items(0..half)),
        NavSection::new("folders", "Folders", items(half..count)).trailing_action(NavAction::new(
            "new-folder",
            Icon::Plus,
            "New folder",
        )),
        NavSection::new(
            "calendars",
            "Calendars",
            vec![
                NavItem::new("home", "Home").checked(true),
                NavItem::new("work", "Work").checked(false),
            ],
        ),
    ]
}

const TALL: (u32, u32) = (280, 2800);

/// The list laid out tall enough to show every row.
fn tall(list: NavList) -> FrameHarness {
    let mut h = FrameHarness::for_root(State::new(), list, TALL, Theme::dark());
    h.relayout_at(TALL).expect("layout");
    h
}

/// The list as a walk that kept no row would publish it now.
fn published_afresh(list: &NavList) -> Option<crate::a11y::AccessNode> {
    list.tree
        .walk_keeping_nothing(|| Widget::accessibility(list))
}

/// The rows of `list`'s published tree.
fn rows_of(list: &NavList) -> Vec<std::sync::Arc<crate::a11y::AccessNode>> {
    Widget::accessibility_shared(list)
        .expect("the list's node")
        .children
}

/// A walk that finds the list as it was (a keystroke in the pane beside
/// it) builds none of its rows and hands each over as the very node of the
/// last walk, however many destinations it lists, and publishes what a
/// walk keeping nothing publishes. Its root and whether it is hidden are
/// read without building a row.
#[test]
fn an_unchanged_list_builds_no_row_and_hands_each_over_as_it_was() {
    for count in [3_usize, 60] {
        let h = tall(NavList::new(destinations(count)));
        let list = h.root::<NavList>();
        let first = rows_of(list);
        assert!(
            first.len() > count,
            "every destination and header is on screen: {} rows for {count}",
            first.len()
        );
        let builds = list.tree.access_row_builds_for_test();
        let again = rows_of(list);
        assert_eq!(
            list.tree.access_row_builds_for_test(),
            builds,
            "a walk of {count} unchanged destinations built a row"
        );
        assert_eq!(first.len(), again.len());
        assert!(
            first
                .iter()
                .zip(&again)
                .all(|(before, after)| std::sync::Arc::ptr_eq(before, after)),
            "an unchanged row is handed over as the very node it was"
        );
        assert!(
            !Widget::accessibility_hidden(list),
            "a list is never hidden"
        );
        let root = Widget::accessibility_root(list);
        assert_eq!(
            list.tree.access_row_builds_for_test(),
            builds,
            "the root and hidden state are read without building rows"
        );
        let mut published = Widget::accessibility(list).expect("the list's node");
        assert_eq!(Some(&published), published_afresh(list).as_ref());
        published.children.clear();
        assert_eq!(
            root,
            Some(published),
            "the root is the published node without its rows"
        );
    }
}

/// What changes a row builds it again, and every walk publishes what a walk
/// keeping nothing publishes: a selection moved, a check toggled before the
/// app confirms it, a destination renamed, the theme changed.
#[test]
fn a_change_builds_the_rows_it_changes_and_publishes_as_a_fresh_walk() {
    let mut h = tall(NavList::new(destinations(12)));
    let rows = rows_of(h.root::<NavList>()).len();
    let builds = h.root::<NavList>().tree.access_row_builds_for_test();

    h.root_mut::<NavList>().set_selected(Some("item-5"));
    h.relayout_at(TALL).expect("layout");
    let list = h.root::<NavList>();
    let _ = rows_of(list);
    let after_select = list.tree.access_row_builds_for_test();
    assert!(
        after_select > builds && after_select <= builds + 2,
        "choosing a destination builds the rows it selected and left: {builds} -> {after_select}"
    );
    assert_eq!(Widget::accessibility(list), published_afresh(list));

    // A check shows at once, before the app's next sections confirm it.
    let work = list.row_rect("work").expect("work");
    point(&mut h, work.center().x, work.center().y);
    h.relayout_at(TALL).expect("layout");
    press(&mut h, winit::event::MouseButton::Left);
    h.relayout_at(TALL).expect("layout");
    let list = h.root::<NavList>();
    let published = Widget::accessibility(list).expect("the list's node");
    let work = published
        .children
        .iter()
        .find(|row| row.name.as_deref() == Some("Work"))
        .expect("the work calendar's row");
    assert_eq!(work.state.checked, Some(true), "the toggled check");
    assert_eq!(Some(published), published_afresh(list));

    // Sections handed over again with a destination renamed.
    let mut renamed = destinations(12);
    renamed[0].items[1].label = "Renamed".to_owned();
    h.root_mut::<NavList>().set_sections(renamed);
    h.relayout_at(TALL).expect("layout");
    let list = h.root::<NavList>();
    let published = Widget::accessibility(list).expect("the list's node");
    assert!(
        published
            .children
            .iter()
            .any(|row| row.name.as_deref() == Some("Renamed")),
        "the renamed destination is published by its new name"
    );
    assert_eq!(Some(published), published_afresh(list));

    // Another theme lays the section's action out anew.
    h.relayout_themed(TALL, Theme::light()).expect("layout");
    let list = h.root::<NavList>();
    assert_eq!(Widget::accessibility(list), published_afresh(list));
    assert_eq!(rows_of(list).len(), rows);
}

/// A destination whose selection action is unavailable is published
/// disabled on every walk, however its row is kept; the rows of other
/// destinations are still handed over as they were.
#[test]
fn an_unavailable_destination_is_published_disabled_and_the_rest_kept() {
    let resource = tilo_actions::Resource::new("navigation", "test", "Navigation").with_action(
        tilo_actions::ActionSpec::operation(
            "open",
            "Open destination",
            tilo_actions::OperationKind::View,
            "test.open",
            &(),
        )
        .expect("an action"),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "open").expect("a binding");
    let list = NavList::new(destinations(8))
        .selection_actions((0..4).map(|index| (format!("item-{index}"), binding.clone())));
    let mut h = tall(list);
    crate::widget::controls::ActionControls::refresh_control_action_availability(
        h.root_mut::<NavList>(),
        &HashSet::new(),
    );
    let list = h.root::<NavList>();
    let first = rows_of(list);
    let again = rows_of(list);
    for (before, after) in first.iter().zip(&again) {
        let name = before.name.as_deref().unwrap_or_default();
        let gated = (0..4).any(|index| name == format!("Folder {index}"));
        assert_eq!(
            after.state.disabled, gated,
            "{name} is disabled exactly when its action is unavailable"
        );
        assert_eq!(
            std::sync::Arc::ptr_eq(before, after),
            !gated,
            "{name}: only the rows the list disables are copied"
        );
    }
    assert_eq!(Widget::accessibility(list), published_afresh(list));
}

/// Whether a list holds an action binding, which the host asks on every
/// frame, is what its full binding inventory says, answered without
/// building a row: the visible destinations' selection actions, and nothing
/// for a list without them.
#[test]
fn whether_a_list_binds_actions_is_answered_without_building_rows() {
    use crate::widget::controls::ActionControls;
    fn binds_an_action(list: &NavList) -> bool {
        list.control_action_bindings()
            .iter()
            .any(|binding| matches!(binding.kind, crate::widget::ControlBindingKind::Action(_)))
    }
    let resource = tilo_actions::Resource::new("navigation", "test", "Navigation").with_action(
        tilo_actions::ActionSpec::operation(
            "open",
            "Open destination",
            tilo_actions::OperationKind::View,
            "test.open",
            &(),
        )
        .expect("an action"),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "open").expect("a binding");
    let lists = [
        NavList::new(destinations(8)),
        NavList::new(destinations(8)).selection_actions([("item-2".to_owned(), binding.clone())]),
        // A destination that is not in the list binds nothing.
        NavList::new(destinations(8)).selection_actions([("elsewhere".to_owned(), binding)]),
    ];
    for (index, list) in lists.into_iter().enumerate() {
        let h = tall(list);
        let list = h.root::<NavList>();
        let _ = rows_of(list);
        let builds = list.tree.access_row_builds_for_test();
        assert_eq!(
            list.has_action_control_bindings(),
            index == 1,
            "list {index}: only a visible destination's selection action is an action binding"
        );
        assert_eq!(
            list.tree.access_row_builds_for_test(),
            builds,
            "list {index}: answered without building rows"
        );
        assert_eq!(
            list.has_action_control_bindings(),
            binds_an_action(list),
            "list {index}: the answer is the inventory's"
        );
    }
}

/// The app's region of a host's scene.
#[cfg(feature = "native")]
struct Region;

#[cfg(feature = "native")]
impl crate::app::AppSurface for Region {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(1280, TALL.1)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

/// One host frame: app frames until the app is at rest, then the walk.
#[cfg(feature = "native")]
fn host_frame(app: &mut crate::app::App, damage: &mut crate::app::SurfaceDamage) {
    let mut scene = vello::Scene::new();
    for _ in 0..3 {
        scene.reset();
        if !app.surface_frame_with_damage(&Region, &mut scene, damage) || !app.unsettled() {
            break;
        }
    }
    let _ = app.refresh_semantic_tree();
}

/// What the walk of a keystroke's frame costs in a field beside a list of
/// `count` destinations: the nodes it built, and the subtrees it compared
/// or built. Also the rows the list publishes.
#[cfg(feature = "native")]
fn keystroke_walk_beside(count: usize) -> (usize, usize, usize) {
    use crate::a11y::Role;
    let shell = crate::widget::NavShell::new()
        .sidebar(Box::new(NavList::new(destinations(count))))
        .content(Box::new(
            crate::widget::TextInput::local().with_accessible_name("Note"),
        ));
    let mut app = crate::app::App::new_with_controlled_inputs(State::new(), Box::new(shell));
    let mut damage = crate::app::SurfaceDamage::default();
    app.init_layout();
    for _ in 0..4 {
        host_frame(&mut app, &mut damage);
    }
    let field = app
        .semantic_tree()
        .iter()
        .find(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Note"))
        .map(|node| node.id.clone())
        .expect("the field is published");
    assert_eq!(app.invoke_semantic_action(&field, "focus", None), Ok(true));
    for _ in 0..4 {
        host_frame(&mut app, &mut damage);
    }
    let key = Key::Character("s".into());
    app.surface_input(
        &Region,
        crate::synthetic_input::key_input(&key, ElementState::Pressed),
        false,
    );
    host_frame(&mut app, &mut damage);
    let tree = app.semantic_tree();
    assert!(
        tree.iter()
            .any(|node| node.role == Role::TextInput && node.value.as_deref() == Some("s")),
        "the frame that shows the key publishes it"
    );
    let rows = tree
        .iter()
        .filter(|node| node.role == Role::TreeItem)
        .count();
    (
        tree.nodes_built_for_test(),
        tree.subtrees_compared_for_test(),
        rows,
    )
}

/// A keystroke in a field beside a navigation list costs the walk what the
/// field changed: the list hands every row over as the very node it handed
/// over before, so the walk builds and compares the same beside a list of
/// three destinations as beside one of sixty.
#[cfg(feature = "native")]
#[test]
fn a_keystroke_beside_a_navigation_list_walks_the_same_nodes_however_long_it_is() {
    let (few_built, few_compared, few_rows) = keystroke_walk_beside(3);
    let (many_built, many_compared, many_rows) = keystroke_walk_beside(60);
    assert!(
        few_rows > 3 && many_rows > 60,
        "the list shows every destination: {few_rows} and {many_rows} rows"
    );
    assert_eq!(
        (few_built, few_compared),
        (many_built, many_compared),
        "a keystroke beside {few_rows} rows built and compared ({few_built}, {few_compared}); \
         beside {many_rows}, ({many_built}, {many_compared})"
    );
}

#[test]
fn the_first_tap_after_pointer_use_chooses_the_row_it_landed_on() {
    let chosen = Rc::new(Cell2::new(Vec::<String>::new()));
    let seen = Rc::clone(&chosen);
    let items = (0..8)
        .map(|row| NavItem::new(format!("row-{row}"), format!("Row {row}")))
        .collect();
    let list = NavList::new(vec![NavSection::untitled("all", items)])
        .on_select(move |id| seen.borrow_mut().push(id));
    let mut h = harness(list, Theme::dark());
    let seen_row = h.root::<NavList>().row_rect("row-5").expect("row 5 shown");

    h.tap_at(seen_row.center().x, seen_row.center().y);

    assert_eq!(
        chosen.borrow().as_slice(),
        ["row-5"],
        "the tap chooses the row the person saw under their finger, not the one \
         the touch-sized rows put there"
    );
    let touch_row = h.root::<NavList>().row_rect("row-5").expect("row 5 shown");
    assert!(
        touch_row.height() > seen_row.height()
            && touch_row.height() + 0.5
                >= f64::from(crate::InteractionEnvironment::TOUCH.minimum_collection_row()),
        "once the tap has landed the rows are touch-sized: {seen_row:?} then {touch_row:?}"
    );
}
