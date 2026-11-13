use tilo_automerge::ApplyIncoming as _;
use tilo_document::action::{ImportFormat, RichTextAction};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use word_processor::framework::{Document, WordProcessorApp};

fn picture() -> Vec<u8> {
    (0_u32..3000)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()[0])
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = byte(0) << 16 | byte(1) << 8 | byte(2);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(
                    ALPHABET[usize::try_from((n >> shift) & 63).unwrap()],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn page(picture: &[u8]) -> Vec<u8> {
    format!(
        r#"<h1>Trip</h1><p><img src="data:image/png;base64,{}" alt="the view"></p>"#,
        base64(picture)
    )
    .into_bytes()
}

fn holds(haystack: &[u8], picture: &[u8]) -> bool {
    haystack.windows(32).any(|window| window == &picture[..32])
}

fn app() -> TestApp<WordProcessorApp> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<WordProcessorApp>::new().unwrap()
}

fn record(writer: &mut TestApp<WordProcessorApp>) -> automerge::AutoCommit {
    writer.flush().unwrap();
    automerge::AutoCommit::load(&writer.record_document(&writer.record().clone()).unwrap()).unwrap()
}

fn apply(app: &mut TestApp<WordProcessorApp>, action: &RichTextAction) {
    app.invoke_confirmed(Invoke::<Document>::edit(&word_processor::model::edit(action)).unwrap())
        .unwrap();
}

fn blocks(doc: &automerge::AutoCommit) -> Vec<tilo_doc::snapshot::BlockView> {
    let c = word_processor::model::open(doc).unwrap();
    tilo_doc::model::materialize(doc, &c).blocks
}

fn pictures(doc: &automerge::AutoCommit) -> Vec<String> {
    blocks(doc)
        .into_iter()
        .filter(|block| block.kind == "image")
        .filter_map(|block| match block.attrs.get("blob") {
            Some(tilo_doc::AttrVal::Str(reference)) => Some(reference.clone()),
            _ => None,
        })
        .collect()
}

fn texts(doc: &automerge::AutoCommit) -> Vec<String> {
    blocks(doc).into_iter().map(|block| block.text).collect()
}

#[test]
fn a_files_pictures_are_ciphertext_in_the_store_and_every_member_opens_them() {
    let picture = picture();
    let mut alice = app();
    apply(
        &mut alice,
        &RichTextAction::ReplaceBody {
            markdown: "Draft".into(),
        },
    );
    let mut bob = record(&mut alice);
    let bobs_store = tilo_content::ContentStore::memory();
    let window = alice.content().sealing();

    let action =
        word_processor::import_action(&page(&picture), word_processor::WpFormat::Html, &window)
            .unwrap();
    apply(&mut alice, &action);
    let mut alices = record(&mut alice);
    let [reference] = &pictures(&alices)[..] else {
        panic!("one picture, got {:?}", texts(&alices));
    };
    assert!(tilo_content::is_sealed(reference), "{reference}");
    assert_eq!(alice.content().read(reference), Some(picture.clone()));
    assert!(
        !alice.content().has(&tilo_content::hash_bytes(&picture)),
        "never kept in the clear"
    );
    let address = tilo_content::content_address(reference).unwrap();
    let ciphertext = alice.content().read(&address).unwrap();
    assert!(!holds(&ciphertext, &picture), "the store holds ciphertext");
    let recorded = serde_json::to_value(&action).unwrap();
    assert!(recorded.get("ReplaceFormatted").is_none(), "{recorded}");

    bob.merge_incoming(&mut alices).unwrap();
    assert_eq!(texts(&bob), texts(&alices));
    assert_eq!(&pictures(&bob), std::slice::from_ref(reference));
    assert_eq!(
        bobs_store.read(reference),
        None,
        "not here before it arrives"
    );
    bobs_store.put_public(&ciphertext).unwrap();
    assert_eq!(bobs_store.read(reference), Some(picture.clone()));

    let carol = record(&mut alice);
    assert_eq!(&pictures(&carol), std::slice::from_ref(reference));
    let carols_store = tilo_content::ContentStore::memory();
    carols_store.put_public(&ciphertext).unwrap();
    assert_eq!(carols_store.read(reference), Some(picture));
}

#[test]
fn a_document_imported_before_pictures_were_sealed_still_opens() {
    let picture = picture();
    let mut app = app();
    let address = app.content().put(&picture).unwrap();
    assert_eq!(address, tilo_content::hash_bytes(&picture));
    apply(
        &mut app,
        &RichTextAction::ReplaceFormatted {
            format: ImportFormat::Html,
            bytes: page(&picture),
        },
    );
    let doc = record(&mut app);
    assert_eq!(
        pictures(&doc),
        std::slice::from_ref(&address),
        "{:?}",
        texts(&doc)
    );
    assert_eq!(app.content().read(&address), Some(picture));
}
