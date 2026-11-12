//! Collaboration through the notebook record: a collaborator's edit reaches
//! the open note, a duplicate carries its bodies with it, and the window
//! tells others which note it shows.

use crate::framework_support::{body, surface, with_note};
use notes::read;
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;

#[test]
fn presence_reports_the_open_note_as_the_tab() {
    let (mut app, page) = with_note("Shared", "Hello");
    let presence = surface(&mut app, |notes| {
        ui::presence::capability::presence_payload(notes)
    });
    match presence {
        Some((PresenceKey::Tab { id }, PresencePayload::Arrived)) => {
            assert_eq!(id, page.to_string());
        }
        other => panic!("expected Tab location presence, got {other:?}"),
    }
}

#[test]
fn peer_edits_and_duplicates_replicate_before_the_other_peer_opens_them() {
    eprintln!("phase a_collaborators_text_edit_reaches_the_open_note");
    {
        let (mut app, page) = with_note("Shared", "Hello");
        app.merge_peer_edit(|doc| {
            let first = read::page_block_snapshots(doc, page)[0].0.clone();
            crate::component_support::edit_page_body(
                doc,
                page,
                &tilo_document::RichTextAction::SpliceText {
                    region: tilo_document::action::RegionKind::Body,
                    block: first,
                    at: 0,
                    del: 0,
                    ins: "ZZZ-".into(),
                },
            )
            .unwrap();
        });
        let shown = surface(&mut app, |notes| notes.editor_text_for_test());
        assert!(
            shown.contains("ZZZ-Hello"),
            "the open note shows the collaborator's text, got: {shown:?}"
        );
    }

    eprintln!("phase duplicate_carries_every_new_page_body");
    {
        let (mut app, source) = with_note("Welcome", "First\n\nSecond");
        let source_blocks = app.read_document(|doc| read::page_block_snapshots(doc, source));
        // The sidebar's context-menu path: the copy and its bodies are one
        // action, so nobody has to open the copy for its body to exist.
        surface(&mut app, |notes| {
            notes.duplicate_page_for_test(source);
        });
        let copy = crate::framework_support::shown(&app)
            .pages()
            .iter()
            .find(|page| page.title == "Welcome copy")
            .expect("duplicate metadata")
            .id;
        let copied_blocks = app.read_document(|doc| read::page_block_snapshots(doc, copy));
        assert_eq!(
            body(&app, copy),
            body(&app, source),
            "copy body came with its metadata"
        );
        assert_ne!(
            copied_blocks[0].0, source_blocks[0].0,
            "copies mint independent stable block ids"
        );
        let replica = app.peer_document();
        assert_eq!(read::page_block_snapshots(&replica, copy), copied_blocks);
    }
}

/// An agent writing in a note a window here shows, the way a person
/// following it sees it: the window is open on the empty notebook, the agent
/// makes the note and appends its paragraphs one change at a time, and the
/// window follows into the note between them. Each paragraph the agent was
/// told it wrote reads in both replicas, in order.
#[test]
fn an_agents_paragraphs_land_in_a_note_a_following_window_shows() {
    use tilo_document::RichTextAction;
    let mut app = crate::framework_support::notes();
    let mut agent = app.peer_document();
    let page = notes::PageId::new_v4();
    crate::component_support::apply_to(
        &mut agent,
        &notes::actions::create_page_action(page, None, "Launch notes"),
    )
    .unwrap();
    agent.commit();
    let lines = [
        "Launch notes, from Claude Code.",
        "Each line lands in the workspace as I write it.",
        "Following me puts this window in front of you.",
        "A second invocation, a moment later.",
    ];
    for (index, line) in lines.iter().enumerate() {
        crate::component_support::edit_page_body(
            &mut agent,
            page,
            &RichTextAction::AppendParagraph {
                text: (*line).to_owned(),
            },
        )
        .unwrap();
        agent.commit();
        app.merge_peer(&mut agent).unwrap();
        if index == 1 {
            // Following brings the note forward in the window.
            crate::framework_support::open(&mut app, page);
        }
    }
    app.merge_peer(&mut agent).unwrap();
    let texts = |doc: &automerge::AutoCommit| -> Vec<String> {
        read::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .filter(|text| !text.is_empty())
            .collect()
    };
    // The window wrote nothing back over them: every change the record
    // holds past its genesis is the agent's.
    let genesis = Some(tilo_actions::FrameworkChange::Genesis.change_message());
    let agent_actor = agent.get_actor().clone();
    assert!(
        agent
            .get_changes(&[])
            .iter()
            .all(|change| change.actor_id() == &agent_actor
                || change.message() == genesis.as_deref()),
        "following the agent made the window write"
    );
    assert_eq!(texts(&agent), lines, "the agent's replica");
    assert_eq!(
        app.read_document(|doc| texts(doc)),
        lines,
        "the window's replica"
    );
}
