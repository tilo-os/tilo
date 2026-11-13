//! A picture placed in a note is sealed where it is placed: the notebook
//! keeps a reference that carries the key, and whatever holds or serves the
//! bytes (this device's store, the workspace's host) holds ciphertext alone.
//! Every member reading the notebook opens it, a member who joins later too,
//! and a picture an agent places is sealed the same way.

use notes::NotesShell;
use notes::model::PageId;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::{Bytes, Invoke};

/// A noisy picture, so it does not compress below the size it is meant to
/// have.
fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let v = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
        let [r, g, b, _] = v.to_le_bytes();
        image::Rgb([r, g, b])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn app() -> TestApp<NotesShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<NotesShell>::new().unwrap()
}

fn with_page(app: &mut TestApp<NotesShell>) -> PageId {
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&notes::actions::create_page_action(page, None, "Trip")).unwrap())
        .unwrap();
    page
}

/// The one picture the notebook keeps.
fn kept(app: &TestApp<NotesShell>) -> Bytes {
    let images = app.read_document(|doc| notes::read::notebook(doc).images);
    let [(_, stored)] = images.iter().collect::<Vec<_>>()[..] else {
        panic!("one picture, got {}", images.len());
    };
    stored.clone()
}

/// The pictures `app` shows, read from its own device's store.
fn shown(app: &TestApp<NotesShell>) -> Vec<Vec<u8>> {
    let store = app.content();
    app.read_document(|doc| {
        notes::notebook::image_snapshot(&notes::read::notebook(doc).images, &store)
            .into_values()
            .collect()
    })
}

fn holds(haystack: &[u8], picture: &[u8]) -> bool {
    haystack.windows(64).any(|window| window == &picture[..64])
}

/// Every file under `root`, read whole.
fn files_under(root: &std::path::Path) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            found.push(bytes);
        }
    }
    found
}

/// What a workspace's host does with `stored`'s content as the record's
/// route carries it from `writer`: keeps the ciphertext under its address in
/// `host`, and serves it back, as it is, to whoever asks.
fn through_host(writer: &TestApp<NotesShell>, stored: &Bytes, host: &std::path::Path) -> Vec<u8> {
    let reference = stored.reference().expect("kept outside the notebook");
    let address = tilo_content::content_address(&reference).unwrap();
    let carried = writer
        .content()
        .read(&address)
        .expect("the writer's device holds what it sealed");
    let keeper = tilo_content::ContentStore::in_dir(host);
    assert_eq!(keeper.put_public(&carried).unwrap(), address);
    keeper.read(&address).unwrap()
}

#[test]
fn a_placed_picture_is_ciphertext_off_its_readers_and_every_member_opens_it() {
    let picture = png(240, 180);
    let mut alice = app();
    let page = with_page(&mut alice);
    let mut record =
        automerge::AutoCommit::load(&alice.record_document(&alice.record().clone()).unwrap())
            .unwrap();
    let mut bob = TestApp::<NotesShell>::with_workspace_record(&mut record).unwrap();

    // Alice places it from her window.
    alice
        .invoke(
            Invoke::apply(&notes::actions::place_image_action(
                page,
                picture.clone(),
                Some("the view"),
            ))
            .unwrap(),
        )
        .unwrap();
    let stored = kept(&alice);
    let reference = stored.reference().unwrap();
    assert!(tilo_content::is_sealed(&reference), "{reference}");
    assert_eq!(stored.hash(), *blake3::hash(&picture).as_bytes());
    assert!(
        !alice.content().has(&tilo_content::hash_bytes(&picture)),
        "nothing kept in the clear on her device"
    );

    // The host keeps and serves ciphertext alone.
    let host = tempfile::tempdir().unwrap();
    let served = through_host(&alice, &stored, host.path());
    assert!(!holds(&served, &picture), "the host serves ciphertext");
    let on_disk = files_under(host.path());
    assert!(on_disk.iter().any(|file| file == &served));
    assert!(
        on_disk.iter().all(|file| !holds(file, &picture)),
        "nothing on the host's disk holds the picture"
    );

    // Bob, a member with the notebook open, takes Alice's change and the
    // ciphertext the host served, and sees the picture.
    assert!(alice.deliver_changes_to(&mut bob).unwrap());
    assert_eq!(kept(&bob), stored);
    assert!(shown(&bob).is_empty(), "not here before it arrives");
    bob.content().put_public(&served).unwrap();
    assert_eq!(shown(&bob), std::slice::from_ref(&picture));
    assert_eq!(shown(&alice), std::slice::from_ref(&picture));

    // Carol joins later: the notebook's history reaches her, and the host
    // she fetches the picture from still holds it sealed.
    let mut joined =
        automerge::AutoCommit::load(&alice.record_document(&alice.record().clone()).unwrap())
            .unwrap();
    let carol = TestApp::<NotesShell>::with_workspace_record(&mut joined).unwrap();
    carol
        .content()
        .put_public(
            &tilo_content::ContentStore::in_dir(host.path())
                .read(&tilo_content::content_address(&reference).unwrap())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(shown(&carol), [picture]);
}

#[test]
fn a_picture_an_agent_places_is_sealed_where_it_invokes() {
    let picture = png(200, 160);
    let mut app = app();
    let page = with_page(&mut app);
    let agent = ActionInvoker::actor("agent:scribe")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    app.call_by(
        agent,
        &notes::actions::page_resource_id(page),
        "place-image",
        serde_json::json!({ "bytes": picture, "alt": "a chart" }),
    )
    .unwrap_or_else(|error| panic!("place-image refused: {error:?}"));
    let stored = kept(&app);
    assert!(tilo_content::is_sealed(&stored.reference().unwrap()));
    assert!(!app.content().has(&tilo_content::hash_bytes(&picture)));

    let host = tempfile::tempdir().unwrap();
    let served = through_host(&app, &stored, host.path());
    assert!(
        files_under(host.path())
            .iter()
            .all(|file| !holds(file, &picture))
    );
    let reader = tilo_content::ContentStore::memory();
    reader.put_public(&served).unwrap();
    assert_eq!(stored.read(&reader), Some(picture.clone()));
    assert_eq!(shown(&app), [picture]);
}
