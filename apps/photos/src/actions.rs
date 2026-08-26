//! The exported **actions** and the **resource tree** the app publishes.
//!
//! ## What is an action here, and what is not
//!
//! An [`Action`] is a value that deterministically mutates the shared CRDT.
//! Sorting this app's interactions by that rule leaves a short, honest list:
//!
//! - **Actions.** Rotating, cropping, and resetting. Each is a user-authored
//!   decision about the photograph that survives a restart, replicates to a
//!   collaborator, and belongs on the undo stack. Each carries the **absolute**
//!   resulting [`Edits`] value rather than a delta, so [`Action::apply`] is a
//!   pure function of `(payload, document)` — no wall-clock, no RNG, no read of
//!   local view state — and two replicas replaying it land on the identical
//!   document. A rotate authored as "turn by one" would instead depend on how
//!   many turns the *receiving* replica had already seen, which is exactly the
//!   divergence the model exists to prevent.
//!
//! - **Not actions.** Zoom, pan, and which region the viewport happens to be
//!   showing. They are local presentation and never leave the machine.
//!   Copying to the clipboard is not one either, and that is a deliberate
//!   call rather than an omission: an action replays on *every* peer, so a
//!   clipboard write modelled as one would reach into the selection of every
//!   machine in the room the moment one person pressed Copy. It stays an
//!   affordance of the app instance the user is touching.
//!
//! ## Addressing
//!
//! Everything is named by id. The open photograph is a resource whose id is
//! derived from **its content**, and each edit is an [`ActionSpec`] with a
//! stable id (`rotate-right`, `crop`, …). No screen position appears anywhere
//! in this surface: a caller with no display enumerates by id and title, and
//! invokes by id, which is what lets the palette, a remote peer, and a
//! headless agent all drive the same operations.
//!
//! The content rather than the path, because an address is only worth having
//! if it means the same thing to everybody holding it — see [`resource_id`].

use automerge::AutoCommit;

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::PhotosDoc;
use crate::model::{Edits, OpenImage, Rect01};

/// Registry `type_id` for every action this app exports.
pub const PHOTOS_ACTION_TYPE: &str = "photos.action";

/// The resource `kind` an open photograph publishes under.
pub const IMAGE_KIND: &str = "photos.image";

/// Every collaborative operation on the open photograph.
///
/// One variant, carrying the absolute post-edit state. See the module docs for
/// why it is absolute rather than incremental.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotosAction {
    /// Replace the edits in force with exactly this value.
    SetEdits {
        /// Region of the original kept, normalized.
        crop: Rect01,
        /// Clockwise quarter turns, `0..=3`.
        rotate_quarters: u8,
    },
}

impl PhotosAction {
    /// Build the action that lands `edits`.
    pub fn set(edits: &Edits) -> Self {
        Self::SetEdits {
            crop: edits.crop,
            rotate_quarters: edits.rotate_quarters,
        }
    }

    /// The edit state this action produces.
    fn edits(&self) -> Edits {
        match self {
            Self::SetEdits {
                crop,
                rotate_quarters,
            } => Edits {
                // Clamp on the way in: a payload from a peer is untrusted, and
                // a crop rect outside the unit square would otherwise index
                // outside the buffer on the next render.
                crop: crop.clamped(),
                rotate_quarters: rotate_quarters % 4,
            },
        }
    }
}

impl Action for PhotosAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        PhotosDoc::set_edits(doc, &self.edits());
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // The edit is a small mutation of a document this client already holds;
        // there is nothing a host could do better.
        ExecutionLocus::ClientLocal
    }
}

/// Register the decoder so a replayed envelope reconstructs the concrete type.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<PhotosAction>(PHOTOS_ACTION_TYPE);
}

/// Encode into the wire form the palette and the on-screen buttons both
/// dispatch — the one place an action becomes a replayable envelope.
pub fn encode(action: &PhotosAction) -> EncodedAction {
    EncodedAction::encode(PHOTOS_ACTION_TYPE, action).expect("PhotosAction is always serializable")
}

/// Parse the `x,y,w,h` argument a prompted crop collects.
///
/// Accepts four comma-separated numbers in `0..1`. Returns `None` for anything
/// else, which the caller turns into "leave the photograph alone" rather than a
/// panic — this string comes from a human or a remote caller.
pub fn parse_crop_arg(arg: &str) -> Option<Rect01> {
    let parts: Vec<f64> = arg
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let [x, y, w, h] = parts.as_slice() else {
        return None;
    };
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return None;
    }
    if *w <= 0.0 || *h <= 0.0 {
        return None;
    }
    Some(
        Rect01 {
            x: *x,
            y: *y,
            w: *w,
            h: *h,
        }
        .clamped(),
    )
}

/// Publishes the open photograph and the edits it supports.
///
/// Enumeration is a **read**: [`resources`](ResourceProvider::resources) takes
/// the document by shared reference, builds specs from values already in it,
/// and performs no I/O — listing what is available never rotates anything, and
/// never touches the file on disk.
pub struct PhotosResources;

impl ResourceProvider for PhotosResources {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Nothing open: publish nothing. An app with no photograph exports no
        // operations on one.
        let Some(image) = PhotosDoc::image(doc) else {
            return Vec::new();
        };
        let edits = PhotosDoc::edits(doc);
        let (w, h) = image.edited_size(&edits);
        // Minted once and used for the resource and every child, so the parent
        // and its facts cannot drift apart.
        let id = resource_id(&image);

        let spec = |id: &str, label: &str, next: Edits| {
            ActionSpec::immediate(id, label, PHOTOS_ACTION_TYPE, &PhotosAction::set(&next))
                .expect("PhotosAction is always serializable")
        };

        let mut resource = Resource::new(id.clone(), IMAGE_KIND, &image.name)
            .with_action(
                spec("rotate-right", "Rotate right", edits.rotated_right())
                    .with_description("Turn the photograph a quarter turn clockwise"),
            )
            .with_action(
                spec("rotate-left", "Rotate left", edits.rotated_left())
                    .with_description("Turn the photograph a quarter turn anticlockwise"),
            )
            .with_action(
                spec(
                    "crop-square",
                    "Crop to a square",
                    edits.cropped_from_view(square_of(w, h, subject_at(&image))),
                )
                .with_description(
                    "Keep the largest square of what is currently shown, placed \
                     over the detail rather than the middle",
                ),
            )
            .with_action(
                ActionSpec::prompted(
                    "crop",
                    "Crop",
                    "Region to keep, as x,y,w,h between 0 and 1",
                    "0.25,0.25,0.5,0.5",
                    ExecutionLocus::ClientLocal,
                    move |arg| {
                        // An unparseable argument leaves the photograph exactly
                        // as it is; it never crops to nothing.
                        let next = match parse_crop_arg(arg) {
                            Some(rect) => edits.cropped_from_view(rect),
                            None => edits,
                        };
                        encode(&PhotosAction::set(&next))
                    },
                )
                .with_description(
                    "Keep a region of what is currently shown, in normalized coordinates",
                ),
            );

        // Reset only appears when there is something to undo, so the surface
        // never offers an operation that would do nothing.
        if !edits.is_identity() {
            resource = resource.with_action(
                spec("reset", "Reset edits", Edits::default())
                    .with_description("Discard the crop and rotation, returning to the original"),
            );
        }

        // The dimensions ride along in the label of a child resource rather
        // than in a side channel, so a caller that can only read ids and
        // titles still learns the size it is working with.
        resource = resource.with_child(Resource::new(
            format!("{id}#size"),
            "photos.size",
            format!("{w} x {h} pixels"),
        ));

        // Where the picture actually is, so a crop can be chosen for the
        // photograph rather than computed from its rectangle.
        //
        // Without this the only facts about an image are its dimensions, and
        // the one square you can derive from a width and a height is the
        // middle one — correct arithmetic, and the same answer whatever the
        // photograph happens to show. This says which part carries the
        // detail, in the same normalized coordinates the crop argument takes,
        // so the choice is about the subject.
        //
        // Stated as a fact rather than an operation because it is not
        // something anyone can *do* — it is something true about the file,
        // which is exactly what a child resource exporting no actions is for.
        if let Some(subject) = subject_of(&image) {
            resource = resource.with_child(Resource::new(
                format!("{id}#subject"),
                "photos.subject",
                subject,
            ));
        }

        vec![resource]
    }
}

/// Where the detail in a photograph sits, in the crop argument's own
/// coordinates.
///
/// Returns a sentence, because it is read by something that reads sentences,
/// and the numbers are in it so a caller can act on them exactly.
///
/// The measure is deliberately crude: local contrast, summed over a coarse
/// grid. Sky, water and studio backgrounds are flat and score near zero;
/// edges — a mast, a face, a horizon full of boats — score high. It is not
/// object recognition and does not pretend to be. What it has to beat is the
/// alternative, which is knowing only the width and the height, from which
/// the one derivable square is the middle one, whatever the picture shows.
///
/// Reads the picture's bytes rather than the decoded surface: this runs where
/// the resource tree is built, which has the document and not the pixels, and
/// a photograph is read once per picture rather than per frame.
///
/// Through [`crate::doc::bytes_of`], so a session that was handed the content
/// and never had a file for it measures the same photograph everybody else
/// does. Reading the path directly made `crop-square` a different operation
/// depending on who ran it: the participant that staged the file put the
/// square over the detail, and the one holding only the bytes had nothing to
/// measure and centred it. Same address, same picture, two crops.
fn subject_at(image: &OpenImage) -> Option<(f64, f64)> {
    // Once per picture, not once per publish.
    //
    // `resources()` is rebuilt whenever the document changes -- it is a
    // function of the document, deliberately, so nothing here is ever stale.
    // Reading and decoding the photograph inside it made every one of those
    // rebuilds pay for a full PNG decode, and the app stopped answering:
    // an agent that opened this editor watched it arrive with its content
    // and never publish an operation, and gave up after thirty seconds with
    // `the open did not produce what it promised`.
    //
    // Remembered under the same name the resource is addressed by, so two
    // sessions holding one photograph share one answer for it. A failure is
    // remembered too: bytes that cannot be decoded will not start decoding by
    // being tried again every frame.
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashMap<String, Option<(f64, f64)>>>> = OnceLock::new();
    let key = subject_key(image);
    let cache = SEEN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(seen) = cache.lock() {
        if let Some(hit) = seen.get(&key) {
            return *hit;
        }
    }
    let answer = crate::doc::bytes_of(image).as_deref().and_then(measure_subject);
    if let Ok(mut seen) = cache.lock() {
        seen.insert(key, answer);
    }
    answer
}

/// What the measurement is remembered under: the content when the picture
/// names any, and otherwise the path, which is all an older document has.
fn subject_key(image: &OpenImage) -> String {
    image.content.clone().unwrap_or_else(|| image.path.clone())
}

/// The measurement itself, without the remembering. Separated so the cache
/// above has one thing to cache and this has one thing to be right about.
fn measure_subject(bytes: &[u8]) -> Option<(f64, f64)> {
    const GRID: u32 = 24;
    let img = image::load_from_memory(bytes).ok()?.to_luma8();
    let (w, h) = img.dimensions();
    if w < GRID || h < GRID {
        return None;
    }
    let (cw, ch) = (w / GRID, h / GRID);
    let mut best = 0.0f64;
    let mut total = 0.0f64;
    // Centre of mass of contrast, and the busiest cell, in one pass.
    let (mut mx, mut my) = (0.0f64, 0.0f64);
    for gy in 0..GRID {
        for gx in 0..GRID {
            let (x0, y0) = (gx * cw, gy * ch);
            let mut lo = 255u8;
            let mut hi = 0u8;
            // Corners and centre: five reads a cell, not cw*ch of them.
            for (dx, dy) in [(0, 0), (cw - 1, 0), (0, ch - 1), (cw - 1, ch - 1), (cw / 2, ch / 2)] {
                let v = img.get_pixel(x0 + dx, y0 + dy).0[0];
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let contrast = f64::from(hi - lo);
            total += contrast;
            mx += contrast * f64::from(gx);
            my += contrast * f64::from(gy);
            best = best.max(contrast);
        }
    }
    if total <= 0.0 {
        // A flat image has no subject to point at, and saying nothing is
        // better than pointing at the middle and calling it a finding.
        return None;
    }
    let cx = (mx / total + 0.5) / f64::from(GRID);
    let cy = (my / total + 0.5) / f64::from(GRID);
    Some((cx, cy))
}

/// The measurement, in words: what the child resource carries and what the
/// tests below pin. One decode behind it, remembered by content.
fn subject_of(image: &OpenImage) -> Option<String> {
    subject_at(image).map(|(cx, cy)| subject_sentence(cx, cy))
}

/// The measurement, said in words, for the child resource that carries it.
fn subject_sentence(cx: f64, cy: f64) -> String {
    let where_across = if cx < 0.42 {
        "left of centre"
    } else if cx > 0.58 {
        "right of centre"
    } else {
        "centred"
    };
    let where_down = if cy < 0.42 {
        "high"
    } else if cy > 0.58 {
        "low"
    } else {
        "level"
    };
    format!(
        "most of the detail is {where_across} and sits {where_down}, around \
         x={cx:.2} y={cy:.2} of the frame"
    )
}

/// The largest square that fits the picture, slid over the detail.
///
/// Expressed in the same normalized coordinates the prompted crop takes, so
/// the two operations mean the same thing by a rectangle.
///
/// This exists so that "make it square" needs no argument. A square is a
/// property of the *picture*, and the picture is the one thing the agent
/// cannot see: it would have to be handed the pixel dimensions, work out an
/// aspect ratio and write a rect, and every one of those steps is a place for
/// it to pass nothing at all — which is what happened, and the prompted crop
/// is written to leave the photograph untouched rather than guess, so it
/// reported success over an unchanged picture. An operation that takes no
/// argument cannot be invoked without one.
pub fn square_of(w: u32, h: u32, subject: Option<(f64, f64)>) -> Rect01 {
    if w == 0 || h == 0 {
        return Rect01::FULL;
    }
    let (wf, hf) = (f64::from(w), f64::from(h));
    let side = wf.min(hf);
    // The square as a fraction of each axis. On the short axis it is the
    // whole of it, which is what makes this the largest square available.
    let (sw, sh) = (side / wf, side / hf);
    // Centre it on the detail when there is any, on the middle when there is
    // not, then slide it back inside the frame.
    let (cx, cy) = subject.unwrap_or((0.5, 0.5));
    let x = (cx - sw / 2.0).clamp(0.0, 1.0 - sw);
    let y = (cy - sh / 2.0).clamp(0.0, 1.0 - sh);
    Rect01 { x, y, w: sw, h: sh }
}

/// The address of a photograph: what it is, not where a machine put it.
///
/// # Why the path was the wrong name
///
/// This used to be `format!("{IMAGE_KIND}:{path}")`, and the path is true on
/// exactly one machine. Content handed to a room is staged by each session
/// under its own runtime directory, so one photograph had one address per
/// participant — `…/qlva…/qos-opened/<hash>.png` here, `…/qlvx…/` there,
/// `…/qlvq…/` in the third. An address is resolved by string equality
/// ([`qos_actions::door::resolve`]), so an operation invoked on one of them
/// reached exactly one session: the one whose path had won. An agent cropped
/// the editor in `qlva…` while the person watching the editor in `qlvx…` saw
/// their picture sit there unchanged. The run before, the same invocation
/// worked — because that time the id happened to name the watcher's copy.
///
/// So the name is the content's, through the hash the document already
/// carries beside the path (`OpenImage::content`). Content is the same word in
/// every session, which is the property an address has to have before anyone
/// can act through one: everybody says the same thing about the picture, and
/// the operation lands on it wherever it is open.
///
/// # Why the file's name is not in it
///
/// Because a name is a property of whoever wrote it down, exactly as the path
/// was. A participant who opened `harbour.png` off their own disk and one who
/// was handed the same bytes staged as `<hash>.png` would be back to two
/// addresses for one picture. What a person reads is the resource's **label**,
/// which is the file's own name and travels beside the id everywhere one is
/// rendered — the palette row, `qos_actions::FlatAction::resource_label`, the
/// agent's survey line, the chip on a remark.
///
/// # A photograph with no content hash
///
/// Named by its path, as it always was. A document written before the hash
/// existed parses with `content: None`, and the path is the only thing such a
/// picture has: as good an address as it ever was in the one session the path
/// is true of, and better than publishing nothing. Nothing is migrated,
/// because there is nothing stored to migrate — this tree is a pure function
/// of the document, rebuilt every frame, so a picture gains its content
/// address the moment its bytes reach the store.
pub fn resource_id(image: &OpenImage) -> String {
    match &image.content {
        Some(hash) => content_address(hash),
        None => format!("{IMAGE_KIND}:{}", image.path),
    }
}

/// The address the content named by `hash` is published under.
///
/// Split out so a caller holding the bytes but no document — the offscreen
/// renderer posing a scenario, a host staging content for an app — spells the
/// address the one way, rather than assembling a second spelling of it that
/// can drift from the one the app answers to.
pub fn content_address(hash: &str) -> String {
    format!("{IMAGE_KIND}:{hash}")
}

/// Length of a blake3 hash written as hex, which is what a content-named id
/// carries after its kind.
const CONTENT_HEX_LEN: usize = 64;

/// The content a resource id names, if it names content.
///
/// The inverse of [`resource_id`] for the ids minted from a hash, and `None`
/// for the path-shaped id a document written before content hashes still
/// produces. This is what keeps a portable address useful rather than merely
/// identical everywhere: a session that was handed the picture and never had a
/// file for it goes from the address to the bytes through
/// `ui::blobstore::get`, the same route [`crate::doc::bytes_of`] takes for the
/// photograph it is drawing.
pub fn content_of(id: &str) -> Option<&str> {
    let tail = id.strip_prefix(IMAGE_KIND)?.strip_prefix(':')?;
    let is_content = tail.len() == CONTENT_HEX_LEN
        && tail
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_content.then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OpenImage;
    use qos_actions::flatten;

    /// A 240x160 image, flat grey, with a patch of hard checkerboard — all the
    /// contrast there is — centred on `(fx, fy)` of the frame.
    fn image_with_detail_at(dir: &std::path::Path, name: &str, fx: f64, fy: f64) -> OpenImage {
        let (w, h) = (240u32, 160u32);
        let mut img = image::GrayImage::from_pixel(w, h, image::Luma([128u8]));
        let (cx, cy) = ((fx * f64::from(w)) as u32, (fy * f64::from(h)) as u32);
        for y in cy.saturating_sub(24)..(cy + 24).min(h) {
            for x in cx.saturating_sub(24)..(cx + 24).min(w) {
                let v = if (x / 2 + y / 2) % 2 == 0 { 0u8 } else { 255u8 };
                img.put_pixel(x, y, image::Luma([v]));
            }
        }
        let path = dir.join(name);
        img.save(&path).expect("the test writes a readable png");
        OpenImage::from_path(&path.to_string_lossy(), w, h)
    }

    /// The fact has to follow the photograph, not its rectangle.
    ///
    /// Three pictures of identical size whose detail sits in different places.
    /// If the sentence is the same for all three, the measure is reading the
    /// frame rather than the image, and the agent choosing a crop from it
    /// would put the square in the same place whatever it was given — which
    /// is exactly the behaviour this fact exists to replace.
    #[test]
    fn where_the_detail_is_follows_the_picture() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let left = image_with_detail_at(dir.path(), "left.png", 0.20, 0.5);
        let right = image_with_detail_at(dir.path(), "right.png", 0.80, 0.5);
        let high = image_with_detail_at(dir.path(), "high.png", 0.5, 0.18);

        let l = subject_of(&left).expect("a picture with detail has a subject");
        let r = subject_of(&right).expect("a picture with detail has a subject");
        let t = subject_of(&high).expect("a picture with detail has a subject");

        assert!(l.contains("left of centre"), "left-weighted picture said: {l}");
        assert!(r.contains("right of centre"), "right-weighted picture said: {r}");
        assert!(t.contains("high"), "top-weighted picture said: {t}");
        assert_ne!(l, r, "two different pictures must not produce one fact");
    }

    /// Nothing to point at is said by saying nothing.
    ///
    /// A flat image has no subject, and naming the middle of it would be a
    /// finding invented to fill a field — the failure this whole fact is meant
    /// to avoid.
    #[test]
    fn a_flat_picture_has_no_subject() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("flat.png");
        image::GrayImage::from_pixel(240, 160, image::Luma([128u8]))
            .save(&path)
            .expect("the test writes a readable png");
        let flat = OpenImage::from_path(&path.to_string_lossy(), 240, 160);
        assert_eq!(subject_of(&flat), None);
    }

    /// A picture is measured from its bytes, so a session that was handed the
    /// content and never had a file for it reads the same photograph.
    ///
    /// Without this `crop-square` is two different operations under one
    /// address: the session holding the file puts the square over the detail,
    /// and the one holding only the bytes centres it.
    #[test]
    fn the_subject_is_found_from_content_when_there_is_no_file_to_read() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let staged = image_with_detail_at(dir.path(), "left.png", 0.20, 0.5);
        let bytes = std::fs::read(&staged.path).expect("the fixture wrote a file");
        let hash = ui::blobstore::put(&bytes);

        // The same picture, as a participant who was handed the content holds
        // it: a path that is true on somebody else's machine, and the hash.
        let handed = OpenImage {
            path: "/run/user/1000/qlvx-another-session/qos-opened/left.png".to_string(),
            content: Some(hash),
            ..staged.clone()
        };
        assert!(
            std::fs::read(&handed.path).is_err(),
            "the stand-in for somebody else's path is readable here, so it \
             is not standing in for one"
        );
        assert_eq!(
            subject_at(&handed),
            subject_at(&staged),
            "one photograph measured two ways, so `crop-square` is two \
             operations depending on who invokes it"
        );
    }

    fn doc_with_image() -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&img));
        doc
    }

    /// The address of a photograph that names its content is that content.
    ///
    /// Asserted on the published tree rather than on `resource_id` alone, so
    /// it is the id a caller actually resolves against.
    #[test]
    fn a_photograph_that_names_its_content_is_addressed_by_it() {
        const HASH: &str =
            "623fdbf0c19749ddd1116f8c68b561ad648e3023ca4f304a732bf7b06d1ffb82";
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/run/user/1000/qlva3099960/qos-opened/x.png", 800, 400)
            .with_content(HASH);
        PhotosDoc::set_image(&mut doc, Some(&img));

        let res = PhotosResources.resources(&doc);
        assert_eq!(res[0].id.0, format!("photos.image:{HASH}"));
        assert_eq!(
            content_of(&res[0].id.0),
            Some(HASH),
            "the address does not lead back to the content it names"
        );
        // And the file's name is still what a person reads.
        assert_eq!(res[0].label, "x.png");
    }

    /// The path-shaped id an older document produces is not mistaken for a
    /// content address. A caller that got one has to fall back to the path,
    /// and would otherwise ask the content store for a directory name.
    #[test]
    fn a_path_shaped_address_names_no_content() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        assert_eq!(content_of(&res[0].id.0), None);
        assert_eq!(content_of("photos.image:"), None);
        assert_eq!(content_of("files.entry:/x"), None);
        // Right length, wrong alphabet: hex is what a hash is written in.
        assert_eq!(content_of(&format!("photos.image:{}", "z".repeat(64))), None);
    }

    #[test]
    fn nothing_open_publishes_no_resources() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        assert!(PhotosResources.resources(&doc).is_empty());
    }

    #[test]
    fn the_open_photograph_is_published_with_its_name_and_size() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].kind, IMAGE_KIND);
        assert_eq!(res[0].label, "harbour.png");
        assert_eq!(res[0].id.0, "photos.image:/var/tmp/qos/home/quinten/harbour.png");
        // The size is discoverable from titles alone.
        assert_eq!(res[0].children.len(), 1);
        assert_eq!(res[0].children[0].label, "800 x 400 pixels");
    }

    #[test]
    fn every_action_is_addressable_by_a_stable_id() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let ids: Vec<&str> = res[0].actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["rotate-right", "rotate-left", "crop-square", "crop"]
        );
    }

    #[test]
    fn reset_appears_only_once_there_is_an_edit_to_discard() {
        let mut doc = doc_with_image();
        let before = PhotosResources.resources(&doc);
        assert!(!before[0].actions.iter().any(|a| a.id == "reset"));

        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let after = PhotosResources.resources(&doc);
        assert!(after[0].actions.iter().any(|a| a.id == "reset"));
    }

    /// Enumerating must be a pure read. If listing what is on offer could
    /// mutate the document, a palette opening would silently edit the user's
    /// photograph.
    #[test]
    fn enumeration_fires_no_side_effects() {
        let mut doc = doc_with_image();
        let before_heads = doc.get_heads();
        let before_bytes = doc.save();

        let res = PhotosResources.resources(&doc);
        let flat = flatten(&res);
        // Touch every field a caller would read, including building the label
        // of each row.
        for f in &flat {
            let _ = (&f.key, &f.resource_label, &f.action.label, f.action.locus);
        }
        assert!(!flat.is_empty(), "there was nothing to enumerate");

        assert_eq!(doc.get_heads(), before_heads, "enumeration moved the document");
        assert_eq!(doc.save(), before_bytes, "enumeration changed the document");
    }

    #[test]
    fn applying_rotate_right_advances_the_document_by_one_quarter() {
        let mut doc = doc_with_image();
        let action = PhotosAction::set(&PhotosDoc::edits(&doc).rotated_right());
        action.apply(&mut doc).unwrap();
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
    }

    /// The determinism contract: the same envelope applied to two replicas
    /// that started equal leaves them equal.
    #[test]
    fn the_same_action_replays_to_the_same_state() {
        let mut a = doc_with_image();
        let mut b = doc_with_image();
        let action = PhotosAction::SetEdits {
            crop: Rect01 {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.5,
            },
            rotate_quarters: 3,
        };
        action.apply(&mut a).unwrap();
        action.apply(&mut b).unwrap();
        assert_eq!(PhotosDoc::edits(&a), PhotosDoc::edits(&b));
    }

    #[test]
    fn an_action_round_trips_through_the_registry() {
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let original = PhotosAction::set(&Edits::default().rotated_left());
        let decoded = registry.decode(&encode(&original)).expect("decode");
        let mut doc = doc_with_image();
        decoded.apply(&mut doc).unwrap();
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 3);
    }

    #[test]
    fn a_crop_payload_from_a_peer_is_clamped_before_it_reaches_the_document() {
        let mut doc = doc_with_image();
        PhotosAction::SetEdits {
            crop: Rect01 {
                x: -2.0,
                y: 0.5,
                w: 9.0,
                h: 9.0,
            },
            rotate_quarters: 250,
        }
        .apply(&mut doc)
        .unwrap();
        let e = PhotosDoc::edits(&doc);
        assert!(e.crop.x >= 0.0 && e.crop.y >= 0.0);
        assert!(e.crop.x + e.crop.w <= 1.0 + 1e-9, "{:?}", e.crop);
        assert!(e.rotate_quarters < 4);
    }

    #[test]
    fn crop_arguments_parse_and_reject() {
        let r = parse_crop_arg("0.25, 0.25, 0.5, 0.5").expect("four numbers");
        assert!((r.w - 0.5).abs() < 1e-9);
        assert!(parse_crop_arg("").is_none());
        assert!(parse_crop_arg("0.1,0.2,0.3").is_none(), "three is not a rect");
        assert!(parse_crop_arg("a,b,c,d").is_none());
        assert!(parse_crop_arg("0,0,0,0").is_none(), "an empty crop is refused");
    }

    /// A prompted crop that cannot be parsed must leave the photograph alone
    /// rather than crop it to nothing.
    #[test]
    fn an_unparseable_crop_argument_is_a_no_op() {
        let mut doc = doc_with_image();
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let res = PhotosResources.resources(&doc);
        let crop = res[0]
            .actions
            .iter()
            .find(|a| a.id == "crop")
            .expect("crop is published");
        let qos_actions::ActionInput::Prompted { build, .. } = &crop.input else {
            panic!("crop collects an argument");
        };
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry.decode(&build("nonsense")).unwrap().apply(&mut doc).unwrap();
        let e = PhotosDoc::edits(&doc);
        assert!(e.crop.is_full(), "a bad argument cropped the photograph: {:?}", e.crop);
        assert_eq!(e.rotate_quarters, 1, "and it must not undo the rotation either");
    }

    #[test]
    fn a_prompted_crop_composes_onto_the_edits_in_force() {
        let mut doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let crop = res[0].actions.iter().find(|a| a.id == "crop").unwrap();
        let qos_actions::ActionInput::Prompted { build, .. } = &crop.input else {
            panic!("crop collects an argument");
        };
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
            .decode(&build("0,0,0.5,0.5"))
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let e = PhotosDoc::edits(&doc);
        assert!((e.crop.w - 0.5).abs() < 1e-9, "{:?}", e.crop);
        assert!((e.crop.h - 0.5).abs() < 1e-9, "{:?}", e.crop);
    }
}
