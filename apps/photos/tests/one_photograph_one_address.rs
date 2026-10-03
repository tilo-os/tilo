//! One photograph, one address — in every session that has it.
//!
//! # The loss this rules out
//!
//! A photograph published as an addressable resource used to be named by the
//! absolute path the local machine had staged it at:
//!
//! ```text
//! photos.image:/run/user/1000/qlva3099960/tilo-opened/623fdbf0….png
//! ```
//!
//! Each session stages the content it is handed under its *own* runtime
//! directory, so one picture had one id per participant — `qlva…` here,
//! `qlvx…` there, `qlvq…` in the third. Same bytes, same blake3 hash, three
//! names.
//!
//! An address is resolved by string equality (`tilo_actions::door::resolve`),
//! so an operation invoked on one of those ids reached exactly one session:
//! the one whose path happened to win. That is what happened — an agent
//! cropped the editor in `qlva…` while the person watching was looking at the
//! editor in `qlvx…`, whose picture never moved. The run before it the same
//! invocation worked, because that time the id named the watcher's path. Which
//! is to say the outcome was decided by which path won, not by anything
//! anybody chose.
//!
//! So the tests here drive replicas with genuinely different staging
//! directories and invoke across them. The claim is not that two ids happen to
//! match — it is the consequence: **an operation invoked on a photograph acts
//! on that photograph in a session that staged it somewhere else.**

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use photos::PhotosApp;
use photos::actions::{PhotosResources, register};
use photos::doc::PhotosDoc;
use tilo_actions::ActionRegistry;
use tilo_actions::door::{self, ActionAddress};

/// The bytes of one photograph. Identical in every session below — that is the
/// premise: one picture, staged more than once.
fn photograph() -> Vec<u8> {
    photos::pixels::encode_png(&photos::shots_pattern(900, 600)).expect("the pattern encodes")
}

/// The region a `crop` invocation squares the 900×600 fixture to: the largest
/// centred square, in the `x,y,w,h` argument `crop` collects.
///
/// Stands in for a dedicated "square it" operation — out of this port's
/// scope, since the one in the source it was ported from existed only for a
/// film script to invoke. The address-resolution property below does not
/// depend on *which* operation reshapes the picture, only on the address
/// reaching a session that did not mint it, so a `crop` with a fixed region
/// proves the same thing.
const SQUARE_900X600: &str = "0.166667,0,0.666667,1";

/// One participant's session: its own runtime directory, its own copy of the
/// bytes on disk, its own document.
///
/// Nothing is shared between two of these except the content, which is the
/// situation the whole defect lives in.
struct Session {
    _dir: tempfile::TempDir,
    doc: AutoCommit,
    path: String,
    /// This session's content store, which its open put the bytes in.
    content: tilo_content::ContentStore,
}

impl Session {
    /// Stage `bytes` under a runtime directory of this session's own and open
    /// them through the app's ordinary open, which is what puts the content in
    /// the store and records its hash.
    ///
    /// `file_name` is a parameter because the two things that vary between
    /// sessions are the directory and — when one participant opened the file
    /// from their own disk and another was handed a staged copy — the name.
    fn staging(prefix: &str, file_name: &str, bytes: &[u8]) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("a runtime directory for this session");
        // The shape `shell::shell::agent_opens::staging_dir` writes: under the
        // session's own runtime directory, in `tilo-opened`.
        let staged = dir.path().join("tilo-opened");
        std::fs::create_dir_all(&staged).expect("a staging directory");
        let path = staged.join(file_name);
        std::fs::write(&path, bytes).expect("the session stages its own copy");
        let path = path.to_string_lossy().into_owned();

        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let content = tilo_content::ContentStore::memory();
        let mut app = PhotosApp::with_content(content.clone());
        app.open_path(&mut doc, &path);
        assert!(
            PhotosDoc::image(&doc).is_some(),
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

    /// Staged the way content handed to a room is staged: named by its own
    /// content hash.
    fn handed_the_content(prefix: &str, bytes: &[u8]) -> Self {
        let name = format!("{}.png", tilo_content::hash_bytes(bytes));
        Self::staging(prefix, &name, bytes)
    }

    /// The address of the open photograph, as this session publishes it.
    fn published_id(&self) -> String {
        let published = PhotosResources.publish_resources(&self.doc);
        assert_eq!(
            published.len(),
            2,
            "a session with one photograph open publishes its active resource and library"
        );
        published[0].id.0.clone()
    }

    /// Resolve `address` against **this** session's resource tree and perform
    /// it on this session's document — the whole route an out-of-process
    /// caller takes, with no shortcut past the id.
    fn perform(
        &mut self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<(), door::DoorError> {
        let resources = PhotosResources.publish_resources(&self.doc);
        let encoded = door::encode(&resources, address, argument)?;
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
            .decode(&encoded)
            .expect("the app's own registry decodes what its own tree encoded")
            .apply(&mut self.doc)
            .expect("the crop applies");
        Ok(())
    }

    /// The size of the picture as edited — what a person would see change.
    fn edited_size(&self) -> (u32, u32) {
        let image = PhotosDoc::image(&self.doc).expect("something is open");
        image.edited_size(&PhotosDoc::edits(&self.doc))
    }
}

#[test]
fn operations_act_on_the_photograph_addressed_by_its_content() {
    // The headline property: the address one session publishes is an address
    // every session can act through.
    //
    // Both directions, because which session's path wins is a race nobody chose —
    // the same reason the defect looked intermittent.
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

        // The agent enumerates its own session and squares the picture there.
        let address = ActionAddress::new(agent.published_id(), "crop");
        agent
            .perform(&address, Some(SQUARE_900X600))
            .expect("the agent's own session performs it");
        assert_eq!(
            agent.edited_size(),
            (600, 600),
            "the operation did not square the picture even where it was invoked"
        );

        // The person watching is looking at the *other* session. The same address
        // has to reach the same photograph there.
        watcher
            .perform(&address, Some(SQUARE_900X600))
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

        // And the other way round, since neither session is privileged.
        let mut third = Session::handed_the_content("qlvq-", &bytes);
        let from_the_watcher = ActionAddress::new(watcher.published_id(), "crop");
        third
            .perform(&from_the_watcher, Some(SQUARE_900X600))
            .expect("an address minted in the watcher's session reaches the third");
        assert_eq!(third.edited_size(), (600, 600));
    }

    // A photograph opened **because a participant asked for it by content** is
    // addressed by that content.
    //
    // This is the route the demo takes, and the one no test covered: nobody
    // chooses a file. A participant with no window names a hash
    // (`CollabRequest::OpenApp { app_id, with }`), the session it asks writes
    // those bytes under its own runtime directory named by their own hash
    // (`shell::shell::agent_opens::stage_content`) and launches the application
    // with that path as its argument, and the application opens what it was
    // given. Every session that is asked does this into a directory of its own.
    //
    // So the claim has two halves and both matter. The address must be the same
    // in all of them — that is `an_operation_invoked_on_a_photograph…` above —
    // and it must be **the hash that was asked for**, so a participant that
    // named the content can address what it asked for without waiting to be told
    // where the machine put it.
    //
    // Read a take's logs and this is the line that says whether it holds: an
    // address with a `/` in it is a session's own directory, and an operation
    // invoked on one reaches the session that happened to stage it.
    eprintln!("phase a_photograph_asked_for_by_content_is_addressed_by_the_content_asked_for");
    {
        let bytes = photograph();
        // What the participant names. It has no filesystem of its own and says
        // nothing about anybody else's.
        let asked_for = tilo_content::hash_bytes(&bytes);

        let staging = ["qlva-", "qlvq-", "qlvx-"];
        let sessions: Vec<Session> = staging
            .iter()
            .map(|prefix| Session::handed_the_content(prefix, &bytes))
            .collect();

        // The premise: three sessions really did put it in three places.
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
                format!("photos.image:{asked_for}"),
                "the session staging into {prefix}… published an address for \
                 where it put the picture rather than for the picture that was \
                 asked for"
            );
            assert!(
                !id.contains('/'),
                "the published address carries a filesystem path, which is true \
                 on one machine: {id}"
            );
        }
    }
}

#[test]
fn one_content_is_one_openable_address_that_survives_missing_content() {
    // The name is not the identity either.
    //
    // One participant opens a photograph off their own disk; another is handed
    // the same content staged under its hash. The bytes are the same picture, so
    // it is the same resource — and an address minted on one side has to name it
    // on the other. Folding the file's name into the id would put this case
    // straight back where the path had it.
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

    // An address has to be resolvable back to something openable here.
    //
    // A participant that never held either path — it was handed the content, not
    // a file — must be able to go from the address alone to the bytes it names.
    // Without that the id would be portable and useless: everybody could say the
    // same word for the picture and nobody but the session that staged it could
    // open one.
    eprintln!("phase an_address_names_content_this_machine_can_open");
    {
        let bytes = photograph();
        let session = Session::handed_the_content("qlva-", &bytes);
        let id = session.published_id();

        let content = photos::actions::content_of(&id)
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

    // A document written before a photograph had a content hash still publishes
    // an address, and it is the one it always had.
    //
    // Nothing is migrated: the resource tree is a pure function of the document
    // and is rebuilt every frame, so there are no stored ids to rewrite. A
    // picture that only has a path is named by it — exactly as good as that ever
    // was in the one session the path is true of, and better than publishing
    // nothing.
    eprintln!("phase a_photograph_that_names_no_content_keeps_the_address_it_had");
    {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let old = photos::OpenImage::from_path("/var/workspace/photos/harbour.png", 800, 600);
        assert!(
            old.content.is_none(),
            "the fixture is not an older document"
        );
        PhotosDoc::set_image(&mut doc, Some(&old));

        let published = PhotosResources.publish_resources(&doc);
        assert_eq!(
            published[0].id.0, "photos.image:/var/workspace/photos/harbour.png",
            "an older document's photograph lost the only address it had"
        );
    }
}
