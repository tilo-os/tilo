//! A note's reactions: someone on another replica reacts, both replicas
//! converge, and the note's header shows the reaction under the tags; a
//! press on its pill is the person's own reaction, as the person.

use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::permissions::MemberId;
use ui::tilo_resource::{Invoke, Replica};

const ME: &str = "test-author";

fn reactions_of(app: &TestApp<NotesShell>, page: PageId) -> Vec<(String, Vec<String>)> {
    let parts = crate::framework_support::parts(app);
    notes::Pages::of(&parts)
        .get(&notes::notebook::page_id(page))
        .expect("the note")
        .reactions
        .groups()
        .into_iter()
        .map(|group| {
            let by = group
                .reactors
                .iter()
                .map(|r| r.by.id().to_owned())
                .collect();
            (group.emoji, by)
        })
        .collect()
}

#[test]
fn a_reaction_on_another_replica_converges_and_shows_in_the_header() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call_on(
        &notes::note_list::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Plan" }),
    )
    .unwrap();
    app.settle();
    let page = crate::framework_support::shown(&app).pages()[0].id;
    app.harness()
        .presence_mut()
        .set_display_name(MemberId::person("bob"), "Bob");

    // Bob reacts on his replica.
    let (part, peer) = crate::framework_support::peer_part(&app, page);
    let mut bob = Replica::<notes::NoteList>::from_doc(peer).as_author("bob");
    let field = notes::NoteList::paths()
        .pages
        .item(&notes::notebook::page_id(page))
        .reactions;
    bob.apply(Invoke::edit(&field.react("🎉")).unwrap())
        .unwrap();
    crate::framework_support::merge_part(&mut app, &part, bob.doc_mut());
    app.settle();

    let ours = reactions_of(&app, page);
    assert_eq!(ours, vec![("🎉".to_owned(), vec!["bob".to_owned()])]);
    let theirs = bob
        .state()
        .unwrap()
        .pages
        .get(&notes::notebook::page_id(page))
        .unwrap()
        .reactions
        .clone();
    assert_eq!(theirs.groups().len(), 1, "both replicas hold it");

    let panel = app.surface_mut::<notes::NotesApp>().main_panel_mut();
    let shown = panel
        .reactions_for_test()
        .expect("the header shows the reactions");
    assert_eq!(shown.groups.len(), 1);
    assert_eq!(shown.groups[0].emoji, "🎉");
    assert_eq!(
        shown.groups[0].reactors[0].name.as_deref(),
        Some("Bob"),
        "the pill names who reacted"
    );
    assert!(shown.can_react, "the note is shared: the person may react");
    let strip = panel
        .header_rects_for_test()
        .into_iter()
        .find_map(|(id, rect)| (id == "notes.reactions").then_some(rect))
        .expect("the strip stands in the header");
    assert!(strip.width() > 0.0 && strip.height() > 0.0, "{strip:?}");

    // A press on the pill is the person's own reaction with it.
    app.harness().click_at(strip.x0 + 12.0, strip.center().y);
    app.settle();
    assert_eq!(
        reactions_of(&app, page),
        vec![("🎉".to_owned(), vec!["bob".to_owned(), ME.to_owned()])],
        "the press reacts as the person: {:?}",
        app.refused()
    );
}
