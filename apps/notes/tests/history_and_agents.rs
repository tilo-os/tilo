//! An agent works on the notebook one call at a time — each change a verb on
//! the note it acts on or a field edit at a path — and what it did reads back
//! as the app's own sentences, naming the notes as they were called.

use notes::{NotesShell, PageId};
use tilo_document::RichTextAction;
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionInvoker;

fn note_titled(app: &TestApp<NotesShell>, title: &str) -> PageId {
    crate::framework_support::shown(app)
        .pages()
        .iter()
        .find(|page| page.title == title)
        .unwrap_or_else(|| panic!("a note titled {title}"))
        .id
}

#[test]
fn an_agent_changes_notes_in_one_call_each_and_history_tells_what_it_did() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let agent = ActionInvoker::agent("claude").acting_for("ada");
    let pages = notes::note_list::chain_resource();
    let kind = pages.as_str();
    let call = |app: &mut TestApp<NotesShell>, resource: &str, id: &str, args| {
        app.call_by(agent.clone(), resource, id, args)
            .unwrap_or_else(|error| panic!("{id}: {error:?}"));
    };

    call(
        &mut app,
        kind,
        "new-note",
        serde_json::json!({ "title": "Plan" }),
    );
    let plan = note_titled(&app, "Plan");
    let item = notes::actions::page_resource_id(plan);
    let body = notes::NoteList::paths()
        .pages
        .item(&notes::notebook::page_id(plan))
        .body
        .perform(&RichTextAction::AppendParagraph {
            text: "Ship it".into(),
        })
        .unwrap();
    call(
        &mut app,
        kind,
        "field-edit",
        serde_json::json!({ "edit": body }),
    );
    call(
        &mut app,
        &item,
        "new-child",
        serde_json::json!({ "title": "Step one" }),
    );
    // The person made the tag the agent tags with.
    app.call("new-tag", serde_json::json!({ "name": "Release" }))
        .unwrap();
    call(
        &mut app,
        &item,
        "tag",
        serde_json::json!({ "name": "Release" }),
    );
    call(
        &mut app,
        &item,
        "rename",
        serde_json::json!({ "title": "Roadmap" }),
    );
    call(&mut app, &item, "duplicate", serde_json::json!({}));
    let copy = note_titled(&app, "Roadmap copy");
    // Undo takes a deleted note back, so an agent's delete runs at once
    // like a person's.
    app.call_by(
        agent.clone(),
        &notes::actions::page_resource_id(copy),
        "delete",
        serde_json::json!({}),
    )
    .expect("an undoable delete asks nothing");

    let shown = crate::framework_support::shown(&app);
    assert_eq!(shown.children_of(Some(plan)).len(), 1);
    assert_eq!(shown.tags_for_page(plan)[0].name, "Release");
    assert_eq!(
        crate::framework_support::part_doc(&app, plan, |doc| notes::read::page_block_snapshots(
            doc, plan
        ))
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>(),
        ["Ship it"]
    );

    let entries: Vec<_> = crate::framework_support::history(&app)
        .into_iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.acting.agent_id() == Some("claude"))
        })
        .collect();
    let sentences = app.sentences(&entries);
    assert_eq!(
        sentences,
        [
            // A note's making names it as it is called now; the steps after it as it was then.
            "Created “Roadmap”",
            "Edited “Plan”",
            "Created Step one in Plan",
            "Tagged Plan with Release",
            "Renamed Plan to Roadmap",
            "Duplicated Roadmap",
            "Deleted Roadmap copy",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.acting.is_agent(), invoker.acting.acted_for_person()),
        (true, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}

/// A note another app's effect asks for someone (Tasks writing an issue's
/// note) is theirs: `create-page` answers the note's resource, which the
/// asking app links, makes one step in the notebook's history by them, and
/// their undo takes it back while the person's own notes stay.
#[test]
fn a_note_made_for_someone_through_another_app_is_one_step_of_theirs() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call_on(
        &notes::note_list::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Groceries" }),
    )
    .unwrap();
    let pages = notes::note_list::chain_resource();
    let kind = pages.as_str();
    let before = crate::framework_support::versions(&app).len();
    let agent = ActionInvoker::agent("scribe").acting_for("ada");

    let made = app
        .call_by(
            agent.clone(),
            kind,
            "create-page",
            serde_json::json!({ "title": "#1 Plan the launch" }),
        )
        .unwrap();
    let note = note_titled(&app, "#1 Plan the launch");
    let resource = notes::actions::page_resource_id(note);
    assert_eq!(made.resources.first(), Some(&resource), "{made:?}");
    assert_eq!(
        ui::tilo_resource::items::item_kind(&resource).as_deref(),
        Some("notes.pages.pages")
    );

    let versions = crate::framework_support::versions(&app);
    assert_eq!(versions.len(), before + 1, "one step: {versions:?}");
    let by = versions
        .last()
        .and_then(|version| version.context.as_ref())
        .and_then(|context| context.invoker.as_ref())
        .expect("the step names who made it");
    assert_eq!(
        by.acting.member_id(),
        Some(&ui::tilo_resource::MemberId::agent("scribe"))
    );
    assert_eq!(by.acting.acted_for_person(), Some("ada"));

    // Their undo takes back their step in the part the note is kept in.
    app.call_by(agent, kind, "undo", serde_json::json!({ "part": 0 }))
        .unwrap();
    let titles: Vec<String> = crate::framework_support::shown(&app)
        .pages()
        .iter()
        .map(|page| page.title.clone())
        .collect();
    assert_eq!(titles, ["Groceries"]);
}

/// History shows a note as an older typing step left it, drawn by the window's own view of the
/// note, and restores that version as a new change.
#[test]
fn history_shows_an_older_typing_step_of_a_note_and_restores_it() {
    let (mut app, page) = crate::framework_support::with_note("Plan", "");
    type_into_note(&mut app, "First draft");
    let first = part_heads(&app, page);
    crate::framework_support::apply(
        &mut app,
        &notes::actions::rename_page_action(page, "Roadmap"),
    );
    type_into_note(&mut app, " and a second thought");
    let typed = crate::framework_support::body(&app, page).join("\n");
    assert!(typed.contains("second thought"), "{typed}");

    select_version(&mut app, &first);
    let shown = preview_text(&mut app);
    assert!(shown.contains("First draft"), "{shown}");
    assert!(
        !shown.contains("second thought"),
        "the preview is the older step, not the note now: {shown}"
    );

    let changes = part_changes(&app, page);
    press_named(&mut app, "Restore…");
    press_named(&mut app, "Restore version");
    assert_eq!(
        crate::framework_support::body(&app, page).join("\n"),
        "First draft"
    );
    assert_eq!(
        crate::framework_support::shown(&app)
            .pages()
            .iter()
            .find(|shown| shown.id == page)
            .map(|shown| shown.title.clone()),
        Some("Plan".to_owned())
    );
    assert_eq!(
        part_changes(&app, page),
        changes + 1,
        "the restore is one new change"
    );
}

/// A press on History's Restore… opens its review of a change of the notebook's own document, as it
/// does of a part's.
#[test]
fn a_click_on_restore_opens_the_review_of_a_notebook_change() {
    let (mut app, _) = crate::framework_support::with_note("Plan", "");
    app.call("new-tag", serde_json::json!({ "name": "Release" }))
        .expect("the notebook takes the tag");
    let own = app
        .versions()
        .last()
        .expect("the tag is one change of the notebook")
        .hash;
    select_version(&mut app, &[own]);
    press_named(&mut app, "Restore…");
    assert!(
        app.harness().a11y_node_named("Restore version").is_some(),
        "the press on Restore… opens the review"
    );
}

/// Press the control History draws with `name`: a move onto it, then a press and a release, as a
/// person's pointer delivers them.
fn press_named(app: &mut TestApp<NotesShell>, name: &str) {
    let bounds = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.label() == Some(name))
                .then(|| node.bounds())
                .flatten()
        })
        .unwrap_or_else(|| panic!("a control named {name} is drawn"));
    let (x, y) = (
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    app.harness().click_at(x, y);
    app.harness().relayout();
}

fn type_into_note(app: &mut TestApp<NotesShell>, text: &str) {
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .unwrap();
    app.harness().click_at(rect.x1 - 4.0, rect.y0 + 10.0);
    app.harness().type_text(text);
    app.flush().unwrap();
}

fn part_heads(app: &TestApp<NotesShell>, page: PageId) -> Vec<automerge::ChangeHash> {
    crate::framework_support::part_doc(app, page, |doc| doc.clone().get_heads())
}

fn part_changes(app: &TestApp<NotesShell>, page: PageId) -> usize {
    crate::framework_support::part_doc(app, page, |doc| doc.clone().get_changes(&[]).len())
}

fn timeline(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("activity_timeline") {
        return Some(node);
    }
    node.children.iter().find_map(timeline)
}

fn targets(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.action_target.clone());
    for child in &node.children {
        targets(child, out);
    }
}

/// Open History and choose the row of the change that left the part at `version`.
fn select_version(app: &mut TestApp<NotesShell>, version: &[automerge::ChangeHash]) {
    let [hash] = version else {
        panic!("a single-author edit leaves one head");
    };
    app.harness().open_history().unwrap();
    let entry = format!("timeline-entry:{}", &hash.to_string()[..32]);
    for _ in 0..4 {
        let inspection = app.harness().inspection();
        let mut published = Vec::new();
        if let Some(semantics) = timeline(&inspection.root).and_then(|node| node.semantics.as_ref())
        {
            targets(semantics, &mut published);
        }
        if published.contains(&entry) {
            break;
        }
        for group in published
            .iter()
            .filter(|target| target.starts_with("timeline-group:"))
        {
            let _ = app.harness().perform_semantic_named(
                "activity_timeline",
                Some(group),
                &ui::a11y::Action::Expand,
            );
        }
        app.harness().relayout();
    }
    // A click on the row away from its link chooses it, as a person picks a version; activating
    // the row opens the note its change acted on instead.
    let inspection = app.harness().inspection();
    let row = timeline(&inspection.root)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|semantics| access_node(semantics, &entry))
        .and_then(|node| node.bounds)
        .expect("the row is drawn")
        .to_rect();
    app.harness().click_at(row.x1 - 6.0, row.center().y);
    app.harness().relayout();
}

fn access_node<'a>(
    node: &'a ui::a11y::AccessNode,
    target: &str,
) -> Option<&'a ui::a11y::AccessNode> {
    if node.action_target.as_deref() == Some(target) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| access_node(child, target))
}

fn preview_text(app: &mut TestApp<NotesShell>) -> String {
    fn find(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("history-preview") {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    let inspection = app.harness().inspection();
    find(&inspection.root)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|semantics| semantics.name.clone())
        .expect("History shows a preview of the selected version")
}
