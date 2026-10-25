use super::*;
use tilo_desktop_model::WorkspaceId;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::Affine;

use crate::workspace_settings::CapRole;
use crate::workspace_settings::overlay::WorkspaceSettingsOverlay;

const NOW: u64 = 1_800_000_000;

fn workspace() -> Workspace {
    let mut ws = Workspace::new(WorkspaceId(71), "Studio");
    ws.set_owner("owner");
    ws.admit("vera".to_string(), CapRole::Viewer);
    ws.admit("cora".to_string(), CapRole::Commenter);
    ws.admit("eddie".to_string(), CapRole::Editor);
    ws.admit("mona".to_string(), CapRole::Moderator);
    ws
}

fn entries() -> Vec<EmojiEntry> {
    ["eddie", "mona", "owner"]
        .iter()
        .map(|who| EmojiEntry {
            name: format!("{who}_wave"),
            added_by: (*who).to_owned(),
        })
        .collect()
}

fn model<'a>(entries: &'a [EmojiEntry], ws: &Workspace, me: &'a str) -> EmojiPaneModel<'a> {
    EmojiPaneModel {
        available: true,
        entries,
        pictures: None,
        grants: EmojiGrants::of(ws, me, NOW),
        me,
        notice: None,
        choosing: false,
    }
}

fn pane() -> Rect {
    Rect::new(0.0, 0.0, 640.0, 520.0)
}

/// The room's roles decide, at the node of the app keeping the record:
/// a Viewer or Commenter changes nothing, an Editor adds and removes their
/// own, and removing someone else's is moderation.
#[test]
fn the_rooms_roles_decide_who_adds_and_removes_emoji() {
    let ws = workspace();
    let grants = |who: &str| EmojiGrants::of(&ws, who, NOW);
    for nobody in ["vera", "cora", "stranger"] {
        assert_eq!(grants(nobody), EmojiGrants::default(), "{nobody}");
    }
    assert_eq!(
        grants("eddie"),
        EmojiGrants {
            add: true,
            remove_own: true,
            remove_any: false
        }
    );
    for moderator in ["mona", "owner"] {
        assert_eq!(
            grants(moderator),
            EmojiGrants {
                add: true,
                remove_own: true,
                remove_any: true
            },
            "{moderator}"
        );
    }
    for verb in [ADD, REMOVE_OWN, REMOVE_ANY] {
        assert!(
            emoji_action(&ws, verb).is_some(),
            "the record declares {verb}"
        );
    }
    assert!(emoji_action(&ws, "rename-emoji").is_none());
}

#[test]
fn a_viewer_sees_every_emoji_and_no_live_control() {
    let ws = workspace();
    let entries = entries();
    let layout = emoji_pane_layout(pane(), &model(&entries, &ws, "vera"), 0.0, 0.0);
    assert_eq!(layout.cells.len(), entries.len());
    assert_eq!(layout.add, Rect::ZERO, "Add is painted, not live");
    assert!(layout.add_slot.area() > 0.0);
    assert!(layout.cells.iter().all(|cell| cell.remove == Rect::ZERO));
    assert_eq!(layout.hit(layout.add_slot.center()), None);
}

#[test]
fn an_editor_adds_and_removes_only_their_own() {
    let ws = workspace();
    let entries = entries();
    let layout = emoji_pane_layout(pane(), &model(&entries, &ws, "eddie"), 0.0, 0.0);
    assert_eq!(
        layout.hit(layout.add.center()),
        Some(EventOutcome::AddEmoji)
    );
    let removable: Vec<_> = layout
        .cells
        .iter()
        .filter(|cell| cell.remove.area() > 0.0)
        .collect();
    assert_eq!(removable.len(), 1);
    assert_eq!(removable[0].name, "eddie_wave");
    assert_eq!(
        layout.hit(removable[0].remove.center()),
        Some(EventOutcome::RemoveEmoji("eddie_wave".into(), Removal::Own))
    );
    // A press on someone else's cell is no removal.
    let mona = layout.cells.iter().find(|cell| cell.name == "mona_wave");
    assert_eq!(layout.hit(mona.expect("listed").rect.center()), None);
}

#[test]
fn a_moderator_removes_anyones_as_moderation() {
    let ws = workspace();
    let entries = entries();
    let layout = emoji_pane_layout(pane(), &model(&entries, &ws, "mona"), 0.0, 0.0);
    let removal = |name: &str| {
        layout
            .cells
            .iter()
            .find(|cell| cell.name == name)
            .and_then(|cell| layout.hit(cell.remove.center()))
    };
    assert_eq!(
        removal("mona_wave"),
        Some(EventOutcome::RemoveEmoji("mona_wave".into(), Removal::Own))
    );
    assert_eq!(
        removal("eddie_wave"),
        Some(EventOutcome::RemoveEmoji(
            "eddie_wave".into(),
            Removal::Moderation
        ))
    );
    assert_eq!(Removal::Moderation.verb(), REMOVE_ANY);
    assert_eq!(Removal::Own.verb(), REMOVE_OWN);
}

#[test]
fn add_waits_for_an_open_chooser_and_an_unavailable_record() {
    let ws = workspace();
    let entries = entries();
    let mut choosing = model(&entries, &ws, "eddie");
    choosing.choosing = true;
    assert_eq!(
        emoji_pane_layout(pane(), &choosing, 0.0, 0.0).add,
        Rect::ZERO
    );
    let unavailable = EmojiPaneModel::unavailable("eddie");
    let layout = emoji_pane_layout(pane(), &unavailable, 0.0, 0.0);
    assert_eq!(layout.add, Rect::ZERO);
    assert!(layout.cells.is_empty());
}

/// The grid fills the width in columns, keeps every cell inside it, and
/// scrolls what does not fit.
#[test]
fn the_grid_scrolls_what_does_not_fit() {
    let ws = workspace();
    let many: Vec<EmojiEntry> = (0..60)
        .map(|n| EmojiEntry {
            name: format!("e{n}"),
            added_by: "eddie".into(),
        })
        .collect();
    let at = |scroll| emoji_pane_layout(pane(), &model(&many, &ws, "eddie"), 0.0, scroll);
    let top = at(0.0);
    assert!(top.max_scroll > 0.0);
    assert!(top.cells.len() < many.len());
    let columns: std::collections::BTreeSet<_> = top
        .cells
        .iter()
        .map(|cell| ui::num::i64_of_f64((cell.rect.x0 * 10.0).round()))
        .collect();
    assert_eq!(columns.len(), 2, "two columns at this width");
    for cell in &top.cells {
        assert!(cell.rect.x0 >= top.grid.x0 && cell.rect.x1 <= top.grid.x1 + 1e-9);
        assert!(cell.remove.area() == 0.0 || top.grid.contains(cell.remove.center()));
    }
    let bottom = at(f64::MAX);
    assert_eq!(
        bottom.cells.last().map(|cell| cell.name.as_str()),
        Some("e59")
    );
}

/// The lens paints the tab and routes presses on what it painted.
#[test]
fn the_lens_paints_the_emoji_tab_and_routes_its_presses() {
    let mut desktop = tilo_desktop_model::DesktopTree::default();
    let ws = workspace();
    desktop.workspaces = vec![ws.clone()];
    desktop.active = ws.id;
    let entries = entries();
    let me = "eddie".to_string();
    let notice = EmojiNotice::not_chosen("the chooser closed");
    let mut emoji = model(&entries, &ws, &me);
    emoji.notice = Some(&notice);
    let model = crate::workspace_settings::overlay::WorkspaceSettingsModel {
        desktop: &desktop,
        tab: crate::workspace_settings::WsTab::Emoji,
        grant_draft: crate::workspace_settings::GrantDraft::default(),
        acting_user: &me,
        people: &crate::collaborators::PeopleNames::default(),
        editing_name: None,
        name_selection: ui::text_selection::Selection::collapsed(0),
        governs_active_room: true,
        owner_controls: false,
        apps: &crate::workspace_settings::NoAppDirectory,
        devices: &[],
        archived: &[],
        public: &crate::workspace_settings::overlay::PublicCard::Asking,
        emoji,
    };
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
    overlay.force_settled();
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    overlay.draw_contents(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &model,
    );
    let tab = crate::workspace_settings::WsTab::Emoji;
    let pill = overlay.tab_pill_rect(tab);
    assert!(pill.area() > 0.0, "the strip offers the Emoji tab");
    assert_eq!(
        overlay.hit_test(pill.center(), crate::workspace_settings::WsTab::General),
        EventOutcome::SelectTab(tab)
    );
    let layout = overlay.emoji_layout().clone();
    assert_eq!(layout.cells.len(), entries.len());
    assert!(layout.notice.area() > 0.0 && layout.dismiss.area() > 0.0);
    assert!(
        layout.notice.y1 <= layout.grid.y0,
        "the notice stands above the grid"
    );
    assert_eq!(
        overlay.hit_test(layout.add.center(), tab),
        EventOutcome::AddEmoji
    );
    assert_eq!(
        overlay.hit_test(layout.dismiss.center(), tab),
        EventOutcome::DismissEmojiNotice
    );
    let own = layout
        .cells
        .iter()
        .find(|cell| cell.name == "eddie_wave")
        .expect("listed");
    assert_eq!(
        overlay.hit_test(own.remove.center(), tab),
        EventOutcome::RemoveEmoji("eddie_wave".into(), Removal::Own)
    );
    // Another tab's press never reaches the Emoji tab's controls.
    assert_ne!(
        overlay.hit_test(layout.add.center(), crate::workspace_settings::WsTab::Apps),
        EventOutcome::AddEmoji
    );
}
