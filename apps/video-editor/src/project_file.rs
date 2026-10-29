//! The editor's **project file**: a copy of the project a person exports,
//! hands to someone else, and opens back in.
//!
//! The project itself is the app's resource, kept, versioned and synced
//! like any other; nothing has to be saved for work to be safe. The file is
//! a projection of it: Export project file writes one, and Open project
//! file brings one in as the project, one undoable edit.
//!
//! ## The payload is the CRDT document
//!
//! A project file is a header followed by a serialized Automerge document
//! holding the [`Project`] model exactly as the editor stores it. That
//! choice is not incidental:
//!
//!   * **It round-trips losslessly.** The document shape *is* the editor's
//!     storage shape, so saving is not a lossy projection into some other
//!     schema that then has to be kept in step.
//!   * **It stays collaborative and undoable.** What comes back out of a
//!     file is a document, merges like a document, and is applied through
//!     the ordinary action pipeline — so opening a project is one undo
//!     boundary and replicates to a peer like any other edit.
//!   * **It absorbs model growth for free.** The project's reader,
//!     [`Project`]'s own read, reads
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
//! Refusing is the point. The record store already learned this for its
//! documents: a document it cannot understand is left alone rather
//! than opened empty and saved back over the top. A file this module cannot
//! understand gets [`ProjectFileError::TooNew`], carrying both versions, and
//! the caller says so on screen.

use std::path::Path;

use ui::tilo_resource::Replica;

use crate::model::Project;

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
    let mut replica = Replica::<Project>::new();
    replica
        .apply(
            ui::tilo_resource::Invoke::edit(&crate::resource::load_project(project))
                .map_err(|e| ProjectFileError::Corrupt(e.to_string()))?,
        )
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
    Replica::<Project>::load(payload)
        .and_then(|replica| replica.state())
        .map_err(|e| ProjectFileError::Corrupt(e.to_string()))
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

/// Point `project`'s media that sit beside the file it came from, in `dir`,
/// at their place there.
///
/// A `.qvp` can be copied somewhere with its footage beside it, and a
/// project built by hand (a script, the export probe) can carry a path
/// relative to where it was written. Neither is a workspace reference: once
/// the project is this app's resource the file's directory is gone, so a
/// relative path that only resolves beside the file is made absolute as the
/// file comes in. A path the workspace or this device already resolves is
/// left alone.
pub fn anchor_media(project: &mut Project, dir: &Path) {
    for media in &mut project.media {
        let relative = Path::new(&media.path);
        if media.path.is_empty()
            || relative.is_absolute()
            || crate::media::resolve(&media.path).is_decodable()
        {
            continue;
        }
        let beside = dir.join(relative);
        if beside.exists() {
            media.path = beside.to_string_lossy().into_owned();
        }
    }
}

/// How a project file is named in notices: its stem, which is what the
/// user named the project.
pub fn label(path: &Path) -> String {
    name_from_path(path).unwrap_or_else(|| path.to_string_lossy().into_owned())
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
        {
            let path = std::env::temp_dir()
                .join(format!("tilo-ve-proj-{}.{EXTENSION}", std::process::id()));
            let p = rich_project();
            std::fs::write(&path, encode(&p).expect("the project encodes")).expect("saves");
            let back = load(&path).expect("loads");
            assert_eq!(back.name, p.name);
            assert_eq!(back.clips.len(), 1);
            std::fs::remove_file(&path).ok();
        }
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
            assert_eq!(header_fields(&bytes).unwrap().0, FORMAT_VERSION + 7);
        }
    }

    #[test]
    fn project_header_refuses_foreign_truncated_newer_or_damaged_files() {
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
                header_fields(&[0u8; 4]).unwrap_err(),
                ProjectFileError::NotAProject
            );
        }
        {
            let bytes = encode(&Project::new("t")).expect("the project encodes");
            for n in 0..HEADER_LEN {
                assert_eq!(
                    header_fields(&bytes[..n]).unwrap_err(),
                    ProjectFileError::NotAProject,
                    "{n} bytes"
                );
            }
            assert_eq!(
                header_fields(&bytes[..HEADER_LEN]).map(|(v, m, _)| (v, m)),
                Ok((FORMAT_VERSION, MIN_READER_VERSION))
            );
        }
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
        {
            let missing = Path::new("/definitely/not/here-xyz.qvp");
            match load(missing) {
                Err(ProjectFileError::Io(msg)) => assert!(msg.contains("Could not open"), "{msg}"),
                other => panic!("a missing file is an IO error, got {other:?}"),
            }
        }
    }

    #[test]
    fn project_names_and_missing_sources_are_tracked() {
        {
            assert_eq!(with_extension("Holiday"), "Holiday.qvp");
            assert_eq!(with_extension("Holiday.qvp"), "Holiday.qvp");
            assert_eq!(with_extension("Holiday.QVP"), "Holiday.QVP");
            // A name that merely contains a dot is not already extended.
            assert_eq!(with_extension("v1.2 cut"), "v1.2 cut.qvp");
            assert_eq!(with_extension("   "), "Untitled.qvp");
        }
        {
            assert_eq!(
                name_from_path(Path::new("/home/a/Holiday.qvp")).as_deref(),
                Some("Holiday"),
            );
            assert_eq!(name_from_path(Path::new("/")), None);
        }
        {
            let mut p = Project::new("moved");
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
        // Case `media_beside_the_file_is_found_there`: a relative path that
        // only resolves beside the file it came from is pointed there, and
        // one found nowhere is left as the workspace reference it is.
        {
            let dir = std::env::temp_dir().join(format!("tilo-ve-beside-{}", std::process::id()));
            std::fs::create_dir_all(dir.join("footage")).unwrap();
            std::fs::write(dir.join("footage/take.mp4"), b"bytes").unwrap();
            let mut p = Project::new("beside");
            for path in ["footage/take.mp4", "footage/elsewhere.mp4"] {
                p.media.push(MediaItem {
                    id: Uuid::new_v4(),
                    kind: MediaKind::Video,
                    path: path.into(),
                    ..Default::default()
                });
            }
            anchor_media(&mut p, &dir);
            assert_eq!(
                Path::new(&p.media[0].path),
                dir.join("footage/take.mp4").as_path()
            );
            assert_eq!(p.media[1].path, "footage/elsewhere.mp4");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
