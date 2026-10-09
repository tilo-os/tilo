//! The editor's **project file** — what a user saves, copies, and hands to
//! someone else.
//!
//! Until this existed the editor had exactly one project: an implicit
//! autosaved document keyed by the app's package id
//! ([`ui::persist::open`]). Nothing was ever lost, but a user could not
//! have two projects, could not copy one, and could not see where their
//! work lived. This module is the file behind Save / Save As / Open.
//!
//! ## The payload is the CRDT document
//!
//! A project file is a header followed by a serialized Automerge document
//! carrying exactly the maps [`crate::doc`] defines — `project`, `media`,
//! `tracks`, `clips`. That choice is not incidental:
//!
//!   * **It round-trips losslessly.** The document shape *is* the editor's
//!     storage shape, so saving is not a lossy projection into some other
//!     schema that then has to be kept in step.
//!   * **It stays collaborative and undoable.** What comes back out of a
//!     file is a document, merges like a document, and is applied through
//!     the ordinary action pipeline — so opening a project is one undo
//!     boundary and replicates to a peer like any other edit.
//!   * **It absorbs model growth for free.** The project's reader,
//!     [`VideoDoc::load_project`](crate::doc::VideoDoc::load_project), reads
//!     every field with an explicit default for "this document predates the
//!     field". The model grew
//!     repeatedly in a single day — `solo`, `transition_duration`, `text`,
//!     `Transform`, `speed`, keyframes, markers — and every one of those
//!     additions is already backward-compatible at the document layer. The
//!     file inherits that for nothing.
//!
//! ## What is deliberately *not* in the file
//!
//! **Media bytes.** A clip references its source ([`crate::media`]); the
//! bytes live on the FUSE workspace or on local disk. Copying gigabytes of
//! footage into a project file would defeat the whole projection/hydration
//! design — and a second collaborator resolves the same reference against
//! *their* mount. So a project stays small, and a moved or not-yet-hydrated
//! source is a normal state the app discloses rather than an error (see
//! [`missing_sources`]).
//!
//! **Session state.** Recent-file lists, view state, the playhead: none of
//! it is project content, and a recents list in particular must not ride
//! along inside a file handed to somebody else. The writer builds the
//! document from the project model, so only project content can reach it.
//!
//! ## Versioning
//!
//! ```text
//! offset  size  field
//! 0       9     magic          b"TiloVEPRJ"
//! 9       2     version        u16 LE — what wrote this file
//! 11      2     min_reader     u16 LE — the oldest reader that may open it
//! 13      4     payload_len    u32 LE — length of the Automerge document
//! 17      N     payload        Automerge document bytes
//! ```
//!
//! Two version numbers, because the two questions are different. `version`
//! records what wrote the file, for diagnostics. `min_reader` is the
//! compatibility contract: a writer that only *adds* fields — the ordinary
//! case, since the document layer defaults absent fields — leaves it at 1,
//! so a build from today still opens a file from a much later one. A writer
//! that changes the *structure*, renaming a root map or rekeying clips,
//! raises it, and older builds then refuse the file instead of quietly
//! loading half a project.
//!
//! Refusing is the point. [`ui::persist`] already learned this for the
//! autosave store: a document it cannot understand is left alone rather
//! than opened empty and saved back over the top. A file this module cannot
//! understand gets [`ProjectFileError::TooNew`], carrying both versions, and
//! the caller says so on screen.

use std::path::Path;

use ui::tilo_resource::Replica;

use crate::model::Project;
use crate::resource::VideoProject;

/// First nine bytes of every project file.
pub const MAGIC: &[u8; 9] = b"TiloVEPRJ";

/// The format version this build writes.
pub const FORMAT_VERSION: u16 = 1;

/// The oldest reader this build's files require. Raised only by a change an
/// older reader would *misread* — never by one it would merely not notice.
/// See the module docs.
pub const MIN_READER_VERSION: u16 = 1;

/// Bytes before the Automerge payload.
pub const HEADER_LEN: usize = 17;

/// The extension a project is saved under.
pub const EXTENSION: &str = "qvp";

/// How a project file is described in a file chooser and in prose.
pub const KIND_LABEL: &str = "Video project";

/// Everything that can go wrong opening or writing a project file, in terms
/// a person can act on rather than an `io::Error`'s terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFileError {
    /// The bytes are not a project file at all — no magic. Usually means the
    /// user picked the wrong file, so say that rather than "parse error".
    NotAProject,
    /// The header says the payload is longer than the file is. A copy or a
    /// write that was interrupted looks exactly like this, and loading the
    /// prefix would produce a *plausible* half-project — the worst outcome.
    Truncated { declared: usize, present: usize },
    /// Written by a newer editor whose layout this build would misread.
    TooNew {
        file_min_reader: u16,
        this_reader: u16,
    },
    /// The header parsed but Automerge would not load the payload.
    Corrupt(String),
    /// The file could not be read or written.
    Io(String),
}

impl std::fmt::Display for ProjectFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectFileError::NotAProject => write!(f, "Not a video project file"),
            ProjectFileError::Truncated { declared, present } => {
                write!(
                    f,
                    "Project file is incomplete ({present} of {declared} bytes)"
                )
            }
            ProjectFileError::TooNew {
                file_min_reader, ..
            } => {
                write!(
                    f,
                    "Project needs a newer video editor (format {file_min_reader})"
                )
            }
            ProjectFileError::Corrupt(e) => write!(f, "Project file is damaged: {e}"),
            ProjectFileError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProjectFileError {}

/// Serialize `project` into project-file bytes.
///
/// The document is built fresh from the model rather than cloned from the
/// live session, which is what keeps session-only root keys (recents, view
/// state) and the whole local edit history — including the paths of sources
/// since deleted — out of a file the user may hand to someone else.
///
/// # Errors
///
/// [`ProjectFileError::Corrupt`] when the project does not apply to a fresh
/// document.
pub fn encode(project: &Project) -> Result<Vec<u8>, ProjectFileError> {
    let mut replica = Replica::<VideoProject>::new();
    replica
        .apply_action(&crate::actions::load_project_action(project))
        .map_err(|e| ProjectFileError::Corrupt(e.to_string()))?;
    let payload = replica.save();

    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&MIN_READER_VERSION.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// The fixed header's fields: `(version, min_reader, declared payload length)`.
fn header_fields(bytes: &[u8]) -> Result<(u16, u16, u32), ProjectFileError> {
    let Some([magic @ .., v0, v1, r0, r1, l0, l1, l2, l3]) = bytes.get(..HEADER_LEN) else {
        return Err(ProjectFileError::NotAProject);
    };
    if magic != MAGIC.as_slice() {
        return Err(ProjectFileError::NotAProject);
    }
    Ok((
        u16::from_le_bytes([*v0, *v1]),
        u16::from_le_bytes([*r0, *r1]),
        u32::from_le_bytes([*l0, *l1, *l2, *l3]),
    ))
}

/// The `version` / `min_reader` a file declares, without loading it.
///
/// # Errors
///
/// [`ProjectFileError::NotAProject`] when `bytes` do not open with a whole
/// project-file header.
pub fn header_versions(bytes: &[u8]) -> Result<(u16, u16), ProjectFileError> {
    let (version, min_reader, _) = header_fields(bytes)?;
    Ok((version, min_reader))
}

/// Read project-file `bytes` back into the model.
///
/// # Errors
///
/// [`ProjectFileError::NotAProject`] without a project header,
/// [`ProjectFileError::TooNew`] when the file needs a newer reader,
/// [`ProjectFileError::Truncated`] when the payload is shorter than the header
/// declares, and [`ProjectFileError::Corrupt`] when the payload does not load.
pub fn decode(bytes: &[u8]) -> Result<Project, ProjectFileError> {
    let (_version, min_reader, declared) = header_fields(bytes)?;
    if min_reader > MIN_READER_VERSION {
        return Err(ProjectFileError::TooNew {
            file_min_reader: min_reader,
            this_reader: MIN_READER_VERSION,
        });
    }
    let declared = declared as usize;
    let present = bytes.len() - HEADER_LEN;
    let Some(payload) = bytes.get(HEADER_LEN..HEADER_LEN + declared) else {
        return Err(ProjectFileError::Truncated { declared, present });
    };
    Replica::<VideoProject>::load(payload)
        .and_then(|replica| replica.state())
        .map_err(|e| ProjectFileError::Corrupt(e.to_string()))
}

/// Write `project` to `path`.
///
/// Deliberately a plain write rather than the usual write-temp-then-rename:
/// under a strict sandbox the portal grant is for *the file the user chose*,
/// not for creating siblings next to it, so a temp file in the same
/// directory is a write the app has no right to make. The length in the
/// header is what covers the risk that buys — a write cut short is detected
/// as [`ProjectFileError::Truncated`] on the way back in, instead of loading
/// as a convincing half-project.
///
/// # Errors
///
/// [`ProjectFileError::Io`] when the file cannot be written.
pub fn save(path: &Path, project: &Project) -> Result<(), ProjectFileError> {
    crate::files::save(path, &encode(project)?)
        .map_err(|e| ProjectFileError::Io(format!("Could not save: {e}")))
}

/// Read a project from `path`.
///
/// # Errors
///
/// [`ProjectFileError::Io`] when the file cannot be read, otherwise whatever
/// [`decode`] reports for its contents.
pub fn load(path: &Path) -> Result<Project, ProjectFileError> {
    let bytes = crate::files::read(path)
        .map_err(|e| ProjectFileError::Io(format!("Could not open: {e}")))?;
    decode(&bytes)
}

/// Give `name` the project extension, leaving one it already has alone.
///
/// Seeds the Save dialog's filename field, so a user is never made to
/// remember the extension — and never ends up with `Holiday.qvp.qvp`.
pub fn with_extension(name: &str) -> String {
    let stem = name.trim();
    let stem = if stem.is_empty() { "Untitled" } else { stem };
    if stem
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(EXTENSION))
    {
        return stem.to_string();
    }
    format!("{stem}.{EXTENSION}")
}

/// The project name a file at `path` implies: its stem, so opening
/// `Holiday.qvp` on a project still called "Untitled" is not a mystery.
pub fn name_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
}

/// The sources in `project` whose bytes are not readable on this device
/// right now, split into the two states the editor already has words for.
///
/// A project file carries references, so this is a normal thing to find
/// after opening one somewhere else — or after the footage moved. The two
/// halves are genuinely different and the editor must not blur them:
/// [`crate::media::Hydration::Projected`] means the bytes are simply not
/// here yet (fetching them, or finding the file again, fixes it), while a
/// source that resolves to a real file nothing can decode will not be fixed
/// by fetching it again.
///
/// Text titles are excluded: they have no bytes to be missing.
pub fn missing_sources(project: &Project) -> Vec<(String, crate::media::Hydration)> {
    project
        .media
        .iter()
        .filter(|m| m.kind != crate::model::MediaKind::Text)
        .filter_map(|m| {
            let src = crate::media::resolve(&m.path);
            (!src.is_decodable()).then(|| (m.name.clone(), src.hydration))
        })
        .collect()
}

/// The recently-opened project list.
///
/// **Deliberately not in the CRDT.** The editor's document is the
/// collaborative one: anything written into it replicates to whoever is
/// sharing the project, and a list of the paths on this machine is the last
/// thing that should. It is also not project content — it belongs to this
/// device, and it must survive an Open, which replaces every project map in
/// the document. So it lives beside the app's own state, in the same
/// per-package directory the autosave document uses.
pub mod recents {
    use std::path::{Path, PathBuf};

    /// How many projects are remembered. Long enough to reach back past a
    /// detour, short enough that the menu stays a menu.
    pub const MAX: usize = 8;

    const FILE: &str = "recent-projects.json";

    /// This app's per-package state directory.
    ///
    /// Mirrors `cli::standalone_state_dir` — `$XDG_DATA_HOME/<package-id>`,
    /// falling back to `~/.local/share/<package-id>` — which is where
    /// [`ui::persist`] already puts the autosave document. Replicated here
    /// rather than depended on, because it is four lines and the alternative
    /// is a new crate dependency for one path join; if that resolver moves,
    /// this is the sibling that has to move with it.
    fn dir() -> Option<PathBuf> {
        let id = std::env::var("TILO_PACKAGE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "so.tilo.video-editor".to_string());
        // Reject an id that is not a plain directory name, exactly as the
        // store does: an id carrying a separator would escape the data home.
        if id.contains('/') || id.contains('\\') || id == "." || id == ".." {
            return None;
        }
        let base = tilo_paths::xdg::data_home()?;
        Some(base.join(id))
    }

    fn path() -> Option<PathBuf> {
        dir().map(|d| d.join(FILE))
    }

    /// The remembered projects, most recent first.
    ///
    /// A path whose file is no longer there is dropped on the way out rather
    /// than offered: a menu entry that cannot open anything is worse than a
    /// shorter menu. Nothing is written back here — a temporarily unmounted
    /// volume must not permanently forget the project on it.
    pub fn load() -> Vec<PathBuf> {
        let Some(p) = path() else { return Vec::new() };
        let Some(text) = crate::files::read(&p)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            return Vec::new();
        };
        let all: Vec<PathBuf> = serde_json::from_str(&text).unwrap_or_default();
        all.into_iter().filter(|p| p.is_file()).take(MAX).collect()
    }

    /// Put `entry` at the front, de-duplicated, capped at [`MAX`].
    ///
    /// Pure, so the ordering rule is testable without touching a disk.
    pub fn promote(list: &[PathBuf], entry: &Path) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(MAX);
        out.push(entry.to_path_buf());
        for p in list {
            if p != entry && out.len() < MAX {
                out.push(p.clone());
            }
        }
        out
    }

    /// Record that `entry` was just opened or saved, and return the new list.
    ///
    /// Best-effort: a state directory that cannot be written costs the user a
    /// recents entry, never their work, so a failure is silent here and the
    /// in-memory list is still correct for this session.
    pub fn remember(list: &[PathBuf], entry: &Path) -> Vec<PathBuf> {
        let next = promote(list, entry);
        if let Some(p) = path()
            && let Ok(text) = serde_json::to_string(&next)
        {
            let _ = crate::files::write(&p, text.as_bytes());
        }
        next
    }

    /// How a recent entry is labelled in the menu: the file's stem, which is
    /// what the user named the project.
    pub fn label(entry: &Path) -> String {
        entry.file_stem().map_or_else(
            || entry.to_string_lossy().into_owned(),
            |s| s.to_string_lossy().into_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, MediaItem, MediaKind, Track, TrackKind};
    use uuid::Uuid;

    /// A project exercising the fields that were added at different times,
    /// so a round-trip failure shows up as a lost feature, not a lost byte.
    fn rich_project() -> Project {
        let mut p = Project::new("Coastline");
        p.fps = 24.0;
        p.width = 3840;
        p.height = 2160;
        p.add_marker(12.5, "Act two", 3);

        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "Drone.mov".into(),
            kind: MediaKind::Video,
            duration: 42.0,
            width: 3840,
            height: 2160,
            fps: 24.0,
            palette: 2,
            path: "media/Drone.mov".into(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            solo: true,
            ..Default::default()
        });
        let mut clip = Clip {
            id: Uuid::new_v4(),
            track: tid,
            media: mid,
            start: 3.0,
            in_point: 1.0,
            out_point: 9.0,
            transition_duration: 1.25,
            ..Default::default()
        };
        clip.transform = crate::model::Transform::pip(0.3, (1.0, -1.0), 0.05);
        clip.set_speed(2.0, true);
        clip.set_key_at(
            crate::param::Param::Opacity,
            0.0,
            0.25,
            crate::param::Interp::Ease,
        );
        p.clips.push(clip);
        p
    }

    #[test]
    fn project_files_round_trip_whole_with_their_ids() {
        // Case `a_saved_project_comes_back_whole`.
        {
            // Not "it decoded" — every field that has ever been added to the
            // model, read back and compared. A format that loses `solo` or the
            // keyframe curve is a format that silently eats a user's work.
            let p = rich_project();
            let back = decode(&encode(&p).expect("the project encodes")).expect("round-trips");

            assert_eq!(back.name, "Coastline");
            assert_eq!((back.fps, back.width, back.height), (24.0, 3840, 2160));
            assert_eq!(back.markers.len(), 1);
            assert_eq!(back.markers[0].name, "Act two");
            assert!((back.markers[0].t - 12.5).abs() < 1e-9);

            assert_eq!(back.media.len(), 1);
            assert_eq!(back.media[0].name, "Drone.mov");
            assert_eq!(back.media[0].path, "media/Drone.mov");
            assert_eq!(back.media[0].width, 3840);

            assert_eq!(back.tracks.len(), 1);
            assert!(back.tracks[0].solo, "solo survives the file");

            assert_eq!(back.clips.len(), 1);
            let c = &back.clips[0];
            assert!((c.start - 3.0).abs() < 1e-9);
            assert!((c.in_point - 1.0).abs() < 1e-9);
            assert!((c.out_point - 9.0).abs() < 1e-9);
            assert!((c.transition_duration - 1.25).abs() < 1e-9);
            assert_eq!(c.transform, p.clips[0].transform, "geometry survives");
            assert_eq!(c.speed, p.clips[0].speed, "speed + reverse survive");
            assert_eq!(c.anim, p.clips[0].anim, "keyframe curves survive");
        }
        // Case `the_ids_survive_so_a_reopened_project_is_the_same_project`.
        {
            // If ids were re-minted on load, a collaborator opening the same file
            // would produce a *disjoint* project that merges into duplicates
            // rather than converging, and every clip would lose its source.
            let p = rich_project();
            let back = decode(&encode(&p).expect("the project encodes")).expect("round-trips");
            assert_eq!(back.media[0].id, p.media[0].id);
            assert_eq!(back.tracks[0].id, p.tracks[0].id);
            assert_eq!(back.clips[0].id, p.clips[0].id);
            assert_eq!(back.clips[0].track, p.tracks[0].id);
            assert_eq!(back.clips[0].media, p.media[0].id);
        }
        // Case `saving_and_loading_a_real_file_round_trips`.
        {
            let path = std::env::temp_dir()
                .join(format!("tilo-ve-proj-{}.{EXTENSION}", std::process::id()));
            let p = rich_project();
            save(&path, &p).expect("saves");
            let back = load(&path).expect("loads");
            assert_eq!(back.name, p.name);
            assert_eq!(back.clips.len(), 1);
            std::fs::remove_file(&path).ok();
        }
        // Case `a_newer_writer_that_only_added_fields_still_opens_here`.
        {
            // The other half of the contract, and the reason there are two
            // version numbers. A future build that adds an optional field bumps
            // `version` and leaves `min_reader` alone; this build must open that
            // file, because the document layer already defaults what it cannot
            // see. Without this, every field addition would orphan old files.
            let mut bytes = encode(&rich_project()).expect("the project encodes");
            bytes[9..11].copy_from_slice(&(FORMAT_VERSION + 7).to_le_bytes());
            let back = decode(&bytes).expect("a compatible newer file still opens");
            assert_eq!(back.clips.len(), 1);
            assert_eq!(
                header_versions(&bytes).unwrap(),
                (FORMAT_VERSION + 7, MIN_READER_VERSION)
            );
        }
    }

    #[test]
    fn project_header_refuses_foreign_truncated_newer_or_damaged_files() {
        // Case `the_header_is_the_documented_layout`.
        {
            let bytes = encode(&Project::new("x")).expect("the project encodes");
            assert_eq!(&bytes[..MAGIC.len()], b"TiloVEPRJ");
            assert_eq!(u16::from_le_bytes([bytes[9], bytes[10]]), FORMAT_VERSION);
            assert_eq!(
                u16::from_le_bytes([bytes[11], bytes[12]]),
                MIN_READER_VERSION
            );
            let declared =
                u32::from_le_bytes([bytes[13], bytes[14], bytes[15], bytes[16]]) as usize;
            assert_eq!(
                declared,
                bytes.len() - HEADER_LEN,
                "the declared payload length is the actual payload length",
            );
        }
        // Case `something_that_is_not_a_project_is_named_as_such`.
        {
            // The overwhelmingly likely cause is picking the wrong file, so the
            // error has to say that and not "parse error at byte 0".
            assert_eq!(
                decode(b"not a project at all").unwrap_err(),
                ProjectFileError::NotAProject
            );
            assert_eq!(decode(b"").unwrap_err(), ProjectFileError::NotAProject);
            // A file shorter than the header is not a readable project either.
            assert_eq!(
                header_versions(&[0u8; 4]).unwrap_err(),
                ProjectFileError::NotAProject
            );
        }
        // Case `every_short_prefix_of_a_header_is_not_a_project`.
        {
            let bytes = encode(&Project::new("t")).expect("the project encodes");
            for n in 0..HEADER_LEN {
                assert_eq!(
                    header_versions(&bytes[..n]),
                    Err(ProjectFileError::NotAProject),
                    "{n} bytes"
                );
            }
            assert_eq!(
                header_versions(&bytes[..HEADER_LEN]),
                Ok((FORMAT_VERSION, MIN_READER_VERSION))
            );
        }
        // Case `a_truncated_file_is_refused_rather_than_half_loaded`.
        {
            // This is the case the length field exists for. Automerge can often
            // load a prefix of a document and hand back *some* of the clips —
            // exactly the plausible-but-wrong result that must never reach a user
            // who believes they opened their project.
            let full = encode(&rich_project()).expect("the project encodes");
            let cut = &full[..full.len() - 8];
            match decode(cut) {
                Err(ProjectFileError::Truncated { declared, present }) => {
                    assert_eq!(declared, full.len() - HEADER_LEN);
                    assert_eq!(present, declared - 8);
                }
                other => panic!("a short file must be refused, got {other:?}"),
            }
        }
        // Case `a_file_from_a_newer_editor_is_refused_with_both_versions`.
        {
            // Refuse, never guess: an unknown layout read with today's rules
            // produces a project that looks fine and is wrong.
            let mut bytes = encode(&rich_project()).expect("the project encodes");
            bytes[11..13].copy_from_slice(&(MIN_READER_VERSION + 1).to_le_bytes());
            match decode(&bytes) {
                Err(ProjectFileError::TooNew {
                    file_min_reader,
                    this_reader,
                }) => {
                    assert_eq!(file_min_reader, MIN_READER_VERSION + 1);
                    assert_eq!(this_reader, MIN_READER_VERSION);
                }
                other => panic!("a newer file must be refused, got {other:?}"),
            }
        }
        // Case `a_damaged_payload_is_reported_as_damage`.
        {
            let mut bytes = encode(&rich_project()).expect("the project encodes");
            // Scribble over the middle of the Automerge document, leaving the
            // header (and so the length) intact.
            let mid = HEADER_LEN + (bytes.len() - HEADER_LEN) / 2;
            for b in &mut bytes[mid..mid + 8] {
                *b ^= 0xff;
            }
            match decode(&bytes) {
                Err(ProjectFileError::Corrupt(_)) => {}
                other => panic!("a scribbled payload must not load, got {other:?}"),
            }
        }
        // Case `opening_something_that_is_not_there_says_so`.
        {
            let missing = Path::new("/definitely/not/here-xyz.qvp");
            match load(missing) {
                Err(ProjectFileError::Io(msg)) => assert!(msg.contains("Could not open"), "{msg}"),
                other => panic!("a missing file is an IO error, got {other:?}"),
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn project_names_recents_and_missing_sources_are_tracked() {
        // Case `the_suggested_filename_gains_the_extension_exactly_once`.
        {
            assert_eq!(with_extension("Holiday"), "Holiday.qvp");
            assert_eq!(with_extension("Holiday.qvp"), "Holiday.qvp");
            assert_eq!(with_extension("Holiday.QVP"), "Holiday.QVP");
            // A name that merely contains a dot is not already extended.
            assert_eq!(with_extension("v1.2 cut"), "v1.2 cut.qvp");
            assert_eq!(with_extension("   "), "Untitled.qvp");
        }
        // Case `the_project_name_follows_the_file_it_was_opened_from`.
        {
            assert_eq!(
                name_from_path(Path::new("/home/a/Holiday.qvp")).as_deref(),
                Some("Holiday"),
            );
            assert_eq!(name_from_path(Path::new("/")), None);
        }
        // Case `the_most_recent_project_comes_first_and_never_appears_twice`.
        {
            use super::recents::promote;
            let a = std::path::PathBuf::from("/p/a.qvp");
            let b = std::path::PathBuf::from("/p/b.qvp");
            let c = std::path::PathBuf::from("/p/c.qvp");

            let list = promote(&[], &a);
            assert_eq!(list, vec![a.clone()]);
            let list = promote(&list, &b);
            assert_eq!(list, vec![b.clone(), a.clone()]);
            // Re-opening one already in the list moves it up rather than adding a
            // second copy — a duplicate entry is a menu that lies about history.
            let list = promote(&list, &a);
            assert_eq!(list, vec![a.clone(), b.clone()]);
            let list = promote(&list, &c);
            assert_eq!(list, vec![c, a, b]);
        }
        // Case `the_recents_list_is_capped`.
        {
            use super::recents::{MAX, promote};
            let mut list = Vec::new();
            for i in 0..MAX + 5 {
                list = promote(&list, std::path::Path::new(&format!("/p/{i}.qvp")));
            }
            assert_eq!(list.len(), MAX, "the menu cannot grow without bound");
            assert_eq!(
                list[0],
                std::path::PathBuf::from(&format!("/p/{}.qvp", MAX + 4))
            );
        }
        // Case `recents_survive_a_restart_and_drop_files_that_are_gone`.
        {
            // Drives the real on-disk path, not just `promote`: a recents list
            // that is only ever held in memory is exactly the bug this replaces.
            let tmp = std::env::temp_dir().join(format!("tilo-ve-recents-{}", std::process::id()));
            std::fs::create_dir_all(&tmp).unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_DATA_HOME", &tmp)
                .set("TILO_PACKAGE_ID", "so.tilo.video-editor-test");

            let present = tmp.join("Present.qvp");
            save(&present, &Project::new("Present")).unwrap();
            let gone = tmp.join("Gone.qvp");

            let list = super::recents::remember(&[], &gone);
            let list = super::recents::remember(&list, &present);
            assert_eq!(list.len(), 2, "both are remembered in memory");

            // A fresh process reads it back — and silently skips the entry whose
            // file no longer exists rather than offering a menu item that fails.
            let reloaded = super::recents::load();
            assert_eq!(
                reloaded,
                vec![present.clone()],
                "only the file that is there"
            );
            assert_eq!(super::recents::label(&present), "Present");

            drop(env);
            std::fs::remove_dir_all(&tmp).ok();
        }
        // Case `missing_sources_separate_absent_bytes_from_titles_and_present_files`.
        {
            let mut p = Project::new("moved");
            // A reference whose bytes are nowhere on this box.
            p.media.push(MediaItem {
                id: Uuid::new_v4(),
                name: "Gone.mov".into(),
                kind: MediaKind::Video,
                path: "/definitely/not/here-xyz.mov".into(),
                ..Default::default()
            });
            // A title has no bytes at all, so it is not "missing".
            p.media.push(MediaItem {
                id: Uuid::new_v4(),
                name: "Card".into(),
                kind: MediaKind::Text,
                path: String::new(),
                text: Some(crate::model::TextSource::new("Card")),
                ..Default::default()
            });
            // A source that really is here.
            let here =
                std::env::temp_dir().join(format!("tilo-ve-here-{}.mp4", std::process::id()));
            std::fs::write(&here, b"bytes").unwrap();
            p.media.push(MediaItem {
                id: Uuid::new_v4(),
                name: "Here.mp4".into(),
                kind: MediaKind::Video,
                path: here.to_string_lossy().into_owned(),
                ..Default::default()
            });

            let missing = missing_sources(&p);
            assert_eq!(
                missing,
                vec![("Gone.mov".to_string(), crate::media::Hydration::Projected)],
                "only the source whose bytes are absent is reported",
            );
            std::fs::remove_file(&here).ok();
        }
    }
}
