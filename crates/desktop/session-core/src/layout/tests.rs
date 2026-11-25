//! What text typed on the canvas finds and stands, and how a query starts.

use super::{
    Target as Placed, found_place, found_resource, most_specific, placed, query_state_from_outcome,
};
use tilo_desktop_search::{Hit, InvokeTarget, ResourceKind, Target};

/// The query's starting state is the app's own search invocation result: a run that was taken
/// keeps searching; a refusal or failure is the query `.failed` with the action's own reason —
/// never a guess read from the app's records.
#[test]
fn a_refused_search_starts_the_query_failed_with_its_own_reason() {
    assert_eq!(
        query_state_from_outcome(Ok(())),
        ("searching".to_owned(), String::new()),
        "a search the app took is still searching"
    );
    let (state, reason) = query_state_from_outcome(Err("Type something to search for".into()));
    assert_eq!(state, "failed", "a refused search is failed");
    assert_eq!(
        reason, "Type something to search for",
        "with the action's own reason"
    );
}

/// A row text typed on the canvas would stand for: what it opens, and what it is.
fn opening(resource: &str, title: &str, kind: ResourceKind) -> Hit {
    Hit {
        row: None,
        preview: None,
        faces: Vec::new(),
        outcome: None,
        kind,
        provider: "test".into(),
        id: resource.into(),
        title: title.into(),
        subtitle: None,
        facts: Vec::new(),
        score: 0,
        icon: None,
        target: Target::Invoke(Box::new(InvokeTarget::open("test", "", resource, resource))),
    }
}

/// A row for an action an app publishes on `resource` : `find` answers it (it is a row a caller
/// acts on) but a place does not stand it, so it is never what typing `text` places.
fn acting(resource: &str, action: &str) -> Hit {
    Hit {
        row: None,
        preview: None,
        faces: Vec::new(),
        outcome: None,
        kind: ResourceKind::Action,
        provider: "test".into(),
        id: format!("{resource}#{action}"),
        title: action.into(),
        subtitle: None,
        facts: Vec::new(),
        score: 0,
        icon: None,
        target: Target::Invoke(Box::new(InvokeTarget::action("test", "", resource, action))),
    }
}

#[test]
fn the_text_places_the_top_result_of_the_hits_a_place_can_show() {
    let hits = vec![
        opening(
            "settings.section:agent_models",
            "Chat models",
            ResourceKind::Record,
        ),
        opening("tl:so.tilo.chat", "Chat", ResourceKind::Application),
    ];
    let showable = |resource: &str| resource == "tl:so.tilo.chat";
    assert_eq!(
        found_resource(&hits, "Chat", showable).as_deref(),
        Some("tl:so.tilo.chat"),
        "a hit the layout cannot place does not stand in the way of one it can"
    );
    assert_eq!(
        found_resource(&hits, "Chat", |_| false),
        None,
        "and with nothing placeable, nothing is placed"
    );
}

#[test]
fn the_text_places_the_hit_whose_resource_it_names() {
    let named = [
        (0, "notes", "Groceries"),
        (1, "notes/i1", "Groceries"),
        (2, "notes/i2", "Groceries"),
    ];
    assert_eq!(
        most_specific(&named, "Groceries"),
        Some(1),
        "a note titled like its notebook is the note, and the top of them"
    );
    assert_eq!(
        most_specific(&named, "my notes"),
        Some(0),
        "a resource the text only describes stays itself"
    );
    assert_eq!(
        most_specific(&[(0, "inbox", "Inbox")], "inbox"),
        Some(0),
        "and the top stays when nothing inside it carries the same name"
    );
}

#[test]
fn the_text_places_the_row_that_opens_the_resource_not_one_that_acts_on_it() {
    // "my mails" ranks Mail's actions far above Mail's root, as the search does: the root's own
    // row stands last of the rows about it.
    let hits = vec![
        acting("tl:so.tilo.mail:root", "rename_label"),
        acting("tl:so.tilo.mail:root/account", "set_signature"),
        opening("tl:so.tilo.mail:root", "Mail", ResourceKind::Record),
    ];
    let at = placed(&hits, "my mails", |_| true);
    assert_eq!(
        at,
        Some(2),
        "the row typing the text stands is the row that opens the resource, \
         not another row about it"
    );
    assert_eq!(
        found_resource(&hits, "my mails", |_| true).as_deref(),
        Some("tl:so.tilo.mail:root"),
        "and the canvas stands its resource"
    );
}

/// Mail's root as the session publishes it: its whole window primary, and the inbox's list as what
/// a place of it shows on its own.
fn mail_root() -> Placed {
    Placed {
        app: "so.tilo.mail".into(),
        views: vec!["mail".into(), "all_inboxes".into(), "unified".into()],
        title: "Mail".into(),
        kind: "tl.so.tilo.mail.Mail".into(),
        place_view: Some("all_inboxes".into()),
        place_title: "All inboxes".into(),
        app_title: "Mail".into(),
    }
}

/// A place shown through its resource's place view is called by that view, never by the record
/// the resource's own label names (an app's root goes by its first account or note): "my mails"
/// stands captioned "All inboxes", not the account's address (canvas.md §7, "Captions name the
/// thing").
#[test]
fn a_place_through_its_place_view_is_called_by_the_view() {
    let mail = Placed {
        title: "sam@rivera.example".into(),
        ..mail_root()
    };
    assert_eq!(mail.title_through("all_inboxes"), "All inboxes");
    assert_eq!(
        mail.title_through("mail"),
        "sam@rivera.example",
        "through another view, the resource's own label"
    );
    let unnamed = Placed {
        place_title: String::new(),
        ..mail
    };
    assert_eq!(
        unnamed.title_through("all_inboxes"),
        "sam@rivera.example",
        "a place view with no name of its own leaves the label"
    );
}

/// Words that name the thing place it through its place view; words that name the app alone place
/// its whole window (canvas.md §7, "The thing, never the app").
#[test]
fn the_words_name_the_thing_unless_they_name_the_app_alone() {
    let mail = mail_root();
    for thing in ["my mails", "mails", "my mail", "my inbox", "all my mail"] {
        assert_eq!(
            mail.view_for(thing),
            "all_inboxes",
            "{thing:?} names the mail"
        );
    }
    for app in ["Mail", "mail", "open Mail", "open the mail app", "  Mail "] {
        assert_eq!(
            mail.view_for(app),
            "",
            "{app:?} names the app: its primary view"
        );
    }
    let notes = Placed {
        place_view: Some("all_notes".into()),
        app_title: "Notes".into(),
        ..Placed::default()
    };
    assert_eq!(
        notes.view_for("my notes"),
        "all_notes",
        "the notes, not Notes"
    );
    assert_eq!(notes.view_for("Notes"), "", "the app named alone");
    assert_eq!(
        Placed::default().view_for("my mails"),
        "",
        "a resource whose default view is its own has no other to stand through"
    );
}

/// The canvas, the REPL and an agent's `find` resolve one way: the resource the hits name, through
/// the view the words place it through.
#[test]
fn what_text_places_is_the_resource_and_its_view() {
    let hits = vec![opening(
        "tl:so.tilo.mail:root",
        "Mail",
        ResourceKind::Record,
    )];
    let target = |resource: &str| (resource == "tl:so.tilo.mail:root").then(mail_root);
    assert_eq!(
        found_place(&hits, "my mails", target),
        Some(("tl:so.tilo.mail:root".into(), "all_inboxes".into()))
    );
    assert_eq!(
        found_place(&hits, "open Mail", target),
        Some(("tl:so.tilo.mail:root".into(), String::new()))
    );
    assert_eq!(found_place(&hits, "my mails", |_| None), None);
}

/// What stands at a click takes its own width as far as the room to the edge of what the person
/// sees allows, never so narrow a row cannot read: then it keeps its width and the canvas pans.
#[test]
fn a_thing_at_a_click_is_as_wide_as_the_room_in_view() {
    let seen = Some(super::CanvasRect {
        x: 0,
        y: 0,
        width: 1280,
        height: 760,
    });
    assert_eq!(
        super::width_in_view(100, 720, seen),
        720,
        "room for all of it"
    );
    assert_eq!(
        super::width_in_view(830, 720, seen),
        1280 - 830 - 24,
        "as wide as the room to the edge"
    );
    assert_eq!(
        super::width_in_view(1100, 720, seen),
        720,
        "too little room: its own"
    );
    assert_eq!(
        super::width_in_view(830, 720, None),
        720,
        "nothing seen said"
    );
}

/// Search offers a person the layout's undo once, and none of the actions agents arrange
/// with: three windows' places were three identical "Stand a place at a point on the canvas"
/// rows under "theme".
#[test]
fn search_offers_people_no_arranging_internals() {
    use tilo_desktop_search::Source as _;
    use tilo_desktop_search::providers::{Published, PublishedActions};
    let words: super::Words = std::rc::Rc::new(|_, _, fallback| fallback.to_owned());
    let mut resources = super::resources(&words);
    for key in ["1", "2", "3"] {
        resources.push(
            tilo_resource::LiveRow::typed::<super::published::Place>(
                super::Item::Place.resource(key),
                "Notes",
            )
            .into_resource(),
        );
    }
    let published = Published(vec![PublishedActions::of("session", "", &resources)]);
    let titles = |needle: &str| -> Vec<String> {
        published
            .query(needle)
            .into_iter()
            .map(|hit| hit.title)
            .collect()
    };
    for needle in ["theme", "place", "canvas", "pin", "close a place"] {
        assert_eq!(titles(needle), Vec::<String>::new(), "{needle}");
    }
    assert_eq!(
        titles("undo"),
        ["Undo the last layout change"],
        "once, on the layout"
    );
    assert_eq!(
        titles("redo"),
        ["Redo the last layout change"],
        "once, on the layout"
    );
}

/// The published actions keep what the hand-built specs declared: the verbs that arrange are
/// listed for agents and scripts and never as a person's palette rows, each names what it did in
/// the layout's history, and a drawn height only reads.
#[test]
fn the_layout_actions_keep_their_audience_history_words_and_read_kind() {
    let words: super::Words = std::rc::Rc::new(|_, _, fallback| fallback.to_owned());
    let layout = super::published::layout_resource(&words);
    let action = |id: &str| {
        layout
            .actions
            .iter()
            .find(|spec| spec.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    for id in ["place", "place-found", "open", "show", "retry-query"] {
        let spec = action(id);
        assert!(
            spec.reaches_agents(),
            "{id} is listed for agents and scripts"
        );
        assert!(
            !spec.reaches_people(),
            "{id} is no row of a person's palette"
        );
    }
    assert_eq!(action("place").action_type().did.as_deref(), Some("showed"));

    let place = tilo_resource::LiveRow::typed::<super::published::Place>(
        super::Item::Place.resource("7"),
        "Notes",
    )
    .into_resource();
    // A place's own verbs name what they did as the layout publishes each place.
    let published_place = super::published::place_kind_resource(&words);
    let pin = published_place
        .actions
        .iter()
        .find(|spec| spec.id == "pin")
        .expect("the place offers its pin");
    assert_eq!(pin.action_type().did.as_deref(), Some("pinned"));
    let drawn = place
        .actions
        .iter()
        .find(|spec| spec.id == "drawn")
        .expect("the place offers its drawn height");
    assert_eq!(
        drawn.action_type().contract.kind,
        tilo_actions::ActionKind::Read,
        "a drawn height is view state: it reads and takes nothing back"
    );
    assert!(drawn.reaches_agents());
}

/// A device's desktop id carries its salt in the high 16 bits, so more than
/// half of them outgrow an `i64`. `place` reads the row, strip and place it is
/// given as the ids they are, and the effect the published spec encodes is the
/// one its handler decodes.
#[test]
fn a_place_takes_the_ids_a_device_mints() {
    let words: super::Words = std::rc::Rc::new(|_, _, fallback| fallback.to_owned());
    let layout = super::published::layout_resource(&words);
    let spec = layout
        .actions
        .iter()
        .find(|spec| spec.id == "place")
        .expect("place");
    let salted = 0xB3A1_0000_0000_0001_u64;
    let arguments = serde_json::json!({
        "workspace": "1",
        "row": salted,
        "strip": salted,
        "place": salted,
        "resource": "notes:1",
        "x": 80,
        "y": 60,
        "width": 720,
        "height": 620,
    })
    .as_object()
    .expect("arguments")
    .clone();
    let encoded = spec.build_json(&arguments).expect("the place encodes");
    assert_eq!(
        encoded.type_id,
        <super::published::LayoutEffect as tilo_resource::Effect>::OPERATION,
        "the place is encoded as its own operation, not a refusal: {:?}",
        String::from_utf8_lossy(&encoded.payload)
    );
    let decoded =
        <super::published::LayoutEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
            .expect("the place decodes");
    let super::published::LayoutEffect::Place {
        row, strip, place, ..
    } = decoded
    else {
        panic!("a place, not {decoded:?}");
    };
    assert_eq!(
        (row, strip, place),
        (Some(salted), Some(salted), Some(salted)),
        "the ids it was given, whole"
    );
}

struct NoLayouts;

impl super::Layouts for NoLayouts {
    fn step(&self, _: &str, _: bool) -> super::Stepped {
        super::Stepped::None
    }

    fn last_step(&self, _: &str) -> Option<(tilo_actions::Actor, String)> {
        None
    }

    fn acted(
        &self,
        _: &tilo_desktop_model::Layout,
        _: &tilo_desktop_model::Layout,
        _: &tilo_actions::Actor,
        _: &super::LayoutAction,
        _: &[(tilo_desktop_model::TabId, String)],
    ) {
    }

    fn shown(&self) -> std::collections::BTreeMap<tilo_desktop_model::TabId, String> {
        std::collections::BTreeMap::new()
    }
}

#[test]
fn surface_at_falls_back_when_active_strip_is_missing_or_stale() {
    let work_id = tilo_desktop_model::WorkspaceId::from_u128(1);
    let mut work = tilo_desktop_model::Workspace::new(work_id, "Work");
    work.strips = vec![
        tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(10), "First"),
        tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(11), "Second"),
    ];
    let tree = tilo_desktop_model::Layout {
        active: work_id,
        workspaces: vec![work],
        ..Default::default()
    };

    let handle = crate::desktop::state::DesktopHandle::new(tree, 7);
    let names: super::Names = std::rc::Rc::new(|_, fallback| fallback.to_owned());
    let words: super::Words = std::rc::Rc::new(|_, _, fallback| fallback.to_owned());
    let layout = super::Layout::new(
        "pat",
        handle.clone(),
        std::rc::Rc::new(NoLayouts),
        names,
        words,
    );

    // Missing active_strip record falls back to the workspace's first strip.
    let missing = layout.surface_at().expect("a surviving row is chosen");
    assert_eq!(missing.workspace, work_id);
    assert_eq!(
        missing.row, 10,
        "a missing active_strip falls back to the workspace's first strip"
    );

    // Stale active_strip record falls back to the workspace's first surviving strip.
    let mut stale_tree = handle.read(Clone::clone);
    stale_tree
        .active_strip
        .insert(work_id, tilo_desktop_model::StripId(99));
    handle.replace_tree(stale_tree);
    let placed = layout.surface_at().expect("a surviving row is chosen");
    assert_eq!(placed.workspace, work_id);
    assert_eq!(
        placed.row, 10,
        "a stale active_strip falls back to the workspace's first surviving strip"
    );

    // With a stale record and no strips left at all, surface_at creates a real row instead of
    // handing back the deleted strip's id.
    let mut empty_tree = handle.read(Clone::clone);
    empty_tree.workspaces[0].strips.clear();
    empty_tree
        .active_strip
        .insert(work_id, tilo_desktop_model::StripId(99));
    handle.replace_tree(empty_tree);
    let created = layout.surface_at().expect("an empty workspace gets a row");
    assert_ne!(created.row, 99, "the deleted strip id is never returned");
    assert!(
        handle.read(|tree| {
            tree.workspaces[0]
                .strips
                .iter()
                .any(|strip| strip.id.0 == created.row)
        }),
        "the created row exists in the workspace"
    );
}

/// What the catalogue holds of a resource of `app`'s: what the session publishes to the person.
fn published(app: &str, kind: &str, title: &str) -> Placed {
    Placed {
        app: app.into(),
        views: Vec::new(),
        title: title.into(),
        kind: kind.into(),
        place_view: None,
        place_title: String::new(),
        app_title: "Video".into(),
    }
}

/// Two members read their recents off the one history of the workspace's layout: each sees their
/// own opens first; one clearing hides what came before from their list alone, the history and the
/// other's list keep it; and a resource the session does not publish to the reader never lists.
#[test]
fn recents_are_each_readers_own_over_one_history() {
    use super::{Catalog, RecentOpens, recent_readings};
    use tilo_desktop_model::recents::{Opened, Scope};

    let opened = |resource: &str, by: &str, at: i64| Opened {
        resource: resource.into(),
        by: by.into(),
        at,
    };
    let history = std::rc::Rc::new(vec![
        opened("tube:a", "ada", 10),
        opened("tube:b", "bob", 20),
        opened("tube:secret", "bob", 25),
        opened("tube:c", "ada", 30),
    ]);
    let mut catalog = Catalog::default();
    for (id, title) in [("tube:a", "A"), ("tube:b", "B"), ("tube:c", "C")] {
        catalog.remember(id, published("so.tilo.tube", "tube.video", title));
    }
    let name = |person: &str| person.to_uppercase();
    let listed = |opens: &RecentOpens, person: &str| -> Vec<(String, bool)> {
        recent_readings(opens, person, &catalog, name)
            .into_iter()
            .map(|recent| (recent.resource, recent.mine))
            .collect()
    };
    let nothing_cleared = RecentOpens {
        opened: std::rc::Rc::clone(&history),
        ..RecentOpens::default()
    };
    assert_eq!(
        listed(&nothing_cleared, "ada"),
        [
            ("tube:c".to_owned(), true),
            ("tube:a".to_owned(), true),
            ("tube:b".to_owned(), false),
        ],
        "her own first, newest first, and never what she cannot read"
    );
    assert_eq!(
        listed(&nothing_cleared, "bob"),
        [
            ("tube:b".to_owned(), true),
            ("tube:c".to_owned(), false),
            ("tube:a".to_owned(), false),
        ]
    );
    let by = recent_readings(&nothing_cleared, "bob", &catalog, name);
    assert_eq!(
        by[1].by, "ADA",
        "who opened it is named as the workspace names them"
    );

    // Ada clears YouTube's recents at 25: her list keeps only what she or anyone opened after.
    let ada_cleared = RecentOpens {
        opened: std::rc::Rc::clone(&history),
        hidden: [(Scope::of(Some("so.tilo.tube"), None).key(), 25)].into(),
        revision: 1,
    };
    assert_eq!(listed(&ada_cleared, "ada"), [("tube:c".to_owned(), true)]);
    assert_eq!(history.len(), 4, "the workspace's history keeps every open");
    assert_eq!(
        listed(&nothing_cleared, "bob").len(),
        3,
        "Bob's list is his own: Ada's clear is not in his view"
    );
    // A clear of another app's recents hides none of YouTube's.
    let other = RecentOpens {
        opened: std::rc::Rc::clone(&history),
        hidden: [(Scope::of(Some("so.tilo.notes"), None).key(), 100)].into(),
        revision: 2,
    };
    assert_eq!(listed(&other, "ada").len(), 3);
}

// An agent arranging the person's layout writes a change that names it acting for them and
// carrying the delegation their device gives it: the workspace admits the change by the agent's
// preset, as it admits every other call the agent makes, and an agent no grant names is refused.
#[test]
fn an_agents_arranging_change_carries_the_delegation_its_device_gives_it() {
    const PERSON: &str = "abababababababababababababababababababababababababababababababab";
    const WORKSPACE: &str = "5ba25f73-87c5-447f-a5a6-880dc4249a81";
    let action = super::LayoutAction::Pin {
        place: 7,
        pinned: true,
    };
    let agent: tilo_actions::Actor = tilo_actions::MemberId::agent("reading", PERSON).into();
    let context = super::step_context(&agent, &action, PERSON);
    let invoker = context
        .invoker
        .as_ref()
        .expect("the change names who asked");
    assert_eq!(
        invoker.acting.member_id(),
        Some(&tilo_actions::MemberId::agent("reading", PERSON)),
        "the change is the agent's, never the person's own"
    );
    assert_eq!(
        invoker.acting.acted_for_person(),
        Some(PERSON),
        "it acts for the person whose layout it arranges"
    );
    assert!(
        !invoker.acting.handed_preset(WORKSPACE).is_empty(),
        "and carries the preset its person's device hands it, so the workspace admits it"
    );

    // The person's own step names no one but them, and carries no delegation.
    let person: tilo_actions::Actor = tilo_actions::MemberId::person(PERSON).into();
    let own = super::step_context(&person, &action, PERSON);
    let invoker = own.invoker.as_ref().expect("the change names who asked");
    assert!(invoker.acting.is_person());
    assert!(
        invoker.acting.handed_preset(WORKSPACE).is_empty(),
        "a person's own change delegates nothing"
    );
}
