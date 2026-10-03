use super::*;
use crate::chrome::{NavRowFace, NavSectionFace, SelectionState, paint_nav_row, paint_nav_section};
use crate::headless::FrameHarness;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::Widget;
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

fn harness(list: NavList, theme: Theme) -> FrameHarness {
    let mut h = FrameHarness::for_root(State::new(), list, (280, 360), theme);
    h.relayout_at((280, 360)).expect("layout");
    h
}

fn key(h: &mut FrameHarness, key: Key) -> bool {
    h.dispatch_to_root(&crate::synthetic_input::key_event(
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
                .icon(Icon::Home)
                .count(NavCount::Neutral(4)),
        );
        paint_nav_row(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &NavRowFace::new(music, "Music")
                .icon(Icon::Music)
                .count(NavCount::Live(2))
                .state(SelectionState::active(true)),
        );
        let ground = theme.tokens.colors.surfaces.content;
        let painted = crate::offscreen::OffscreenRenderer::new(280, 360).render(&scene, ground);
        let listed = h.render_window_rgba();
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
