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
        target: Target::Invoke(Box::new(InvokeTarget::open("test", "", resource))),
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
        opening("tl:so.tilo.lang.chat", "Chat", ResourceKind::Application),
    ];
    let showable = |resource: &str| resource == "tl:so.tilo.lang.chat";
    assert_eq!(
        found_resource(&hits, "Chat", showable).as_deref(),
        Some("tl:so.tilo.lang.chat"),
        "a hit the arrangement cannot place does not stand in the way of one it can"
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
        acting("tl:so.tilo.lang.mail:root", "rename_label"),
        acting("tl:so.tilo.lang.mail:root/account", "set_signature"),
        opening("tl:so.tilo.lang.mail:root", "Mail", ResourceKind::Record),
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
        Some("tl:so.tilo.lang.mail:root"),
        "and the canvas stands its resource"
    );
}

/// Mail's root as the session publishes it: its whole window primary, and the inbox's list as what
/// a place of it shows standing on its own.
fn mail_root() -> Placed {
    Placed {
        app: "so.tilo.lang.mail".into(),
        views: vec!["mail".into(), "all_inboxes".into(), "unified".into()],
        title: "Mail".into(),
        kind: "tl.so.tilo.lang.mail.Mail".into(),
        place_view: Some("all_inboxes".into()),
        app_title: "Mail".into(),
    }
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
        "tl:so.tilo.lang.mail:root",
        "Mail",
        ResourceKind::Record,
    )];
    let target = |resource: &str| (resource == "tl:so.tilo.lang.mail:root").then(mail_root);
    assert_eq!(
        found_place(&hits, "my mails", target),
        Some(("tl:so.tilo.lang.mail:root".into(), "all_inboxes".into()))
    );
    assert_eq!(
        found_place(&hits, "open Mail", target),
        Some(("tl:so.tilo.lang.mail:root".into(), String::new()))
    );
    assert_eq!(found_place(&hits, "my mails", |_| None), None);
}
