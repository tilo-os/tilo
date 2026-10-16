//! What the editor does that is no edit of the project — revealing an item,
//! the project file and render jobs ([`VideoEditorEffect`]) — and the verbs
//! it offers bound to where its person is: their playhead, and the mix their
//! monitor measured ([`surface_declarations`]).
//!
//! Every change of the project is a field edit or a verb of
//! [`Project`](crate::model::Project) ([`crate::resource`]); clip metadata is
//! the document, pixels and sound are produced outside it.

use ui::tilo_resource::{self, ActionDecl, Effect};

use crate::model::Project;
use crate::resource::video_project;

/// What the editor does that is no edit of the project: revealing a track,
/// clip, effect, marker or source, and the project file and render jobs.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "video_editor.effect")]
pub enum VideoEditorEffect {
    /// Reveal track.
    #[effect(view, row, idempotent)]
    RevealTrack {
        #[param("The track's id")]
        track: String,
    },
    /// Reveal clip.
    #[effect(view, row, idempotent)]
    RevealClip {
        #[param("The clip's id")]
        clip: String,
    },
    /// Reveal effect.
    #[effect(view, row, idempotent)]
    RevealEffect {
        #[param("The clip the effect is on")]
        clip: String,
        #[param("The effect's id")]
        effect: String,
    },
    /// Reveal marker.
    #[effect(view, row, idempotent)]
    RevealMarker {
        #[param("The marker's id")]
        marker: String,
        #[param("The clip the marker is on; omit for a project marker")]
        clip: Option<String>,
    },
    /// Reveal media.
    #[effect(view, row, idempotent)]
    RevealMedia {
        #[param("The source's id")]
        media: String,
    },
    /// Import media.
    #[effect(job, device_only)]
    Import {
        #[param("Direct filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Open project.
    #[effect(job, device_only)]
    OpenProject {
        #[param("Direct filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Save project.
    #[effect(job, device_only)]
    Save {
        #[param("Direct filesystem path; omit to save to the current project file")]
        path: Option<String>,
    },
    /// Save project as.
    #[effect(job, device_only)]
    SaveAs {
        #[param("Direct filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Export video.
    #[effect(cancellable, device_only)]
    Export {
        #[param("Direct export path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Cancel export.
    #[effect(destructive)]
    CancelExport,
}

/// Where this participant is, which the verbs it offers are bound to.
pub struct VideoResourceCtx {
    pub project: Project,
    pub playhead: f64,
    /// What the mixer last measured — see [`crate::audio::MixMeters`]: app
    /// state like the playhead, here so a caller with no screen can act on
    /// a mix that went over.
    pub mix: crate::audio::MixSnapshot,
}

/// The project's verbs bound to the playhead and the measured mix: mark in
/// and out, add or remove a marker here, cut or trim a clip here, and trim
/// a mix that went over. Each is the verb itself with its time (or its
/// lanes and level) filled in.
pub fn surface_declarations(ctx: &VideoResourceCtx) -> Vec<ActionDecl<Project>> {
    let playhead = ctx.playhead;
    let mut decls = Vec::new();
    let mut add = |decl: Option<ActionDecl<Project>>| decls.extend(decl);
    add(Project::MARK_IN
        .bind(video_project::MarkIn::paths().at, playhead)
        .declaration("mark-in-here", "Mark in at playhead"));
    add(Project::MARK_OUT
        .bind(video_project::MarkOut::paths().at, playhead)
        .declaration("mark-out-here", "Mark out at playhead"));
    add(Project::PLACE_MARKER
        .bind(video_project::PlaceMarker::paths().at, playhead)
        .declaration("add-marker-here", "Add marker at playhead"));
    add(Project::CUT
        .bind(video_project::Cut::paths().at, playhead)
        .declaration("split-here", "Split clip at playhead"));
    add(Project::TRIM_HEAD
        .bind(video_project::TrimHead::paths().at, playhead)
        .declaration("trim-in-here", "Trim clip in to playhead"));
    add(Project::TRIM_TAIL
        .bind(video_project::TrimTail::paths().at, playhead)
        .declaration("trim-out-here", "Trim clip out to playhead"));
    if let Some(marker) = ctx
        .project
        .markers
        .iter()
        .find(|m| (m.t - playhead).abs() < 0.25)
    {
        add(Project::DELETE_MARKER
            .bind(
                video_project::DeleteMarker::paths().marker,
                ui::tilo_resource::Id::from_raw(marker.id.to_string()),
            )
            .declaration(
                "remove-marker-here",
                format!("Remove marker \u{201c}{}\u{201d}", marker.name),
            ));
    }
    if ctx.mix.clipped() {
        let lanes: Vec<ui::tilo_resource::Id> = ctx
            .mix
            .measured_tracks()
            .iter()
            .map(|id| ui::tilo_resource::Id::from_raw(id.to_string()))
            .collect();
        let over = ctx.mix.worst_over_db();
        let places = ctx.mix.overs.len();
        let lane_words = match ctx.mix.clipped_tracks().len() {
            0 => String::new(),
            1 => ", 1 lane over on its own".to_owned(),
            n => format!(", {n} lanes over on their own"),
        };
        add(Project::TRIM_LANES
            .bind(video_project::TrimLanes::paths().tracks, lanes)
            .and(video_project::TrimLanes::paths().db, over)
            .declaration("trim-mix", "Trim the mix to fit")
            .map(|decl| {
                decl.describe(format!(
                    "the mix was measured {over:+.1} dB over full scale in {places} place{}\
                     {lane_words}; every lane comes down by the same amount, so the balance is \
                     unchanged",
                    if places == 1 { "" } else { "s" },
                ))
            }));
    }
    decls
}
