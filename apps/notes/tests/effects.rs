//! Notes' effects as a person, the palette and an agent reach them: files
//! through the host's chooser, the clipboard, links and moving between notes.

use crate::framework_support::{add_note, body, notes, open, surface, with_note};
use notes::NotesEffect;
use notes::actions::page_resource_id;
use ui::portal::PortalOutcome;

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png"));

fn shown(app: &mut ui::framework::testing::TestApp<notes::NotesShell>) -> Option<notes::PageId> {
    surface(app, |notes| notes.selected_for_test())
}

#[test]
fn a_chosen_markdown_file_becomes_a_shown_note() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Project brief.md");
    std::fs::write(&file, "# Scope\n\n- Fast\n").unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = notes();

    recorder.respond_to_pick(PortalOutcome::Opened(file));
    app.effect(&NotesEffect::ImportMarkdown).unwrap();
    app.settle();

    assert_eq!(recorder.pick_requests()[0].title(), "Import Markdown");
    let page = app
        .state()
        .pages()
        .iter()
        .find(|page| page.title == "Project brief")
        .map(|page| page.id)
        .expect("the file became a note named after it");
    assert_eq!(body(&app, page), ["Scope", "Fast"]);
    assert_eq!(shown(&mut app), Some(page), "and the note is shown");
    assert!(app.undo(), "the import is one undo step");
    assert!(app.state().pages().is_empty());
}

#[test]
fn a_cancelled_or_unreadable_choice_adds_no_note() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("binary.md");
    std::fs::write(&binary, [0xff, 0xfe]).unwrap();
    let oversized = dir.path().join("oversized.md");
    std::fs::write(
        &oversized,
        "a".repeat(notes::actions::MAX_MARKDOWN_IMPORT_BYTES + 1),
    )
    .unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = notes();

    recorder.respond_to_pick(PortalOutcome::Cancelled);
    app.effect(&NotesEffect::ImportMarkdown)
        .expect("a cancelled choice is no failure");
    for file in [binary, oversized] {
        recorder.respond_to_pick(PortalOutcome::Opened(file.clone()));
        assert!(
            app.effect(&NotesEffect::ImportMarkdown).is_err(),
            "{} is refused",
            file.display()
        );
    }
    app.settle();
    assert!(app.state().pages().is_empty());
}

#[test]
fn an_inserted_image_leaves_and_returns_inside_its_exported_note() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("photo.png");
    std::fs::write(&image, IMAGE).unwrap();
    let exported = dir.path().join("guide.md");
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = with_note("Guide", "Intro");
    let asset = format!("asset:{}", blake3::hash(IMAGE).to_hex());

    recorder.respond_to_pick(PortalOutcome::Opened(image));
    app.effect_on(&page_resource_id(page), &NotesEffect::InsertImage { page })
        .unwrap();
    app.settle();
    assert_eq!(body(&app, page), ["Intro", &format!("![photo]({asset})")]);

    recorder.respond_to_pick(PortalOutcome::Opened(exported.clone()));
    app.effect_on(
        &page_resource_id(page),
        &NotesEffect::ExportMarkdown { page },
    )
    .unwrap();
    let markdown = std::fs::read_to_string(&exported).expect("the chosen file was written");
    assert!(markdown.starts_with("Intro\n"), "{markdown}");
    assert!(
        markdown.contains("![photo](data:image/png;base64,"),
        "the image travels inside the file: {markdown}"
    );

    recorder.respond_to_pick(PortalOutcome::Opened(exported));
    app.effect(&NotesEffect::ImportMarkdown).unwrap();
    app.settle();
    let again = app
        .state()
        .pages()
        .iter()
        .find(|note| note.title == "guide")
        .map(|note| note.id)
        .expect("the exported file imports as a note");
    assert_eq!(
        body(&app, again),
        ["Intro", &format!("![photo]({asset})")],
        "its image is the notebook's own again"
    );
    assert_eq!(
        recorder
            .pick_requests()
            .iter()
            .map(|request| request.title().to_owned())
            .collect::<Vec<_>>(),
        ["Insert image", "Export Markdown", "Import Markdown"]
    );
}

#[test]
fn a_note_exported_as_content_is_markdown_read_back_by_address() {
    use ui::tilo_resource::Effect as _;
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = with_note("Guide", "Intro to the guide");
    let spec = NotesEffect::ExportMarkdownContent { page }.bound();
    assert_eq!(
        spec.contract.result,
        tilo_actions::ResultSpec::Resource {
            resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
        }
    );
    assert!(
        !spec
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
        "keeping a note as content needs no device of its own"
    );

    let output = app
        .effect_on(
            &page_resource_id(page),
            &NotesEffect::ExportMarkdownContent { page },
        )
        .unwrap();
    assert!(recorder.pick_requests().is_empty(), "no chooser is asked");
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = address
        .strip_prefix("tilo.content:")
        .expect("a content address");
    let bytes = app.content().read(hash).expect("the bytes are kept");
    let markdown = String::from_utf8(bytes).unwrap();
    assert_eq!(markdown, "# Guide\n\nIntro to the guide\n");

    app.effect(&NotesEffect::ImportMarkdownContent {
        title: "Guide".into(),
        body: markdown,
    })
    .unwrap();
    app.settle();
    let again = app
        .state()
        .pages()
        .iter()
        .rfind(|note| note.title == "Guide")
        .map(|note| note.id)
        .unwrap();
    assert_ne!(again, page);
    assert_eq!(
        body(&app, again),
        ["Intro to the guide"],
        "the title is not repeated"
    );
}

#[test]
fn a_chosen_file_that_is_no_image_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.png");
    std::fs::write(&text, "not an image").unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = with_note("Guide", "Intro");

    recorder.respond_to_pick(PortalOutcome::Opened(text));
    assert!(
        app.effect_on(&page_resource_id(page), &NotesEffect::InsertImage { page })
            .is_err()
    );
    assert_eq!(body(&app, page), ["Intro"]);
}

#[test]
fn copying_a_note_link_copies_its_current_title() {
    let (services, _recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, page) = with_note("Welcome", "Hello");

    app.effect_on(&page_resource_id(page), &NotesEffect::CopyLink { page })
        .unwrap();
    assert_eq!(ui::clipboard_paste().as_deref(), Some("[[Welcome]]"));

    let gone = notes::PageId::new_v4();
    assert!(
        app.effect_on(
            &page_resource_id(gone),
            &NotesEffect::CopyLink { page: gone }
        )
        .is_err(),
        "a note that does not exist copies nothing"
    );
    assert_eq!(ui::clipboard_paste().as_deref(), Some("[[Welcome]]"));
}

#[test]
fn back_and_forward_move_between_the_notes_shown() {
    let mut app = notes();
    let first = add_note(&mut app, "First", "");
    let second = add_note(&mut app, "Second", "");
    let third = add_note(&mut app, "Third", "");
    for page in [first, second] {
        open(&mut app, page);
    }
    assert!(
        app.effect(&NotesEffect::Forward).is_err(),
        "nothing lies ahead yet"
    );

    app.effect(&NotesEffect::Back).unwrap();
    app.settle();
    assert_eq!(shown(&mut app), Some(first));
    app.effect(&NotesEffect::Forward).unwrap();
    app.settle();
    assert_eq!(shown(&mut app), Some(second));

    app.effect(&NotesEffect::Back).unwrap();
    app.settle();
    open(&mut app, third);
    assert!(
        app.effect(&NotesEffect::Forward).is_err(),
        "opening another note drops the way forward"
    );
}

#[test]
fn only_safe_links_are_opened() {
    let mut app = notes();
    assert!(
        app.effect(&NotesEffect::OpenLink {
            url: "javascript:alert(1)".into(),
        })
        .is_err()
    );
}

#[test]
fn markdown_text_an_agent_hands_over_becomes_a_shown_note() {
    let mut app = notes();
    app.effect(&NotesEffect::ImportMarkdownContent {
        title: "Plan".into(),
        body: "# Plan\n\nShip it\n".into(),
    })
    .unwrap();
    app.settle();
    let page = app
        .state()
        .pages()
        .iter()
        .find(|page| page.title == "Plan")
        .map(|page| page.id)
        .expect("the text became a note");
    assert_eq!(body(&app, page), ["Ship it"], "its title is not repeated");
    assert_eq!(shown(&mut app), Some(page));
}

/// A new note titled `title` made through the notebook's `new-page` action,
/// with `text` as an agent hands it over.
fn new_page(
    app: &mut ui::framework::testing::TestApp<notes::NotesShell>,
    title: &str,
    text: &str,
) -> notes::PageId {
    app.call(
        "new-page",
        serde_json::json!({ "value": title, "text": text }),
    )
    .unwrap();
    app.settle();
    app.state()
        .pages()
        .iter()
        .find(|page| page.title == title)
        .map(|page| page.id)
        .expect("new-page made the note")
}

/// Note `page` as the Markdown an agent reads back.
fn exported(
    app: &mut ui::framework::testing::TestApp<notes::NotesShell>,
    page: notes::PageId,
) -> String {
    let output = app
        .effect_on(
            &page_resource_id(page),
            &NotesEffect::ExportMarkdownContent { page },
        )
        .unwrap();
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = address
        .strip_prefix("tilo.content:")
        .expect("a content address");
    String::from_utf8(app.content().read(hash).expect("the bytes are kept")).unwrap()
}

#[test]
fn a_markdown_table_given_to_a_new_note_exports_as_one_table() {
    let mut app = notes();
    let page = new_page(
        &mut app,
        "Pancakes",
        "| Ingredient | Amount |\n| --- | --- |\n| Flour | 300 g |\n| Milk | 450 ml |\n",
    );
    assert_eq!(
        exported(&mut app, page),
        "# Pancakes\n\n| Ingredient | Amount |\n| --- | --- |\n| Flour | 300 g |\n| Milk | 450 ml |\n"
    );
}

#[test]
fn headings_lists_and_checklists_given_to_a_new_note_keep_their_structure() {
    let mut app = notes();
    let page = new_page(
        &mut app,
        "Plan",
        "## Method\n- [ ] Preheat the pan\n- [x] Whisk the eggs\n1. Pour\n2. Flip\n- Serve **warm**\n  - with syrup\n### Notes\nKeep it *light*.\n",
    );
    assert_eq!(
        exported(&mut app, page),
        "# Plan\n\n## Method\n\n- [ ] Preheat the pan\n\n- [x] Whisk the eggs\n\n1. Pour\n\n2. Flip\n\n\
         - Serve **warm**\n\n  - with syrup\n\n### Notes\n\nKeep it *light*.\n"
    );
}

#[test]
fn plain_text_given_to_a_new_note_is_a_paragraph_per_line() {
    let mut app = notes();
    let page = new_page(&mut app, "Groceries", "oat milk\n  bread  \n\n\tapples\r\n");
    assert_eq!(body(&app, page), ["oat milk", "bread", "apples"]);
    assert_eq!(
        exported(&mut app, page),
        "# Groceries\n\noat milk\n\nbread\n\napples\n"
    );
}
