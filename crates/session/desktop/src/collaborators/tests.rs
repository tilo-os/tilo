use super::*;
use tilo_desktop_model::{Collaborator, DesktopTree};

/// Everything a reader is told of `node` and its children: names and descriptions both.
fn spoken(node: &ui::a11y::AccessNode, said: &mut Vec<String>) {
    said.extend(
        [node.name.clone(), node.description.clone()]
            .into_iter()
            .flatten(),
    );
    for child in &node.children {
        spoken(child, said);
    }
}

fn row(id: &str, label: &str) -> RosterRow {
    RosterRow {
        id: id.into(),
        label: label.into(),
        initial: label.chars().next().unwrap_or('?'),
        color: Color::from_rgb8(0x25, 0x63, 0xeb),
        is_agent: false,
        is_bot: false,
        role: None,
        invited: false,
        activity: Some(format!("Viewing {label}'s notes")),
        working_in: None,
        working_app: None,
        status: None,
        recent: Vec::new(),
        followable: true,
        availability: ui::Availability::Available,
        call: None,
        join: None,
        own_device: false,
    }
}

/// A person's screen with one window on it, named by its app.
fn screen(app: &str) -> Screen {
    Screen {
        size: (1280.0, 800.0),
        windows: vec![crate::here::ScreenWindow {
            app: app.into(),
            record: [7; 8],
            rect: Rect::new(0.0, 0.0, 8000.0, 6000.0),
            focused: true,
            name: Some(app.into()),
        }],
    }
}

fn paint(overlay: &mut CollaboratorsOverlay, rows: &[RosterRow], expanded: Option<&str>) {
    paint_in(overlay, rows, expanded, Rect::new(0.0, 0.0, 1400.0, 900.0));
}

fn paint_in(
    overlay: &mut CollaboratorsOverlay,
    rows: &[RosterRow],
    expanded: Option<&str>,
    host: Rect,
) {
    paint_over(overlay, rows, expanded, host, false, &[]);
}

/// The roster kept on screen at an edge, each person's screen under their row.
fn paint_kept(
    overlay: &mut CollaboratorsOverlay,
    rows: &[RosterRow],
    screens: &[(String, Option<Screen>)],
) {
    paint_over(
        overlay,
        rows,
        None,
        Rect::new(0.0, 0.0, 900.0, 900.0),
        true,
        screens,
    );
}

/// Draw the roster the way `host` shows it; `pinned` keeps it at an edge with each person's
/// screen under their row.
fn paint_over(
    overlay: &mut CollaboratorsOverlay,
    rows: &[RosterRow],
    expanded: Option<&str>,
    host: Rect,
    pinned: bool,
    screens: &[(String, Option<Screen>)],
) {
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    overlay.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &CollaboratorsModel {
            host,
            workspace: "Lounge",
            rows,
            may_invite: true,
            may_pin: pinned,
            pinned,
            panel: None,
            screens,
            followed_id: None,
            following_pairs: &[],
            expanded_id: expanded,
            waved: &[],
            waved_until: None,
        },
        None,
    );
}

/// One section takes no label: the header already says who is here, so its rows stand where
/// the list begins.
#[test]
fn a_single_section_takes_no_label() {
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &[row("ada", "Ada")], None);
    assert!(overlay.headings.is_empty(), "{:?}", overlay.headings);
    assert_eq!(overlay.rows[0].rect.y0, overlay.body.y0);
}

/// A phone's roster offers Follow on software's row: an agent is followed
/// to its run like anyone else, and the control stands inside the panel
/// the person can reach at a phone's width.
#[test]
fn a_phone_roster_offers_follow_on_the_agents_row() {
    let mut agent = row("claude", "Claude Code");
    agent.is_agent = true;
    let rows = vec![row("ada", "Ada"), agent];
    let mut overlay = CollaboratorsOverlay::default();
    paint_in(&mut overlay, &rows, None, Rect::new(0.0, 0.0, 390.0, 844.0));
    let place = overlay
        .rows
        .iter()
        .find(|place| place.index == 1)
        .expect("the agent's row is laid out");
    assert!(place.follow.area() > 0.0, "the agent's row offers Follow");
    assert!(
        place.follow.x0 >= overlay.panel.x0 && place.follow.x1 <= overlay.panel.x1,
        "Follow stands inside the panel: {:?} in {:?}",
        place.follow,
        overlay.panel
    );
    assert!(
        place.follow.x0 >= 0.0 && place.follow.x1 <= 390.0,
        "{:?} is on the phone's screen",
        place.follow
    );
    assert_eq!(
        overlay.hit_test(place.follow.center()),
        CollaboratorsHit::Follow(1)
    );
    // A control with no bounds is one nobody can press: what the person
    // sees, the pointer and assistive technology all read this place.
    let reachable = overlay
        .rect_for_hit(CollaboratorsHit::Follow(1))
        .and_then(|rect| {
            let clipped = overlay.view_rect(rect);
            (clipped.area() > 0.0).then_some(clipped)
        });
    assert!(
        reachable.is_some(),
        "Follow is inside the panel's list on screen: {:?} in body {:?}",
        place.follow,
        overlay.body
    );
    let named = overlay
        .accessibility_snapshot()
        .children
        .iter()
        .filter_map(|node| node.name.clone())
        .any(|name| name == t_args("a11y-shell-follow", &[("name", "Claude Code".into())]));
    assert!(named, "the roster offers Follow Claude Code");
}

/// An agent's row says what it is doing now, in its own words, and the window it does it in:
/// its name alone leaves a person watching it no knowing whether it works or idles.
#[test]
fn an_agents_row_says_what_it_is_doing_and_where() {
    let mut agent = row("claude", "Claude Code");
    agent.is_agent = true;
    agent.activity = None;
    agent.working_in = Some((7, "Launch notes".into()));
    agent.status = Some(RosterStatus {
        text: "Writing in Launch notes".into(),
        source: LineSource::Observed,
    });
    assert_eq!(
        doing_line(&agent).as_deref(),
        Some("Writing in Launch notes")
    );
    assert_eq!(
        working_in_line(&agent),
        (
            "Writing in Launch notes".to_owned(),
            Some("Launch notes".to_owned())
        ),
        "what it is doing now, and the window it does it in, under it"
    );
    assert!(
        says_automatically(&agent),
        "software's words about its work are marked as read off the work"
    );
    // Idle, with no line of its own, the row still names the record it worked in last.
    agent.status = None;
    assert_eq!(working_in_line(&agent), ("Launch notes".to_owned(), None));
    assert!(!says_automatically(&agent));
    let rows = vec![row("ada", "Ada"), agent];
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &rows, None);
    assert!(
        overlay.rows[1].working_in.area() > 0.0,
        "the line stands under the agent's row: {:?}",
        overlay.rows[1]
    );
    let mut said = Vec::new();
    spoken(&overlay.accessibility_snapshot(), &mut said);
    let said = said.join(" ");
    assert!(
        said.contains("Launch notes"),
        "a reader is told the record too: {said}"
    );
}

/// An agent with no window to name says whole what it is doing: under its name in its row,
/// which keeps no room for a Wave software never has, and where even that column is too
/// narrow, on a line of its own under the row. Beside room kept for Wave it read "Reading
/// Launch notes in…" even on a wide panel.
#[test]
fn an_agents_doing_line_is_never_cut_short() {
    let mut agent = row("claude", "Claude Code");
    agent.is_agent = true;
    agent.activity = None;
    let doing = "Reading Launch notes in Notes";
    agent.status = Some(RosterStatus {
        text: doing.into(),
        source: LineSource::Observed,
    });
    let rows = vec![agent];
    let theme = Theme::default();
    let mut text = TextRenderer::new();
    let needs = f64::from(
        text.measure_style(doing, None, &theme.text(TextRole::Caption))
            .0,
    );
    for width in [1400.0, 390.0, 360.0] {
        let mut overlay = CollaboratorsOverlay::default();
        paint_in(&mut overlay, &rows, None, Rect::new(0.0, 0.0, width, 900.0));
        let place = overlay.rows[0].clone();
        if place.said.area() > 0.0 {
            assert!(
                needs <= place.said.width(),
                "under the name, the words whole at {width}: {needs} in {:?}",
                place.said
            );
            // A second line of the row: close under the name, never a control's height away.
            assert!(
                place.said.y0 >= place.rect.center().y && place.said.y0 <= place.rect.y1,
                "right under the name at {width}: {:?} in {:?}",
                place.said,
                place.rect
            );
            assert_ne!(
                overlay.hit_test(place.said.center()),
                CollaboratorsHit::OpenTab(0),
                "with no window, the line opens none"
            );
        } else {
            // In the row: the column between its face and Follow, as the row is painted.
            let probe = ListRowFace::new("Claude Code")
                .leading_column(FACE)
                .trailing_width(overlay.row_trailing(&rows[0], false, &theme));
            let start = probe.text_column(place.rect, &mut text, &theme);
            let end = probe
                .trailing_rect(place.rect, &mut text, &theme)
                .map_or(place.rect.x1, |slot| slot.x0);
            assert!(
                needs <= end - start,
                "in the row, the words whole at {width}: {needs} in {}",
                end - start
            );
            assert!(
                place.follow.x0 >= start + needs,
                "and clear of Follow at {width}: {:?}",
                place.follow
            );
        }
        if width >= 1400.0 {
            assert!(
                place.said.area() <= 0.0,
                "a wide panel says it in the row, under the name: {place:?}"
            );
        }
    }
}

/// A kept roster draws people's screens and never a screen for software: an agent's row names
/// the window it works in instead, and pressing that line brings the window forward.
#[test]
fn an_attached_agent_row_names_its_window_instead_of_mirroring_one() {
    let mut agent = row("claude", "Claude Code");
    agent.is_agent = true;
    agent.working_in = Some((7, "Lounge notes".into()));
    let rows = vec![row("ada", "Ada"), agent];
    let screens = [("ada".to_owned(), Some(screen("Notes")))];
    let mut overlay = CollaboratorsOverlay::default();
    paint_kept(&mut overlay, &rows, &screens);
    assert!(
        overlay.mirrors.iter().any(|(index, _)| *index == 0),
        "a person's row keeps its picture"
    );
    assert!(
        !overlay.mirrors.iter().any(|(index, _)| *index == 1),
        "software has no screen to mirror: {:?}",
        overlay.mirrors
    );
    let place = overlay
        .rows
        .iter()
        .find(|place| place.index == 1)
        .expect("the agent's row is laid out");
    assert!(
        place.working_in.area() > 0.0,
        "its row names the window it works in"
    );
    // The card is the row and that line, as a person's is the row and their mirror: the
    // panel's own body holds it, none of it reaching past the card's edge.
    assert!(
        place.working_in.y0 >= place.rect.y1 && place.working_in.y1 <= overlay.body.y1,
        "{:?} under {:?}",
        place.working_in,
        place.rect
    );
    assert_eq!(
        overlay.hit_test(place.working_in.center()),
        CollaboratorsHit::OpenTab(1)
    );
    assert_eq!(
        overlay.rect_for_hit(CollaboratorsHit::OpenTab(1)),
        Some(place.working_in)
    );
    // It is named for a reader by what it says, not by a control's glyph.
    let named = overlay
        .accessibility_snapshot()
        .children
        .iter()
        .filter_map(|node| node.name.clone())
        .any(|name| name == "Lounge notes");
    assert!(named, "the line is offered to assistive technology too");
}

/// A person who keeps where they are to themselves gets no picture of nothing: their card is
/// their row and one quiet line, while the person beside them keeps their picture.
#[test]
fn a_private_screen_is_one_line_and_no_well() {
    let rows = vec![row("ada", "Ada"), row("ben", "Ben"), row("cy", "Cy")];
    let nothing_open = Screen {
        size: (1280.0, 800.0),
        windows: Vec::new(),
    };
    let screens = [
        ("ada".to_owned(), None),
        ("ben".to_owned(), Some(nothing_open)),
        ("cy".to_owned(), Some(screen("Notes"))),
    ];
    let mut overlay = CollaboratorsOverlay::default();
    paint_kept(&mut overlay, &rows, &screens);
    let mut text = TextRenderer::new();
    assert!(
        !overlay.mirrors.iter().any(|(index, _)| *index < 2),
        "no picture where there is no screen to show: {:?}",
        overlay.mirrors
    );
    assert!(overlay.mirrors.iter().any(|(index, _)| *index == 2));
    for (index, said) in [
        (0, t("people-panel-screen-private")),
        (1, t("people-panel-nothing-open")),
    ] {
        assert_eq!(screen_line(screens[index].1.as_ref()), said);
        let place = overlay
            .rows
            .iter()
            .find(|place| place.index == index)
            .expect("the row is laid out");
        assert!(place.said.area() > 0.0, "one line says why there is none");
        let column = overlay.body.x0 + FACE + f64::from(Theme::dark().tokens.spacing.s3);
        assert_eq!(
            place.said.x0, column,
            "the line stands in the column their name does"
        );
        assert!(
            place.said.y0 >= place.rect.y1 && place.said.height() < place.rect.height() / 2.0,
            "the card stays compact: {:?} under {:?}",
            place.said,
            place.rect
        );
        let (needs, _) = text.measure_style(&said, None, &Theme::dark().text(TextRole::Caption));
        assert!(
            f64::from(needs) <= place.said.width(),
            "{said} fits the line it stands on: {needs} in {}",
            place.said.width()
        );
    }
}

/// Software's line says what it is doing and the window it does it in: its own now line where
/// it has one, the record's name alone where it has none.
#[test]
fn software_names_its_now_line_and_its_window() {
    let mut agent = row("cc", "Claude Code");
    agent.is_agent = true;
    agent.working_in = Some((7, "Lounge notes".into()));
    assert_eq!(working_in_line(&agent), ("Lounge notes".into(), None));
    agent.status = Some(RosterStatus {
        text: "searching the workspace".into(),
        source: LineSource::Written,
    });
    assert_eq!(
        working_in_line(&agent),
        (
            "searching the workspace".into(),
            Some("Lounge notes".into())
        ),
        "what it is doing, then the window it is doing it in"
    );
}

/// Software that has published no view yet is still followed: its row
/// offers Follow rather than hiding it until the agent is seen working.
#[test]
fn a_headless_agent_is_followable() {
    use tilo_desktop_model::{Workspace, WorkspaceSlot};

    let mut workspace = Workspace::new(WorkspaceSlot(73), "Lounge");
    workspace
        .collaborators
        .push(Collaborator::new("claude", "Claude Code").with_role(AGENT_ROLE_MARKER));
    assert!(
        workspace.collaborators[0].active_tab.is_none(),
        "a headless agent names no window"
    );
    let desktop = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let rows = collaborator_roster_rows(&desktop, "local");
    let agent = rows.iter().find(|row| row.id == "claude").unwrap();
    assert!(agent.is_agent);
    assert!(agent.followable, "the agent can be followed");
}

/// A wave just sent reads as sent on its row, to the pointer's eye and to assistive technology alike; the others still offer one.
#[test]
fn a_wave_just_sent_reads_as_sent() {
    let rows = vec![row("ada", "Ada"), row("ben", "Ben")];
    let mut overlay = CollaboratorsOverlay::default();
    let waved = ["ada".to_owned()];
    overlay.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &CollaboratorsModel {
            host: Rect::new(0.0, 0.0, 1400.0, 900.0),
            workspace: "Lounge",
            rows: &rows,
            may_invite: true,
            may_pin: false,
            pinned: false,
            panel: None,
            screens: &[],
            followed_id: None,
            following_pairs: &[],
            expanded_id: None,
            waved: &waved,
            waved_until: None,
        },
        None,
    );
    let names: Vec<String> = overlay
        .accessibility_snapshot()
        .children
        .iter()
        .filter_map(|node| node.name.clone())
        .collect();
    assert!(
        names.contains(&t_args("a11y-shell-waved", &[("name", "Ada".into())])),
        "{names:?}"
    );
    assert!(
        names.contains(&t_args("a11y-shell-wave", &[("name", "Ben".into())])),
        "{names:?}"
    );
}

/// Voice is one click away: someone in a call this person can join shows "In a call" and offers Join on their row, reached by pointer and keyboard alike and named for assistive technology; someone in no call offers none.
#[test]
fn a_person_in_a_call_offers_join_on_their_row() {
    let mut ada = row("ada", "Ada");
    ada.call = Some("voice".into());
    ada.join = Some(VoiceChannel {
        app: "so.tilo.chat".into(),
        record: String::new(),
        channel: "c_voice".into(),
    });
    let rows = vec![ada, row("ben", "Ben")];
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &rows, None);
    assert_eq!(
        doing_line(&rows[0]),
        Some(t_args(
            "people-panel-in-call",
            &[("channel", "voice".into())]
        ))
    );
    let ada_place = overlay.rows[0].clone();
    assert!(ada_place.join.area() > 0.0, "Ada's row offers Join");
    assert!(
        ada_place.join.x1 <= ada_place.follow.x0 && ada_place.wave.x1 <= ada_place.join.x0,
        "Join stands beside Follow, Wave before it"
    );
    assert_eq!(
        overlay.hit_test(ada_place.join.center()),
        CollaboratorsHit::JoinCall(0)
    );
    assert_eq!(overlay.join_target(0), rows[0].join.as_ref());
    assert_eq!(overlay.rows[1].join, Rect::ZERO, "Ben is in no call");
    assert!(
        overlay
            .focus_order()
            .contains(&CollaboratorsHit::JoinCall(0))
    );
    assert!(
        !overlay
            .focus_order()
            .contains(&CollaboratorsHit::JoinCall(1))
    );
}

#[test]
fn a_workspaces_bot_is_tagged_as_a_bot_never_as_a_person() {
    use tilo_desktop_model::{Workspace, WorkspaceBot, WorkspaceSlot};

    let device = "ab".repeat(32);
    let mut workspace = Workspace::new(WorkspaceSlot(72), "Lounge");
    workspace.bots.push(WorkspaceBot {
        name: "Standup".into(),
        installation: Some("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6".into()),
        device: Some(device.clone()),
        entry: Some("so.tilo.bots.standup".into()),
        version: Some("1.2.0".into()),
        added_by: "owner".into(),
        requested: true,
        paused: false,
    });
    workspace
        .collaborators
        .push(Collaborator::new(device.clone(), "Standup"));
    workspace
        .collaborators
        .push(Collaborator::new("ada", "Ada"));
    let desktop = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };

    let rows = collaborator_roster_rows(&desktop, "local");
    let bot = rows.iter().find(|row| row.id == device).unwrap();
    assert!(bot.is_bot && bot.is_agent, "a bot is software, tagged Bot");
    let ada = rows.iter().find(|row| row.id == "ada").unwrap();
    assert!(!ada.is_bot && !ada.is_agent);
    assert!(collaborator_is_agent(
        desktop.active_workspace(),
        &Collaborator::new(device, "Standup"),
        0
    ));
}

#[test]
fn agent_classification_uses_stable_identity_not_display_label() {
    use tilo_desktop_model::{Workspace, WorkspaceSlot};

    let mut workspace = Workspace::new(WorkspaceSlot(71), "Shared");
    workspace.set_owner("owner");
    let node = workspace.node();
    workspace.grant(tilo_action_policy::Grant::new(
        "agent-member:scribe",
        tilo_action_policy::Grantee::agent("scribe"),
        node,
        tilo_action_policy::KindSet::READ,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person("owner"),
    ));
    workspace
        .collaborators
        .push(Collaborator::new("scribe", "Release Notes Agent"));
    let desktop = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let rows = collaborator_roster_rows(&desktop, "local");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "Release Notes Agent");
    assert!(
        rows[0].is_agent,
        "a renamed agent must never appear as a person"
    );
}

/// Software's row names the window it works in once: what it is doing there, with the record
/// beside it. A "Viewing Launch notes" above that says the same record twice, so software's row
/// carries none, while a person's still says which record they are looking at.
#[test]
fn an_agents_row_names_the_record_it_works_in_once() {
    use tilo_desktop_model::{
        Status, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceSlot,
    };

    let notes = Tab {
        id: 7,
        title: "Launch notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut main = Strip::new(StripId(1), "Main");
    main.window_groups.push(WindowGroup {
        id: 8,
        active_tab: notes.id,
        tabs: vec![notes],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Design");
    workspace.strips = vec![main];
    workspace.collaborators = vec![
        Collaborator {
            id: "ada".into(),
            name: "Ada".into(),
            active_tab: Some(7),
            status: Some(Status {
                text: "Reviewing the plan".into(),
                automatic: false,
            }),
            ..Collaborator::default()
        },
        Collaborator {
            id: "claude".into(),
            name: "Claude Code".into(),
            role_id: Some(AGENT_ROLE_MARKER.to_owned()),
            active_tab: Some(7),
            status: Some(Status {
                text: "Working in Notes".into(),
                automatic: false,
            }),
            ..Collaborator::default()
        },
    ];
    let desktop = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };

    let rows = collaborator_roster_rows(&desktop, "local");
    let agent = rows.iter().find(|row| row.is_agent).expect("the agent");
    assert_eq!(
        agent.activity, None,
        "software's work line already names the window it works in"
    );
    assert_eq!(agent.working_in, Some((7, "Launch notes".to_owned())));
    assert_eq!(
        working_in_line(agent),
        (
            "Working in Notes".to_owned(),
            Some("Launch notes".to_owned())
        ),
        "what it is doing, and the record it does it in: said once"
    );

    let ada = rows.iter().find(|row| row.id == "ada").expect("Ada");
    let viewing = t_args("people-panel-viewing", &[("title", "Launch notes".into())]);
    assert_eq!(
        ada.activity.as_deref(),
        Some(viewing.as_str()),
        "a person's row says which record they are looking at"
    );
    assert_eq!(ada.working_in, None, "only software works in a window");
}

#[test]
fn the_model_keeps_follow_direction_when_cursors_are_not_visible() {
    let rows = vec![row("ada", "Ada"), row("bob", "Bob")];
    let pairs = vec![("ada".to_string(), "bob".to_string())];
    let model = CollaboratorsModel {
        host: Rect::ZERO,
        workspace: "Lounge",
        rows: &rows,
        may_invite: false,
        may_pin: false,
        pinned: false,
        panel: None,
        screens: &[],
        followed_id: None,
        following_pairs: &pairs,
        expanded_id: None,
        waved: &[],
        waved_until: None,
    };
    assert_eq!(
        model.relationship_for("ada"),
        Some(RosterFollowRelationship::Follows("bob".into()))
    );
    assert_eq!(
        model.relationship_for("bob"),
        Some(RosterFollowRelationship::FollowedBy("ada".into()))
    );
}

/// A follower's row says whom they follow, "you" for this person, after where they are.
#[test]
fn a_followers_row_says_whom_they_follow() {
    let mut rows = vec![row("ada", "Ada"), row("bob", "Bob"), row("cy", "Cy")];
    rows[0].activity = Some("In Notes".into());
    rows[1].activity = None;
    rows[2].activity = None;
    let pairs = [
        ("ada".to_owned(), "bob".to_owned()),
        ("cy".to_owned(), "me".to_owned()),
    ];
    note_following(&mut rows, &pairs, Some("me"));
    assert_eq!(rows[0].activity.as_deref(), Some("In Notes, following Bob"));
    assert_eq!(rows[1].activity, None);
    assert_eq!(rows[2].activity.as_deref(), Some("Following you"));
}

/// The person's other devices are listed as theirs, under their own heading after people, with nobody to wave at.
#[test]
fn the_roster_lists_the_person_s_other_devices_as_their_devices() {
    let mut pi = row("peer@7", "Orange Pi");
    pi.own_device = true;
    let rows = vec![pi, row("ada", "Ada")];
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &rows, None);
    let order: Vec<usize> = overlay.rows.iter().map(|place| place.index).collect();
    assert_eq!(order, vec![1, 0], "people first, then the devices");
    assert_eq!(
        overlay
            .headings
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            t("people-panel-section-people"),
            t("people-panel-section-devices")
        ]
    );
    assert_eq!(overlay.rows[1].wave, Rect::ZERO);
    assert_eq!(overlay.wave_target(0), None);
}

/// People, then agents, each with Follow on its row and Wave for people; Invite at the bottom; a press on a row's controls is that control's, on its body opens what they changed.
#[test]
fn the_roster_lists_people_then_agents_with_their_actions_and_invite() {
    let mut agent = row("scribe", "Release Notes Agent");
    agent.is_agent = true;
    let mut ada = row("ada", "Ada");
    ada.recent = vec![RecentChange {
        text: "Rewrote the intro and two headings".into(),
        when: "4 min ago".into(),
        entry: Some([1; 16]),
    }];
    let mut invited = row("cy", "Cy");
    invited.invited = true;
    let rows = vec![agent, ada, row("ben", "Ben"), invited];
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &rows, None);

    let order: Vec<usize> = overlay.rows.iter().map(|place| place.index).collect();
    assert_eq!(
        order,
        vec![1, 2, 0],
        "people first, then agents; the invited are not here"
    );
    assert_eq!(
        overlay
            .headings
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            t("people-panel-section-people"),
            t("people-panel-section-agents")
        ]
    );
    let ada_place = overlay.rows[0].clone();
    assert_eq!(
        overlay.hit_test(ada_place.follow.center()),
        CollaboratorsHit::Follow(1)
    );
    assert_eq!(
        overlay.hit_test(ada_place.wave.center()),
        CollaboratorsHit::Wave(1)
    );
    assert_eq!(
        overlay.hit_test(Point::new(
            ada_place.rect.x0 + 80.0,
            ada_place.rect.center().y
        )),
        CollaboratorsHit::ToggleDetails(1),
        "Ada has recent changes: her row opens"
    );
    let ben_place = overlay.rows[1].clone();
    assert_eq!(
        overlay.hit_test(Point::new(
            ben_place.rect.x0 + 80.0,
            ben_place.rect.center().y
        )),
        CollaboratorsHit::Absorb,
        "nothing recent: nothing to open"
    );
    assert_eq!(overlay.rows[2].wave, Rect::ZERO, "nobody waves at software");
    assert_eq!(
        overlay.hit_test(overlay.invite_rect().center()),
        CollaboratorsHit::Invite
    );
    assert_eq!(
        overlay.hit_test(Point::new(5.0, 800.0)),
        CollaboratorsHit::Dismiss
    );
    assert_eq!(overlay.wave_target(1), Some("ada"));
    assert_eq!(overlay.wave_target(0), None);
}

/// The header counts the people the list shows, besides you.
#[test]
fn the_header_counts_the_people_listed() {
    ui::i18n::I18nManager::init("en-US");
    assert_eq!(here_line(0), "Just you");
    assert_eq!(here_line(1), "You and 1 other");
    assert_eq!(here_line(3), "You and 3 others");
}

/// Only you: the roster says nobody else is here, and still offers Invite.
#[test]
fn only_you_says_nobody_else_is_here_and_offers_invite() {
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &[], None);
    assert!(overlay.rows.is_empty());
    assert!(overlay.invite_rect().area() > 0.0);
    let snapshot = overlay.accessibility_snapshot();
    assert!(
        snapshot
            .children
            .iter()
            .any(|node| node.name.as_deref() == Some(t("a11y-shell-invite").as_str())),
        "Invite is a control"
    );
}

/// An open row shows what the person changed with the way into History; the keyboard reaches every control without a pointer.
#[test]
fn an_open_row_leads_to_history_and_the_keyboard_reaches_every_control() {
    let _settled = ui::motion::testing::settled();
    let mut ada = row("ada", "Ada");
    ada.recent = vec![
        RecentChange {
            text: "Rewrote the intro".into(),
            when: "4 min ago".into(),
            entry: Some([1; 16]),
        },
        RecentChange {
            text: "Added a heading".into(),
            when: "9 min ago".into(),
            entry: Some([2; 16]),
        },
    ];
    let rows = vec![ada, row("ben", "Ben")];
    let mut overlay = CollaboratorsOverlay::default();
    paint(&mut overlay, &rows, Some("ada"));
    paint(&mut overlay, &rows, Some("ada"));
    let changes = overlay.rows[0].changes;
    assert!(changes.area() > 0.0, "the open row has its link to History");
    assert_eq!(
        overlay.hit_test(changes.center()),
        CollaboratorsHit::OpenChanges(0)
    );
    assert_eq!(
        overlay.rows[0].change_rows.len(),
        2,
        "each change is listed"
    );
    let second = overlay.rows[0].change_rows[1];
    assert_eq!(
        overlay.hit_test(second.center()),
        CollaboratorsHit::OpenChange(0, 1),
        "each change opens History at itself"
    );
    assert_eq!(overlay.change_target(0, 1), Some(("Ada", Some([2; 16]))));

    let mut reached = Vec::new();
    for _ in 0..overlay.focus_order().len() {
        overlay.key(&Key::Named(NamedKey::Tab), false);
        reached.push(overlay.keyboard_focus);
    }
    for wanted in [
        CollaboratorsHit::ToggleDetails(0),
        CollaboratorsHit::Wave(0),
        CollaboratorsHit::Follow(0),
        CollaboratorsHit::OpenChange(0, 0),
        CollaboratorsHit::OpenChange(0, 1),
        CollaboratorsHit::OpenChanges(0),
        CollaboratorsHit::Wave(1),
        CollaboratorsHit::Follow(1),
        CollaboratorsHit::Invite,
    ] {
        assert!(reached.contains(&Some(wanted)), "{wanted:?} is reachable");
    }
    assert_eq!(
        overlay.key(&Key::Named(NamedKey::Escape), false),
        Some(CollaboratorsHit::Dismiss)
    );
}

/// What someone is doing: a call first, then what they wrote, then how available they are; nothing for someone simply here.
#[test]
fn what_someone_is_doing_reads_their_call_status_or_availability() {
    let mut ada = row("ada", "Ada");
    assert_eq!(doing_line(&ada), None);
    ada.availability = ui::Availability::Idle;
    assert_eq!(doing_line(&ada), Some(t("people-panel-idle")));
    ada.status = Some(RosterStatus {
        text: "Reviewing the deck".into(),
        source: LineSource::Written,
    });
    assert_eq!(doing_line(&ada).as_deref(), Some("Reviewing the deck"));
    ada.call = Some("voice".into());
    assert_eq!(
        doing_line(&ada),
        Some(t_args(
            "people-panel-in-call",
            &[("channel", "voice".into())]
        ))
    );
    ada.call = Some(String::new());
    assert_eq!(doing_line(&ada), Some(t("people-panel-in-a-call")));
}
