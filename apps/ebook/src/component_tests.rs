use std::{cell::RefCell, rc::Rc};

use ui::application::{
    ApplicationDefinition, ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget,
    RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::bound_dispatch::BoundActionAddress;
use ui::headless::FrameHarness;
use ui::permissions::{Actor, PermissionContext, Role, UserId};
use ui::portable_storage::PortableStorage;
use ui::record::{RecordCheckout, RecordStore};

use crate::doc::BooksDoc;
use crate::model::AnnoKind;

const DEVICE: &str = "books-device";
const WORKSPACE: &str = "books-room";

struct Fixture {
    records: RecordStore,
    views: Rc<InstanceViewStore>,
    authority: Rc<RefCell<PermissionContext>>,
}

impl Fixture {
    fn new(profile: &str) -> Self {
        let cache = PortableStorage::new(profile, DEVICE).unwrap();
        Self {
            records: ui::record::RecordOwners::portable(cache.clone())
                .open(crate::definition().app_id, WORKSPACE)
                .unwrap(),
            views: Rc::new(InstanceViewStore::portable(cache, WORKSPACE).unwrap()),
            authority: Rc::new(RefCell::new(PermissionContext::from_capabilities(
                Actor::person(Some(Role::Owner), "reader"),
                vec![],
            ))),
        }
    }

    fn driver(
        &self,
        definition: &'static ApplicationDefinition,
        instance: u64,
        target: LaunchTarget,
    ) -> (FrameHarness, RecordCheckout) {
        self.driver_with(definition, instance, target, &bundled_book(), |_| {})
    }

    fn component(
        &self,
        definition: &'static ApplicationDefinition,
        instance: u64,
        target: LaunchTarget,
        host: impl FnOnce(&mut HostContext),
    ) -> Result<ui::application::AppComponent, ui::launch::LaunchError> {
        let mut context = HostContext::prepare(
            definition,
            HostContextInputs {
                records: self.records.clone(),
                views: self.views.clone(),
                device_id: DEVICE.into(),
                instance_id: instance,
                permissions: self.authority.clone(),
                author: UserId("reader".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )?;
        host(&mut context);
        definition.instantiate(context, target, RestoreInput::None)
    }

    /// A mount whose host lends `host` its inputs, settled until `book` resolves.
    fn driver_with(
        &self,
        definition: &'static ApplicationDefinition,
        instance: u64,
        target: LaunchTarget,
        book: &str,
        host: impl FnOnce(&mut HostContext),
    ) -> (FrameHarness, RecordCheckout) {
        let Ok(component) = self.component(definition, instance, target, host) else {
            panic!("Books component");
        };
        let checkout = component.record().clone();
        let Ok(mut driver) = FrameHarness::from_component(
            component,
            (1100, 760),
            ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::new(),
            },
        ) else {
            panic!("frame harness");
        };
        settle(&mut driver, book);
        (driver, checkout)
    }
}

fn bundled_book() -> String {
    super::book_resource(&crate::content::bundled_book_id())
}

/// Frames until the book's search index (built off the UI thread natively)
/// resolves the book's resource, if the library holds it.
fn settle(driver: &mut FrameHarness, book: &str) {
    for _ in 0..100 {
        driver.relayout();
        if driver
            .bound_resources(Some(book))
            .ok()
            .is_some_and(|resources| !resources.is_empty())
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn invoke(
    driver: &mut FrameHarness,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) -> Result<(), tilo_actions::DoorError> {
    let address = BoundActionAddress {
        instance_id: driver.component_instance_id().expect("component"),
        address: tilo_actions::ActionAddress::new(resource, action),
    };
    let serde_json::Value::Object(arguments) = arguments else {
        return Err(tilo_actions::DoorError::InvalidArguments(
            "arguments are an object".into(),
        ));
    };
    driver.invoke_bound_action(&address, &arguments)?;
    driver.relayout();
    Ok(())
}

fn child_labels(driver: &FrameHarness, book: &str) -> Vec<(String, String)> {
    let resource = driver.bound_resources(Some(book)).unwrap();
    resource[0]
        .children
        .iter()
        .map(|r| (r.kind.clone(), r.label.clone()))
        .collect()
}

#[test]
fn a_native_new_library_starts_empty_on_the_shelf() {
    let fixture = Fixture::new("books-native-new");
    let (driver, checkout) = fixture.driver(crate::definition(), 1, LaunchTarget::New);
    let shelf = fixture
        .records
        .read(&checkout.id, BooksDoc::read_library_entries)
        .unwrap();
    assert!(shelf.is_empty(), "apps start empty: {shelf:?}");
    let book = driver.bound_resources(Some(&bundled_book())).unwrap();
    assert!(book.is_empty(), "the native shelf holds no bundled book");
}

#[test]
fn the_browser_library_opens_the_bundled_book_and_annotates_it_through_actions() {
    let fixture = Fixture::new("books-browser-flow");
    let definition = super::bundled_library_definition();
    let (mut first, checkout) = fixture.driver(definition, 1, LaunchTarget::New);
    let book = bundled_book();
    let target = LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    };
    let (mut second, _) = fixture.driver(definition, 2, target);

    let resource = first.bound_resources(Some(&book)).unwrap();
    let actions: Vec<_> = resource[0].actions.iter().map(|a| a.id.as_str()).collect();
    assert!(actions.contains(&"select-passage"), "{actions:?}");
    assert!(
        !actions.contains(&"highlight-selection"),
        "nothing is selected yet"
    );

    // Paragraphs 1–2 of chapter 1 (block 0 is its heading).
    invoke(
        &mut first,
        &book,
        "select-passage",
        serde_json::json!({"start_block": 1, "end_block": 2, "end_char": 40}),
    )
    .expect("select");
    let resource = first.bound_resources(Some(&book)).unwrap();
    let actions: Vec<_> = resource[0].actions.iter().map(|a| a.id.as_str()).collect();
    assert!(actions.contains(&"highlight-selection"), "{actions:?}");
    assert!(actions.contains(&"note-selection"), "{actions:?}");

    invoke(
        &mut first,
        &book,
        "highlight-selection",
        serde_json::json!({}),
    )
    .expect("highlight");
    invoke(
        &mut first,
        &book,
        "note-selection",
        serde_json::json!({"value": "The keeper's debt"}),
    )
    .expect("note");

    let annotations = fixture
        .records
        .read(&checkout.id, |doc| {
            BooksDoc::annotations_for(doc, &crate::content::bundled_book_id())
        })
        .unwrap();
    let highlights: Vec<_> = annotations
        .iter()
        .filter(|a| a.kind == AnnoKind::Highlight)
        .collect();
    assert_eq!(
        highlights.iter().map(|a| a.block).collect::<Vec<_>>(),
        [1, 2],
        "one highlight row per selected paragraph"
    );
    assert_eq!(highlights[1].end, 40);
    let note = annotations
        .iter()
        .find(|a| a.kind == AnnoKind::Note)
        .expect("note");
    assert_eq!(note.note, "The keeper's debt");
    assert_eq!((note.chapter, note.block, note.start), (0, 1, 0));
    assert!(
        note.excerpt.starts_with("The lantern at Cape Marrow"),
        "{:?}",
        note.excerpt
    );

    // The annotations read back as the open book's resources.
    let children = child_labels(&first, &book);
    assert!(
        children.contains(&("ebook.note".into(), "The keeper's debt".into())),
        "{children:?}"
    );
    assert!(
        children.iter().any(|(kind, _)| kind == "ebook.highlight"),
        "{children:?}"
    );

    // The second mount of the record shares the document: once its frame
    // passes run again (the browser runtime re-syncs it when the first
    // mount's record changes), it reads the first mount's annotations.
    second.relayout();
    assert!(
        child_labels(&second, &book)
            .iter()
            .any(|(_, label)| label == "The keeper's debt"),
        "the second mount reads the first mount's note"
    );
}

#[test]
fn a_handed_epub_opens_into_the_library_and_a_second_mount_shows_it_and_its_highlights() {
    let fixture = Fixture::new("books-browser-open");
    let definition = super::bundled_library_definition();
    let content = tilo_content::ContentStore::memory();
    let bytes = crate::test_epub::long_classic(24, 6);
    let book_id = crate::epub::content_id(&bytes);
    let book = super::book_resource(&book_id);
    let (file, receipt) =
        ui::application::OpenFile::new("odyssey.epub", super::EPUB_MEDIA_TYPE, bytes.clone())
            .unwrap();
    let lent = content.clone();
    let (mut phone, checkout) =
        fixture.driver_with(definition, 1, LaunchTarget::New, &book, move |host| {
            host.provide_content(lent);
            host.provide_open_file(file);
        });
    assert_eq!(receipt.opened_resource().as_deref(), Some(book.as_str()));

    let shelf = fixture
        .records
        .read(&checkout.id, BooksDoc::read_library_entries)
        .unwrap();
    let hash = tilo_content::hash_bytes(&bytes);
    assert_eq!(shelf.len(), 1, "the handed book, not the sample: {shelf:?}");
    assert_eq!(shelf[0].id, book_id);
    assert_eq!(shelf[0].content.as_deref(), Some(hash.as_str()));
    assert!(
        content.has(&hash),
        "the bytes live in the host's content store"
    );

    let resource = phone.bound_resources(Some(&book)).unwrap();
    assert_eq!(resource[0].label, "The Test Odyssey");
    let actions: Vec<_> = resource[0].actions.iter().map(|a| a.id.as_str()).collect();
    assert!(actions.contains(&"select-passage"), "{actions:?}");

    // A chapter well past the open one: Book 20, paragraphs 2–3.
    highlight_passage(
        &mut phone,
        &book,
        serde_json::json!({"chapter": 19, "start_block": 2, "end_block": 3}),
    );
    let highlights = book_highlights(&fixture, &checkout, &book_id);
    assert_eq!(
        highlights
            .iter()
            .map(|(chapter, block, _)| (*chapter, *block))
            .collect::<Vec<_>>(),
        [(19, 2), (19, 3)]
    );
    assert!(
        highlights[0].2.starts_with("Book 20, paragraph 2"),
        "{highlights:?}"
    );

    // The laptop mounts the phone's record with no file: the same content
    // store gives it the same book, and the record the same highlight.
    let target = LaunchTarget::Record {
        checkout,
        reveal: None,
    };
    let (mut laptop, checkout) = fixture.driver_with(definition, 2, target, &book, move |host| {
        host.provide_content(content);
    });
    let resource = laptop.bound_resources(Some(&book)).unwrap();
    let highlight = resource[0]
        .children
        .iter()
        .find(|child| child.kind == "ebook.highlight")
        .unwrap_or_else(|| panic!("{:?}", child_labels(&laptop, &book)));
    assert!(
        highlight.label.contains("Book 20, paragraph 2"),
        "{}",
        highlight.label
    );

    // Revealing it moves the laptop to Book 20, which a passage selected
    // without a chapter then lands in.
    let highlight = highlight.id.as_str().to_string();
    invoke(&mut laptop, &highlight, "open", serde_json::json!({})).expect("reveal");
    highlight_passage(&mut laptop, &book, serde_json::json!({"start_block": 5}));
    let highlights = book_highlights(&fixture, &checkout, &book_id);
    assert!(
        highlights
            .iter()
            .any(|(chapter, block, _)| (*chapter, *block) == (19, 5)),
        "{highlights:?}"
    );
}

#[test]
fn go_to_moves_each_mount_to_its_own_chapter_and_block_without_a_selection() {
    let fixture = Fixture::new("books-browser-go-to");
    let definition = super::bundled_library_definition();
    let content = tilo_content::ContentStore::memory();
    let bytes = crate::test_epub::verse_classic(6);
    let book_id = crate::epub::content_id(&bytes);
    let book = super::book_resource(&book_id);
    let (file, _receipt) =
        ui::application::OpenFile::new("odyssey.epub", super::EPUB_MEDIA_TYPE, bytes).unwrap();
    let lent = content.clone();
    let (mut phone, checkout) =
        fixture.driver_with(definition, 1, LaunchTarget::New, &book, move |host| {
            host.provide_content(lent);
            host.provide_open_file(file);
        });
    let target = LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    };
    let (mut laptop, _) = fixture.driver_with(definition, 2, target, &book, move |host| {
        host.provide_content(content);
    });
    let actions = |driver: &FrameHarness| -> Vec<String> {
        driver.bound_resources(Some(&book)).unwrap()[0]
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect()
    };
    assert!(actions(&phone).contains(&"go-to".to_string()));
    assert!(actions(&laptop).contains(&"go-to".to_string()));

    // A selection on the phone is dropped by going somewhere.
    invoke(
        &mut phone,
        &book,
        "select-passage",
        serde_json::json!({"chapter": 2, "start_block": 3}),
    )
    .expect("select");
    assert!(actions(&phone).contains(&"highlight-selection".to_string()));
    invoke(
        &mut phone,
        &book,
        "go-to",
        serde_json::json!({"chapter": 4, "block": 3}),
    )
    .expect("phone goes to Book 3");
    assert!(
        !actions(&phone).contains(&"highlight-selection".to_string()),
        "going to a passage selects nothing"
    );
    invoke(
        &mut laptop,
        &book,
        "go-to",
        serde_json::json!({"chapter": 6, "block": 3}),
    )
    .expect("laptop goes to Book 5");
    assert!(!actions(&laptop).contains(&"highlight-selection".to_string()));

    // Each mount is where it went: a passage selected without a chapter lands
    // in that mount's chapter.
    highlight_passage(&mut phone, &book, serde_json::json!({"start_block": 4}));
    highlight_passage(&mut laptop, &book, serde_json::json!({"start_block": 5}));
    let highlights = book_highlights(&fixture, &checkout, &book_id);
    let at: Vec<_> = highlights
        .iter()
        .map(|(chapter, block, _)| (*chapter, *block))
        .collect();
    assert!(
        at.contains(&(4, 4)) && at.contains(&(6, 5)),
        "{highlights:?}"
    );
    assert!(
        highlights
            .iter()
            .any(|(_, _, excerpt)| excerpt.starts_with("Book 3, stanza 2: tell me")),
        "{highlights:?}"
    );

    // A target the book does not have is refused, not clamped.
    for missing in [
        serde_json::json!({"chapter": 40}),
        serde_json::json!({"chapter": 4, "block": 60}),
        serde_json::json!({"block": -1}),
    ] {
        assert!(
            invoke(&mut phone, &book, "go-to", missing.clone()).is_err(),
            "{missing}"
        );
    }
}

/// Select a passage through the book's action door and highlight it.
fn highlight_passage(driver: &mut FrameHarness, book: &str, passage: serde_json::Value) {
    invoke(driver, book, "select-passage", passage).expect("select");
    invoke(driver, book, "highlight-selection", serde_json::json!({})).expect("highlight");
}

/// The book's highlights in the record: chapter, block and excerpt.
fn book_highlights(
    fixture: &Fixture,
    record: &RecordCheckout,
    book_id: &str,
) -> Vec<(usize, usize, String)> {
    fixture
        .records
        .read(&record.id, |doc| BooksDoc::annotations_for(doc, book_id))
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == AnnoKind::Highlight)
        .map(|a| (a.chapter, a.block, a.excerpt))
        .collect()
}

#[test]
fn a_file_books_does_not_open_or_cannot_read_is_refused() {
    let fixture = Fixture::new("books-browser-open-refused");
    let definition = super::bundled_library_definition();
    let refused = |name: &str, media_type: &str, bytes: Vec<u8>| {
        let (file, receipt) = ui::application::OpenFile::new(name, media_type, bytes).unwrap();
        let error = fixture
            .component(definition, 1, LaunchTarget::New, |host| {
                host.provide_content(tilo_content::ContentStore::memory());
                host.provide_open_file(file);
            })
            .err()
            .expect("refused");
        assert_eq!(receipt.opened_resource(), None);
        error.to_string()
    };
    let pdf = refused("paper.pdf", "application/pdf", b"%PDF-1.7".to_vec());
    assert!(
        pdf.contains("Books does not open application/pdf files"),
        "{pdf}"
    );
    let broken = refused("broken.epub", super::EPUB_MEDIA_TYPE, b"not a zip".to_vec());
    assert!(broken.contains("Couldn’t open broken.epub"), "{broken}");
}

#[test]
fn a_passage_outside_the_chapter_is_refused_without_a_selection() {
    let fixture = Fixture::new("books-browser-refusal");
    let (mut driver, _) = fixture.driver(super::bundled_library_definition(), 1, LaunchTarget::New);
    let book = bundled_book();
    assert!(
        invoke(
            &mut driver,
            &book,
            "select-passage",
            serde_json::json!({"start_block": 900}),
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut driver,
            &book,
            "select-passage",
            serde_json::json!({"start_block": -1}),
        )
        .is_err(),
        "a negative block is refused by the parameter schema"
    );
    let resource = driver.bound_resources(Some(&book)).unwrap();
    assert!(
        resource[0]
            .actions
            .iter()
            .all(|action| action.id != "highlight-selection")
    );
}
