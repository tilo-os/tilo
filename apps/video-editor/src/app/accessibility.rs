#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

// ==========================================================================
// Accessibility
// ==========================================================================

/// Which transport button each index addresses, with the name a reader speaks.
///
/// The glyphs alone do not carry the meaning — two of the five pairs were once
/// drawn near-identically — so the name is the control's real label, not a
/// decoration on top of a picture that already says it.
const TRANSPORT_NAMES: [(TransportAction, &str); 5] = [
    (TransportAction::ToStart, "Jump to start"),
    (TransportAction::StepBack, "Step back one frame"),
    (TransportAction::PlayPause, "Play"),
    (TransportAction::StepFwd, "Step forward one frame"),
    (TransportAction::ToEnd, "Jump to end"),
];

impl VideoEditorApp {
    /// The media rows the bin currently lists, in painted order.
    pub(super) fn a11y_bin_items(&self) -> Vec<MediaId> {
        // `bin_order`, not the project's own order: that is the order the rows
        // are painted in, and a reader walking a differently-ordered list
        // would be told the second row is something the second row is not.
        // It also indexes `focus.bin`, so the keyboard cursor and the
        // highlighted row have to be counting the same list.
        self.bin_order()
    }

    /// The marker rows the inspector lists, in painted order: the reference a
    /// row addresses, the name it is drawn with, and what a reader is told
    /// about it beyond that name.
    ///
    /// Built from [`Project::all_markers`] -- the very call `layout_markers`
    /// makes -- so the spoken list and the drawn list cannot come apart.
    pub(super) fn a11y_marker_rows(&self) -> Vec<(MarkerRef, String, String)> {
        self.project
            .all_markers()
            .into_iter()
            .map(|(at, m, clip)| {
                let r = MarkerRef { id: m.id, clip, at };
                // The stand-in an unnamed marker is painted with. A reader
                // hearing nothing where the screen shows a word is being
                // told a different story about the same row.
                let name = if m.name.trim().is_empty() {
                    "Marker".to_string()
                } else {
                    m.name.clone()
                };
                let mut value = crate::timecode::timecode(at, self.project.fps);
                // A clip marker says which clip it travels with, because
                // moving that clip moves the marker -- the same thing the
                // row's tinted timecode says to the eye.
                if let Some(c) = clip.and_then(|c| self.project.clip(c)) {
                    value.push_str(", on ");
                    value.push_str(&self.a11y_clip_name(c));
                }
                // The note is a second painted line under the name. Leaving
                // it out here would be the tidier story, not the true one.
                if !m.note.trim().is_empty() {
                    value.push_str(", note: ");
                    value.push_str(m.note.trim());
                }
                (r, name, value)
            })
            .collect()
    }

    /// The inspector rows the keyboard can land on, in painted order: every
    /// section header (so a folded one can be opened without a mouse) followed
    /// by that section's controls when it is open.
    pub(super) fn a11y_inspector_rows(&self) -> Vec<InspectorRow> {
        let mut out = Vec::new();
        let clip = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .cloned();
        // The controls the panel draws **above** its first section header, in
        // painted order. This list is both the accessibility tree and the
        // keyboard walk, and it used to begin at the first header — so Opacity
        // was on screen, above the cursor's first stop, and unreachable.
        out.extend(self.a11y_top_rows(clip.as_ref()));
        for section in self.a11y_sections(clip.as_ref()) {
            out.push(InspectorRow::Header(section));
            if self.collapsed.contains(&section) {
                continue;
            }
            // The control that opens a section whose contents are conditional
            // on it. Emitted before the section's knobs, which is where it is
            // drawn, and without it the knobs are unreachable rather than
            // merely awkward.
            match section {
                Section::Key => out.push(InspectorRow::Pick(PickField::KeyEnable)),
                Section::Mask => out.extend(
                    crate::model::MaskShape::ALL
                        .iter()
                        .map(|s| InspectorRow::Pick(PickField::MaskShape(*s))),
                ),
                _ => {}
            }
            // The marker list is not a run of numeric knobs, so it is walked
            // on its own terms: one stop per marked instant, then the button
            // that adds another -- exactly the rows `layout_markers` draws.
            if section == Section::Markers {
                out.extend(
                    self.a11y_marker_rows()
                        .into_iter()
                        .map(|(m, _, _)| InspectorRow::Marker(m)),
                );
                out.push(InspectorRow::AddMarker);
                continue;
            }
            // The project's format is two preset rows and a button, not a run
            // of knobs — exactly the stops `layout_project` draws.
            if section == Section::Project {
                out.extend(
                    (0..FRAME_PRESETS.len()).map(|i| InspectorRow::Pick(PickField::Frame(i))),
                );
                out.extend((0..RATE_PRESETS.len()).map(|i| InspectorRow::Pick(PickField::Rate(i))));
                if self.format_source().is_some() {
                    out.push(InspectorRow::Pick(PickField::MatchFormat));
                }
                continue;
            }
            for f in Self::a11y_section_fields(section, clip.as_ref()) {
                out.push(InspectorRow::Field(f));
            }
            // The buttons a section draws *below* its knobs. Emitted after
            // them, which is where they are painted, and on the same condition
            // the layout uses — a stop on a control the panel is not drawing
            // would put a reader on a different panel from the one on screen.
            if section == Section::Mix
                && self
                    .selected_track()
                    .is_some_and(crate::model::Track::mix_is_set)
            {
                out.push(InspectorRow::Pick(PickField::MixReset));
            }
        }
        out
    }

    /// The stops the panel draws above its first section header: the clip's
    /// level or its grade, its two fades, the transition at its head cut, the
    /// transition's length when there is one, and the seven label colours.
    ///
    /// Built from the same `is_video` test the layout makes, so the walk is
    /// offered exactly the controls that are drawn — an audio clip has a
    /// volume slider where a video clip has four grade sliders, and neither
    /// is offered the other's.
    pub(super) fn a11y_top_rows(&self, clip: Option<&Clip>) -> Vec<InspectorRow> {
        let Some(clip) = clip else {
            return Vec::new();
        };
        let is_video = self
            .project
            .media(clip.media)
            .is_none_or(|m| m.kind.is_visual());
        let mut out: Vec<InspectorRow> = Vec::new();
        if is_video {
            for f in [
                NumField::Opacity,
                NumField::Brightness,
                NumField::Contrast,
                NumField::Saturation,
            ] {
                out.push(InspectorRow::Field(f));
            }
        } else {
            out.push(InspectorRow::Field(NumField::Volume));
        }
        out.push(InspectorRow::Field(NumField::FadeIn));
        out.push(InspectorRow::Field(NumField::FadeOut));
        out.extend(
            Transition::ALL
                .iter()
                .map(|t| InspectorRow::Pick(PickField::Transition(*t))),
        );
        // The length slider is laid out only when there is a ramp to size.
        if clip.transition != Transition::None {
            out.push(InspectorRow::Field(NumField::TransitionLen));
        }
        out.extend((0..7u8).map(|slot| InspectorRow::Pick(PickField::Label(slot))));
        // The link control, in the place it is drawn. Offered only when the
        // selection has something to link or unlink, exactly as the layout
        // decides — a walk that stopped on a control nobody can see would be
        // describing a different panel from the one on screen.
        if self.link_affordance().is_some() {
            out.push(InspectorRow::Pick(PickField::LinkToggle));
        }
        out
    }

    /// Which sections the inspector shows for the current selection. Title is
    /// only meaningful for a title clip; Markers belong to the project and are
    /// listed whether or not a clip is selected.
    pub(super) fn a11y_sections(&self, clip: Option<&Clip>) -> Vec<Section> {
        let Some(clip) = clip else {
            // Both project-level sections, in the order they are laid out.
            return vec![Section::Markers, Section::Project];
        };
        let media = self.project.media(clip.media);
        let is_title = media.is_some_and(|m| m.text.is_some());
        let is_video = media.is_none_or(|m| m.kind.is_visual());
        // The same `is_video` test the layout makes, so the reader is
        // offered exactly the sections that are drawn — no more and no
        // fewer. An audio clip has no picture to place, key, mask or
        // effect, and the panel draws none of those headers for one.
        let mut v = Vec::new();
        if is_video {
            v.push(Section::Curves);
            v.push(Section::Transform);
        }
        v.push(Section::Speed);
        v.push(Section::Mix);
        if is_video {
            v.push(Section::Key);
            v.push(Section::Mask);
        }
        if is_title {
            v.push(Section::Title);
        }
        if is_video {
            v.push(Section::Effects);
        }
        v.push(Section::Markers);
        v.push(Section::Project);
        v
    }

    /// The numeric knobs inside a section, in painted order.
    pub(super) fn a11y_section_fields(section: Section, clip: Option<&Clip>) -> Vec<NumField> {
        let Some(c) = clip else {
            return Vec::new();
        };
        match section {
            // The amount slider is the section's only ranged control; the
            // curve itself is a 2D editor, which is reported as its own node
            // rather than as a knob (see `a11y_section_summary`).
            Section::Curves => vec![NumField::CurveMix],
            Section::Transform => vec![
                NumField::ScaleX,
                NumField::ScaleY,
                NumField::PosX,
                NumField::PosY,
                NumField::Rotation,
                NumField::CropLeft,
                NumField::CropRight,
                NumField::CropTop,
                NumField::CropBottom,
            ],
            Section::Speed => vec![NumField::Speed],
            Section::Mix => vec![NumField::TrackGain, NumField::TrackPan],
            // The key's sliders are only laid out once the key is on, and
            // the mask's only once a shape is chosen. A reader offered
            // controls a sighted user cannot see would be navigating a
            // different panel from the one on screen.
            Section::Key if c.key.enabled => vec![
                NumField::KeyTolerance,
                NumField::KeySoftness,
                NumField::KeySpill,
            ],
            Section::Mask if c.mask.is_active() => vec![
                NumField::MaskX,
                NumField::MaskY,
                NumField::MaskW,
                NumField::MaskH,
                NumField::MaskRotation,
                NumField::MaskFeather,
            ],
            Section::Title => vec![NumField::TitleSize, NumField::TitleBackdrop],
            // `Project` carries no ranged knob: its two controls are preset
            // rows, which reach the walk as `InspectorRow::Pick`.
            Section::Key
            | Section::Mask
            | Section::Effects
            | Section::Markers
            | Section::Project => Vec::new(),
        }
    }

    /// A clip's accessible name: the media it places, so the reader says
    /// "Interview" rather than a UUID the user has never seen.
    pub(super) fn a11y_clip_name(&self, clip: &Clip) -> String {
        self.project
            .media(clip.media)
            .map_or_else(|| "Missing media".into(), |m| m.name.clone())
    }

    /// What a reader says about a clip beyond its name: where it sits, how long
    /// it runs, and which track it is on. Everything the eye reads off the bar's
    /// position and width, in words.
    pub(super) fn a11y_clip_value(&self, clip: &Clip) -> String {
        let fps = self.project.fps;
        let len = (clip.out_point - clip.in_point).max(0.0);
        let track = self
            .project
            .track(clip.track)
            .map_or_else(|| "track".into(), |t| t.name.clone());
        format!(
            "{} to {}, {} long, on {track}",
            crate::timecode::timecode(clip.start, fps),
            crate::timecode::timecode(clip.start + len, fps),
            crate::timecode::clock(len),
        )
    }

    /// Build one pane's group node, marking it focused when the keyboard is in
    /// it so the export layer can nominate an active descendant inside.
    pub(super) fn a11y_pane(pane: Pane, children: Vec<AccessNode>) -> AccessNode {
        AccessNode::new(Role::Group)
            .with_name(pane.label())
            .with_key(pane.key())
            .with_children(children)
    }

    /// The media bin's rows.
    pub(super) fn a11y_bin(&self) -> AccessNode {
        let items = self.a11y_bin_items();
        let n = items.len();
        let cursor = self.focus.pane == Pane::Bin;
        let rows: Vec<AccessNode> = items
            .iter()
            .enumerate()
            .filter_map(|(i, mid)| {
                let m = self.project.media(*mid)?;
                let projected = self.projected_media.contains(mid);
                let mut value =
                    format!("{}, {}", m.kind.label(), crate::timecode::clock(m.duration));
                if projected {
                    // A source whose bytes are not here yet is a real, visible
                    // state; saying so is the difference between "empty" and
                    // "not fetched".
                    value.push_str(", not downloaded yet");
                }
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_name(m.name.clone())
                        .with_value(value)
                        // Identity-keyed: a bin row keeps its node across a
                        // rename, a re-sort, or another import landing above it.
                        .with_hashed_key(m.id)
                        .with_position(i + 1, n)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(cursor && self.focus.bin == i)
                                .selected(self.selected_media == Some(*mid)),
                        ),
                )
            })
            .collect();
        if rows.is_empty() {
            return VideoEditorApp::a11y_pane(
                Pane::Bin,
                vec![AccessNode::new(Role::StaticText)
                    .with_name("No media yet. Press Ctrl+Shift+O to import, or drop files anywhere in the window.")
                    .with_key(0)],
            );
        }
        VideoEditorApp::a11y_pane(Pane::Bin, rows)
    }

    /// The five transport buttons, named.
    pub(super) fn a11y_transport(&self) -> AccessNode {
        let cursor = self.focus.pane == Pane::Transport;
        let mut buttons: Vec<AccessNode> = TRANSPORT_NAMES
            .iter()
            .enumerate()
            .map(|(i, (action, name))| {
                // The play button is the one whose name changes with state; a
                // reader must hear what pressing it will do now. A running
                // shuttle counts: pressing it there stops the shuttle, so
                // "Play" would name something the press does not do.
                let name = if *action == TransportAction::PlayPause && self.is_moving() {
                    "Pause"
                } else {
                    name
                };
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_key(ui::a11y::hash_key(&i))
                    .with_position(i + 1, TRANSPORT_NAMES.len())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(cursor && self.focus.transport == i),
                    )
            })
            .collect();
        // Undo / redo, exported only when the strip actually drew them, and
        // reporting `disabled` exactly when the verb is unavailable. A
        // control that cannot act must not be focusable — undoing the last
        // action empties the stack under the control you are standing on,
        // and a focus ring left on a dead button is how that reads wrong.
        for (which, _) in self.history_buttons(&self.regions()) {
            let can = self.undo.status().can(which);
            buttons.push(
                AccessNode::new(Role::Button)
                    .with_name(which.title())
                    .with_description(self.undo.status().description(which))
                    .with_key(ui::a11y::hash_key(&which.verb()))
                    .with_state(AccessState::new().focusable(can).disabled(!can)),
            );
        }
        // The preview hint's Go to clip, where it is drawn.
        if let Some(rect) = self.go_to_clip_button(&self.regions()) {
            buttons.push(
                AccessNode::new(Role::Button)
                    .with_name(super::go_to_clip::GO_TO_CLIP_LABEL)
                    .with_bounds(rect)
                    .with_key(ui::a11y::hash_key(&super::go_to_clip::GO_TO_CLIP_KEY))
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        let mut node = VideoEditorApp::a11y_pane(Pane::Transport, buttons);
        // The playhead is a live readout: it changes without focus moving, so
        // it rides on the pane as a value the reader can re-read on demand.
        node = node.with_value(crate::timecode::timecode(self.playhead, self.project.fps));
        node
    }

    /// Tracks as containers, clips as the items inside them, plus each track's
    /// mute / solo / lock toggles.
    pub(super) fn a11y_timeline(&self) -> AccessNode {
        let cursor = self.focus.pane == Pane::Timeline;
        let primary = self.selection.primary();
        let mut tracks: Vec<AccessNode> = Vec::new();
        let track_count = self.project.tracks.len();
        for (ti, track) in self.project.tracks.iter().enumerate() {
            let mut kids: Vec<AccessNode> = Vec::new();
            for (name, on) in [
                ("Mute", track.muted),
                ("Solo", track.solo),
                ("Lock", track.locked),
            ] {
                kids.push(
                    AccessNode::new(Role::ToggleButton)
                        .with_name(format!("{name} {}", track.name))
                        .with_hashed_key((track.id, name))
                        .with_state(AccessState::new().focusable(true).checked(on)),
                );
            }
            let clips = self.project.clips_on(track.id);
            let n = clips.len();
            for (i, clip) in clips.iter().enumerate() {
                kids.push(
                    AccessNode::new(Role::ListItem)
                        .with_name(self.a11y_clip_name(clip))
                        .with_value(self.a11y_clip_value(clip))
                        // Identity-keyed: a clip dragged along its track, or to
                        // another track, keeps the node an AT is holding.
                        .with_hashed_key(clip.id)
                        .with_position(i + 1, n)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(self.selection.contains(clip.id))
                                .focused(cursor && primary == Some(clip.id)),
                        ),
                );
            }
            // The lane's fader, its pan and whether it has been caught
            // clipping ride on the group's value. A meter is a picture, and a
            // picture is the one thing a reader cannot have — so everything
            // the bar says has to be said here too, in the place a reader is
            // already standing when they ask about a track.
            let level = self.mix.track(track.id);
            let mix_words = format!(
                ", gain {}, pan {}{}",
                track.gain_words(),
                track.pan_words(),
                if level.is_some_and(|l| l.clipped) {
                    ", clipping"
                } else {
                    ""
                }
            );
            tracks.push(
                AccessNode::new(Role::Group)
                    .with_name(track.name.clone())
                    .with_value(format!(
                        "{} track, {} clip{}{mix_words}",
                        match track.kind {
                            TrackKind::Video => "video",
                            TrackKind::Audio => "audio",
                        },
                        n,
                        if n == 1 { "" } else { "s" }
                    ))
                    .with_hashed_key(track.id)
                    .with_position(ti + 1, track_count)
                    .with_children(kids),
            );
        }
        if tracks.is_empty() {
            tracks.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Timeline is empty. Add media from the bin to start editing.")
                    .with_key(0),
            );
        }
        tracks.extend(self.a11y_timeline_empty(&self.regions()));
        // The master strip, as the first thing in the pane. A reader meets
        // the mix's state before the lanes, which is the order the screen
        // puts it in — and when it has clipped, the node is a *button*,
        // because pressing it walks the playhead to the next place it did.
        if let Some(words) = self.mix_words() {
            let clipped = self.mix.clipped();
            tracks.insert(
                0,
                AccessNode::new(if clipped {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(if clipped {
                    "Go to the next place the mix clipped"
                } else {
                    "Mix level"
                })
                .with_value(words)
                .with_key(ui::a11y::hash_key(&MIX_METER_KEY))
                .with_state(AccessState::new().focusable(clipped)),
            );
        }
        let mut node = VideoEditorApp::a11y_pane(Pane::Timeline, tracks);
        node = node.with_value(format!(
            "playhead at {}, {} of {} clips selected, timeline view starts at {}; Page Up and Page Down scroll the timeline",
            crate::timecode::timecode(self.playhead, self.project.fps),
            self.selection.len(),
            self.project.clips.len(),
            crate::timecode::timecode(self.scroll, self.project.fps),
        ));
        node
    }

    /// The inspector: one group per section carrying its folded state, with the
    /// section's controls inside when it is open. A folded header still states
    /// what it contains, so folding hides controls and never hides state.
    #[allow(
        clippy::indexing_slicing,
        reason = "inspector children are emitted from the same validated section list that supplies their stable semantic keys"
    )]
    pub(super) fn a11y_inspector(&self) -> AccessNode {
        let cursor = self.focus.pane == Pane::Inspector;
        let rows = self.a11y_inspector_rows();
        let clip = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .cloned();
        let markers = self.a11y_marker_rows();
        // With nothing selected on a project holding no clips at all, the
        // panel paints its empty state -- and, under it, the marker list,
        // which belongs to the project rather than to any clip. Returning
        // only the empty-state line here hid every marker on the screen from
        // a reader. The line is now a sibling of the sections, which is what
        // the paint does with it.
        let empty_state = (clip.is_none() && self.project.clips.is_empty()).then(|| {
            // The painted empty state's own words, so one state is not
            // described two ways.
            AccessNode::new(Role::StaticText)
                .with_name(format!("{INSPECTOR_EMPTY_TITLE}. {INSPECTOR_EMPTY_HINT}."))
                .with_key(0)
        });
        // Rows are a flat painted list but a nested tree: walk it once, opening
        // a group at each header and dropping the controls into it.
        let mut groups: Vec<AccessNode> = Vec::new();
        let mut current: Option<(Section, Vec<AccessNode>)> = None;
        for (i, row) in rows.iter().enumerate() {
            let focused = cursor && self.focus.inspector == i;
            match row {
                InspectorRow::Header(section) => {
                    if let Some((sec, kids)) = current.take() {
                        groups.push(self.a11y_section_node(sec, kids, false));
                    }
                    current = Some((*section, Vec::new()));
                    if let Some((_, kids)) = current.as_mut() {
                        // The header itself is the focus stop that folds and
                        // unfolds; it lives on the group as its own state, and
                        // the group carries `focused` for the cursor.
                        let _ = kids;
                    }
                    if focused {
                        groups.push(AccessNode::new(Role::Unknown).with_key(u64::MAX)); // placeholder, replaced below
                        groups.pop();
                    }
                }
                InspectorRow::Field(f) => {
                    let value = self.num_value(*f);
                    let node = AccessNode::new(Role::Slider)
                        .with_name(f.label())
                        .with_optional_value(value.map(|v| f.readout(v)))
                        // Keyed on the variant, not the label: two rows may
                        // legitimately read "Rotation" in their own sections.
                        .with_key(f.a11y_key())
                        .with_state(AccessState::new().focusable(true).focused(focused));
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
                InspectorRow::Marker(m) => {
                    let pos = markers.iter().position(|(r, _, _)| r.id == m.id);
                    let Some(pos) = pos else { continue };
                    let (_, name, value) = &markers[pos];
                    let node = Self::a11y_marker_node(m, name, value, pos, markers.len(), focused);
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
                InspectorRow::AddMarker => {
                    let node = AccessNode::new(Role::Button)
                        .with_name(ADD_MARKER_LABEL)
                        .with_hashed_key(ADD_MARKER_KEY)
                        .with_state(AccessState::new().focusable(true).focused(focused));
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
                InspectorRow::Pick(pk) => {
                    let node = self.a11y_pick_node(*pk, focused);
                    if let Some((_, kids)) = current.as_mut() {
                        kids.push(node);
                    } else {
                        groups.push(node);
                    }
                }
            }
        }
        if let Some((sec, kids)) = current.take() {
            groups.push(self.a11y_section_node(sec, kids, false));
        }
        if let Some(line) = empty_state {
            groups.insert(0, line);
        }
        // Re-mark the focused header now that every group exists: a header's
        // focus lives on its group node.
        if cursor && let Some(InspectorRow::Header(target)) = rows.get(self.focus.inspector) {
            for g in &mut groups {
                if g.key == Some(ui::a11y::hash_key(&target.label())) {
                    g.state.focused = true;
                }
            }
        }
        VideoEditorApp::a11y_pane(Pane::Inspector, groups)
    }

    /// One inspector section, carrying whether it is folded and — when it is —
    /// a summary of what is inside, so a reader never has to unfold a section
    /// to learn its state.
    /// One marker as a reader meets it: the name on the row, when it is, the
    /// clip it travels with and the note written on it -- then the row's three
    /// controls, which the pointer reveals on hover and a reader has no other
    /// way to reach.
    pub(super) fn a11y_marker_node(
        m: &MarkerRef,
        name: &str,
        value: &str,
        pos: usize,
        n: usize,
        focused: bool,
    ) -> AccessNode {
        let verbs: Vec<AccessNode> = MARKER_VERBS
            .iter()
            .map(|verb| {
                AccessNode::new(Role::Button)
                    .with_name(format!("{verb} {name}"))
                    .with_hashed_key((m.id, *verb))
                    .with_state(AccessState::new().focusable(true))
            })
            .collect();
        AccessNode::new(Role::ListItem)
            .with_name(name.to_string())
            .with_value(value.to_string())
            // Identity-keyed: a marker keeps the node an assistive technology
            // is holding across a rename, and across another marker landing
            // earlier on the timeline and pushing this one down the list.
            .with_hashed_key(m.id)
            .with_position(pos + 1, n)
            .with_description("Activate to move the playhead here")
            .with_state(AccessState::new().focusable(true).focused(focused))
            .with_children(verbs)
    }

    /// The clip the inspector is editing, if there is one. A convenience over
    /// the `selected()` + `clip()` pair the panel does constantly.
    pub(super) fn selected_clip(&self) -> Option<&Clip> {
        self.selected().and_then(|id| self.project.clip(id))
    }

    /// One picker segment or switch, as a reader meets it: a radio inside its
    /// group when it is one of a set, a plain button when it stands alone —
    /// and, either way, whether it is the one in force.
    pub(super) fn a11y_pick_node(&self, pk: PickField, focused: bool) -> AccessNode {
        let (name, role, selected) = match pk {
            PickField::Frame(i) => {
                let Some((label, w, h)) = FRAME_PRESETS.get(i) else {
                    return AccessNode::new(Role::Unknown).with_key(pk.a11y_key());
                };
                (
                    // The dimensions, not only the shorthand: "9:16" alone
                    // does not say what a reader is choosing.
                    format!("{label}, {w} by {h}"),
                    Role::RadioButton,
                    *w == self.project.width && *h == self.project.height,
                )
            }
            PickField::Rate(i) => {
                let Some(rate) = RATE_PRESETS.get(i) else {
                    return AccessNode::new(Role::Unknown).with_key(pk.a11y_key());
                };
                (
                    format!("{} fps", rate_words(*rate)),
                    Role::RadioButton,
                    (rate - self.project.fps).abs() < 1e-6,
                )
            }
            PickField::MatchFormat => (
                self.format_source().map_or_else(
                    || "Match media".to_string(),
                    |m| format!("Match the shape of {}", m.name),
                ),
                Role::Button,
                false,
            ),
            PickField::KeyEnable => {
                let on = self.selected_clip().is_some_and(|c| c.key.enabled);
                (
                    // The name says what pressing it will do now, the way the
                    // play button does — a switch whose name never changes
                    // leaves a reader guessing which way it is.
                    if on {
                        "Switch chroma key off".to_string()
                    } else {
                        "Switch chroma key on".to_string()
                    },
                    Role::Checkbox,
                    on,
                )
            }
            PickField::MaskShape(shape) => (
                format!("Mask shape {}", shape.label().to_lowercase()),
                Role::RadioButton,
                self.selected_clip().is_some_and(|c| c.mask.shape == shape),
            ),
            PickField::Transition(t) => (
                format!("Transition {}", t.label().to_lowercase()),
                Role::RadioButton,
                self.selected_clip().is_some_and(|c| c.transition == t),
            ),
            PickField::Label(slot) => (
                format!("Label colour {}", slot + 1),
                Role::RadioButton,
                self.selected_clip().is_some_and(|c| c.palette == slot),
            ),
            // A button, not a switch: the two directions are different
            // operations on different sets of clips, and its name says which
            // one pressing it will do — the same words the button carries.
            PickField::LinkToggle => (
                self.link_affordance().map_or_else(
                    || "Link audio and video".to_string(),
                    |o| o.label().to_string(),
                ),
                Role::Button,
                false,
            ),
            // Named for the lane it puts back, because a reader standing on it
            // has no fader in view to tell them which one that is.
            PickField::MixReset => (
                self.selected_track().map_or_else(
                    || "Reset gain and pan".to_string(),
                    |t| format!("Reset {} to 0.0 dB, centre", t.name),
                ),
                Role::Button,
                false,
            ),
        };
        // `checked` is what accesskit turns into a radio's or a checkbox's
        // "toggled": `selected` alone reads as a highlighted list row, which
        // is not what a reader needs to hear about a picker.
        let checkable = role != Role::Button;
        AccessNode::new(role)
            .with_name(name)
            .with_hashed_key(pk.tag())
            .with_state(AccessState {
                focusable: true,
                focused,
                selected,
                checked: checkable.then_some(selected),
                ..Default::default()
            })
    }

    /// Run whatever a pick row does — the same call the pointer runs on that
    /// control, so a reader and a mouse cannot do two different things.
    pub(super) fn a11y_activate_pick(&mut self, pk: PickField) -> bool {
        match pk {
            PickField::Frame(i) => {
                let Some((_, w, h)) = FRAME_PRESETS.get(i).copied() else {
                    return false;
                };
                self.set_project_frame_size(w, h);
                true
            }
            PickField::Rate(i) => {
                let Some(rate) = RATE_PRESETS.get(i).copied() else {
                    return false;
                };
                self.set_project_fps(rate);
                true
            }
            PickField::MatchFormat => {
                if self.format_source().is_none() {
                    return false;
                }
                self.match_project_format();
                true
            }
            // Each runs the very call the pointer runs on that control.
            PickField::KeyEnable => {
                if self.selected().is_none() {
                    return false;
                }
                self.toggle_key_enabled();
                true
            }
            PickField::MaskShape(shape) => {
                if self.selected().is_none() {
                    return false;
                }
                self.set_mask_shape(shape);
                true
            }
            PickField::Transition(t) => {
                if self.selected().is_none() {
                    return false;
                }
                self.set_transition(t);
                true
            }
            PickField::Label(slot) => {
                if self.selected().is_none() {
                    return false;
                }
                self.set_palette(slot);
                true
            }
            PickField::LinkToggle => {
                if self.link_affordance().is_none() {
                    return false;
                }
                self.toggle_link_selected();
                true
            }
            // Refused when the lane is already at unity and centre, which is
            // exactly when the panel draws no button: activating a control
            // that is not on screen would be a route only a reader has.
            PickField::MixReset => {
                if !self
                    .selected_track()
                    .is_some_and(crate::model::Track::mix_is_set)
                {
                    return false;
                }
                self.reset_selected_track_mix();
                true
            }
        }
    }

    pub(super) fn a11y_section_node(
        &self,
        section: Section,
        kids: Vec<AccessNode>,
        focused: bool,
    ) -> AccessNode {
        let folded = self.collapsed.contains(&section);
        AccessNode::new(Role::Group)
            .with_name(section.label())
            .with_optional_value(folded.then(|| self.a11y_section_summary(section)))
            .with_hashed_key(section.label())
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(focused)
                    .expanded(!folded),
            )
            .with_children(kids)
    }

    /// A clip's geometry as it is at the playhead: every channel read through
    /// `param_at`, so an animated one gives the value the compositor is
    /// actually placing the picture with.
    ///
    /// Cropping and the anchor are absent because the model does not animate
    /// them — they frame the shot rather than move it — so their constants
    /// really are their values, and a caller reads them off `clip.transform`.
    pub(super) fn geometry_at_playhead(&self, clip: &Clip) -> GeometryNow {
        let at = self.playhead;
        GeometryNow {
            scale_x: clip.param_at(Param::ScaleX, at),
            scale_y: clip.param_at(Param::ScaleY, at),
            pos_x: clip.param_at(Param::PosX, at),
            pos_y: clip.param_at(Param::PosY, at),
            rotation: clip.param_at(Param::Rotation, at),
        }
    }

    /// Whether any of the geometry channels carries a curve.
    ///
    /// A fact about the section in its own right, not merely a way of reading
    /// its numbers: keying a parameter from its default leaves the constant at
    /// exactly the aspect-fit, so a summary gated on the constants alone
    /// reported nothing at all about a clip whose picture crosses the frame.
    pub(super) fn transform_is_animated(clip: &Clip) -> bool {
        [
            Param::ScaleX,
            Param::ScaleY,
            Param::PosX,
            Param::PosY,
            Param::Rotation,
        ]
        .into_iter()
        .any(|p| clip.anim.is_animated(p))
    }

    /// What a folded section says about itself.
    ///
    /// Every animatable number here is read through `param_at` at the playhead
    /// — the one door every other reader (compositor, mixer, export, the
    /// sliders themselves) already goes through. Read as raw fields they told a
    /// reader the value the clip is *not* playing: a keyframe supersedes the
    /// constant, so a clip pushing in reported "scale 1.00×" and a key ramping
    /// on reported that it was keying nothing.
    pub(super) fn a11y_section_summary(&self, section: Section) -> String {
        let clip = self.selected().and_then(|id| self.project.clip(id));
        let at = self.playhead;
        match (section, clip) {
            (Section::Transform, Some(c)) => {
                let g = self.geometry_at_playhead(c);
                let mut s = format!(
                    "scale {:.2}×, position {:+.2}, {:+.2}, rotation {:.0}°",
                    g.scale_x, g.pos_x, g.pos_y, g.rotation
                );
                // Whether those numbers are going anywhere. Without it a
                // reader hears one still frame's worth of a moving picture and
                // has no way to know the section is animated at all.
                if Self::transform_is_animated(c) {
                    s.push_str(", animated");
                }
                s
            }
            (Section::Speed, Some(c)) => c.speed.label(),
            (Section::Mix, Some(c)) => match self.project.track(c.track) {
                Some(t) => {
                    let level = self.mix.track(t.id);
                    format!(
                        "{}, gain {}, pan {}{}",
                        t.name,
                        t.gain_words(),
                        t.pan_words(),
                        if level.is_some_and(|l| l.clipped) {
                            ", clipping"
                        } else {
                            ""
                        }
                    )
                }
                None => "no lane".into(),
            },
            // Worded from the same fields the visible summary reads, so a
            // reader is never told a tidier story than the screen tells.
            (Section::Key, Some(c)) => {
                let tolerance = c.param_at(Param::KeyTolerance, at);
                if !c.key.enabled {
                    "off".into()
                } else if tolerance <= 1e-6 {
                    "on, but the tolerance keys nothing".into()
                } else {
                    format!(
                        "keying {:.0}% tolerance, {:.0}% softness, {:.0}% spill removal",
                        tolerance * 100.0,
                        c.param_at(Param::KeySoftness, at) * 100.0,
                        c.param_at(Param::KeySpill, at) * 100.0
                    )
                }
            }
            (Section::Mask, Some(c)) => {
                if c.mask.is_active() {
                    format!(
                        "{} {:.0}% by {:.0}%, {}",
                        c.mask.shape.label().to_lowercase(),
                        c.param_at(Param::MaskW, at) * 100.0,
                        c.param_at(Param::MaskH, at) * 100.0,
                        if c.mask.invert {
                            "keeping the outside"
                        } else {
                            "keeping the inside"
                        }
                    )
                } else {
                    "off".into()
                }
            }
            (Section::Effects, Some(c)) => {
                let n = c.effects.len();
                format!("{n} effect{}", if n == 1 { "" } else { "s" })
            }
            (Section::Markers, _) => {
                // Every marked instant the list shows -- the project's own and
                // each clip's, which is what `layout_markers` walks and what
                // the painted summary counts. Counting only the project's told
                // a reader a smaller number than the panel had rows for.
                let n = self.project.all_markers().len();
                format!("{n} marker{}", if n == 1 { "" } else { "s" })
            }
            // The same words the folded header shows, so a reader is never
            // told a tidier story than the panel tells.
            (Section::Curves, Some(c)) => {
                curve_summary(c, self.playhead).unwrap_or_else(|| "unchanged".into())
            }
            (Section::Title, Some(_)) => "title properties".into(),
            // Worded, not counted: the frame is the fact a reader needs from
            // this header whether or not it is folded.
            (Section::Project, _) => {
                format_words(self.project.width, self.project.height, self.project.fps)
            }
            _ => "collapsed".into(),
        }
    }

    /// The unsaved-work question, carrying the same three answers the card
    /// paints and the same account of what each one costs.
    pub(super) fn a11y_discard_dialog(&self) -> AccessNode {
        let Some(pending) = &self.pending_open else {
            return AccessNode::new(Role::Dialog).with_name("Save first?");
        };
        let opening = crate::project_file::recents::label(&pending.path);
        let kids = vec![
            AccessNode::new(Role::StaticText)
                .with_name("What happens")
                .with_value(format!(
                    "{} has never been saved to a file. Opening {opening} replaces it. \
                     Ctrl+Z brings it back.",
                    pending.losing,
                ))
                .with_key(0),
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_key(1)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Open anyway")
                .with_key(2)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Save…")
                .with_key(3)
                .with_state(AccessState::new().focusable(true)),
        ];
        AccessNode::new(Role::Dialog)
            .with_name(format!("Save {} first?", pending.losing))
            .with_key(ui::a11y::hash_key(&"discard"))
            .with_state(AccessState::new().modal(true))
            .with_children(kids)
    }

    /// The render's state as a node of its own, outside the dialog's subtree.
    ///
    /// `export_status` was read in exactly two places and both were inside the
    /// `show_export` gate, so an assistive technology whose user had dismissed
    /// the dialog was told nothing at all about a render that was still going:
    /// no progress, no outcome, no failure. This is the same live region in
    /// the tree whenever the dialog is not there to carry it — so a running
    /// export is announced exactly once at any moment, and never zero times.
    pub(super) fn a11y_export_status(&self) -> Option<AccessNode> {
        if self.presentation.export_open {
            // The dialog carries its own progress node; two live regions
            // reporting one render would narrate it twice.
            return None;
        }
        let key = ui::a11y::hash_key(&"export-status");
        match &self.export_status {
            ExportStatus::Idle => None,
            ExportStatus::Rendering { progress } => Some(
                AccessNode::new(Role::ProgressBar)
                    .with_name("Export progress")
                    .with_value(format!("{}%", (progress * 100.0).round() as i32))
                    .with_key(key)
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_state(AccessState::new().busy(true)),
            ),
            // Finished: operable, because activating it opens the dialog that
            // holds the whole path and the settings that produced it.
            ExportStatus::Done { path } => Some(
                AccessNode::new(Role::Button)
                    .with_name("Export finished")
                    .with_value(path.clone())
                    .with_key(key)
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_state(AccessState::new().focusable(true)),
            ),
            // A render that did not happen interrupts: the user is very
            // likely to act on the next thing believing a file exists.
            ExportStatus::Failed { reason } => Some(
                AccessNode::new(Role::Button)
                    .with_name("Export failed")
                    .with_value(reason.clone())
                    .with_key(key)
                    .with_live(ui::a11y::Politeness::Assertive)
                    .with_state(AccessState::new().focusable(true)),
            ),
        }
    }

    pub(super) fn a11y_export_dialog(&self) -> AccessNode {
        let mut kids = vec![
            // The same honest size line the dialog paints. On a box whose
            // encoders ignore their bitrate target this withholds the megabyte
            // figure rather than stating a confident wrong number — and a
            // reader must get that caveat too, not a tidier fiction.
            AccessNode::new(Role::StaticText)
                .with_name("Output")
                .with_value(format!(
                    "{}, {}, {}",
                    self.export.resolution.label(),
                    self.export.container.label(),
                    self.export.estimate_label(&self.project),
                ))
                .with_key(0),
        ];
        // The same actions the card paints: while a render runs, only its
        // Cancel; otherwise Export and Close.
        if self.export_running() {
            kids.push(
                AccessNode::new(Role::Button)
                    .with_name("Cancel export")
                    .with_key(2)
                    .with_state(AccessState::new().focusable(true)),
            );
        } else {
            kids.push(
                AccessNode::new(Role::Button)
                    .with_name("Start export")
                    .with_key(1)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .disabled(!self.export_is_offered()),
                    ),
            );
            kids.push(
                AccessNode::new(Role::Button)
                    .with_name("Close export")
                    .with_key(2)
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        if let ExportStatus::Rendering { progress } = self.export_status {
            kids.push(
                AccessNode::new(Role::ProgressBar)
                    .with_name("Export progress")
                    .with_value(format!("{}%", (progress * 100.0).round() as i32))
                    .with_key(3)
                    // A render's progress changes with no focus movement, so it
                    // must announce itself or the user is left guessing.
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_state(AccessState::new().busy(true)),
            );
        }
        if let ExportStatus::Failed { reason } = &self.export_status {
            // The window bar's Export opens this dialog, as Ctrl+E does from
            // the keyboard, and this value is
            // deliberately the complete worker/encoder message for a screen
            // reader.
            kids.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Export failure details")
                    .with_value(reason.clone())
                    .with_key(ui::a11y::hash_key(&"export.failure-details"))
                    .with_live(ui::a11y::Politeness::Assertive),
            );
        }
        AccessNode::new(Role::Dialog)
            .with_name("Export video")
            .with_key(ui::a11y::hash_key(&"export"))
            .with_state(AccessState::new().modal(true))
            .with_children(kids)
    }
}

impl VideoEditorApp {
    /// The shortcut sheet as accessible content — every binding readable in
    /// order, so the sheet is as useful to a reader as it is to the eye.
    pub(super) fn a11y_help_sheet() -> AccessNode {
        let mut kids: Vec<AccessNode> = Vec::new();
        let mut key = 0u64;
        for (group, rows) in SHORTCUTS {
            kids.push(
                AccessNode::new(Role::Heading { level: 2 })
                    .with_name(group)
                    .with_key(key),
            );
            key += 1;
            for (combo, what) in rows {
                kids.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(*what)
                        .with_value(*combo)
                        .with_key(key),
                );
                key += 1;
            }
        }
        kids.push(
            AccessNode::new(Role::Button)
                .with_name("Close shortcuts")
                .with_key(key)
                .with_state(AccessState::new().focusable(true)),
        );
        AccessNode::new(Role::Dialog)
            .with_name("Keyboard shortcuts")
            .with_key(ui::a11y::hash_key(&"help"))
            .with_state(AccessState::new().modal(true))
            .with_children(kids)
    }

    /// Move to the next / previous pane, clamping each pane's cursor to what it
    /// currently holds so Tab never lands on a row that has since been deleted.
    pub(super) fn focus_pane(&mut self, back: bool) {
        if self.bin_overlay_is_open() {
            self.focus.pane = Pane::Bin;
            self.clamp_pane_cursor();
            return;
        }
        self.focus.pane = self.focus.pane.step(back);
        self.clamp_pane_cursor();
    }

    /// Whether `pane` should paint the keyboard cursor right now.
    ///
    /// Gated on the host having actually given this widget the keyboard: a ring
    /// drawn while the focus is somewhere else entirely would point at a
    /// control the next key will not reach, which is the opposite of what a
    /// focus indicator is for. Only the pane holding the cursor paints one —
    /// the other three keep their cursors so Tab returns to them, but they are
    /// not where the keyboard is.
    pub(super) fn shows_focus_cursor(&self, pane: Pane) -> bool {
        self.presentation.keyboard_focus && self.focus.pane == pane
    }

    /// Move the keyboard cursor to whatever a press landed on.
    ///
    /// A press claims the keyboard, but until now it left the cursor wherever
    /// it already was — so clicking a bin row and then pressing Down carried
    /// on from some other row entirely, and the ring appeared somewhere the
    /// user had not been looking. The pointer and the keyboard address the
    /// same controls; a click is the plainest possible statement of which one
    /// you mean next.
    ///
    /// Resolved through the same geometry the ring and the hit test read, and
    /// the bin is matched by media id rather than by position so the painted
    /// order and the walked order cannot drift into disagreeing.
    pub(super) fn place_cursor_at(&mut self, r: &Regions, p: Point) {
        if let Some((mid, _)) = self
            .bin_item_rects(r)
            .into_iter()
            .find(|(_, rect)| rect.contains(p) && VideoEditorApp::bin_viewport(r).contains(p))
            && let Some(i) = self.a11y_bin_items().iter().position(|m| *m == mid)
        {
            self.focus.pane = Pane::Bin;
            self.focus.bin = i;
            return;
        }
        if let Some(i) = self
            .transport_buttons(r)
            .iter()
            .position(|(_, rect)| rect.contains(p))
        {
            self.focus.pane = Pane::Transport;
            self.focus.transport = i;
            return;
        }
        if self.inspector_viewport(r).contains(p) {
            let l = self.inspector_layout(r);
            let rows = self.a11y_inspector_rows();
            if let Some(i) = rows.iter().position(|row| {
                Self::inspector_row_rect(&l, *row).is_some_and(|rect| rect.contains(p))
            }) {
                self.focus.pane = Pane::Inspector;
                self.focus.inspector = i;
            }
        }
    }

    /// The box the keyboard cursor's ring is drawn in for one inspector row,
    /// resolved through the very layout the renderer and the hit test read.
    ///
    /// A slider row is more than its 9px track — the label above it and the key
    /// control at its head belong to the same stop — so a `Field`'s ring is
    /// grown up over them, which is also what keeps the ring inside the 34px
    /// row pitch instead of hugging a bar the eye reads as the value. Every
    /// other row is its own painted box already.
    ///
    /// `None` means the panel is not drawing that control right now, in which
    /// case there is nothing to ring.
    pub(super) fn inspector_focus_band(l: &InspectorLayout, row: InspectorRow) -> Option<Rect> {
        let rect = Self::inspector_row_rect(l, row)?;
        if matches!(row, InspectorRow::Field(_)) {
            return Some(Rect::new(
                rect.x0 - 3.0,
                rect.y0 - 27.0,
                rect.x1 + 3.0,
                rect.y1 + 2.0,
            ));
        }
        Some(rect.inset(2.0))
    }

    /// The rect the panel laid out for one walked row. The row list and the
    /// layout are built by different code, so this is the single place that
    /// maps between them — the keyboard cursor and the tests that prove every
    /// stop is a control the panel really drew both read it.
    pub(super) fn inspector_row_rect(l: &InspectorLayout, row: InspectorRow) -> Option<Rect> {
        match row {
            InspectorRow::Header(s) => l.sections.iter().find(|(x, _)| *x == s).map(|(_, r)| *r),
            InspectorRow::Field(NumField::Opacity) => l.opacity,
            InspectorRow::Field(NumField::Volume) => l.volume,
            InspectorRow::Field(NumField::Brightness) => l.brightness,
            InspectorRow::Field(NumField::Contrast) => l.contrast,
            InspectorRow::Field(NumField::Saturation) => l.saturation,
            InspectorRow::Field(NumField::FadeIn) => l.fade_in,
            InspectorRow::Field(NumField::FadeOut) => l.fade_out,
            InspectorRow::Field(NumField::TransitionLen) => l.transition_len,
            InspectorRow::Field(f) => l.num(f),
            InspectorRow::Marker(m) => l
                .marker_rows
                .iter()
                .find(|(x, _)| x.id == m.id)
                .map(|(_, r)| *r),
            InspectorRow::AddMarker => l.add_marker,
            InspectorRow::Pick(PickField::Frame(i)) => {
                FRAME_PRESETS.get(i).and_then(|(_, w, h)| {
                    l.project_frames
                        .iter()
                        .find(|((pw, ph), _)| pw == w && ph == h)
                        .map(|(_, r)| *r)
                })
            }
            InspectorRow::Pick(PickField::Rate(i)) => RATE_PRESETS.get(i).and_then(|rate| {
                l.project_rates
                    .iter()
                    .find(|(x, _)| (x - rate).abs() < 1e-9)
                    .map(|(_, r)| *r)
            }),
            InspectorRow::Pick(PickField::MatchFormat) => l.project_match,
            InspectorRow::Pick(PickField::KeyEnable) => l.key_enable,
            InspectorRow::Pick(PickField::MaskShape(shape)) => l
                .mask_shapes
                .iter()
                .find(|(x, _)| *x == shape)
                .map(|(_, r)| *r),
            InspectorRow::Pick(PickField::Transition(t)) => {
                l.transitions.iter().find(|(x, _)| *x == t).map(|(_, r)| *r)
            }
            InspectorRow::Pick(PickField::Label(slot)) => {
                l.colors.iter().find(|(x, _)| *x == slot).map(|(_, r)| *r)
            }
            InspectorRow::Pick(PickField::LinkToggle) => l.link,
            InspectorRow::Pick(PickField::MixReset) => l.mix_reset,
        }
    }

    pub(super) fn clamp_pane_cursor(&mut self) {
        let bin = self.a11y_bin_items().len();
        self.focus.bin = self.focus.bin.min(bin.saturating_sub(1));
        self.focus.transport = self.focus.transport.min(TRANSPORT_NAMES.len() - 1);
        let rows = self.a11y_inspector_rows().len();
        self.focus.inspector = self.focus.inspector.min(rows.saturating_sub(1));
    }

    /// Keep the keyboard/AT active descendant in the inspector's actual body
    /// viewport.  The fixed identity card deliberately is not scrollable, so
    /// its height must be part of this calculation rather than treating the
    /// whole side panel as available space.
    pub(super) fn reveal_inspector_focus(&mut self) {
        let r = self.regions();
        let rows = self.a11y_inspector_rows();
        let Some(row) = rows.get(self.focus.inspector).copied() else {
            return;
        };
        let Some(band) = Self::inspector_focus_band(&self.inspector_layout(&r), row) else {
            return;
        };
        let view = self.inspector_viewport(&r);
        if band.y0 < view.y0 {
            self.inspector_scroll -= view.y0 - band.y0;
        } else if band.y1 > view.y1 {
            self.inspector_scroll += band.y1 - view.y1;
        }
        let max = self.inspector_max_scroll(&r);
        self.inspector_scroll = self.inspector_scroll.clamp(0.0, max);
    }

    /// Arrow navigation inside the focused pane. Returns whether the key was
    /// consumed — the timeline deliberately consumes only Up/Down, leaving
    /// Left/Right to nudge the selection and step the playhead as every
    /// non-linear editor does.
    pub(super) fn nav_arrow(&mut self, up: bool, vertical: bool, state: &AutoCommit) -> bool {
        let _ = state;
        match self.focus.pane {
            Pane::Bin if vertical => {
                let n = self.a11y_bin_items().len();
                if n == 0 {
                    return false;
                }
                self.focus.bin = step_index(self.focus.bin, n, up);
                self.selected_media = self.a11y_bin_items().get(self.focus.bin).copied();
                true
            }
            Pane::Transport if !vertical => {
                self.focus.transport = step_index(self.focus.transport, TRANSPORT_NAMES.len(), up);
                true
            }
            Pane::Inspector if vertical => {
                let n = self.a11y_inspector_rows().len();
                if n == 0 {
                    return false;
                }
                self.focus.inspector = step_index(self.focus.inspector, n, up);
                self.reveal_inspector_focus();
                true
            }
            // Up/Down walk the clips across tracks; Left/Right stay editing
            // keys, which is what an editor's arrows mean.
            Pane::Timeline if vertical => self.step_clip_across_tracks(up),
            _ => false,
        }
    }

    /// Move the selection to the clip on the previous / next track that sits
    /// nearest the current one in time — the "same place, one track up" move,
    /// not an arbitrary index walk.
    #[allow(
        clippy::indexing_slicing,
        reason = "track stepping computes the target ordinal from the non-empty ordered track list before reading it"
    )]
    pub(super) fn step_clip_across_tracks(&mut self, up: bool) -> bool {
        let Some(current) = self
            .selection
            .primary()
            .and_then(|id| self.project.clip(id))
            .cloned()
        else {
            // Nothing selected: take the first clip on the first track that has one.
            if let Some(first) = self.project.clips.first().map(|c| c.id) {
                self.selection.select_only(first);
                return true;
            }
            return false;
        };
        let Some(cur_track) = self
            .project
            .tracks
            .iter()
            .position(|t| t.id == current.track)
        else {
            return false;
        };
        let n = self.project.tracks.len();
        // Walk outward until a track with any clip is found, so an empty track
        // between two full ones is stepped over rather than swallowing the key.
        for hop in 1..=n {
            let idx = if up {
                cur_track.checked_sub(hop)
            } else {
                (cur_track + hop < n).then_some(cur_track + hop)
            };
            let Some(idx) = idx else { break };
            let tid = self.project.tracks[idx].id;
            let clips = self.project.clips_on(tid);
            if clips.is_empty() {
                continue;
            }
            // Nearest by start time to where we already are.
            let best = clips
                .iter()
                .min_by(|a, b| {
                    (a.start - current.start)
                        .abs()
                        .total_cmp(&(b.start - current.start).abs())
                })
                .map(|c| c.id);
            if let Some(best) = best {
                self.selection.select_only(best);
                return true;
            }
        }
        false
    }

    /// Activate whatever the pane cursor is on — the keyboard's Enter, routed
    /// through the same `perform_action` an assistive technology uses, so
    /// the two can never do different things.
    pub(super) fn activate_focused(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        use ui::a11y::{Action, hash_key};
        let pane = self.focus.pane;
        let rest: Vec<u64> = match pane {
            Pane::Bin => match self.a11y_bin_items().get(self.focus.bin) {
                Some(m) => vec![hash_key(m)],
                None => return false,
            },
            Pane::Transport => vec![hash_key(&self.focus.transport)],
            Pane::Timeline => {
                let Some(clip) = self
                    .selection
                    .primary()
                    .and_then(|id| self.project.clip(id))
                else {
                    return false;
                };
                vec![hash_key(&clip.track), hash_key(&clip.id)]
            }
            Pane::Inspector => match self.a11y_inspector_rows().get(self.focus.inspector) {
                Some(InspectorRow::Header(sec)) => vec![hash_key(&sec.label())],
                // A marker's path runs through the MARKERS section, the same
                // shape a control's does.
                Some(InspectorRow::Marker(m)) => {
                    vec![hash_key(&Section::Markers.label()), hash_key(&m.id)]
                }
                Some(InspectorRow::AddMarker) => {
                    vec![
                        hash_key(&Section::Markers.label()),
                        hash_key(&ADD_MARKER_KEY),
                    ]
                }
                Some(InspectorRow::Field(f)) => {
                    let key = f.a11y_key();
                    Self::a11y_row_path(&self.a11y_inspector_rows(), self.focus.inspector, key)
                }
                Some(InspectorRow::Pick(pk)) => {
                    let key = pk.a11y_key();
                    Self::a11y_row_path(&self.a11y_inspector_rows(), self.focus.inspector, key)
                }
                None => return false,
            },
        };
        let mut path = vec![pane.key()];
        path.extend(rest);
        let root = automerge::ROOT;
        self.perform_action(&Action::Activate, &path, state, &root)
    }

    /// The hover label for whatever icon-only control sits under `p`, or
    /// `None` over anything that already carries its own words.
    pub(super) fn tooltip_at(&self, p: Point) -> Option<&str> {
        let r = self.regions();
        for (action, rect) in self.transport_buttons(&r) {
            if rect.contains(p) {
                return Some(match action {
                    TransportAction::ToStart => "Jump to start",
                    TransportAction::StepBack => "Step back one frame",
                    TransportAction::PlayPause => {
                        if self.is_moving() {
                            "Pause"
                        } else {
                            "Play"
                        }
                    }
                    TransportAction::StepFwd => "Step forward one frame",
                    TransportAction::ToEnd => "Jump to end",
                });
            }
        }
        None
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "bin commands decode only the fixed key layout emitted by a11y_bin"
    )]
    pub(super) fn a11y_bin_action(&mut self, action: &ui::a11y::Action, rest: &[u64]) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&key) = rest.first() else {
            return false;
        };
        let items = self.a11y_bin_items();
        // A row an AT is holding can be gone; match nothing rather than acting
        // on whichever import now occupies that slot.
        let Some(i) = items.iter().position(|m| hash_key(m) == key) else {
            return false;
        };
        self.focus.pane = Pane::Bin;
        self.focus.bin = i;
        match action {
            Action::Focus => {
                self.selected_media = Some(items[i]);
                true
            }
            Action::Activate => {
                self.selected_media = Some(items[i]);
                self.add_media_to_timeline(items[i]);
                true
            }
            _ => false,
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "transport commands decode only the fixed key layout emitted by a11y_transport"
    )]
    pub(super) fn a11y_transport_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&key) = rest.first() else {
            return false;
        };
        if key == hash_key(&super::go_to_clip::GO_TO_CLIP_KEY) {
            return match action {
                Action::Focus => true,
                Action::Activate => self.go_to_clip(),
                _ => false,
            };
        }
        // History first: its keys are hashed from the verb, so they cannot
        // collide with the transport's positional ones.
        for which in [ui::UndoRequest::Undo, ui::UndoRequest::Redo] {
            if hash_key(&which.verb()) != key {
                continue;
            }
            return match action {
                Action::Focus => true,
                // `request` refuses an unavailable verb, so activating a node
                // the tree reported disabled does nothing rather than
                // pretending to.
                Action::Activate => self.undo.request(which),
                _ => false,
            };
        }
        let Some(i) = (0..TRANSPORT_NAMES.len()).find(|i| hash_key(i) == key) else {
            return false;
        };
        self.focus.pane = Pane::Transport;
        self.focus.transport = i;
        match action {
            Action::Focus => true,
            Action::Activate => {
                self.transport_action(TRANSPORT_NAMES[i].0);
                true
            }
            _ => false,
        }
    }

    pub(super) fn a11y_timeline_action(&mut self, action: &ui::a11y::Action, rest: &[u64]) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&track_key) = rest.first() else {
            return false;
        };
        // The master strip's node. Activating it is the same walk the red
        // chip does under the pointer — one route, so a reader and a mouse
        // land in the same place.
        if track_key == hash_key(&super::timeline_empty::TIMELINE_EMPTY_KEY) {
            return self.a11y_timeline_empty_action(action, rest.get(1..).unwrap_or_default());
        }
        if track_key == hash_key(&MIX_METER_KEY) {
            return match action {
                Action::Focus => true,
                Action::Activate => self.go_to_next_clipping(),
                _ => false,
            };
        }
        let Some(track) = self
            .project
            .tracks
            .iter()
            .find(|t| hash_key(&t.id) == track_key)
        else {
            return false;
        };
        let tid = track.id;
        self.focus.pane = Pane::Timeline;
        let Some(&child_key) = rest.get(1) else {
            // The track itself: focus is all a container affords.
            return matches!(action, Action::Focus);
        };
        // A track's own toggles come first, then its clips.
        for name in ["Mute", "Solo", "Lock"] {
            if hash_key(&(tid, name)) == child_key {
                return match action {
                    Action::Focus => true,
                    Action::Activate | Action::ToggleCheck => {
                        match name {
                            "Mute" => self.toggle_track_mute(tid),
                            "Solo" => self.toggle_track_solo(tid),
                            _ => self.toggle_track_lock(tid),
                        }
                        true
                    }
                    _ => false,
                };
            }
        }
        let Some(clip) = self
            .project
            .clips_on(tid)
            .into_iter()
            .find(|c| hash_key(&c.id) == child_key)
            .map(|c| c.id)
        else {
            return false;
        };
        match action {
            Action::Focus => {
                self.selection.set_primary(clip);
                true
            }
            Action::Activate => {
                // Activating a clip selects it alone and parks the playhead at
                // its head — what double-clicking it does.
                self.selection.select_only(clip);
                if let Some(c) = self.project.clip(clip) {
                    self.playhead = c.start;
                    self.playback.chase_playhead = true;
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn a11y_inspector_action(
        &mut self,
        action: &ui::a11y::Action,
        rest: &[u64],
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let Some(&section_key) = rest.first() else {
            return false;
        };
        self.focus.pane = Pane::Inspector;
        let rows = self.a11y_inspector_rows();
        // A control lives inside its section, so its path is
        // `[inspector, section, control]`; the section header itself is one
        // level shorter. Address whichever the path actually names.
        let key = rest.get(1).copied().unwrap_or(section_key);
        for (i, row) in rows.iter().enumerate() {
            match row {
                InspectorRow::Header(sec) if rest.len() == 1 && hash_key(&sec.label()) == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    return match action {
                        Action::Focus => true,
                        Action::Activate | Action::Expand | Action::Collapse => {
                            if self.collapsed.contains(sec) {
                                self.collapsed.remove(sec);
                            } else {
                                self.collapsed.insert(*sec);
                            }
                            true
                        }
                        _ => false,
                    };
                }
                InspectorRow::Field(f) if f.a11y_key() == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    return match action {
                        Action::Focus => true,
                        Action::Increment => self.a11y_nudge_field(*f, 1.0),
                        Action::Decrement => self.a11y_nudge_field(*f, -1.0),
                        _ => false,
                    };
                }
                InspectorRow::Marker(m) if hash_key(&m.id) == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    let m = *m;
                    // `[inspector, MARKERS, marker, verb]` names one of the
                    // row's three buttons; one level shorter is the row
                    // itself, whose body seeks when it is clicked.
                    if let Some(&verb) = rest.get(2) {
                        return self.a11y_marker_verb(m, verb, action);
                    }
                    return match action {
                        Action::Focus => true,
                        Action::Activate => {
                            self.seek_to_marker(m);
                            true
                        }
                        _ => false,
                    };
                }
                InspectorRow::AddMarker if hash_key(&ADD_MARKER_KEY) == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    return match action {
                        Action::Focus => true,
                        Action::Activate => {
                            self.add_marker_here();
                            true
                        }
                        _ => false,
                    };
                }
                InspectorRow::Pick(pk) if pk.a11y_key() == key => {
                    self.focus.inspector = i;
                    self.reveal_inspector_focus();
                    let pk = *pk;
                    return match action {
                        Action::Focus => true,
                        Action::Activate => self.a11y_activate_pick(pk),
                        _ => false,
                    };
                }
                _ => {}
            }
        }
        false
    }

    /// One of a marker row's three buttons, addressed by an assistive
    /// technology. Each runs the same call the pointer runs on that button,
    /// so a reader and a mouse cannot do two different things to a marker.
    ///
    /// `Activate` opens the inline editor the pointer opens, for a user
    /// driving the real keyboard through the row. `SetValue` writes the text
    /// outright, through the same commit the inline editor ends at -- an
    /// assistive technology has no caret to type into an `EditableLabel`
    /// with, so an editor it cannot reach would be an affordance in name
    /// only. It reports the commit's own verdict, so the refusal of an empty
    /// name reaches the caller as a refusal rather than a silent no-op.
    pub(super) fn a11y_marker_verb(
        &mut self,
        m: MarkerRef,
        verb: u64,
        action: &ui::a11y::Action,
    ) -> bool {
        use ui::a11y::{Action, hash_key};
        let field = if hash_key(&(m.id, MARKER_VERBS[0])) == verb {
            MarkerField::Note
        } else if hash_key(&(m.id, MARKER_VERBS[1])) == verb {
            MarkerField::Name
        } else if hash_key(&(m.id, MARKER_VERBS[2])) == verb {
            return match action {
                Action::Focus => true,
                Action::Activate => {
                    self.delete_marker(m);
                    true
                }
                _ => false,
            };
        } else {
            return false;
        };
        match action {
            Action::Focus => true,
            Action::Activate => {
                self.begin_marker_edit(m, field);
                true
            }
            Action::SetValue(text) => {
                let text = text.clone();
                self.commit_marker_text(m, field, &text)
            }
            _ => false,
        }
    }

    /// Step a numeric control one twentieth of its span — the same grain a
    /// keyboard arrow gives a slider elsewhere in the toolkit — through the
    /// same `set_num` a mouse drag writes, then commit it the way releasing a
    /// drag does. An adjustment an assistive technology makes must land in the
    /// document exactly like a dragged one, or the two disagree about what the
    /// clip is.
    pub(super) fn a11y_nudge_field(&mut self, f: NumField, dir: f64) -> bool {
        let Some(v) = self.num_value(f) else {
            return false;
        };
        // A fade and a transition ramp are measured against the clip they sit
        // on, so a twentieth of *their* span is a twentieth of the clip — not
        // of `NumField::range`'s widest-possible pair, which would step an
        // eight-second clip's fade in three-minute jumps.
        let next = match self.field_span(f) {
            Some((lo, hi)) => (v + dir * 0.05 * (hi - lo)).clamp(lo, hi),
            None => f.at_frac((f.to_frac(v) + dir * 0.05).clamp(0.0, 1.0)),
        };
        self.set_num(f, next);
        if f.is_media() {
            self.dispatch_selected_media();
        } else if f.is_track() {
            // The lane fader writes the lane, exactly as the drag commits it.
            // Dispatching a clip write here would report success and change
            // nothing — a keyboard and a pointer must not disagree about what
            // they did.
            self.dispatch_selected_track();
        } else if let Some(id) = self.selected() {
            self.dispatch_clip(id);
        }
        true
    }

    pub(super) fn a11y_export_action(&mut self, action: &ui::a11y::Action, rest: &[u64]) -> bool {
        use ui::a11y::Action;
        let Some(&key) = rest.first() else {
            return false;
        };
        match (key, action) {
            // A disabled node that answers `true` tells an assistive
            // technology the action succeeded when nothing happened — the
            // node is correctly marked `disabled`, and this used to claim the
            // press worked anyway. The undo path above already refuses
            // properly; this now matches it.
            (1, Action::Activate) => {
                if self.export_is_offered() && !self.export_running() {
                    self.request_export();
                    true
                } else {
                    false
                }
            }
            // While a render runs this is its Cancel, as the card paints it.
            // Otherwise it closes, which is safe: the render reports from its
            // own live region whether or not this dialog is up.
            (2, Action::Activate) => {
                if self.export_running() {
                    if let Some(job) = self.export_job.as_ref() {
                        job.cancel();
                    }
                } else {
                    self.presentation.export_open = false;
                }
                true
            }
            (_, Action::Focus) => true,
            _ => false,
        }
    }
}

impl VideoEditorApp {
    /// The path a control's key hangs off: through its section header when it
    /// has one, and directly off the pane when it does not.
    ///
    /// Not every inspector control lives inside a section — the panel draws
    /// its level, grade, fade, transition and label controls **above** the
    /// first header — and the old lookup returned "no path" for those, which
    /// is one of the reasons pressing Enter on them did nothing.
    pub(super) fn a11y_row_path(rows: &[InspectorRow], i: usize, key: u64) -> Vec<u64> {
        let section = rows
            .get(..=i)
            .unwrap_or(rows)
            .iter()
            .rev()
            .find_map(|r| match r {
                InspectorRow::Header(sec) => Some(*sec),
                _ => None,
            });
        match section {
            Some(sec) => vec![ui::a11y::hash_key(&sec.label()), key],
            None => vec![key],
        }
    }
}

/// Step an index within `0..n`, clamping at both ends rather than wrapping:
/// arrow navigation that wraps silently teleports a reader from the last row
/// to the first, which reads as the list having restarted.
fn step_index(i: usize, n: usize, up: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if up {
        i.saturating_sub(1)
    } else {
        (i + 1).min(n - 1)
    }
}
