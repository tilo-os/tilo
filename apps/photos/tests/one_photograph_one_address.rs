//! One photograph, one address — in every session that has it. Same bytes, same blake3 hash, three
//! names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use photos::PhotosApp;
use photos::model::PhotoLibrary;
use tilo_actions::ActionRegistry;
use ui::tilo_resource::Resource as _;

fn new_doc() -> AutoCommit {
    let mut doc = AutoCommit::new();
    PhotoLibrary::initialize(&mut doc).expect("a new library initializes");
    doc
}

fn library(doc: &AutoCommit) -> PhotoLibrary {
    PhotoLibrary::read(doc).expect("the library reads")
}

fn published(doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
    vec![ui::tilo_resource::publish::<PhotoLibrary>(
        "photos.library",
        "Photos",
        &library(doc),
    )]
}
use tilo_actions::door::{self, ActionAddress};

fn photograph() -> Vec<u8> {
    photos::pixels::encode_png(&photos::shots_pattern(900, 600)).expect("the pattern encodes")
}

const SQUARE_900X600: [f64; 4] = [0.166_667, 0.0, 0.666_667, 1.0];

struct Session {
    _dir: tempfile::TempDir,
    doc: AutoCommit,
    path: String,
    content: tilo_content::ContentStore,
}

impl Session {
    fn staging(prefix: &str, file_name: &str, bytes: &[u8]) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("a runtime directory for this session");
        let staged = dir.path().join("tilo-opened");
        std::fs::create_dir_all(&staged).expect("a staging directory");
        let path = staged.join(file_name);
        std::fs::write(&path, bytes).expect("the session stages its own copy");
        let path = path.to_string_lossy().into_owned();

        let mut doc = new_doc();
        let content = tilo_content::ContentStore::memory();
        let dispatch = ui::framework::testing::dispatcher("so.tilo.photos");
        let app = PhotosApp::with_content(content.clone()).with_dispatcher(dispatch.clone());
        app.state().open_path(&path);
        ui::framework::testing::resolve(&dispatch, &mut doc).expect("the photograph imports");
        assert!(
            app.state().open_photo_id().is_some() && library(&doc).photos.len() == 1,
            "the fixture failed to open its own staged photograph, so nothing \
             below proves anything"
        );
        Self {
            _dir: dir,
            doc,
            path,
            content,
        }
    }

    fn handed_the_content(prefix: &str, bytes: &[u8]) -> Self {
        let name = format!("{}.png", tilo_content::hash_bytes(bytes));
        Self::staging(prefix, &name, bytes)
    }

    fn published_id(&self) -> String {
        let published = published(&self.doc);
        let shown: Vec<_> = published[0]
            .children
            .iter()
            .filter(|child| child.kind == photos::actions::PHOTO_KIND)
            .collect();
        assert_eq!(
            shown.len(),
            1,
            "a session with one photograph publishes it once"
        );
        shown[0].id.0.clone()
    }

    fn perform(
        &mut self,
        address: &ActionAddress,
        region: [f64; 4],
    ) -> Result<(), door::DoorError> {
        let resources = published(&self.doc);
        let spec = door::resolve(&resources, address)
            .ok_or_else(|| door::DoorError::Unknown(address.clone()))?;
        let [x, y, w, h] = region;
        let encoded = spec
            .build_json(
                serde_json::json!({ "region": { "x": x, "y": y, "w": w, "h": h } })
                    .as_object()
                    .expect("an object"),
            )
            .map_err(|error| door::DoorError::InvalidArguments(error.to_string()))?;
        let mut registry = ActionRegistry::new();
        PhotoLibrary::register(&mut registry);
        registry
            .decode(&encoded)
            .expect("the app's own registry decodes what its own tree encoded")
            .apply(&mut self.doc)
            .expect("the crop applies");
        Ok(())
    }

    fn edited_size(&self) -> (u32, u32) {
        let library = library(&self.doc);
        let photo = library.all_photos().next().unwrap();
        photo.image.edited_size(&photo.edits)
    }
}

#[test]
fn operations_act_on_the_photograph_addressed_by_its_content() {
    eprintln!(
        "phase an_operation_invoked_on_a_photograph_acts_on_it_in_a_session_that_staged_it_elsewhere"
    );
    {
        let bytes = photograph();
        let mut watcher = Session::handed_the_content("qlvx-", &bytes);
        let mut agent = Session::handed_the_content("qlva-", &bytes);

        assert_ne!(
            watcher.path, agent.path,
            "the two sessions staged to the same place, so this proves nothing \
             about a picture that is in two of them"
        );
        assert_ne!(
            watcher.edited_size(),
            (600, 600),
            "the fixture starts out square, so squaring it would show nothing"
        );

        let address = PhotoLibrary::CROP.on(agent.published_id());
        agent
            .perform(&address, SQUARE_900X600)
            .expect("the agent's own session performs it");
        assert_eq!(
            agent.edited_size(),
            (600, 600),
            "the operation did not square the picture even where it was invoked"
        );

        watcher
            .perform(&address, SQUARE_900X600)
            .unwrap_or_else(|e| {
                panic!(
                    "the address the agent acted through — {} — names nothing in \
                     the session the person is watching, so their picture never \
                     moved: {e}",
                    address.resource
                )
            });
        assert_eq!(
            watcher.edited_size(),
            (600, 600),
            "the operation resolved in the watched session but left its \
             photograph the shape it was"
        );

        let mut third = Session::handed_the_content("qlvq-", &bytes);
        let from_the_watcher = PhotoLibrary::CROP.on(watcher.published_id());
        third
            .perform(&from_the_watcher, SQUARE_900X600)
            .expect("an address minted in the watcher's session reaches the third");
        assert_eq!(third.edited_size(), (600, 600));
    }

    eprintln!("phase a_photograph_asked_for_by_content_is_addressed_by_the_content_asked_for");
    {
        let bytes = photograph();
        let asked_for = tilo_content::hash_bytes(&bytes);

        let staging = ["qlva-", "qlvq-", "qlvx-"];
        let sessions: Vec<Session> = staging
            .iter()
            .map(|prefix| Session::handed_the_content(prefix, &bytes))
            .collect();

        for (a, b) in sessions.iter().zip(sessions.iter().skip(1)) {
            assert_ne!(
                a.path, b.path,
                "two sessions staged to one place, so this proves nothing about a \
                 picture that is in three of them"
            );
        }

        for (session, prefix) in sessions.iter().zip(staging) {
            let id = session.published_id();
            assert_eq!(
                id,
                photos::resource::photo_resource_id(&format!("photos.photo:{asked_for}")),
                "the session staging into {prefix}… published an address for \
                 where it put the picture rather than for the picture that was \
                 asked for"
            );
            let photo = photos::resource::photo_of_resource(&id).expect("it names a photograph");
            assert!(
                !photo.contains('/'),
                "the published address carries a filesystem path, which is true \
                 on one machine: {id}"
            );
        }
    }
}

#[test]
fn one_content_is_one_openable_address_that_survives_missing_content() {
    eprintln!("phase the_same_content_under_two_names_is_one_address");
    {
        let bytes = photograph();
        let theirs = Session::handed_the_content("qlvx-", &bytes);
        let mine = Session::staging("qlva-", "harbour.png", &bytes);

        assert!(
            mine.path.ends_with("harbour.png") && !theirs.path.ends_with("harbour.png"),
            "the fixture gave both sessions the same file name, so it is not \
             standing in for two"
        );
        assert_eq!(
            mine.published_id(),
            theirs.published_id(),
            "one photograph opened under two names published two addresses"
        );
    }

    eprintln!("phase an_address_names_content_this_machine_can_open");
    {
        let bytes = photograph();
        let session = Session::handed_the_content("qlva-", &bytes);
        let id = session.published_id();

        let photo = photos::resource::photo_of_resource(&id).expect("it names a photograph");
        let content = photos::actions::content_of(&photo)
            .expect("an address for an open photograph names its content");
        let fetched = session
            .content
            .read(content)
            .expect("the content the address names is in this machine's store");
        let decoded = photos::pixels::decode(&fetched).expect("and it decodes to a picture");
        assert_eq!(
            (decoded.w, decoded.h),
            (900, 600),
            "the address resolved to bytes that are not the photograph it names"
        );
    }
}
