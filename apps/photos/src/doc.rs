//! Automerge document layer.
//!
//! What the user decided lives in the CRDT: which file is open, and the edits
//! applied to it. That makes every edit undoable through `State::commit_action`
//! and replicable to a collaborator for free.
//!
//! ```text
//! root
//! ├── "image"  (Str)   # serde_json of Option<OpenImage>  — path + name + intrinsic size
//! └── "edits"  (Str)   # serde_json of Edits              — crop rect + quarter turns
//! ```
//!
//! # What is deliberately not here
//!
//! The pixels. The document holds a path and two small edit fields and never
//! the image itself, which is the same invariant
//! `crates/doc/qdoc/src/blobstore.rs` documents for a document's blobs: an
//! automerge change carrying a multi-megabyte payload bloats every replica and
//! durable host log and makes later edits wait behind the media. A path plus a
//! crop rect is a change of a few dozen bytes.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, Value};

use crate::model::{Edits, OpenImage};

/// The bytes of the picture `open` names, from wherever this machine has them.
///
/// The path first: when this session is the one that opened the picture, the
/// path is its own and reading it is a file read rather than a lookup. Then the
/// content, which is the half that works for everybody else. The document is
/// shared and the path in it is not — the same photograph is staged under a
/// different absolute path in every session that receives it, so every
/// participant except the one whose path won is holding a path into somebody
/// else's runtime directory. Resolving the hash is what turns "a photograph a
/// collaborator opened" into pixels here rather than into an empty canvas.
///
/// Missing both is not an error to report: `blobstore::get` asks the room for
/// the content on its way through, so the honest answer now is no picture, and
/// the moment after the bytes land there is one.
///
/// Public, and the only answer to this question, because the app is not the
/// only thing that asks it. Anything acting on the open photograph — the
/// editor drawing it, a participant with no window rendering the crop it was
/// asked for — needs the same bytes, and a second copy of "read the path" is
/// how one of those callers ends up reading a path that is true on another
/// machine.
pub fn bytes_of(open: &OpenImage) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(&open.path) {
        return Some(bytes);
    }
    qdoc::blobstore::get(open.content.as_deref()?)
}

pub struct PhotosDoc;

impl PhotosDoc {
    /// True when the document has never been written to.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(doc.get(&ROOT, "edits"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Seed default state: **no image open** and no edits.
    ///
    /// There is no sample photograph here and no fabricated recent list. An
    /// app with nothing open shows an empty state that says so; content only
    /// ever arrives from a file the user actually opened.
    /// Idempotent — only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit) {
        if Self::get_str(doc, "image").is_none() {
            Self::write_json(doc, "image", &Option::<OpenImage>::None);
        }
        if Self::get_str(doc, "edits").is_none() {
            Self::write_json(doc, "edits", &Edits::default());
        }
    }

    /// The photograph currently open, if any.
    ///
    /// Reads **every** value the key holds, not only the one Automerge would
    /// hand back, and a photograph beats no photograph. See
    /// [`Self::conflicting`] for why that is the difference between a
    /// collaborator's picture surviving and a collaborator's picture being
    /// closed by somebody else's app starting up.
    pub fn image(doc: &AutoCommit) -> Option<OpenImage> {
        Self::conflicting::<Option<OpenImage>>(doc, "image")
            .into_iter()
            .flatten()
            .next()
    }

    /// Open `image`, resetting the edits — a newly opened file starts
    /// untouched, never inheriting the previous one's crop.
    pub fn set_image(doc: &mut AutoCommit, image: Option<&OpenImage>) {
        Self::write_json(doc, "image", &image.cloned());
        Self::write_json(doc, "edits", &Edits::default());
    }

    /// The edits in force.
    ///
    /// Resolved like [`Self::image`], and for the same reason: an app that has
    /// just started writes untouched edits, and untouched edits arriving
    /// beside somebody's crop must not be what silently undoes the crop.
    pub fn edits(doc: &AutoCommit) -> Edits {
        Self::conflicting::<Edits>(doc, "edits")
            .into_iter()
            .find(|e| !e.is_identity())
            .unwrap_or_default()
    }

    /// Replace the edits in force.
    pub fn set_edits(doc: &mut AutoCommit, edits: &Edits) {
        Self::write_json(doc, "edits", edits);
    }

    // -- scalar helpers ----------------------------------------------------

    fn get_str(doc: &AutoCommit, key: &str) -> Option<String> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Scalar(s), _))) => s.to_str().map(std::string::ToString::to_string),
            _ => None,
        }
    }

    fn write_json<T: serde::Serialize>(doc: &mut AutoCommit, key: &str, value: &T) {
        if let Ok(s) = serde_json::to_string(value) {
            let _ = doc.put(&ROOT, key, s);
        }
    }

    /// Every value `key` currently holds, parsed.
    ///
    /// # Why a key can hold more than one value
    ///
    /// Two replicas that write the same key without having seen each other's
    /// write leave both values in the document. Automerge keeps them all and
    /// `get` returns one — deterministically, so every replica agrees, but it
    /// decides by actor id, which is a coin toss nobody chose.
    ///
    /// That is not an abstract hazard. An app of this kind starts by writing
    /// what it holds, and a second participant's photo editor starting up
    /// holds nothing: it writes "no photograph", concurrently with the
    /// photograph somebody else has open. Half the time the empty write wins
    /// the toss and the picture disappears off both screens, in the middle of
    /// somebody working on it, with nothing anywhere reporting a loss.
    ///
    /// So the choice is made here instead, on what the values *say* rather
    /// than on who wrote them: **a value with something in it beats one with
    /// nothing in it.** That is a property of the data, so it holds whatever
    /// order the documents merged in and whichever way the actor ids happened
    /// to sort. It stays convergent because `get_all` orders its answers by op
    /// id, which every replica sees identically — two replicas holding the
    /// same conflict make the same choice.
    ///
    /// A write that *saw* the other one is not a conflict at all: it
    /// supersedes it and is the only value here. Closing a photograph, or
    /// resetting a crop, therefore still does exactly what it says.
    fn conflicting<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> Vec<T> {
        let Ok(values) = doc.get_all(&ROOT, key) else {
            return Vec::new();
        };
        values
            .into_iter()
            .filter_map(|(value, _)| match value {
                Value::Scalar(s) => s.to_str().and_then(|raw| serde_json::from_str(raw).ok()),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect01;

    #[test]
    fn a_seeded_document_has_no_image_open() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        assert!(
            PhotosDoc::image(&doc).is_none(),
            "a fresh document must not fabricate a photograph"
        );
        assert!(PhotosDoc::edits(&doc).is_identity());
    }

    #[test]
    fn seeding_twice_does_not_discard_state() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/x/harbour.png", 800, 600);
        PhotosDoc::set_image(&mut doc, Some(&img));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());

        PhotosDoc::seed(&mut doc);
        assert_eq!(PhotosDoc::image(&doc).unwrap().name, "harbour.png");
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
    }

    #[test]
    fn edits_round_trip_through_the_crdt() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let e = Edits {
            crop: Rect01 {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.25,
            },
            rotate_quarters: 3,
        };
        PhotosDoc::set_edits(&mut doc, &e);
        assert_eq!(PhotosDoc::edits(&doc), e);
    }

    #[test]
    fn opening_a_new_image_clears_the_previous_crop() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let img = OpenImage::from_path("/x/other.png", 10, 10);
        PhotosDoc::set_image(&mut doc, Some(&img));
        assert!(
            PhotosDoc::edits(&doc).is_identity(),
            "a newly opened photograph must start untouched"
        );
    }

    /// A copy of `doc` as another machine would hold it, for merging back.
    fn copy(doc: &mut AutoCommit) -> AutoCommit {
        AutoCommit::load(&doc.save()).expect("a document this side just wrote must load")
    }

    /// One replica, with an actor id fixed so the test decides the tie rather
    /// than the random id the run happens to mint.
    fn replica(actor: u8) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from([actor; 16].as_slice()));
        PhotosDoc::seed(&mut doc);
        doc
    }

    /// A participant's editor starting up must not close the photograph
    /// another participant is working on.
    ///
    /// This is data loss between collaborators, and it happened: a second
    /// person's photo editor came up holding nothing, wrote "no photograph"
    /// into the room's document concurrently with the picture that was open in
    /// it, and the picture left both screens mid-edit — the app went from four
    /// operations to none, and the participant that had asked for a crop was
    /// left addressing a window with nothing in it.
    ///
    /// Every way the tie can fall, because none of them is a choice anybody
    /// made. Automerge resolves two concurrent writes to one key by the op's
    /// Lamport stamp — its counter first, the actor id second. The counter is
    /// just how much history that replica's document happens to carry, which
    /// for a real app is however much its toolkit wrote before the app itself
    /// wrote anything; the actor id is random. So the empty replica is run
    /// both behind and ahead of the populated one, under both actor
    /// orderings, and both merge directions are checked because it is one
    /// document afterwards and both participants have to be looking at it.
    #[test]
    fn a_replica_that_opens_empty_never_closes_the_photograph_the_room_holds() {
        for (open_actor, empty_actor) in [(1u8, 2u8), (2u8, 1u8)] {
            for empty_is_ahead in [false, true] {
                run_the_empty_replica_against_the_open_one(open_actor, empty_actor, empty_is_ahead);
            }
        }
    }

    fn run_the_empty_replica_against_the_open_one(
        open_actor: u8,
        empty_actor: u8,
        empty_is_ahead: bool,
    ) {
        {
            // Somebody with a picture open, and part-way through editing it.
            let mut has_picture = replica(open_actor);
            let img = OpenImage::from_path("/var/tmp/qos/workspace/harbour.png", 800, 600);
            PhotosDoc::set_image(&mut has_picture, Some(&img));
            PhotosDoc::set_edits(&mut has_picture, &Edits::default().rotated_right());

            // Somebody else's editor, just started: its own history, nothing
            // in it, and it has never seen the picture.
            let mut just_started = AutoCommit::new();
            just_started.set_actor(automerge::ActorId::from([empty_actor; 16].as_slice()));
            if empty_is_ahead {
                // The history that replica carried before the app wrote a
                // line of its own — a toolkit seeding its containers, a
                // session that had been up longer. Nobody chose it, and
                // without it the empty write loses the Lamport comparison for
                // a reason that has nothing to do with what either document
                // says.
                for n in 0..8i64 {
                    just_started
                        .put(&ROOT, "toolkit-scratch", n)
                        .expect("a scalar put on a fresh document cannot fail");
                }
            }
            PhotosDoc::seed(&mut just_started);

            let clock = if empty_is_ahead { "ahead" } else { "behind" };
            // The room hands each of them the other's whole document.
            for (mut doc, mut incoming, side) in [
                (
                    copy(&mut has_picture),
                    copy(&mut just_started),
                    "the screen with the picture on it",
                ),
                (
                    copy(&mut just_started),
                    copy(&mut has_picture),
                    "the screen that had just started",
                ),
            ] {
                doc.merge(&mut incoming)
                    .expect("two replicas of one app's document must merge");
                assert_eq!(
                    PhotosDoc::image(&doc).map(|i| i.name),
                    Some("harbour.png".to_string()),
                    "actors {open_actor}/{empty_actor}, empty replica {clock}: \
                     an editor starting up with nothing open must not take \
                     the photograph off {side}",
                );
                assert_eq!(
                    PhotosDoc::edits(&doc).rotate_quarters,
                    1,
                    "actors {open_actor}/{empty_actor}, empty replica {clock}: \
                     nor undo the turn somebody had already applied to it, \
                     on {side}",
                );
            }
        }
    }

    /// And the rule is about concurrency, not about refusing to let go.
    ///
    /// Closing a photograph is a write that *saw* the open one, so it
    /// supersedes it rather than competing with it — there is one value under
    /// the key afterwards and it is the close. Without this the fix above
    /// would read as "a photograph can never be closed once any replica has
    /// seen it", which would be a worse defect than the one it replaces.
    #[test]
    fn a_participant_who_closes_the_photograph_closes_it_everywhere() {
        let mut here = replica(1);
        let img = OpenImage::from_path("/var/tmp/qos/workspace/harbour.png", 800, 600);
        PhotosDoc::set_image(&mut here, Some(&img));
        PhotosDoc::set_edits(&mut here, &Edits::default().rotated_right());

        // A collaborator, holding the same document, closes it and resets the
        // turn — both after seeing what was there.
        let mut there = copy(&mut here);
        there.set_actor(automerge::ActorId::from([2u8; 16].as_slice()));
        PhotosDoc::set_image(&mut there, None);

        let mut back = copy(&mut here);
        back.merge(&mut copy(&mut there))
            .expect("the collaborator's document must merge back");
        assert!(
            PhotosDoc::image(&back).is_none(),
            "a close that saw the open supersedes it; the photograph must go",
        );
        assert!(
            PhotosDoc::edits(&back).is_identity(),
            "and opening or closing a photograph resets the edits with it",
        );
    }

    /// The invariant the blob store exists to protect, asserted here rather
    /// than assumed: a document that has an image open is still tiny, because
    /// the pixels are not in it.
    #[test]
    fn the_document_never_carries_the_pixels() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 4000, 3000);
        PhotosDoc::set_image(&mut doc, Some(&img));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let saved = doc.save().len();
        assert!(
            saved < 4096,
            "the document grew to {saved} bytes; an image must never travel inside a change"
        );
    }
}
