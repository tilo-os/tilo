use super::*;
use tilo_desktop_model::{RoomId, Workspace, WorkspaceId};

/// Three workspaces with the middle one open, so both moves and Delete
/// are live.
fn desktop() -> DesktopTree {
    DesktopTree {
        active: WorkspaceId(2),
        workspaces: vec![
            Workspace::new(WorkspaceId(1), "Home"),
            Workspace::new(WorkspaceId(2), "Studio"),
            Workspace::new(WorkspaceId(3), "Garden"),
        ],
        ..DesktopTree::default()
    }
}

fn devices() -> Vec<WorkspaceDevice> {
    ["Laptop", "Phone", "Living room"]
        .iter()
        .enumerate()
        .map(|(index, label)| WorkspaceDevice {
            label: (*label).to_owned(),
            shown: index != 1,
            here: index == 0,
        })
        .collect()
}

fn archived() -> Vec<ArchivedWorkspace> {
    ["Old project", "Trip"]
        .iter()
        .map(|name| ArchivedWorkspace {
            room: RoomId::new(),
            name: (*name).to_owned(),
        })
        .collect()
}

fn public() -> PublicCard {
    PublicCard::Known {
        public: true,
        description: String::new(),
        default_role: tilo_action_policy::Role::Viewer,
    }
}

/// Lay the lens out over `viewport` and draw its General tab once, as the
/// shell does every frame.
fn drawn(viewport: Rect, model: &WorkspaceSettingsModel<'_>) -> WorkspaceSettingsOverlay {
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.layout(viewport);
    overlay.draw_contents(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        model,
    );
    overlay
}

/// Scroll the body until `rect` of the overlay stands wholly inside it.
fn reach(overlay: &mut WorkspaceSettingsOverlay, rect: impl Fn(&WorkspaceSettingsOverlay) -> Rect) {
    let below = rect(overlay).y1 - overlay.general_body_rect().y1;
    if below > 0.0 {
        overlay.scroll_general(below);
    }
    let above = overlay.general_body_rect().y0 - rect(overlay).y0;
    if above > 0.0 {
        overlay.scroll_general(-above);
    }
    let body = overlay.general_body_rect();
    let shown = rect(overlay);
    assert!(
        shown.area() > 0.0 && shown.y0 >= body.y0 - 0.5 && shown.y1 <= body.y1 + 0.5,
        "{shown:?} scrolls into {body:?}"
    );
}

macro_rules! model {
    ($desktop:expr, $devices:expr, $archived:expr, $public:expr, $user:expr) => {
        WorkspaceSettingsModel {
            desktop: $desktop,
            tab: WsTab::General,
            grant_draft: GrantDraft::default(),
            acting_user: $user,
            people: &crate::collaborators::PeopleNames::default(),
            editing_name: None,
            name_selection: ui::text_selection::Selection::collapsed(0),
            owner_controls: true,
            apps: &super::super::NoAppDirectory,
            devices: $devices,
            archived: $archived,
            public: $public,
            emoji: super::super::emoji_pane::EmojiPaneModel::unavailable($user),
        }
    };
}

#[test]
fn compact_general_body_scroll_reaches_summary_and_keeps_hit_tests_in_clip() {
    let (desktop, devices, archived, public, user) = (
        desktop(),
        devices(),
        archived(),
        public(),
        "owner".to_string(),
    );
    let model = model!(&desktop, &devices, &archived, &public, &user);
    let mut overlay = drawn(Rect::new(0.0, 0.0, 960.0, 540.0), &model);
    let name = overlay.name_rect();
    assert!(overlay.general_body_rect().contains(name.center()));
    assert_eq!(
        overlay.hit_test(name.center(), WsTab::General),
        EventOutcome::EditName
    );
    let below = overlay.general_summary_rect().y1 - overlay.general_body_rect().y1;
    assert!(below > 0.0, "a compact body starts with the summary below");
    reach(&mut overlay, WorkspaceSettingsOverlay::general_summary_rect);
    // The name field scrolled out of the body: a press where it was lands
    // on what is there now, never on the hidden field.
    assert_ne!(
        overlay.hit_test(name.center(), WsTab::General),
        EventOutcome::EditName
    );
    assert!(overlay.scroll_general(-1000.0));
    assert_eq!(overlay.name_rect(), name);
}

/// Archived workspaces are listed, each with Restore and Delete, and
/// scroll into view; nothing is listed while none is archived.
#[test]
fn archived_workspaces_are_listed_with_restore_and_delete() {
    let (desktop, devices, archived, public, user) = (
        desktop(),
        devices(),
        archived(),
        public(),
        "owner".to_string(),
    );
    let none = model!(&desktop, &devices, &[], &public, &user);
    let overlay = drawn(Rect::new(0.0, 0.0, 960.0, 540.0), &none);
    assert_eq!(overlay.general.rect(&general::restore_id(0)), Rect::ZERO);

    let model = model!(&desktop, &devices, &archived, &public, &user);
    let mut overlay = drawn(Rect::new(0.0, 0.0, 960.0, 540.0), &model);
    assert!(
        overlay.general.rect(&general::restore_id(0)).y0
            > overlay.general.rect(general::PUBLIC_SWITCH).y1,
        "listed below the Public group"
    );
    reach(&mut overlay, |o| o.general.rect(&general::restore_id(1)));
    let restore = overlay.general.rect(&general::restore_id(1));
    assert_eq!(
        overlay.hit_test(restore.center(), WsTab::General),
        EventOutcome::RestoreArchived(1)
    );
    reach(&mut overlay, |o| {
        o.general.rect(&general::delete_archived_id(0))
    });
    let delete = overlay.general.rect(&general::delete_archived_id(0));
    assert_eq!(
        overlay.hit_test(delete.center(), WsTab::General),
        EventOutcome::DeleteArchived(0)
    );
}

/// The devices scroll fully into view and their controls answer as the
/// device toggles and the moves; Delete answers from its own row.
#[test]
fn the_devices_scroll_into_view_and_their_controls_answer() {
    let (desktop, devices, archived, public, user) = (
        desktop(),
        devices(),
        archived(),
        public(),
        "owner".to_string(),
    );
    let model = model!(&desktop, &devices, &archived, &public, &user);
    let mut overlay = drawn(Rect::new(0.0, 0.0, 960.0, 540.0), &model);
    for (index, _) in devices.iter().enumerate() {
        reach(&mut overlay, |o| o.general.rect(&general::device_id(index)));
        let toggle = overlay.general.rect(&general::device_id(index));
        assert_eq!(
            overlay.hit_test(toggle.center(), WsTab::General),
            EventOutcome::ToggleDevice(index)
        );
    }
    for (id, outcome) in [
        (general::MOVE_UP, EventOutcome::MoveWorkspace(-1)),
        (general::MOVE_DOWN, EventOutcome::MoveWorkspace(1)),
        (general::DELETE, EventOutcome::DeleteWorkspace),
    ] {
        reach(&mut overlay, |o| o.general.rect(id));
        let rect = overlay.general.rect(id);
        assert_eq!(overlay.hit_test(rect.center(), WsTab::General), outcome);
    }
}

/// The only workspace cannot be deleted, and the first cannot move
/// earlier: those buttons answer no press.
#[test]
fn controls_that_take_no_input_answer_no_press() {
    let only = DesktopTree {
        active: WorkspaceId(1),
        workspaces: vec![Workspace::new(WorkspaceId(1), "Home")],
        ..DesktopTree::default()
    };
    let user = "owner".to_string();
    let public = public();
    let model = model!(&only, &[], &[], &public, &user);
    let overlay = drawn(Rect::new(0.0, 0.0, 1280.0, 1400.0), &model);
    for id in [general::DELETE, general::MOVE_UP, general::MOVE_DOWN] {
        assert_eq!(overlay.general.rect(id), Rect::ZERO, "{id} is inert");
    }
}

/// On a phone-wide window every control keeps its own place: buttons
/// that do not fit move to a line of their own, none overlaps another or
/// runs past the body, and Delete stands last on its own row, a section
/// apart from everything else.
#[test]
fn at_420_px_no_control_overlaps_and_delete_stands_apart() {
    let (desktop, devices, archived, public, user) = (
        desktop(),
        devices(),
        archived(),
        public(),
        "owner".to_string(),
    );
    let model = model!(&desktop, &devices, &archived, &public, &user);
    let overlay = drawn(Rect::new(0.0, 0.0, 420.0, 900.0), &model);
    let body = overlay.general_body_rect();
    let rects: Vec<(String, Rect)> = [
        general::NAME,
        general::INVITE,
        general::MOVE_UP,
        general::MOVE_DOWN,
        general::PUBLIC_SWITCH,
        general::PUBLIC_ROLE,
        general::PUBLIC_DESCRIBE,
        general::DELETE,
    ]
    .iter()
    .map(|id| (*id).to_owned())
    .chain((0..devices.len()).map(general::device_id))
    .chain((0..archived.len()).map(general::restore_id))
    .chain((0..archived.len()).map(general::delete_archived_id))
    .map(|id| {
        let rect = overlay.general.rect(&id);
        (id, rect)
    })
    .collect();
    for (index, (id, rect)) in rects.iter().enumerate() {
        assert!(rect.area() > 0.0, "{id} is laid out");
        assert!(
            rect.x0 >= body.x0 - 0.5 && rect.x1 <= body.x1 + 0.5,
            "{id} {rect:?} stays inside the body {body:?}"
        );
        for (other, other_rect) in &rects[index + 1..] {
            assert!(
                rect.intersect(*other_rect).area() <= 0.0,
                "{id} {rect:?} overlaps {other} {other_rect:?}"
            );
        }
    }
    let delete = overlay.general.rect(general::DELETE);
    let gap = f64::from(ui::tokens::Spacing::BASE.s4);
    for (id, rect) in &rects {
        if id != general::DELETE {
            assert!(
                delete.y0 >= rect.y1 + gap,
                "Delete stands a section below {id}: {delete:?} vs {rect:?}"
            );
        }
    }
}

/// Descriptions wrap to their whole text in a narrow lens, as Settings'
/// do, rather than being cut short.
#[test]
fn descriptions_wrap_to_their_whole_text_at_420_px() {
    use ui::widget::Widget;
    fn find<'a>(widget: &'a dyn Widget, text: &str) -> Option<&'a dyn Widget> {
        if widget
            .accessibility()
            .and_then(|node| node.name)
            .is_some_and(|name| name == text)
        {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find(child, text))
    }
    let (desktop, devices, archived, user) =
        (desktop(), devices(), archived(), "owner".to_string());
    let private = PublicCard::Known {
        public: false,
        description: String::new(),
        default_role: tilo_action_policy::Role::Viewer,
    };
    let model = model!(&desktop, &devices, &archived, &private, &user);
    let description = crate::i18n::t("workspace-settings-public-off");
    let shown = |page: &general::GeneralPage| {
        let page = page.widget().expect("the page is laid out");
        find(page, &description)
            .expect("the Public description is shown")
            .get_rect()
    };
    let overlay = drawn(Rect::new(0.0, 0.0, 420.0, 900.0), &model);
    let narrow = shown(&overlay.general);
    assert!(narrow.width() <= overlay.general_body_rect().width() + 0.5);
    // The same page with room for every description on one line.
    let mut wide = general::GeneralPage::default();
    wide.lay_out(
        &general::content(&model),
        4000.0,
        &ui::chrome::shell::on_panel(&Theme::dark()),
        &mut TextRenderer::new(),
    );
    let line = shown(&wide).height();
    assert!(
        narrow.height() >= line * 1.8,
        "the description wraps onto more lines at 420 px: {} vs one line {line}",
        narrow.height()
    );
}

/// Without a network to ask the host through, the Public group is left
/// out: nothing about it could be learned or changed from here.
#[test]
fn offline_leaves_the_public_group_out() {
    let (desktop, devices, user) = (desktop(), devices(), "owner".to_string());
    let offline = model!(&desktop, &devices, &[], &PublicCard::Offline, &user);
    assert!(general::content(&offline).public.is_none());
    let unavailable = PublicCard::Unavailable("why".into());
    let model = model!(&desktop, &devices, &[], &unavailable, &user);
    assert_eq!(
        general::content(&model)
            .public
            .map(|(group, _)| group.description),
        Some("why".to_owned())
    );
}
