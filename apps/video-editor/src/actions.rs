//! What the editor does beyond the project's own verbs — revealing an item,
//! showing a panel, splitting what its person selected, the project file and
//! render jobs ([`VideoEditorEffect`]) — and the verbs
//! it offers bound to where its person is: their playhead, and the mix their
//! monitor measured ([`surface_declarations`]).
//!
//! Every change of the project is a field edit or a verb of
//! [`Project`] ([`crate::resource`]); clip metadata is
//! the document, pixels and sound are produced outside it.

use ui::tilo_resource::{self, ActionDecl, Effect};

use crate::model::Project;
use crate::resource::video_project;

/// What the editor does beyond the project's own verbs: revealing a track,
/// clip, effect, marker or source, showing the media, the inspector or the
/// export settings, splitting the selection, and the project file and render
/// jobs.
#[derive(Clone, Debug, PartialEq, Effect)]
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
    #[effect(job)]
    Import {
        #[param("Direct filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Open project.
    #[effect(job)]
    OpenProject {
        #[param("Direct filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Save project.
    #[effect(job)]
    Save {
        #[param("Direct filesystem path; omit to save to the current project file")]
        path: Option<String>,
    },
    /// Save project as.
    #[effect(job)]
    SaveAs {
        #[param("Direct filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Export video.
    ///
    /// Writes the rendered timeline to a file, an outside effect like any
    /// other.
    #[effect(cancellable)]
    Export {
        #[param("Direct export path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Cancel export.
    #[effect(destructive)]
    CancelExport,
    /// Import content.
    ///
    /// Brings a video, sound or picture the room holds, by its content
    /// address, into the bin. Its kind, length, frame size and rate come
    /// from its container. Answers the new source: place it with add-clip.
    /// Importing the same content again answers the source already there.
    #[effect(kind = "undoable", returns, idempotent, audience = "agents")]
    ImportContent {
        #[param(content, "The media's content address, tilo.content:<hash>")]
        content: String,
        #[param("The source's name in the bin, e.g. interview.mp4; made up when omitted")]
        name: Option<String>,
        #[param("Seconds the source lasts, used only when its container does not say")]
        duration: Option<f64>,
    },
    /// Export timeline.
    ///
    /// Keeps the timeline as JSON content, what read answers with, and
    /// answers its content address. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportTimelineContent,
    /// Show export settings.
    ///
    /// Opens the export dialog: where the video goes, its format and size,
    /// and a running export's progress and Cancel.
    #[effect(view, idempotent)]
    ShowExport,
    /// Show or hide media.
    #[effect(view)]
    ToggleMedia,
    /// Show or hide the inspector.
    #[effect(view)]
    ToggleInspector,
    /// Go to clip.
    ///
    /// Moves the playhead to the start of the next clip after it, or of the
    /// previous one when none follows, so the preview shows a picture.
    #[effect(view)]
    GoToClip,
    /// Split selected clips at playhead.
    ///
    /// Cuts every selected clip that covers the playhead, and the clips
    /// linked to it, as one step.
    #[effect(edit)]
    SplitSelection,
    /// Render video.
    ///
    /// Renders the timeline, or a part of it, on this device into a video
    /// kept as content, and answers with its content address, the range it
    /// covers and its size. Changes nothing; putting the video in a file is
    /// another action that takes the content. An agent's host renders it in
    /// its render service, as mp4 at 720p or 1080p.
    #[effect(kind = "read", job, cancellable, returns, host_render)]
    RenderTimeline {
        #[param("Seconds on the timeline where it begins; omit for the render range's start")]
        start: Option<f64>,
        #[param("Seconds where it ends; omit for the render range's end")]
        end: Option<f64>,
        #[param("The file format; mp4 when omitted", choices = ["mp4", "webm", "mov"])]
        format: Option<String>,
        #[param("The picture's height; 1080p when omitted", choices = ["720p", "1080p", "2160p"])]
        size: Option<String>,
    },
    /// Export video content.
    ///
    /// Renders the timeline (its render range when marked) to an MP4 kept as
    /// content, deterministically: the same cut at the same settings is the
    /// same content. Answers state done with the content address, frames,
    /// duration and size; or state rendering with a job to ask
    /// export-video-status about when the wait ran out. Sources must be room
    /// content (import-content); at most two minutes. Changes nothing.
    #[effect(kind = "read", returns, audience = "agents", host_render)]
    ExportVideoContent {
        #[param("The file format: mp4, the only one; mp4 when omitted", choices = ["mp4"])]
        format: Option<String>,
        #[param(
            "Frame width in pixels, even, at most 1920; from the height and the project's aspect when omitted, 1280 when both are"
        )]
        width: Option<u32>,
        #[param(
            "Frame height in pixels, even, at most 1080 (1920 when portrait); 720 when both are omitted"
        )]
        height: Option<u32>,
        #[param("Frames per second: 24, 30 or 60; 30 when omitted")]
        fps: Option<u32>,
        #[param(
            "Seconds to wait for the render before answering with its job, 0 to 120; 60 when omitted"
        )]
        wait: Option<u32>,
    },
    /// Export video status.
    ///
    /// Answers where a render export-video-content started has got to: done
    /// with its content, still rendering with frames done of total, or failed
    /// with why. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents", host_render)]
    ExportVideoStatus {
        #[param("The render's job, from export-video-content's answer")]
        job: String,
        #[param("Seconds to wait for it to finish, 0 to 120; 60 when omitted")]
        wait: Option<u32>,
    },
    /// Cancel video export.
    ///
    /// Stops a render export-video-content started; answers it as failed.
    #[effect(kind = "read", returns, idempotent, audience = "agents", host_render)]
    CancelVideoExport {
        #[param("The render's job, from export-video-content's answer")]
        job: String,
    },
    /// View frame content.
    ///
    /// Draws the timeline at one time as a PNG kept as content, to check a
    /// cut by eye: answers its content address, the time and its size.
    /// Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents", host_render)]
    ViewFrameContent {
        #[param("Seconds on the timeline to view")]
        at: f64,
        #[param("The picture's width in pixels, even, 16 to 1920; 640 when omitted")]
        width: Option<u32>,
    },
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
    // Two angles of one moment, side by side: offered only where two
    // pictures meet that may be moved to their halves, so the control is
    // absent rather than inert.
    if ctx.project.side_by_side_available(playhead) {
        add(Project::PLACE_SIDE_BY_SIDE
            .bind(video_project::PlaceSideBySide::paths().at, playhead)
            .declaration("place-two-angles-here", "Place side by side"));
    }
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
