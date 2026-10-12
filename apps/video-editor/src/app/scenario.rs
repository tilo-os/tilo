//! Deterministic screenshot scenarios and their one-shot setup lifecycle.

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// One-shot capture scenarios selected by the `--shots` entry points. Each
/// deterministically arranges editor state so a single offscreen frame shows
/// a specific feature (filters, a fresh split). `None` is the live editor.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ShotScenario {
    /// Default: the live editor with its auto-selected clip.
    Live,
    /// The graded drone clip selected, filters + fades visible in the
    /// inspector, playhead inside the clip.
    SelectedFilters,
    /// A split performed at the playhead, leaving two contiguous clips.
    JustSplit,
    /// A bin item's context menu open (Add to timeline / Rename / Delete).
    BinMenu,
    /// A bin item being renamed in place through the shared editable label.
    BinRename,
    /// A bin clip mid-drag over a timeline lane, with the ghost + drop marker.
    DragToTimeline,
    /// The graded drone selected with its effects stack scrolled into view.
    Effects,
    /// Several clips selected at once, showing the anchor's full outline
    /// against the lighter outlines of the rest and the inspector's count.
    MultiSelect,
    /// A locked video track: the header's lock pill lit, the lane hatched
    /// over its clips, and the inspector naming the lock.
    LockedTrack,
    /// Zoomed in and scrolled into the middle of the timeline, so the
    /// horizontal scrollbar is showing part-way along its track.
    Scrolled,
    /// The keyboard-shortcut sheet open over the editor.
    HelpSheet,
    /// The go-to-timecode field open with a partly-typed time.
    GotoTimecode,
    /// A timeline clip's context menu open (cut / copy / duplicate / split /
    /// delete).
    ClipMenu,
    /// A rubber-band selection mid-sweep across a lane.
    Marquee,
    /// A composed frame: a clip inset into a corner, a title in the lower
    /// third, named markers on the ruler.
    Composed,
    /// The inspector scrolled to the geometry and rate sections, on a clip
    /// that is inset, re-timed and keyframed.
    Geometry,
    /// A keyed and masked clip with both sections open: the key's colour
    /// swatches and its three sliders, and an ellipse mask's geometry.
    KeyAndMask,
    /// A title clip selected, with its words, size, alignment and colour
    /// showing, and the marker list under them.
    TitleClip,
    /// A keyframed clip with the playhead sitting on a key: the key control
    /// filled, the curve's diamonds on the clip body.
    Keyframes,
    /// The marker list, holding timeline markers and a clip's own, with the
    /// ruler flags they correspond to.
    Markers,
    /// The keyframe menu open on a key under the playhead: remove, the three
    /// shapes with the one in force ticked, and the steps either side.
    KeyMenu,
    /// The project menu open under the project button: Open, Save, Save As,
    /// and the recent projects under a separator.
    ProjectMenu,
    /// The window while the OS is hovering a file over it, so the drop
    /// invitation can be seen to cover everything a drop actually reaches.
    FileDrop,
    /// The question asked before an Open replaces a project that was never
    /// saved anywhere.
    DiscardOpen,
    /// A project that has been saved to a file: the button carries the file's
    /// name and no unsaved badge, and the notice says where it went.
    SavedProject,
    /// The tone-curve editor, shaped: an S on the master with the amount
    /// keyed, so the box, the channel chooser, the amount slider and its
    /// key control are all on screen at once.
    Curves,
    /// Two other participants in the room: their playheads down the lanes
    /// with their names on them, and the clips they have selected outlined
    /// in their colours -- next to this participant's own selection, so the
    /// capture shows the two reading as different people.
    Collaborators,
    /// A render in flight with its dialog already dismissed: the editor is
    /// fully usable and the export still reports itself, from the chip under
    /// the Export button.
    ExportRunning,
    /// The render after it failed, with the reason it gives, so the failing
    /// half of the chip can be reviewed as well as the working half.
    ExportFailed,
    /// The export dialog open over the editor, before a render starts.
    ExportDialog,
    /// The export dialog showing why a render failed, at full length, so the
    /// reason's wrapping can be reviewed in the details surface.
    ExportFailedDialog,
    /// In and out marks on the timeline: the band across the ruler, the
    /// bracket at each mark, and the lanes outside the range greyed back — so
    /// what a render will and will not cover can be seen at a glance.
    RenderRange,
    /// The project's own format, with nothing selected: the PROJECT section
    /// open under the marker list, stating the frame and offering the size
    /// and rate presets.
    ProjectFormat,
    /// The keyboard cursor inside the inspector, on a slider row -- the ring
    /// that says where the next arrow key lands. The panel's controls are on
    /// the focus walk, and a walk you cannot see is a walk you cannot use.
    FocusInspector,
    /// The same cursor in the media bin, on a row: the ring inside the
    /// selection outline the row already carries, so the two read apart.
    FocusBin,
    /// The same cursor on a transport button, where it rings the outside of a
    /// round control rather than the inside of a rectangular one.
    FocusTransport,
    /// A running J/L shuttle: the playhead moving at 8x with the transport
    /// naming the direction and the rate, and the play button wearing the
    /// pause glyph because something is already moving.
    Shuttle,
    /// Two shots whose sound was split onto its own lane: one pair selected,
    /// so the anchor's outline, the other half's dashed one and the tie
    /// between them are all on screen, and one pair left alone, so the quiet
    /// chain a linked clip wears standing still can be judged next to it.
    LinkedAudio,
    /// The mixer, on a mix that has been played and went over: lane faders
    /// off unity and off centre, the meters showing it, the master strip's
    /// standing CLIP warning, the marks on the ruler saying where, and the
    /// inspector's MIX section open on the lane the selection sits on — with
    /// the keyboard cursor resting on the button that puts that lane back, so
    /// the walk through the section can be seen as well as read about.
    Mix,
}

impl ShotScenario {
    /// Scenarios arranged from a rect-dependent interaction (menu / rename /
    /// drag / effects), applied in `draw` once the layout rect is known rather
    /// than during `sync` (where the rect is still zero).
    pub(super) fn is_interaction(self) -> bool {
        matches!(
            self,
            ShotScenario::BinMenu
                | ShotScenario::BinRename
                | ShotScenario::DragToTimeline
                | ShotScenario::Effects
                | ShotScenario::MultiSelect
                | ShotScenario::LockedTrack
                | ShotScenario::Scrolled
                | ShotScenario::ClipMenu
                | ShotScenario::Marquee
                | ShotScenario::Geometry
                | ShotScenario::KeyAndMask
                | ShotScenario::TitleClip
                | ShotScenario::Keyframes
                | ShotScenario::Markers
                | ShotScenario::KeyMenu
                | ShotScenario::ProjectMenu
                | ShotScenario::Curves
                | ShotScenario::Collaborators
                | ShotScenario::ProjectFormat
                | ShotScenario::FocusInspector
                | ShotScenario::LinkedAudio
                | ShotScenario::Mix
        )
    }

    /// Parse a `--scenario <name>` selector. Returns `None` for an unknown
    /// name so the caller can fall back to the plain editor rather than
    /// silently capturing the wrong thing.
    /// Every name [`Self::from_arg`] accepts.
    ///
    /// The launcher validates `--scenario` against a list, so it needs one;
    /// `from_arg` is a match and cannot be enumerated. A test asserts every
    /// name here parses and that the counts agree, so the two cannot drift
    /// into "this scenario exists but the launcher rejects it".
    pub const ALL_NAMES: &'static [&'static str] = &[
        "filters",
        "split",
        "menu",
        "rename",
        "drag",
        "effects",
        "multiselect",
        "locked",
        "scrolled",
        "clipmenu",
        "marquee",
        "composed",
        "geometry",
        "keymask",
        "title",
        "keyframes",
        "markers",
        "keymenu",
        "help",
        "goto",
        "projectmenu",
        "filedrop",
        "discard",
        "saved",
        "curves",
        "collaborators",
        "exporting",
        "exportfailed",
        "exportdialog",
        "exportfaileddialog",
        "range",
        "format",
        "focusinspector",
        "focusbin",
        "focustransport",
        "shuttle",
        "linked",
        "mix",
    ];

    pub fn from_arg(name: &str) -> Option<Self> {
        Some(match name {
            "filters" => ShotScenario::SelectedFilters,
            "split" => ShotScenario::JustSplit,
            "menu" => ShotScenario::BinMenu,
            "rename" => ShotScenario::BinRename,
            "drag" => ShotScenario::DragToTimeline,
            "effects" => ShotScenario::Effects,
            "multiselect" => ShotScenario::MultiSelect,
            "locked" => ShotScenario::LockedTrack,
            "scrolled" => ShotScenario::Scrolled,
            "clipmenu" => ShotScenario::ClipMenu,
            "marquee" => ShotScenario::Marquee,
            "composed" => ShotScenario::Composed,
            "geometry" => ShotScenario::Geometry,
            "keymask" => ShotScenario::KeyAndMask,
            "title" => ShotScenario::TitleClip,
            "keyframes" => ShotScenario::Keyframes,
            "markers" => ShotScenario::Markers,
            "keymenu" => ShotScenario::KeyMenu,
            "help" => ShotScenario::HelpSheet,
            "goto" => ShotScenario::GotoTimecode,
            "projectmenu" => ShotScenario::ProjectMenu,
            "filedrop" => ShotScenario::FileDrop,
            "discard" => ShotScenario::DiscardOpen,
            "saved" => ShotScenario::SavedProject,
            "curves" => ShotScenario::Curves,
            "collaborators" => ShotScenario::Collaborators,
            "exporting" => ShotScenario::ExportRunning,
            "exportfailed" => ShotScenario::ExportFailed,
            "exportdialog" => ShotScenario::ExportDialog,
            "exportfaileddialog" => ShotScenario::ExportFailedDialog,
            "range" => ShotScenario::RenderRange,
            "format" => ShotScenario::ProjectFormat,
            "focusinspector" => ShotScenario::FocusInspector,
            "focusbin" => ShotScenario::FocusBin,
            "focustransport" => ShotScenario::FocusTransport,
            "shuttle" => ShotScenario::Shuttle,
            "linked" => ShotScenario::LinkedAudio,
            "mix" => ShotScenario::Mix,
            _ => return None,
        })
    }
}

/// The capture of the live app on a blank project, as a fresh machine shows.
pub const EMPTY_CAPTURE: &str = "empty";
/// The capture of the collapsed media bin revealed at a narrow width.
pub const NARROW_CAPTURE: &str = "narrow";

/// The project the `composed` capture loads: the demo project with
/// everything geometry made possible, arranged through the production model
/// — an inset over the spine, a title in the lower third, and markers on the
/// ruler.
pub(crate) fn composed_project() -> Project {
    let mut project = crate::seed::sample_project();
    if let Some(cid) = project.top_video_clip_at(23.0).map(|c| c.id)
        && let Some(c) = project.clip_mut(cid)
    {
        c.transform = crate::model::Transform::pip(0.34, (1.0, -1.0), 0.04);
    }
    let title = MediaItem {
        id: Uuid::new_v4(),
        name: "Coastline".into(),
        kind: MediaKind::Text,
        duration: 8.0,
        palette: 4,
        path: String::new(),
        text: Some(crate::model::TextSource {
            content: "COASTLINE\nepisode two".into(),
            size: 0.14,
            align: crate::model::TextAlign::Left,
            ..crate::model::TextSource::default()
        }),
        ..Default::default()
    };
    let tid = title.id;
    project.media.push(title);
    if let Some(v2) = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video && t.index == 2)
        .map(|t| t.id)
        && let Some(cid) = project.add_clip(v2, tid, 34.0)
        && let Some(c) = project.clip_mut(cid)
    {
        c.transform = crate::model::Transform {
            scale_x: 0.6,
            scale_y: 0.6,
            pos_x: -0.1,
            pos_y: 0.27,
            ..Default::default()
        };
    }
    project.add_marker(9.0, "act two", 3);
    project.add_marker(21.0, "sunset", 5);
    project
}

/// One-time lifecycle of deterministic screenshot setup.
#[derive(Default)]
pub(super) struct ShotLifecycle {
    pub(super) applied: bool,
    pub(super) interaction_applied: bool,
}

impl VideoEditorApp {
    /// Stage a `--shots` capture on the hosted editor. The scenario's
    /// project has been loaded through the host already
    /// ([`crate::VideoEditor`]'s `scenario`); this arranges the view state
    /// around it — the selection, the open menu, the drag — on the editor's
    /// next sync and draw, through the same one-shot lifecycle the scenario
    /// table always used. `None` is the plain demo capture.
    pub fn stage_capture(&mut self, scenario: Option<&str>) {
        self.shot_scenario = match scenario {
            // The real live app on a blank project, as a fresh machine shows.
            Some(EMPTY_CAPTURE) => {
                // Nothing on the timeline, so nothing to be part-way through.
                self.playhead = 0.0;
                return;
            }
            Some(NARROW_CAPTURE) => {
                // The collapsed bin revealed over the editor behind its scrim;
                // the animation never runs in a one-shot render.
                self.reconcile_flaps();
                self.bin_flap.snap(true);
                self.navigation.flap_settled = true;
                ShotScenario::Live
            }
            Some(name) => {
                self.settle_flaps_on_next_frame();
                ShotScenario::from_arg(name).unwrap_or(ShotScenario::Live)
            }
            None => {
                self.settle_flaps_on_next_frame();
                ShotScenario::Live
            }
        };
        self.shot_lifecycle.applied = false;
        self.shot_lifecycle.interaction_applied = false;
    }

    /// Start the side flaps at the capture width's natural state on the next
    /// frame instead of animating there from wherever an earlier frame left
    /// them: a still is of the settled layout, whatever the clock did.
    fn settle_flaps_on_next_frame(&mut self) {
        self.navigation.flap_settled = false;
    }

    /// Deterministically arrange editor state for a `--shots` scenario. Real
    /// production edit paths are used (split/grade through the CRDT) so the
    /// captured frame reflects genuine behaviour, not a mock.
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive deterministic screenshot scenario table is kept together so visual fixtures cannot drift across setup paths"
    )]
    pub(super) fn apply_shot_scenario(&mut self, state: &AutoCommit) {
        match self.shot_scenario {
            ShotScenario::Live => {}
            ShotScenario::HelpSheet => self.navigation.help_open = true,
            ShotScenario::GotoTimecode => self.goto_entry = Some(interaction::goto_field("1:30")),
            ShotScenario::ProjectMenu => {
                // Recents the menu can list. Fabricated only for the capture —
                // the live app reads them off disk and never invents one.
                self.recents = ["Reel_2026", "Interview_rough", "Titles_pass"]
                    .iter()
                    .map(|n| PathBuf::from(format!("/home/you/Videos/{n}.qvp")))
                    .collect();
            }
            ShotScenario::FileDrop => self.presentation.file_hovering = true,
            ShotScenario::DiscardOpen => {
                self.pending_open = Some(PendingOpen {
                    path: PathBuf::from("/home/you/Videos/Reel_2026.qvp"),
                    losing: self.project.name.clone(),
                });
            }
            ShotScenario::ExportRunning => {
                // Part-way through a real encode, with the dialog dismissed:
                // the state the editor used to have nowhere to show.
                self.presentation.export_open = false;
                self.export_status = ExportStatus::Rendering { progress: 0.62 };
            }
            ShotScenario::ExportFailed => {
                // The persistent chip, with the dialog dismissed.
                self.presentation.export_open = false;
                self.export_status = ExportStatus::Failed {
                    reason: "no encoder for H.264 on this machine".to_string(),
                };
            }
            ShotScenario::ExportDialog => self.presentation.export_open = true,
            ShotScenario::ExportFailedDialog => {
                self.presentation.export_open = true;
                self.export_status = ExportStatus::Failed {
                    reason: "No H.264 encoder is available for the selected export preset. Install an H.264-capable GStreamer plugin, then retry the export."
                        .to_string(),
                };
            }
            // The keyboard cursor, which nothing painted at all until now.
            // `has_focus` is what the host sets when it hands this widget the
            // keyboard, and it is the gate the ring is drawn behind -- so a
            // capture of the cursor has to stand in for that hand-over.
            ShotScenario::FocusBin => {
                self.presentation.keyboard_focus = true;
                self.focus.pane = Pane::Bin;
                self.focus.bin = 2;
                // Arrowing onto a bin row selects it, so the capture shows the
                // ring against the selection outline it has to stay legible
                // beside rather than against a bare row.
                self.selected_media = self.a11y_bin_items().get(self.focus.bin).copied();
            }
            ShotScenario::FocusTransport => {
                self.presentation.keyboard_focus = true;
                self.focus.pane = Pane::Transport;
                // The play button: the one control in the strip with a filled
                // accent circle under it, which is the hardest ground for a
                // ring in the accent-derived focus colour to stay visible on.
                self.focus.transport = 2;
                self.playhead = 12.0;
            }
            ShotScenario::Shuttle => {
                // Three presses of L, through the production key path: 1x, 2x,
                // then 4x... and a fourth for 8x, which is fast enough that a
                // still with no readout would be indistinguishable from a
                // stopped playhead.
                self.playhead = 8.0;
                for _ in 0..4 {
                    self.shuttle_key(1.0);
                }
            }
            ShotScenario::LinkedAudio => {
                // Built through the production path — the same
                // `attach_linked_audio` a real import runs — so the capture is
                // of the feature and not of a hand-arranged imitation of it.
                //
                // The drone open (0..8) and the market cutaway (20..26) both
                // sit over free A1 time, which is where their sound lands.
                for name in ["Drone_Coastline.mov", "Cutaway_Market.mp4"] {
                    let Some(mid) = self
                        .project
                        .media
                        .iter()
                        .find(|m| m.name == name)
                        .map(|m| m.id)
                    else {
                        continue;
                    };
                    if let Some(m) = self.project.media.iter_mut().find(|m| m.id == mid) {
                        m.has_audio = true;
                    }
                    self.dispatch_media(mid);
                    let Some(cid) = self
                        .project
                        .clips
                        .iter()
                        .find(|c| c.media == mid)
                        .map(|c| c.id)
                    else {
                        continue;
                    };
                    self.attach_linked_audio(cid);
                }
                // Select the market pair's picture half, and park the playhead
                // inside it, so one shot shows the selected state and the drone
                // open — off to the left, unselected — shows the resting one.
                // Picked by source rather than by "the top clip at 22s", which
                // is the sunset insert on V2 and is not linked to anything.
                if let Some(cid) = self
                    .project
                    .clips
                    .iter()
                    .filter(|c| c.link.is_some())
                    .filter(|c| {
                        self.project
                            .track(c.track)
                            .is_some_and(|t| t.kind == TrackKind::Video)
                    })
                    .find(|c| {
                        self.project
                            .media(c.media)
                            .is_some_and(|m| m.name == "Cutaway_Market.mp4")
                    })
                    .map(|c| c.id)
                {
                    self.selection.select_only(cid);
                    self.selected_media = self.project.clip(cid).map(|c| c.media);
                }
                self.playhead = 22.0;
                self.zoom = 26.0;
            }
            ShotScenario::Mix => {
                // Real faders, written through the same action a drag
                // commits, so the capture is of the feature rather than of a
                // hand-arranged imitation of it: A1 pushed up for a voice
                // under music, A2 pulled down and opened out to the left.
                let lanes: Vec<(TrackId, f64, f64)> = self
                    .project
                    .display_tracks()
                    .iter()
                    .filter(|t| t.kind == TrackKind::Audio)
                    .enumerate()
                    .map(|(i, t)| {
                        if i == 0 {
                            (t.id, 4.5, -0.35)
                        } else {
                            (t.id, -6.0, 0.2)
                        }
                    })
                    .collect();
                for (tid, gain_db, pan) in lanes {
                    if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                        t.gain_db = gain_db;
                        t.pan = pan;
                    }
                    if let Some(t) = self.project.track(tid).cloned() {
                        self.submit_track(&t);
                    }
                }
                // What a monitor would have measured playing this: a run of
                // ordinary blocks, and three that went over. Staged through
                // the production recording path, so the latch, the merge and
                // the timeline positions are the real ones — a capture with
                // no audio device is still a capture of the real meter.
                let audio_lanes: Vec<TrackId> = self
                    .project
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Audio)
                    .map(|t| t.id)
                    .collect();
                let block = |l: f32, r: f32, lanes: &[TrackId]| crate::audio::MixReport {
                    master: crate::audio::BlockMeter { peak: [l, r] },
                    tracks: lanes
                        .iter()
                        .enumerate()
                        .map(|(i, id)| {
                            let share = if i == 0 { 0.85 } else { 0.4 };
                            (
                                *id,
                                crate::audio::BlockMeter {
                                    peak: [l * share, r * share],
                                },
                            )
                        })
                        .collect(),
                };
                for (at, l, r) in [
                    (2.0, 0.42, 0.31),
                    (7.5, 1.31, 1.12),
                    (12.0, 0.55, 0.48),
                    (16.0, 1.18, 1.04),
                    (21.0, 0.61, 0.72),
                ] {
                    self.stage_mix_block(at, &block(l, r, &audio_lanes));
                }
                // Last, so the bars are showing a live level rather than only
                // the holds — the resting state of a meter is not what a
                // capture of a meter should show.
                self.stage_mix_block(24.0, &block(0.74, 0.66, &audio_lanes));
                self.playhead = 24.0;
            }
            ShotScenario::RenderRange => {
                // A range that starts inside the timeline and ends well
                // before its end, so both brackets and both scrims are on
                // screen at once.
                self.project.set_mark_out(19.0);
                self.project.set_mark_in(6.5);
                self.playhead = 11.0;
            }
            ShotScenario::SavedProject => {
                let path = PathBuf::from("/home/you/Videos/Coastline_Doc_v3.qvp");
                self.project_notice = Some(Notice::ok(project_workflow::saved_words(&path)));
                self.project_path = Some(path);
                // Saved *as it stands*, so the badge is off — which is the
                // half of the button this scenario exists to show.
                self.saved_heads = Some(state.committed_heads());
                self.doc_heads = state.committed_heads();
            }
            ShotScenario::SelectedFilters => {
                // Select the graded drone clip on V1 and park the playhead a
                // few seconds in (past its fade-in) so the inspector shows a
                // graded clip with fades and the preview shows the grade.
                if let Some(c) = self.project.top_video_clip_at(4.0) {
                    self.selection.select_only(c.id);
                    self.selected_media = Some(c.media);
                }
                self.playhead = 4.0;
                // Push the saturation up a touch through the real edit path so
                // the grade is unmistakable in the capture.
                self.set_num(NumField::Saturation, 0.72);
                if let Some(id) = self.selected() {
                    self.dispatch_clip(id);
                }
                self.zoom = 30.0;
            }
            ShotScenario::Composed => {
                // The project the scenario loaded is composed already
                // ([`composed_project`]); look at the inset under the title.
                self.playhead = 23.0;
                self.zoom = 26.0;
                if let Some(c) = self.project.top_video_clip_at(23.0) {
                    self.selection.select_only(c.id);
                }
            }
            ShotScenario::JustSplit => {
                // Select the interview body clip and split it at the playhead,
                // then select the right half — exactly what pressing S does.
                if let Some(c) = self.project.top_video_clip_at(12.0) {
                    self.selection.select_only(c.id);
                }
                self.playhead = 14.0;
                self.split_selected();
                self.zoom = 40.0;
            }
            // Rect-dependent interactions are arranged in `draw` (the layout
            // rect is still zero here in `sync`).
            s if s.is_interaction() => {}
            _ => {}
        }
    }

    /// Arrange a rect-dependent interaction scenario for a `--shots` capture,
    /// once, from `draw` where the layout rect is known. Uses the same
    /// production affordances a live interaction drives.
    /// Stage two other participants for the `collaborators` capture.
    ///
    /// Their frames are built by [`crate::presence::point_frame`] and
    /// [`crate::presence::range_frame`] -- the very functions this editor
    /// publishes itself with -- and folded in through
    /// [`PresenceCtx::apply_remote_presence`], the one decoder every inbound
    /// frame goes through. So what the capture shows is what a real peer
    /// produces; only the socket between them is missing.
    pub(super) fn seed_shot_collaborators(&mut self) {
        use ui::permissions::UserId;
        let mut ctx = PresenceCtx::empty();
        // Clips to hand out, walked track by track from the top so the pick
        // is the same on every run rather than whatever order a map iterated
        // in -- and so the first peer lands on the topmost lane, where a name
        // chip has no room above it and has to fall inside the clip instead.
        // One per track, walked from the top, so two participants are never
        // stacked on one lane -- and so the first lands on the topmost track,
        // where a name chip has no room above it and has to fall inside the
        // clip instead.
        let clips: Vec<(ClipId, f64, f64)> = self
            .project
            .display_tracks()
            .iter()
            .filter_map(|t| {
                self.project
                    .clips_on(t.id)
                    .into_iter()
                    .find(|c| Some(c.id) != self.selected())
                    .map(|c| (c.id, c.start, c.end()))
            })
            .collect();

        for (i, (id, name, at)) in [("ada", "Ada", 6.5), ("bo", "Bo", 21.0)]
            .into_iter()
            .enumerate()
        {
            ctx.names.insert(UserId(id.to_string()), name.to_string());
            let (k, p) = crate::presence::point_frame(None, at);
            ctx.apply_remote_presence(id.to_string(), k, p);
            // Each takes a different clip, and neither takes the one this
            // participant has, so the three selections are distinguishable.
            if let Some((cid, start, end)) = clips.get(i).copied() {
                let (k, p) = crate::presence::range_frame(Some((cid, start, end)));
                ctx.apply_remote_presence(id.to_string(), k, p);
            }
        }
        self.shot_presence = Some(ctx);
    }

    /// The regions with the bin showing: folded away on a narrow window, the
    /// bin is opened (settled, since a one-shot render runs no animation).
    fn reveal_bin_for_capture(&mut self, r: &Regions) -> Regions {
        if !r.bin_overlay {
            return *r;
        }
        self.reconcile_flaps();
        self.bin_flap.snap(true);
        self.regions()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the capture script: one arm per shot, each setting up exactly what it shows"
    )]
    pub(super) fn apply_shot_interaction(&mut self, r: &Regions) {
        // A capture that is about the inspector has to show it, and on a
        // narrow window it starts out of the way. Opening it here is what
        // selecting a clip does in the live app.
        if matches!(
            self.shot_scenario,
            ShotScenario::Geometry
                | ShotScenario::TitleClip
                | ShotScenario::Keyframes
                | ShotScenario::Markers
                | ShotScenario::Effects
                | ShotScenario::FocusInspector
                | ShotScenario::LinkedAudio
                | ShotScenario::Mix
        ) {
            self.reveal_inspector();
            self.reconcile_flaps();
            self.inspector_flap.snap(true);
        }
        match self.shot_scenario {
            ShotScenario::Collaborators => {
                self.seed_shot_collaborators();
            }
            ShotScenario::Mix => {
                // Select a clip on the lane the fader belongs to, and scroll
                // the panel to its MIX section — a capture of a control
                // nobody can see is a capture of nothing.
                if let Some(cid) = self
                    .project
                    .clips
                    .iter()
                    .find(|c| {
                        self.project
                            .track(c.track)
                            .is_some_and(|t| t.kind == TrackKind::Audio)
                    })
                    .map(|c| c.id)
                {
                    self.selection.select_only(cid);
                    self.selected_media = self.project.clip(cid).map(|c| c.media);
                }
                // Anchored on the MIX header rather than a guessed offset:
                // measured from the layout the panel really draws, the whole
                // section fits — the fader, the pan and the button under them.
                // A fixed 150px put the button half off the bottom edge, and a
                // capture of a control nobody can see is a capture of nothing.
                self.inspector_scroll = 0.0;
                let head = self
                    .inspector_layout(r)
                    .sections
                    .iter()
                    .find(|(s, _)| *s == Section::Mix)
                    .map(|(_, rect)| rect.y0);
                if let Some(y) = head {
                    let want = y - self.inspector_viewport(r).y0;
                    self.inspector_scroll = want.clamp(0.0, self.inspector_max_scroll(r));
                }
                // …with the keyboard cursor on the button that puts the lane
                // back. It is the newest stop on the walk and the one that
                // spent longest as a pointer-only control, so a capture of the
                // mixer that could not show a ring on it would be a capture of
                // the half of the feature that already worked.
                self.presentation.keyboard_focus = true;
                self.focus.pane = Pane::Inspector;
                self.focus.inspector = self
                    .a11y_inspector_rows()
                    .iter()
                    .position(|row| *row == InspectorRow::Pick(PickField::MixReset))
                    .unwrap_or(0);
            }
            ShotScenario::LinkedAudio => {
                // Bring the link control into view. It is laid out under the
                // label swatches, which on a panel this tall sits just past
                // the bottom edge — and a capture of a control nobody can see
                // is a capture of nothing.
                self.inspector_scroll = 150.0_f64.min(self.inspector_max_scroll(r));
            }
            ShotScenario::FocusInspector => {
                if let Some(c) = self.project.top_video_clip_at(4.0) {
                    self.selection.select_only(c.id);
                    self.selected_media = Some(c.media);
                }
                self.playhead = 4.0;
                self.inspector_scroll = 0.0;
                self.presentation.keyboard_focus = true;
                self.focus.pane = Pane::Inspector;
                // On one of the grade sliders: the block above the first
                // section header, which is the part of the panel no keyboard
                // could reach at all until recently, and the reason a cursor
                // nobody can see matters more here than it used to.
                self.focus.inspector = self
                    .a11y_inspector_rows()
                    .iter()
                    .position(|row| *row == InspectorRow::Field(NumField::Brightness))
                    .unwrap_or(0);
            }
            ShotScenario::ProjectMenu => {
                let btn = r.project_btn;
                if btn.width() > 0.0 {
                    self.open_project_menu(Point::new(btn.x0, btn.y1 + 4.0));
                }
            }
            ShotScenario::BinMenu => {
                // A row is only pressed where it can be seen: on a window
                // narrow enough to fold the bin away, the bin is summoned
                // first, as a person would have to.
                let r = &self.reveal_bin_for_capture(r);
                if let Some((mid, row)) = self.bin_item_rects(r).into_iter().nth(2) {
                    let at = Point::new(row.center().x, row.center().y);
                    self.open_bin_menu(mid, at);
                }
            }
            ShotScenario::BinRename => {
                let r = &self.reveal_bin_for_capture(r);
                if let Some((mid, _row)) = self.bin_item_rects(r).into_iter().nth(2) {
                    self.begin_rename(mid);
                }
            }
            ShotScenario::DragToTimeline => {
                // A video bin item mid-drag over the V1 lane.
                let item = self
                    .bin_item_rects(r)
                    .into_iter()
                    .find(|(m, _)| self.project.media(*m).is_some_and(|mm| mm.kind.is_visual()));
                let lane = self
                    .lane_rects(r)
                    .into_iter()
                    .find(|(_, k, _)| *k == TrackKind::Video);
                if let (Some((mid, row)), Some((_, _, lane))) = (item, lane) {
                    self.selected_media = Some(mid);
                    self.bin_drag = Some(BinDrag {
                        media: mid,
                        origin: row.center(),
                        active: true,
                    });
                    self.last_mouse = Point::new(lane.x0 + 470.0, lane.center().y);
                }
            }
            ShotScenario::Effects => {
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    self.selection.select_only(c.id);
                    self.selected_media = Some(c.media);
                }
                self.playhead = 6.0;
                // Scroll the inspector so the effects stack starts at the top
                // of the body, its header and every row whole.
                self.inspector_scroll = 0.0;
                let top = self.inspector_viewport(r).y0;
                let header = self
                    .inspector_layout(r)
                    .sections
                    .iter()
                    .find(|(s, _)| *s == Section::Effects)
                    .map(|(_, rect)| rect.y0);
                self.inspector_scroll = header.map_or_else(
                    || self.inspector_max_scroll(r),
                    |y| (y - top - S2).clamp(0.0, self.inspector_max_scroll(r)),
                );
            }
            // A clip that has actually been placed, re-timed and keyframed,
            // with the inspector scrolled to the controls that did it. Every
            // value goes through the production model, so the capture is what
            // the compositor is drawing rather than a posed panel.
            ShotScenario::Geometry => {
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        c.transform =
                            crate::model::Transform::pip(PIP_SCALE, (1.0, -1.0), PIP_INSET);
                        c.transform.rotation = -6.0;
                        c.transform.crop_left = 0.08;
                        c.transform.crop_right = 0.08;
                        c.set_speed(2.0, false);
                        // Two keys, so the row reads as animated and the clip
                        // carries diamonds.
                        let start = c.start;
                        c.set_key_at(Param::PosX, start + 0.5, 0.30, Interp::Ease);
                        c.set_key_at(Param::PosX, start + 3.0, -0.30, Interp::Ease);
                    }
                    // 2x halves the clip's length, so a playhead left where it
                    // was would now sit past its end and the preview would go
                    // black. Land it inside what the clip now occupies.
                    if let Some(c) = self.project.clip(cid) {
                        self.playhead = c.start + c.duration() * 0.45;
                    }
                }
                // Past the levels, onto the geometry and rate sections.
                self.inspector_scroll = (self.inspector_max_scroll(r) * 0.62).max(0.0);
            }
            ShotScenario::Curves => {
                if let Some(c) = self.project.top_video_clip_at(23.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    self.playhead = 23.0;
                    if let Some(c) = self.project.clip_mut(cid) {
                        // An S on the master and a lifted blue, so the
                        // chooser's other channels are not empty either.
                        c.curves.master.set_point(0.28, 0.14);
                        c.curves.master.set_point(0.72, 0.88);
                        c.curves.blue.set_point(0.5, 0.62);
                        c.curves.mix = 0.8;
                        // Keyed, so the amount row reads as animated and the
                        // clip body carries its diamonds.
                        let start = c.start;
                        c.set_key_at(Param::CurveMix, start + 0.4, 0.0, Interp::Ease);
                        c.set_key_at(Param::CurveMix, start + 3.0, 0.8, Interp::Ease);
                    }
                }
                // The section sits at the top of the controls, so nothing has
                // to be folded away for the whole of it to fit.
                self.inspector_scroll = 0.0;
                let head = self
                    .inspector_layout(r)
                    .sections
                    .iter()
                    .find(|(s, _)| *s == Section::Curves)
                    .map(|(_, rect)| rect.y0);
                if let Some(y) = head {
                    let want = y - self.inspector_viewport(r).y0;
                    self.inspector_scroll = want.clamp(0.0, self.inspector_max_scroll(r));
                }
            }
            ShotScenario::KeyAndMask => {
                if let Some(c) = self.project.top_video_clip_at(23.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    self.playhead = 23.0;
                    if let Some(c) = self.project.clip_mut(cid) {
                        c.key = crate::model::ChromaKey {
                            enabled: true,
                            color: crate::model::KEY_PRESETS[0].1,
                            tolerance: 0.28,
                            softness: 0.14,
                            spill: 0.55,
                        };
                        c.mask = crate::model::Mask {
                            shape: crate::model::MaskShape::Ellipse,
                            x: 0.08,
                            y: -0.05,
                            w: 0.62,
                            h: 0.7,
                            rotation: 0.0,
                            feather: 0.22,
                            invert: false,
                        };
                        // Keyed, so the rows read as animated and the clip
                        // body carries its diamonds.
                        let start = c.start;
                        c.set_key_at(Param::MaskX, start + 0.4, -0.2, Interp::Ease);
                        c.set_key_at(Param::MaskX, start + 3.0, 0.24, Interp::Ease);
                    }
                }
                // The two sections sit below the levels and the geometry.
                // Fold what is above them away *and* scroll their header to
                // the top of the panel, so the capture shows both whole
                // rather than however much happened to fit.
                self.collapsed.insert(Section::Transform);
                self.collapsed.insert(Section::Speed);
                self.inspector_scroll = 0.0;
                let head = self
                    .inspector_layout(r)
                    .sections
                    .iter()
                    .find(|(s, _)| *s == Section::Key)
                    .map(|(_, rect)| rect.y0);
                if let Some(y) = head {
                    // Land the header at the top of the scrolled body, whole.
                    let want = y - self.inspector_viewport(r).y0;
                    self.inspector_scroll = want.clamp(0.0, self.inspector_max_scroll(r));
                }
            }
            ShotScenario::TitleClip => {
                // Build the title through the model the app itself uses, then
                // select it — so what the capture shows is a real title clip,
                // not an inspector posed to look like one.
                let item = MediaItem {
                    id: Uuid::new_v4(),
                    name: "Coastline".into(),
                    kind: MediaKind::Text,
                    duration: 6.0,
                    width: self.project.width,
                    height: self.project.height,
                    fps: self.project.fps,
                    palette: 4,
                    path: String::new(),
                    text: Some(TextSource {
                        content: "Coastline".into(),
                        size: 0.16,
                        color: TITLE_COLORS[2],
                        align: TextAlign::Left,
                        backdrop: 0.25,
                        ..TextSource::default()
                    }),
                    ..Default::default()
                };
                let mid = item.id;
                self.project.media.push(item);
                let track = self
                    .project
                    .display_tracks()
                    .into_iter()
                    .find(|t| t.kind == TrackKind::Video)
                    .map(|t| t.id);
                // On the top lane's free stretch, after the logo bug and well
                // before the sunset, so it shares the lane with nothing.
                if let Some(track) = track
                    && let Some(cid) = self.project.add_clip(track, mid, 7.0)
                {
                    self.selection.select_only(cid);
                    self.selected_media = Some(mid);
                    self.playhead = 11.5;
                }
                self.project.add_marker(9.0, "act two", 1);
                self.project.add_marker(18.5, "sunset", 3);
                self.collapsed.insert(Section::Curves);
                self.collapsed.insert(Section::Transform);
                self.collapsed.insert(Section::Speed);
                // Land the section's own header near the top of the panel
                // rather than at a fraction of the scroll: the panel's height
                // moves whenever a section is added, and a fraction that once
                // framed the title would then frame whatever had taken its
                // place.
                self.inspector_scroll = 0.0;
                let head = self
                    .inspector_layout(r)
                    .sections
                    .iter()
                    .find(|(s, _)| *s == Section::Title)
                    .map(|(_, rect)| rect.y0);
                if let Some(y) = head {
                    let want = y - self.inspector_viewport(r).y0;
                    self.inspector_scroll = want.clamp(0.0, self.inspector_max_scroll(r));
                }
            }
            ShotScenario::Keyframes => {
                self.playhead = 6.0;
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        let start = c.start;
                        // Opacity ramps across the clip, with a key exactly
                        // under the playhead so the control shows its
                        // on-a-key state; brightness is animated but has no
                        // key here, so it shows the between-keys state.
                        c.set_key_at(Param::Opacity, start + 1.0, 0.15, Interp::Linear);
                        c.set_key_at(Param::Opacity, 6.0, 1.0, Interp::Ease);
                        c.set_key_at(Param::Opacity, start + 8.0, 0.4, Interp::Hold);
                        c.set_key_at(Param::Brightness, start + 2.0, 0.5, Interp::Linear);
                        c.set_key_at(Param::Brightness, start + 7.0, 0.8, Interp::Linear);
                    }
                }
                self.zoom = 42.0;
            }
            ShotScenario::KeyMenu => {
                self.playhead = 6.0;
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        let start = c.start;
                        c.set_key_at(Param::Opacity, start + 1.0, 0.2, Interp::Linear);
                        c.set_key_at(Param::Opacity, 6.0, 1.0, Interp::Ease);
                        c.set_key_at(Param::Opacity, start + 8.0, 0.4, Interp::Linear);
                    }
                }
                // Open the menu on the key the playhead is sitting on, from
                // the control's own rect — the same call the click makes.
                let l = self.inspector_layout(r);
                if let Some((_, rect)) = l.keys.iter().find(|(p, _)| *p == Param::Opacity).copied()
                {
                    self.open_key_menu(Param::Opacity, Point::new(rect.center().x, rect.y1));
                }
            }
            ShotScenario::Markers => {
                self.playhead = 14.0;
                self.project.add_marker(4.0, "cold open", 1);
                let act_two = self.project.add_marker(9.5, "act two", 3);
                self.project.add_marker(21.0, "sunset", 5);
                // One marker carries a note, so the list shows both shapes a
                // row takes: a name on its own, and a name with what it means
                // under it.
                if let Some(m) = self.project.marker_mut(act_two) {
                    m.note = "hold on the wide before the cut".into();
                }
                if let Some(c) = self.project.top_video_clip_at(23.0) {
                    let cid = c.id;
                    let start = c.start;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        c.add_marker(start + 1.5, "cutaway", 2);
                        // Re-timed, so the folded SPEED header has something
                        // to report — a fold must never hide state.
                        c.set_speed(2.0, true);
                    }
                }
                self.collapsed.insert(Section::Transform);
                self.collapsed.insert(Section::Speed);
                self.collapsed.insert(Section::Effects);
                self.inspector_scroll = self.inspector_max_scroll(r);
            }
            ShotScenario::ProjectFormat => {
                // Nothing selected, so the panel is in its project-level
                // state: the marker list and, under it, the format.
                self.selection.clear();
                self.selected_media = None;
                self.project.add_marker(4.0, "cold open", 1);
                // Vertical, which is the shape the editor could not previously
                // be told about at all.
                self.project.set_frame_size(1080, 1920);
                self.project.set_fps(24.0);
                self.collapsed.remove(&Section::Project);
                self.inspector_scroll = self.inspector_max_scroll(r);
            }
            ShotScenario::MultiSelect => {
                // Everything on the top video lane, plus an audio clip, so the
                // capture shows a selection spanning tracks.
                let mut ids: Vec<ClipId> = self
                    .lane_rects(r)
                    .into_iter()
                    .take(2)
                    .flat_map(|(tid, _, _)| self.project.clips_on(tid))
                    .map(|c| c.id)
                    .collect();
                ids.truncate(3);
                self.selection.set(ids);
                self.playhead = 8.0;
            }
            ShotScenario::LockedTrack => {
                // Lock the first video track and select a clip on it, so both
                // the lane hatch and the inspector's note are visible.
                if let Some((tid, _, _)) = self.lane_rects(r).into_iter().next() {
                    if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                        t.locked = true;
                    }
                    if let Some(c) = self.project.clips_on(tid).first() {
                        let (id, media) = (c.id, c.media);
                        self.selection.select_only(id);
                        self.selected_media = Some(media);
                    }
                }
                self.playhead = 8.0;
            }
            ShotScenario::Scrolled => {
                // Zoom well in, then park the view in the middle of the
                // timeline so the scrollbar thumb sits part-way along.
                self.zoom = 120.0;
                self.playhead = self.project.duration() * 0.5;
                self.scroll_to_time(self.playhead, r);
            }
            ShotScenario::ClipMenu => {
                if let Some((tid, _, lane)) = self.lane_rects(r).into_iter().next()
                    && let Some(clip) = self.project.clips_on(tid).first().copied()
                {
                    let cr = self.clip_rect(clip, &lane, r);
                    self.last_mouse = cr.center();
                    self.open_clip_menu(clip.id, Point::new(cr.center().x, cr.y0 + 10.0));
                }
            }
            ShotScenario::Marquee => {
                if let Some((_, _, lane)) = self.lane_rects(r).into_iter().next() {
                    let band = Rect::new(
                        r.lanes.x0 + 60.0,
                        lane.y0 - 6.0,
                        r.lanes.x0 + 430.0,
                        lane.y1 + 34.0,
                    );
                    self.drag = Some(Drag::Marquee {
                        origin: Point::new(band.x0, band.y0),
                        additive: false,
                    });
                    self.marquee = Some(band);
                    self.last_mouse = Point::new(band.x1, band.y1);
                    self.release_marquee(Point::new(band.x0, band.y0), false, r);
                    // Keep the band drawn: release_marquee only computes the
                    // selection, and the capture wants both on screen.
                    self.marquee = Some(band);
                }
            }
            _ => {}
        }
    }
}
