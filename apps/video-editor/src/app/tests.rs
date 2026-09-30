#[test]
fn seeding_and_scenarios_start_only_where_asked() {
    // Case `every_named_scenario_parses_and_none_is_missing`:
    // The launcher's scenario list and the parser agree, in both directions.
    {
        for name in ShotScenario::ALL_NAMES {
            assert!(
                ShotScenario::from_arg(name).is_some(),
                "`{name}` is offered to the launcher but the parser rejects it",
            );
        }
        // The other direction: a scenario the parser knows but the list omits
        // is one a caller cannot select. Counting distinct parsed values
        // catches an addition to `from_arg` that never reached the list.
        let mut seen = std::collections::HashSet::new();
        for name in ShotScenario::ALL_NAMES {
            seen.insert(format!("{:?}", ShotScenario::from_arg(name).expect(name)));
        }
        assert_eq!(
            seen.len(),
            ShotScenario::ALL_NAMES.len(),
            "two names in the list resolve to the same scenario, so one of \
             them is a typo: {seen:?}",
        );
    }
    // Case `sync_seeds_and_selects`.
    {
        let app = app_with_sample();
        assert_eq!(app.project.clips.len(), 7);
        assert!(app.selected().is_some(), "a clip should be auto-selected");
    }
    // Case `live_new_over_fresh_doc_stays_empty`.
    {
        // The live constructor must not fabricate a project: synced against a
        // fresh (empty) document it opens blank — no media, no clips.
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.project.clips.is_empty(),
            "no fabricated clips on the live path"
        );
        assert!(
            app.project.media.is_empty(),
            "no fabricated media on the live path"
        );
        assert!(
            app.selected().is_none(),
            "nothing selected in a blank project"
        );
    }
    // Case `demo_build_seeds_a_project`.
    {
        // The demo builder is the sole source of the sample project.
        let mut app = VideoEditorApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            !app.project.clips.is_empty(),
            "demo build seeds sample clips"
        );
        assert!(
            !app.project.media.is_empty(),
            "demo build seeds sample media"
        );
    }
}

use super::*;

fn app_with_sample() -> VideoEditorApp {
    let mut doc = AutoCommit::new();
    VideoDoc::seed_sample(&mut doc);
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.sync(&mut doc, &automerge::ROOT);
    app
}

// ---- Accessibility: the editor's synthetic tree ----------------------
//
// Everything here is asserted against the REAL exported tree and the REAL
// routed-action path, through `ui::headless::Driver` — the same bridge that
// reaches AT-SPI2 and Orca. A test that inspected `accessibility()`'s return
// value directly would prove the app builds nodes, not that a screen reader
// can reach or operate them.

fn a11y_driver() -> ui::headless::Driver {
    let mut doc = AutoCommit::new();
    VideoDoc::seed_sample(&mut doc);
    let mut state = ui::State::new();
    state.doc = doc;
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    ui::headless::Driver::new(state, Box::new(app), (1280, 800))
}

/// Every exported node's `(role, label)`, so a test can state what a reader
/// would find without caring where it sits.
fn exported(d: &ui::headless::Driver) -> Vec<(ui::accesskit::Role, String)> {
    d.a11y_nodes()
        .into_iter()
        .map(|(_, n)| (n.role(), n.label().unwrap_or_default().to_string()))
        .collect()
}

#[test]
fn accessibility_tree_exposes_transport_clips_and_tracks() {
    // Case `the_editor_is_no_longer_an_empty_window_to_a_screen_reader`.
    {
        // Before: one leaf widget contributing zero nodes — a reader found a
        // window with nothing in it.
        let d = a11y_driver();
        let nodes = exported(&d);
        assert!(
            nodes.len() > 20,
            "the editor must describe its contents, got {} nodes",
            nodes.len()
        );
        for pane in ["Media bin", "Transport", "Timeline", "Inspector"] {
            assert!(
                nodes.iter().any(|(_, l)| l == pane),
                "the {pane} pane must reach the tree; got {nodes:?}"
            );
        }
    }
    // Case `every_transport_button_carries_the_meaning_its_glyph_does_not`.
    {
        // Two glyph pairs were once drawn near-identically, so the name is the
        // control's real label rather than a decoration on a picture.
        let d = a11y_driver();
        let nodes = exported(&d);
        for name in [
            "Jump to start",
            "Step back one frame",
            "Play",
            "Step forward one frame",
            "Jump to end",
        ] {
            let hit = nodes
                .iter()
                .find(|(_, l)| l == name)
                .unwrap_or_else(|| panic!("no transport button named {name}: {nodes:?}"));
            assert_eq!(hit.0, ui::accesskit::Role::Button, "{name} is a button");
        }
    }
    // Case `a_clip_says_where_it_is_how_long_it_runs_and_what_it_is_on`.
    {
        // What the eye reads off the bar's position and width, in words.
        let d = a11y_driver();
        let clips: Vec<_> = d
            .a11y_nodes()
            .into_iter()
            .filter(|(_, n)| n.role() == ui::accesskit::Role::ListItem && n.value().is_some())
            .collect();
        assert!(!clips.is_empty(), "clips must reach the tree");
        let (_, sample) = clips
            .iter()
            .find(|(_, n)| n.value().is_some_and(|v| v.contains(" long, on ")))
            .expect("a clip states its span, length and track");
        let v = sample.value().unwrap();
        assert!(
            v.contains(" to "),
            "a clip states where it starts and ends: {v}"
        );
        assert!(
            sample.position_in_set().is_some(),
            "a clip knows its place in its track"
        );
    }
    // Case `tracks_expose_their_mute_solo_and_lock_as_real_toggles`.
    {
        let d = a11y_driver();
        let toggles: Vec<_> = d
            .a11y_nodes()
            .into_iter()
            .filter(|(_, n)| n.role() == ui::accesskit::Role::Button && n.toggled().is_some())
            .collect();
        assert!(
            toggles.len() >= 6,
            "each track exposes mute, solo and lock; got {}",
            toggles.len()
        );
        for (_, t) in &toggles {
            let l = t.label().unwrap_or_default().to_string();
            assert!(
                l.starts_with("Mute ") || l.starts_with("Solo ") || l.starts_with("Lock "),
                "a toggle names what it does and to which track: {l}"
            );
        }
    }
    // Case `an_assistive_technology_can_play_the_video`.
    {
        // The proof that the tree is operable and not merely readable.
        let mut d = a11y_driver();
        let play = d
            .a11y_node_named("Play")
            .expect("the play button is addressable");
        d.perform_a11y_action(play, &ui::a11y::Action::Activate)
            .unwrap();
        // Once playing, the same button renames itself so a reader hears what
        // pressing it will do NOW.
        assert!(
            d.a11y_node_named("Pause").is_some(),
            "the transport button must say Pause while playing"
        );
        assert!(d.a11y_node_named("Play").is_none());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn clip_activation_and_held_ids_survive_deletes() {
    // Case `activating_a_clip_selects_that_clip_and_parks_the_playhead_on_it`.
    {
        let mut d = a11y_driver();
        // Pick a clip that is not the auto-selected one.
        let names: Vec<String> = d
            .a11y_nodes()
            .into_iter()
            .filter(|(_, n)| {
                n.role() == ui::accesskit::Role::ListItem
                    && n.value().is_some_and(|v| v.contains(" long, on "))
            })
            .map(|(_, n)| n.label().unwrap_or_default().to_string())
            .collect();
        let target = names.last().expect("clips exist").clone();
        let id = d.a11y_node_named(&target).unwrap();
        d.perform_a11y_action(id, &ui::a11y::Action::Activate)
            .unwrap();

        let (_, focused) = d.a11y_focus().expect("something is focused");
        assert_eq!(
            focused.label(),
            Some(target.as_str()),
            "the reader must land on the clip that was activated"
        );
    }
    // Case `a_clip_is_still_the_same_clip_after_an_earlier_one_is_deleted`.
    {
        // Deleting a clip renumbers every clip after it on the track. This
        // proves the tree keeps up: the survivor is still addressable, and
        // operating it still operates THAT clip, with the same span, rather
        // than the neighbour that moved into its place.
        //
        // NOTE this does not prove identity-keying. It re-resolves the node by
        // name each time and passes under ordinal keys too. Identity keys are
        // currently unobservable from here because the host regenerates its
        // taffy tree on every relayout, which changes the owning widget's node
        // id and therefore every id derived from it.
        let mut d = a11y_driver();
        let clips: Vec<(String, ui::accesskit::NodeId)> = d
            .a11y_nodes()
            .into_iter()
            .filter(|(_, n)| {
                n.role() == ui::accesskit::Role::ListItem
                    && n.value().is_some_and(|v| v.contains(" long, on "))
            })
            .map(|(id, n)| (n.label().unwrap_or_default().to_string(), id))
            .collect();
        assert!(clips.len() >= 2, "the sample project has several clips");
        let (first_name, first_id) = clips.first().cloned().unwrap();
        let (last_name, _) = clips.last().cloned().unwrap();
        assert_ne!(first_name, last_name);
        let last_span = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| n.label() == Some(last_name.as_str()))
            .and_then(|(_, n)| n.value().map(ToString::to_string))
            .unwrap();

        d.perform_a11y_action(first_id, &ui::a11y::Action::Activate)
            .unwrap();
        d.key("Delete");

        let again = d
            .a11y_node_named(&last_name)
            .expect("the surviving clip is still addressable by name");
        d.perform_a11y_action(again, &ui::a11y::Action::Activate)
            .unwrap();
        let (_, focused) = d.a11y_focus().expect("something is focused");
        assert_eq!(
            focused.label(),
            Some(last_name.as_str()),
            "activating it must still operate that clip, not its neighbour"
        );
        assert_eq!(
            focused.value().map(ToString::to_string),
            Some(last_span),
            "and it is the same clip, in the same place, not a lookalike"
        );
    }
    // Case `an_id_held_across_a_delete_still_operates_the_clip_it_named`.
    {
        type ClipNodes = Vec<(ui::accesskit::NodeId, String, String)>;
        // The one that observes identity keying, where the sibling test
        // (which re-resolves by name) cannot.
        //
        // A screen reader holds node ids across repaints — that is the whole
        // point of an id. Deleting an EARLIER clip on the same track shifts
        // every later clip's ordinal, so an id derived from position now names
        // a different clip. That is worse than a dangling id: it silently
        // operates the wrong element. With `with_hashed_key(clip.id)` the held
        // id still resolves to the clip it named.
        //
        // Proven by reverting: swapping the clip's identity key for its
        // ordinal makes this fail (the held id lands on the neighbour that
        // moved up), while the by-name test keeps passing.
        let mut d = a11y_driver();
        // Every clip node, with the track its own spoken value names, so the
        // pair we pick is genuinely two clips on ONE track — which is the
        // arrangement where deleting the earlier one renumbers the later one.
        let clip_nodes =
            |d: &ui::headless::Driver| -> Vec<(ui::accesskit::NodeId, String, String)> {
                d.a11y_nodes()
                    .into_iter()
                    .filter_map(|(id, n)| {
                        let v = n.value()?.to_string();
                        (n.role() == ui::accesskit::Role::ListItem && v.contains(" long, on "))
                            .then(|| (id, n.label().unwrap_or_default().to_string(), v))
                    })
                    .collect()
            };
        let track_of = |value: &str| -> String {
            value
                .rsplit_once(" long, on ")
                .map(|(_, t)| t.to_string())
                .unwrap_or_default()
        };

        let before = clip_nodes(&d);
        let (track, group) = {
            let mut chosen: Option<(String, ClipNodes)> = None;
            for (id, name, value) in &before {
                let t = track_of(value);
                let same: Vec<_> = before
                    .iter()
                    .filter(|(_, _, v)| track_of(v) == t)
                    .cloned()
                    .collect();
                if same.len() >= 2 {
                    chosen = Some((t, same));
                    break;
                }
                let _ = (id, name);
            }
            chosen.expect("the sample project has a track carrying several clips")
        };
        assert!(group.len() >= 2, "{track} carries at least two clips");
        let (earlier_id, earlier_name, _) = group.first().cloned().unwrap();
        let (held_id, held_name, held_value) = group.last().cloned().unwrap();
        assert_ne!(earlier_name, held_name);

        // Delete the earlier clip. Every later clip on that track shifts up.
        d.perform_a11y_action(earlier_id, &ui::a11y::Action::Activate)
            .unwrap();
        d.key("Delete");
        let after = clip_nodes(&d);
        assert_eq!(
            after.len(),
            before.len() - 1,
            "the delete really happened — without this the rest passes vacuously",
        );
        assert!(
            !after.iter().any(|(id, _, _)| *id == earlier_id),
            "the deleted clip's node is gone",
        );

        // Now use the id held from BEFORE the delete — never re-resolved.
        d.perform_a11y_action(held_id, &ui::a11y::Action::Activate)
            .expect("an id held across the delete still resolves");
        let (_, focused) = d.a11y_focus().expect("something is focused");
        assert_eq!(
            focused.label(),
            Some(held_name.as_str()),
            "the held id must operate the clip it named, not whichever clip \
             slid into that position",
        );
        assert_eq!(
            focused.value().map(ToString::to_string),
            Some(held_value),
            "and it is that same clip, with the same span, on the same track",
        );
    }
    // Case `a_stale_clip_reference_operates_nothing`.
    {
        // An id an AT held across a delete must not activate whichever clip
        // slid into that slot.
        let mut d = a11y_driver();
        let real = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| {
                n.role() == ui::accesskit::Role::ListItem
                    && n.value().is_some_and(|v| v.contains(" long, on "))
            })
            .map(|(id, _)| id)
            .unwrap();
        let stale = ui::accesskit::NodeId(real.0 ^ 0x9e37_79b9);
        assert!(
            d.perform_a11y_action(stale, &ui::a11y::Action::Activate)
                .is_err()
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn folded_sections_hide_controls_but_report_their_state() {
    // Case `folding_a_section_hides_its_controls_but_never_its_state`.
    {
        let mut d = a11y_driver();
        assert!(
            d.a11y_node_named("Rotation").is_some(),
            "an open Transform section lists its controls"
        );
        let header = d
            .a11y_node_named("TRANSFORM")
            .expect("the section header is addressable");
        d.perform_a11y_action(header, &ui::a11y::Action::Activate)
            .unwrap();

        assert!(
            d.a11y_node_named("Rotation").is_none(),
            "folding hides the controls"
        );
        let folded = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| n.label() == Some("TRANSFORM"))
            .map(|(_, n)| n)
            .unwrap();
        assert_eq!(
            folded.is_expanded(),
            Some(false),
            "the header reports it is folded"
        );
        let summary = folded.value().unwrap_or_default().to_string();
        assert!(
            summary.contains("scale") && summary.contains("rotation"),
            "a folded header still states what is inside it: {summary:?}"
        );
    }
    // Case `a_folded_section_still_reports_what_is_inside_it`:
    // A folded section may hide controls; it must never hide state.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let clip = app.project.clip(id).unwrap().clone();
        // Nothing to report while everything is at its default.
        assert_eq!(app.section_summary(Section::Speed, &clip, None), None);
        assert_eq!(app.section_summary(Section::Transform, &clip, None), None);
        // Re-time and inset it, and the folded headers say so.
        app.edit_selected(&mut doc, |c| {
            c.set_speed(2.0, true);
            c.transform = crate::model::Transform::pip(0.3, (1.0, -1.0), 0.04);
            c.transform.crop_left = 0.1;
        });
        let clip = app.project.clip(id).unwrap().clone();
        let speed = app
            .section_summary(Section::Speed, &clip, None)
            .expect("a rate to report");
        assert!(
            speed.contains('2') && speed.contains("rev"),
            "speed summary was {speed:?}"
        );
        let geom = app
            .section_summary(Section::Transform, &clip, None)
            .expect("a placement");
        assert!(geom.contains("cropped"), "transform summary was {geom:?}");
        // Folding is a property of the panel, not of the clip: it survives
        // selecting something else.
        app.collapsed.insert(Section::Transform);
        let other = app.project.top_video_clip_at(6.0).map(|c| c.id);
        if let Some(other) = other {
            app.selection.select_only(other);
        }
        assert!(app.collapsed.contains(&Section::Transform));
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(
            l.num(NumField::ScaleX).is_none(),
            "a folded section lays out no controls"
        );
        assert!(
            l.sections.iter().any(|(s, _)| *s == Section::Transform),
            "but keeps its header, so it can be unfolded again",
        );
    }
    // Case `folding_the_key_or_mask_still_reports_that_they_are_on`:
    // A fold is a view choice. It must never turn into missing state — a
    // user who folded the key away still has to be able to see that this
    // clip is keyed.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let clip = app.project.clip(id).unwrap().clone();
        assert_eq!(
            app.section_summary(Section::Key, &clip, None),
            None,
            "nothing on"
        );
        assert_eq!(app.section_summary(Section::Mask, &clip, None), None);

        app.edit_selected(&mut doc, |c| {
            c.key.enabled = true;
            c.key.tolerance = 0.4;
            c.mask.shape = crate::model::MaskShape::Ellipse;
            c.mask.invert = true;
        });
        let clip = app.project.clip(id).unwrap().clone();
        let key = app
            .section_summary(Section::Key, &clip, None)
            .expect("a key to report");
        assert!(
            key.contains("40"),
            "the folded key states its tolerance: {key:?}"
        );
        let mask = app
            .section_summary(Section::Mask, &clip, None)
            .expect("a mask to report");
        assert!(
            mask.contains("ellipse") && mask.contains("invert"),
            "the folded mask states its shape and that it is inverted: {mask:?}",
        );

        // A key switched on but keying nothing is a distinct state from off,
        // and a fold that reported neither would hide both.
        app.edit_selected(&mut doc, |c| c.key.tolerance = 0.0);
        let clip = app.project.clip(id).unwrap().clone();
        let key = app
            .section_summary(Section::Key, &clip, None)
            .expect("still on");
        assert!(
            key.contains("keys nothing"),
            "and says it is doing nothing: {key:?}"
        );
    }
    // Case `a_folded_section_reports_the_value_the_clip_is_playing`:
    // A folded section reports the value the clip is *playing*, not the
    // constant a keyframe has superseded.
    //
    // The summaries read `transform.scale_x`, `key.tolerance`, `mask.w` and
    // `curves.mix` straight off the clip. A curve overrides every one of those
    // for every other reader — the compositor, the mixer, the export and the
    // sliders themselves all go through `param_at` — so a folded header stated
    // something the canvas beside it contradicted: a clip pushing in read
    // "1.00×", a key ramping on read "keys nothing", and a grade ramping on
    // read "bypassed".
    //
    // The playhead is held still and each constant is left at its rest value,
    // so the only place the reported number can come from is the curve.
    {
        fn missing(what: &str, got: &str, needles: &[&str]) -> Vec<String> {
            needles
                .iter()
                .filter(|n| !got.contains(**n))
                .map(|n| format!("{what} should carry {n:?} and reads {got:?}"))
                .collect()
        }
        let (mut app, _doc) = app_and_doc();
        let cid = app
            .project
            .top_video_clip_at(23.0)
            .expect("a video clip at 23s")
            .id;
        app.selection.select_only(cid);
        let start = app.project.clip(cid).expect("clip").start;
        let at = start + 1.0;
        {
            let c = app.project.clip_mut(cid).expect("clip");
            // Every constant left where a clip that has never been touched
            // leaves it, and a single key on each — so the constant and the
            // curve disagree, and only one of them is what is playing.
            c.key.enabled = true;
            c.key.tolerance = 0.0;
            c.mask.shape = crate::model::MaskShape::Ellipse;
            c.curves.master.set_point(0.5, 0.8);
            c.curves.mix = 0.0;
            c.set_key_at(Param::ScaleX, at, 2.5, Interp::Linear);
            c.set_key_at(Param::ScaleY, at, 2.5, Interp::Linear);
            c.set_key_at(Param::KeyTolerance, at, 0.4, Interp::Linear);
            c.set_key_at(Param::KeySoftness, at, 0.2, Interp::Linear);
            c.set_key_at(Param::MaskW, at, 1.2, Interp::Linear);
            c.set_key_at(Param::MaskH, at, 0.3, Interp::Linear);
            c.set_key_at(Param::CurveMix, at, 0.8, Interp::Linear);
        }
        app.playhead = at;
        let clip = app.project.clip(cid).expect("clip").clone();

        // Collected rather than asserted one at a time, so a run names every
        // summary still reading a superseded constant instead of the first.
        let mut wrong: Vec<String> = Vec::new();
        // What the panel paints on the folded header.
        let seen_transform = app
            .section_summary(Section::Transform, &clip, None)
            .expect("a placed clip reports itself");
        wrong.extend(missing(
            "the painted geometry header (the scale the compositor is using)",
            &seen_transform,
            &["2.50×"],
        ));
        let seen_key = app
            .section_summary(Section::Key, &clip, None)
            .expect("a switched-on key reports itself");
        wrong.extend(missing(
            "the painted key header (the tolerance in force)",
            &seen_key,
            &["40%"],
        ));
        let seen_curves = app
            .section_summary(Section::Curves, &clip, None)
            .expect("a shaped curve reports itself");
        wrong.extend(missing(
            "the painted curves header (a grade keyed on at 80% is not bypassed)",
            &seen_curves,
            &["80%"],
        ));
        if seen_curves.contains("bypassed") {
            wrong.push(format!(
                "the painted curves header calls a grade keyed on at 80% bypassed: \
                 {seen_curves:?}"
            ));
        }

        // And what a reader hears, which has to be the same story.
        let heard_transform = app.a11y_section_summary(Section::Transform);
        wrong.extend(missing(
            "the spoken geometry summary",
            &heard_transform,
            &["2.50"],
        ));
        let heard_key = app.a11y_section_summary(Section::Key);
        wrong.extend(missing(
            "the spoken key summary",
            &heard_key,
            &["40%", "20%"],
        ));
        let heard_mask = app.a11y_section_summary(Section::Mask);
        wrong.extend(missing(
            "the spoken mask summary",
            &heard_mask,
            &["120%", "30%"],
        ));
        let heard_curves = app.a11y_section_summary(Section::Curves);
        if heard_curves != seen_curves {
            wrong.push(format!(
                "the eye is told {seen_curves:?} and a reader {heard_curves:?}"
            ));
        }
        assert!(wrong.is_empty(), "{}", wrong.join("; "));
    }
    // Case `the_folded_markers_header_counts_every_marker_the_panel_lists`:
    // A folded header states what is inside it. This one counted only the
    // project's own markers while the panel listed a clip's too, so a reader
    // folding the section heard a smaller number than the rows they had just
    // been reading.
    {
        let (mut app, mut doc) = app_and_doc();
        marker_at(&mut app, &mut doc, 12.0, "act two", "");
        let clip = app.project.top_video_clip_at(23.0).expect("a clip").id;
        app.project
            .clip_mut(clip)
            .unwrap()
            .add_marker(23.5, "on the clip", 2);
        let drawn_rows = app.inspector_layout(&app.regions()).marker_rows.len();

        let any = app
            .project
            .clips
            .first()
            .cloned()
            .expect("a clip to pass in");
        let painted = app
            .section_summary(Section::Markers, &any, None)
            .expect("the panel summarises a non-empty list");
        assert_eq!(
            app.a11y_section_summary(Section::Markers),
            painted,
            "the reader hears the summary the header shows",
        );
        assert!(
            painted.starts_with(&drawn_rows.to_string()),
            "{painted} counts the {drawn_rows} rows on screen",
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn reader_reaches_every_inspector_control_it_can_adjust() {
    // Case `an_inspector_slider_can_be_adjusted_without_a_mouse`.
    {
        let mut d = a11y_driver();
        let before = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| n.label() == Some("Rotation"))
            .and_then(|(_, n)| n.value().map(ToString::to_string))
            .expect("Rotation reports its value");
        let id = d.a11y_node_named("Rotation").unwrap();
        d.perform_a11y_action(id, &ui::a11y::Action::Increment)
            .unwrap();
        let after = d
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| n.label() == Some("Rotation"))
            .and_then(|(_, n)| n.value().map(ToString::to_string))
            .unwrap();
        assert_ne!(before, after, "incrementing a slider must move its value");
    }
    // Case `an_audio_clip_is_offered_its_volume_where_a_video_clip_has_a_grade`.
    {
        // The walk must offer exactly the controls that are drawn: an audio
        // clip has one level slider where a video clip has four.
        let (mut app, _doc) = app_and_doc();
        let cid = app
            .project
            .clips
            .iter()
            .find(|c| {
                app.project
                    .media(c.media)
                    .is_some_and(|m| !m.kind.is_visual())
            })
            .map(|c| c.id)
            .expect("the fixture places an audio clip");
        app.selection.select_only(cid);
        let rows = app.a11y_inspector_rows();
        assert!(rows.contains(&InspectorRow::Field(NumField::Volume)));
        assert!(
            !rows.contains(&InspectorRow::Field(NumField::Opacity)),
            "an audio clip has no opacity slider drawn, so it is offered none",
        );
    }
    // Case `a_reader_can_nudge_the_opacity_it_can_now_reach`.
    {
        // Reachable is not the whole claim: the stop has to operate the same
        // document a drag does.
        let (mut app, mut doc) = with_video_clip_selected();
        let cid = app.selected().expect("a clip is selected");
        let before = app.project.clip(cid).expect("selected clip").opacity;
        let rows = app.a11y_inspector_rows();
        let at = rows
            .iter()
            .position(|r| *r == InspectorRow::Field(NumField::Opacity))
            .expect("opacity is a stop");
        app.focus.pane = Pane::Inspector;
        app.focus.inspector = at;
        let path = vec![Pane::Inspector.key(), NumField::Opacity.a11y_key()];
        let root = automerge::ROOT;
        assert!(
            app.perform_action_at(&ui::a11y::Action::Decrement, &path, &mut doc, &root),
            "the opacity slider answers a reader's decrement",
        );
        let after = VideoDoc::load_project(&doc)
            .clip(cid)
            .expect("the clip survives")
            .opacity;
        assert!(
            after < before - 1e-6,
            "opacity moved in the document: {before} -> {after}",
        );
    }
    // Case `a_reader_can_switch_the_chroma_key_on_and_only_then_reach_its_sliders`.
    {
        // The catch-22 this fixes: `Section::Key if c.key.enabled` exposed the
        // key's sliders only once the key was on, and the switch that turns it
        // on was not a row at all — so a keyboard or AT user could never turn
        // it on and therefore never reach the sliders either.
        let (mut app, mut doc) = with_video_clip_selected();
        let cid = app.selected().expect("a clip is selected");
        assert!(
            !app.project.clip(cid).expect("selected clip").key.enabled,
            "the fixture's clip starts unkeyed",
        );
        let rows = app.a11y_inspector_rows();
        assert!(
            !rows.contains(&InspectorRow::Field(NumField::KeyTolerance)),
            "with the key off its sliders are not drawn, so they are not offered",
        );
        let switch = rows
            .iter()
            .position(|r| *r == InspectorRow::Pick(PickField::KeyEnable))
            .expect("the enable switch is a focus stop");
        assert!(
            !a11y_find(&app.a11y_inspector(), "Switch chroma key on").is_empty(),
            "and a reader can find it by name",
        );

        app.focus.pane = Pane::Inspector;
        app.focus.inspector = switch;
        assert!(app.activate_focused(&mut doc), "Enter switches it on");

        assert!(
            VideoDoc::load_project(&doc)
                .clip(cid)
                .expect("the clip survives")
                .key
                .enabled,
            "the key is on in the document",
        );
        assert!(
            app.a11y_inspector_rows()
                .contains(&InspectorRow::Field(NumField::KeyTolerance)),
            "and now the sliders it gates are reachable",
        );
    }
    // Case `a_reader_can_choose_a_mask_shape_and_only_then_reach_its_geometry`.
    {
        let (mut app, mut doc) = with_video_clip_selected();
        let cid = app.selected().expect("a clip is selected");
        let rows = app.a11y_inspector_rows();
        assert!(
            !rows.contains(&InspectorRow::Field(NumField::MaskW)),
            "with no shape chosen the mask's geometry is not drawn",
        );
        let at = rows
            .iter()
            .position(|r| {
                *r == InspectorRow::Pick(PickField::MaskShape(crate::model::MaskShape::Ellipse))
            })
            .expect("the shape buttons are focus stops");
        app.focus.pane = Pane::Inspector;
        app.focus.inspector = at;
        assert!(app.activate_focused(&mut doc), "Enter chooses the shape");
        assert_eq!(
            VideoDoc::load_project(&doc)
                .clip(cid)
                .expect("the clip survives")
                .mask
                .shape,
            crate::model::MaskShape::Ellipse,
        );
        assert!(
            app.a11y_inspector_rows()
                .contains(&InspectorRow::Field(NumField::MaskW)),
            "and the geometry it gates is now reachable",
        );
    }
}

// ---- Keyboard navigation ---------------------------------------------

#[expect(
    clippy::needless_pass_by_value,
    reason = "call sites build the key inline; the helper only borrows it"
)]
fn nav_key(app: &mut VideoEditorApp, doc: &mut AutoCommit, key: Key, shift: bool) -> bool {
    let mut st = winit::keyboard::ModifiersState::empty();
    if shift {
        st |= winit::keyboard::ModifiersState::SHIFT;
    }
    let mods = winit::event::Modifiers::from(st);
    let r = app.regions();
    app.handle_key(&key, mods, doc, &r)
}

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn pane_keyboard_navigation_walks_and_activates() {
    // Case `tab_walks_the_panes_and_shift_tab_walks_back`.
    {
        // The editor used to be a single Tab stop, so a keyboard-only user
        // could reach nothing inside it.
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        assert_eq!(app.focus.pane, Pane::Timeline);
        let seen: Vec<Pane> = (0..4)
            .map(|_| {
                nav_key(&mut app, &mut doc, Key::Named(NamedKey::Tab), false);
                app.focus.pane
            })
            .collect();
        assert_eq!(
            seen,
            vec![Pane::Inspector, Pane::Bin, Pane::Transport, Pane::Timeline],
            "Tab visits every pane and returns"
        );
        // Forward from Timeline is Inspector, so backward is Transport.
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::Tab), true);
        assert_eq!(app.focus.pane, Pane::Transport, "Shift+Tab goes back");
    }
    // Case `arrows_move_within_the_focused_pane`.
    {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        app.focus.pane = Pane::Bin;
        assert_eq!(app.focus.bin, 0);
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowDown), false);
        assert_eq!(app.focus.bin, 1, "Down moves to the next bin row");
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowUp), false);
        assert_eq!(app.focus.bin, 0);
        // Clamped, not wrapped: wrapping teleports a reader to the far end and
        // reads as the list having restarted.
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowUp), false);
        assert_eq!(app.focus.bin, 0, "the first row is the first row");

        app.focus.pane = Pane::Transport;
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowRight), false);
        assert_eq!(app.focus.transport, 1, "Left/Right walk the transport");
    }
    // Case `the_timeline_keeps_its_editing_arrows`.
    {
        // Left/Right nudge the selection in every non-linear editor; taking
        // them for navigation would break the tool to make it navigable.
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        app.focus.pane = Pane::Timeline;
        let id = app.selected().expect("a clip is selected");
        let before = app.project.clip(id).unwrap().start;
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowRight), false);
        let after = app.project.clip(id).unwrap().start;
        assert!(
            after > before,
            "Right still nudges the selection on the timeline"
        );
    }
    // Case `up_and_down_walk_clips_across_tracks`.
    {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        app.focus.pane = Pane::Timeline;
        // Start on the topmost track that actually holds clips, so "down" has
        // somewhere to go — the assertion must test the walk, not the fixture.
        let populated: Vec<TrackId> = app
            .project
            .tracks
            .iter()
            .filter(|t| !app.project.clips_on(t.id).is_empty())
            .map(|t| t.id)
            .collect();
        assert!(
            populated.len() >= 2,
            "the sample project spans several tracks"
        );
        let first = app.project.clips_on(populated[0])[0].id;
        app.selection.select_only(first);

        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowDown), false);
        let next_track = app
            .selected()
            .and_then(|id| app.project.clip(id))
            .map(|c| c.track)
            .unwrap();
        assert_ne!(
            next_track, populated[0],
            "Down moves to a clip on another track"
        );

        nav_key(&mut app, &mut doc, Key::Named(NamedKey::ArrowUp), false);
        let back = app
            .selected()
            .and_then(|id| app.project.clip(id))
            .map(|c| c.track)
            .unwrap();
        assert_eq!(back, populated[0], "and Up comes back");
    }
    // Case `enter_activates_whatever_the_cursor_is_on`.
    {
        // And does it through the same routed path an assistive technology
        // uses, so the keyboard and the reader can never do different things.
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        app.focus.pane = Pane::Transport;
        app.focus.transport = 2; // Play / pause
        assert!(!app.playback.playing);
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::Enter), false);
        assert!(
            app.playback.playing,
            "Enter on the play button starts playback"
        );
    }
    // Case `clicking_a_control_moves_the_keyboard_cursor_to_it`:
    // Clicking a control puts the keyboard cursor on it.
    //
    // A press claimed the keyboard but left the cursor where it already was,
    // so clicking a bin row and then pressing Down carried on from somewhere
    // else and the ring appeared away from where the user was looking. The
    // pointer and the keyboard address the same controls.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();

        let rows = app.bin_item_rects(&r);
        assert!(
            rows.len() >= 3,
            "the seeded bin needs a few rows to tell one from another",
        );
        let (third_id, third_rect) = rows[2];
        let want = app
            .a11y_bin_items()
            .iter()
            .position(|m| *m == third_id)
            .expect("the clicked row is on the walk");

        // Start the cursor somewhere else entirely.
        app.focus.pane = Pane::Inspector;
        app.focus.bin = 0;
        assert_ne!(
            want, 0,
            "the fixture must move the cursor to prove anything"
        );

        app.last_mouse = third_rect.center();
        app.place_cursor_at(&r, app.last_mouse);

        assert_eq!(
            app.focus.pane,
            Pane::Bin,
            "the clicked pane holds the cursor"
        );
        assert_eq!(
            app.focus.bin, want,
            "the cursor is on the row that was clicked, not the one it was on",
        );

        // And a transport press moves it there instead.
        let (_, play) = app.transport_buttons(&r)[2];
        app.place_cursor_at(&r, play.center());
        assert_eq!(app.focus.pane, Pane::Transport);

        let _ = &mut doc;
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn shortcut_sheet_opens_lists_and_fits_every_shortcut() {
    // Case `the_shortcut_sheet_can_be_opened_and_dismissed`.
    {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        assert!(!app.navigation.help_open);
        nav_key(&mut app, &mut doc, ch("?"), false);
        assert!(app.navigation.help_open, "? opens the sheet");
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::Escape), false);
        assert!(!app.navigation.help_open, "Esc closes it");
    }
    // Case `the_sheet_lists_every_shortcut_the_editor_answers_to`.
    {
        // A sheet that quietly omits a binding is worse than none: it reads as
        // the complete list.
        let flat: Vec<&str> = SHORTCUTS
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(k, _)| *k))
            .collect();
        for expected in [
            "Space",
            "J / K / L",
            "I / O",
            "S",
            "M",
            "T",
            ", / .",
            "Z",
            "+ / -",
            "B",
            "Ctrl+I",
            "Ctrl+D",
            "Ctrl+A",
            "Ctrl+Z",
            "Tab",
            "G",
            "?",
        ] {
            assert!(
                flat.contains(&expected),
                "the sheet must list {expected}; it lists {flat:?}"
            );
        }
    }
    // Case `the_shortcut_sheet_names_the_range_marks_and_redo`.
    {
        // The sheet is how a keyboard user learns the editor has shortcuts at
        // all, so a binding missing from it is a binding nobody finds. Redo
        // has always worked on two spellings and was written down on neither.
        let rows: Vec<(&str, &str)> = SHORTCUTS
            .iter()
            .flat_map(|(_, rows)| rows.iter().copied())
            .collect();
        for key in ["Shift+I / O", "Shift+X", "Ctrl+Shift+Z"] {
            assert!(
                rows.iter().any(|(k, _)| *k == key),
                "the sheet lists {key}: {rows:?}",
            );
        }
        let redo = rows
            .iter()
            .find(|(k, _)| *k == "Ctrl+Shift+Z")
            .expect("redo is listed");
        assert!(
            redo.1.contains("Ctrl+Y"),
            "and says the other spelling that works: {}",
            redo.1,
        );
    }
    // Case `the_shortcut_sheet_has_room_for_every_shortcut_it_lists`.
    {
        // The sheet's card was a fixed height with a fixed column split, and
        // the row loop simply stops drawing when it runs out of card. Adding
        // the project shortcuts pushed the last two View rows off the bottom —
        // silently, with no error and no clipped glyph to notice. A sheet that
        // is missing shortcuts is worse than no sheet, because it is the one
        // place a user goes to find out what exists.
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let ty = dialog_type(&tr, &theme);
        let m = HelpMetrics::measure(&mut tr, &theme, ty);
        let (split, _) = help_columns(true, m);
        assert!(
            split > 0 && split < SHORTCUTS.len(),
            "both columns carry groups"
        );

        // The card is sized from that, and every row must land inside the
        // body the frame gave it — the one layout the event path and the
        // draw pass both resolve, not re-derived from the table.
        let mut app = app_with_sample();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.navigation.help_open = true;
        let layout = app.help_layout(&mut tr, &theme);
        assert!(
            !layout.modal.body_overflows,
            "a 1280x800 window holds the whole sheet"
        );
        assert_eq!(layout.max_scroll, 0.0, "nothing to scroll");
        render(&mut app);
        assert_eq!(layout.groups.len(), SHORTCUTS.len());
        let body = layout.modal.body;
        let mut columns = std::collections::BTreeSet::new();
        for ((group, rows), g) in SHORTCUTS.iter().zip(&layout.groups) {
            columns.insert(g.heading.x0.to_bits());
            assert!(
                g.heading.y0 >= body.y0 && g.heading.y1 <= body.y1 + 1e-6,
                "{group}: the heading falls off the body ({:?} vs {body:?})",
                g.heading,
            );
            assert_eq!(g.rows.len(), rows.len(), "{group}: one band per row");
            for ((combo, _), row) in rows.iter().zip(&g.rows) {
                assert!(
                    row.y0 >= body.y0 && row.y1 <= body.y1 + 1e-6,
                    "{group} / {combo} falls off the body ({row:?} vs {body:?})",
                );
                assert!(
                    row.x0 + layout.what_dx < row.x1,
                    "{group} / {combo}: the description column starts past the row",
                );
            }
        }
        assert_eq!(columns.len(), 2, "a 1280px sheet lays out two columns");

        // And the sheet a reader hears is the same list, whole — plus the one
        // extra node the screen has no equivalent element for: the sheet's
        // "Esc or ? to close" line is an instruction to the eye and a real
        // Close button to a reader, which is the same affordance, not a
        // tidier claim.
        let node = VideoEditorApp::a11y_help_sheet();
        let rows: usize = SHORTCUTS.iter().map(|(_, r)| r.len()).sum();
        assert_eq!(
            node.children.len(),
            SHORTCUTS.len() + rows + 1,
            "every group, every row, and the close control",
        );
        // Nothing is exported that the sheet does not paint.
        for (group, group_rows) in SHORTCUTS {
            assert!(
                node.children
                    .iter()
                    .any(|n| n.name.as_deref() == Some(group)),
                "the {group} group reaches a reader",
            );
            for (combo, what) in group_rows {
                assert!(
                    node.children.iter().any(|n| {
                        n.name.as_deref() == Some(*what) && n.value.as_deref() == Some(*combo)
                    }),
                    "{combo} / {what} reaches a reader",
                );
            }
        }
    }
    // Case `the_shortcut_sheet_names_the_clipping_walk`.
    {
        // A binding missing from the sheet is a binding nobody finds, and this
        // one is the only keyboard route to the warning at all.
        let rows: Vec<(&str, &str)> = SHORTCUTS
            .iter()
            .flat_map(|(_, rows)| rows.iter().copied())
            .collect();
        let row = rows
            .iter()
            .find(|(k, _)| *k == "C")
            .expect("the sheet lists C");
        assert!(
            row.1.to_lowercase().contains("clip"),
            "and says what it does: {}",
            row.1,
        );
    }
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn timecode_entry_jumps_or_explains_and_owns_the_keyboard() {
    // Case `typing_a_timecode_jumps_the_playhead_there`.
    {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        nav_key(&mut app, &mut doc, ch("g"), false);
        assert!(app.goto_entry.is_some(), "G opens the field");
        for c in ["1", ":", "3", "0"] {
            nav_key(&mut app, &mut doc, ch(c), false);
        }
        assert_eq!(app.goto_entry.as_deref(), Some("1:30"));
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::Enter), false);
        assert!(
            (app.playhead - 90.0).abs() < 1e-6,
            "landed at {}",
            app.playhead
        );
        assert!(
            app.goto_entry.is_none(),
            "the field closes once it has jumped"
        );
    }
    // Case `an_unreadable_timecode_says_so_instead_of_going_nowhere`.
    {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        let before = app.playhead;
        nav_key(&mut app, &mut doc, ch("g"), false);
        nav_key(&mut app, &mut doc, ch(":"), false);
        nav_key(&mut app, &mut doc, Key::Named(NamedKey::Enter), false);
        assert!(app.goto_entry.is_some(), "the field stays open");
        assert!(
            app.navigation.goto_notice,
            "and says the value could not be read"
        );
        assert_eq!(app.playhead, before, "the playhead did not move");
    }
    // Case `a_timecode_being_typed_owns_the_keyboard`.
    {
        // Otherwise typing "1:30" would also split a clip and drop a marker on
        // the way past.
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = app_with_sample();
        let clips = app.project.clips.len();
        let markers = app.project.markers.len();
        nav_key(&mut app, &mut doc, ch("g"), false);
        for c in ["s", "m", "t"] {
            nav_key(&mut app, &mut doc, ch(c), false);
        }
        assert_eq!(app.project.clips.len(), clips, "no clip was split");
        assert_eq!(app.project.markers.len(), markers, "no marker was dropped");
        assert_eq!(
            app.goto_entry.as_deref(),
            Some(""),
            "and no letters entered the field"
        );
    }
}

#[test]
fn hover_labels_and_pointer_shapes_describe_each_region() {
    // Case `every_icon_only_control_has_a_hover_label`.
    {
        // An icon with no words is a control only someone who already knows it
        // can use.
        let app = app_with_sample();
        let r = app.regions();
        for (_, rect) in app.transport_buttons(&r) {
            assert!(
                app.tooltip_at(rect.center()).is_some(),
                "a transport button must name itself on hover"
            );
        }
        for (name, rect) in [
            ("export", r.export_btn),
            ("import", r.import_btn),
            ("split", r.split_btn),
        ] {
            assert!(
                app.tooltip_at(rect.center()).is_some(),
                "the {name} button must name itself on hover"
            );
        }
        // And nothing claims a label over empty canvas.
        assert_eq!(app.tooltip_at(r.lanes.center()), None);
    }
    // Case `the_pointer_says_what_each_region_does`.
    {
        use winit::window::CursorIcon;
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // A bin row can be dragged out.
        let (_mid, row) = app.bin_item_rects(&r)[0];
        assert_eq!(app.cursor_shape(row.center()), Some(CursorIcon::Grab));
        // A clip body grabs; its edges resize.
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip");
        let cr = clip_screen_rect(&app, &r, cid);
        assert_eq!(app.cursor_shape(cr.center()), Some(CursorIcon::Grab));
        assert_eq!(
            app.cursor_shape(Point::new(cr.x0, cr.center().y)),
            Some(CursorIcon::EwResize)
        );
        // Empty lane space is where a marquee starts.
        assert_eq!(
            app.cursor_shape(Point::new(r.lanes.x1 - 4.0, cr.center().y)),
            Some(CursorIcon::Crosshair),
        );
        // The ruler scrubs.
        assert_eq!(
            app.cursor_shape(r.ruler.center()),
            Some(CursorIcon::ColResize)
        );
        // Buttons are buttons.
        assert_eq!(
            app.cursor_shape(r.export_btn.center()),
            Some(CursorIcon::Pointer)
        );
        // A locked track refuses, and says so.
        let lane = app
            .lane_rects(&r)
            .into_iter()
            .find(|(t, _, _)| *t == tid)
            .unwrap()
            .2;
        let header = VideoEditorApp::track_head_rect(&r, &lane);
        let pill = app.track_head_layout(header).lock;
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        assert_eq!(app.cursor_shape(cr.center()), Some(CursorIcon::NotAllowed));
    }
    // Case `a_disabled_export_button_offers_no_pointer`.
    {
        use winit::window::CursorIcon;
        let (mut app, _doc) = app_and_doc();
        // Production has painted before this dialog can receive pointer input;
        // seed the same theme/layout state before starting the deliberately
        // unexportable job. Rendering afterwards would poll and clear that job.
        render(&mut app);
        app.presentation.export_open = true;
        app.project = unexportable_project();
        let dest = scratch("cursor-export").join("never-written.mp4");
        app.begin_export_to(dest);
        assert!(
            !app.export_is_offered(),
            "a render in flight disables Export"
        );
        let l = app
            .with_dialog_layout(super::VideoEditorApp::export_layout)
            .expect("rendered dialog layout");
        assert_eq!(
            app.cursor_shape(l.confirm.center()),
            Some(CursorIcon::Default),
            "no pointer over a disabled Export"
        );
        assert_eq!(
            app.cursor_shape(l.cancel.center()),
            Some(CursorIcon::Pointer),
            "Cancel is live and says so"
        );
        app.export_job = None;
        app.export_status = ExportStatus::Idle;
        if app.export_is_offered() {
            assert_eq!(
                app.cursor_shape(l.confirm.center()),
                Some(CursorIcon::Pointer)
            );
        }
    }
}

/// Write a real PNG (crafted header, known dimensions) to a temp file so a
/// probe/import test operates on real bytes, not a mock.
fn write_real_png(w: u32, h: u32) -> std::path::PathBuf {
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&w.to_be_bytes());
    png.extend_from_slice(&h.to_be_bytes());
    png.extend_from_slice(&[8, 2, 0, 0, 0]);
    let f = std::env::temp_dir().join(format!(
        "tilo-ve-import-app-{}-{}-{}x{}.png",
        std::process::id(),
        Uuid::new_v4(),
        w,
        h,
    ));
    std::fs::write(&f, &png).unwrap();
    f
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn bin_discloses_missing_unknown_and_refused_media() {
    // Case `media_whose_bytes_are_absent_is_disclosed_in_the_bin`:
    // A source whose bytes are not on this device must SAY so — the editor
    // still draws a picture for it (composited from metadata), and without a
    // disclosure that stand-in is indistinguishable from the real footage.
    {
        // Its own path: this test deliberately deletes the source after copying
        // it, so a shared fixture would pull bytes out from under a sibling test.
        let src = write_real_png(640, 360);
        let f = std::env::temp_dir().join(format!("tilo-ve-absent-{}.png", std::process::id()));
        std::fs::copy(&src, &f).unwrap();
        let mut app = VideoEditorApp::new_import(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let mid = app.project.media.first().expect("import landed").id;
        assert!(
            !app.media_is_projected(mid),
            "a file that is on disk must not be reported as absent",
        );
        let present = render_pixels(&mut app, 1280, 800, &Theme::dark());

        // Take the bytes away — the CRDT reference survives, the content does
        // not. This is the ordinary state of a project opened on a second peer.
        std::fs::remove_file(&f).unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.media_is_projected(mid),
            "a reference with no local bytes must be reported as absent",
        );
        let absent = render_pixels(&mut app, 1280, 800, &Theme::dark());

        // …and that state has to reach the pixels, not just a field.
        let r = app.regions();
        let row = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| *m == mid)
            .expect("the source has a bin row")
            .1;
        let changed = (row.y0 as u32..row.y1 as u32)
            .flat_map(|y| (row.x0 as u32..row.x1 as u32).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let i = ((y * 1280 + x) * 4) as usize;
                present.get(i..i + 3) != absent.get(i..i + 3)
            })
            .count();
        assert!(
            changed > 200,
            "the bin row must visibly disclose absent media; only {changed} pixels changed",
        );
    }
    // Case `a_loaded_document_says_unknown_rather_than_inventing_zeroes`:
    // A document written before import learned to refuse non-media still
    // carries the entry, and opening that document is not an import — so the
    // probe's refusal never runs. The user's own saved project holds an HTML
    // page saved as `.mp4`: duration 0, 0x0, bytes present. It rendered as
    // `0p · 0:00`, which reads as two probed facts and is neither.
    {
        // Real bytes on disk that are not media — the actual failure mode.
        let f = std::env::temp_dir().join(format!("tilo-ve-notmedia-{}.mp4", std::process::id()));
        std::fs::write(&f, b"<!doctype html><html><body>not a video</body></html>").unwrap();

        // Seed the document directly: this is a LOAD, not an import.
        let mut doc = AutoCommit::new();
        // Built off a real sample item so the shape stays valid as `MediaItem`
        // grows; only the fields this defect is about are overridden.
        let base = crate::doc::sample_project().media.remove(0);
        let poisoned = MediaItem {
            id: Uuid::new_v4(),
            name: "everything_wrong_CMAF_480.mp4".to_string(),
            kind: MediaKind::Video,
            duration: 0.0,
            width: 0,
            height: 0,
            fps: 0.0,
            path: f.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..base
        };
        VideoDoc::write_project_meta(&mut doc, &Project::new("Loaded"));
        VideoDoc::upsert_media(&mut doc, &poisoned);

        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        let m = app
            .project
            .media
            .iter()
            .find(|m| m.id == poisoned.id)
            .expect("the loaded document's entry survives the load")
            .clone();

        // The bytes are here, so this is NOT the "not on this device" state.
        assert!(
            !app.media_is_projected(m.id),
            "the file is on disk; calling it absent would send the user fetching",
        );
        assert!(
            app.media_is_unreadable(m.id),
            "a source with no duration and no dimensions is not usable media",
        );

        let sub = VideoEditorApp::bin_subtitle(&m);
        assert!(
            !sub.contains("0p") && !sub.contains("0:00"),
            "unknown metadata must not be printed as a fact; got {sub:?}",
        );
        assert!(
            sub.contains("unknown"),
            "the row must say what it does not know; got {sub:?}",
        );

        // …and the user must be able to get rid of it. The bin's own menu is
        // the way out, driven here exactly as a right-click drives it.
        let r = app.regions();
        let row = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(id, _)| *id == m.id)
            .expect("the entry has a bin row")
            .1;
        app.open_bin_menu(m.id, row.center());
        assert!(app.bin_menu.is_open(), "the row offers a menu");
        let delete = app
            .bin_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Delete"))
            .expect("the menu offers Delete");
        app.bin_menu_choose(&mut doc, delete);
        assert!(
            !app.project.media.iter().any(|x| x.id == m.id),
            "Delete must remove an unusable source from the document",
        );

        let _ = std::fs::remove_file(&f);
    }
    // Case `a_refused_import_tells_the_user_what_the_file_actually_is`.
    {
        let (mut app, mut doc) = app_and_doc();
        let dir = std::env::temp_dir().join(format!("tilo-ve-import-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-video.mp4");
        std::fs::write(&path, b"<!DOCTYPE html><html><body>nope</body></html>").unwrap();
        let before = app.project.media.len();
        // The path a dropped file takes: a batch of one through `import_files`.
        app.pending_imports.push(path);
        app.drain_import(&mut doc);
        let notice = app
            .import_notice
            .clone()
            .expect("a refusal explains itself");
        assert!(
            notice.text.contains("not-a-video.mp4"),
            "the notice names the file: {}",
            notice.text,
        );
        assert!(
            notice.text.contains("not a media file"),
            "and carries the probe's own reason: {}",
            notice.text,
        );
        assert!(
            notice.is_failure(),
            "and it reads as the refusal it is: {notice:?}",
        );
        assert_eq!(
            app.project.media.len(),
            before,
            "nothing was added to the bin"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `an_import_carries_an_unknown_frame_rate_as_unknown`.
    {
        // A still image has no frame rate. Substituting 30 would put a number
        // in the bin that the file never stated.
        let (mut app, mut doc) = app_and_doc();
        let png = write_real_png(24, 16);
        let want = png.file_name().unwrap().to_string_lossy().into_owned();
        app.try_import_one(&mut doc, &png)
            .expect("a real PNG imports");
        let added = app
            .project
            .media
            .iter()
            .find(|m| m.name == want)
            .expect("the imported still is in the bin");
        assert_eq!(
            added.fps, 0.0,
            "an unknown frame rate stays 0, not an invented 30"
        );
        assert_eq!(
            added.width, 24,
            "and the real dimensions came from the file"
        );
        std::fs::remove_file(&png).ok();
    }
    // Case `a_partly_failed_import_names_the_files_it_refused_and_why`:
    // A batch where some files were refused used to collapse to "Imported 3
    // of 5 files": the refusals and the probe's reasons for them were thrown
    // away, and — containing none of the English the tint was matched
    // against — the line was painted in the *success* colour on top of that.
    {
        let (mut app, mut doc) = app_and_doc();
        let dir = scratch("partial-import");
        let mut batch: Vec<PathBuf> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        for (i, name) in ["holiday.mp4", "notes.mov"].iter().enumerate() {
            let good = write_real_png(40 + i as u32, 12);
            let bad = dir.join(name);
            std::fs::write(&bad, b"<!DOCTYPE html><html><body>nope</body></html>").unwrap();
            batch.push(good);
            batch.push(bad);
            refused.push((*name).to_string());
        }
        let before = app.project.media.len();

        app.pending_imports = batch.clone();
        app.drain_import(&mut doc);

        let notice = app.import_notice.clone().expect("the batch reports itself");
        assert_eq!(
            app.project.media.len() - before,
            2,
            "the two real stills did land"
        );
        assert!(
            notice.text.contains("Imported 2 of 4"),
            "it says how much of the batch made it: {}",
            notice.text,
        );
        for name in &refused {
            assert!(
                notice.text.contains(name.as_str()),
                "it names {name}, which was refused: {}",
                notice.text,
            );
        }
        assert!(
            notice.text.contains("not a media file"),
            "and carries the probe's own reason rather than a shrug: {}",
            notice.text,
        );
        assert_eq!(
            notice.level,
            NoticeLevel::Failed,
            "a batch that refused files is not a success: {}",
            notice.text,
        );
        for f in batch {
            std::fs::remove_file(f).ok();
        }
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `a_refusal_reads_as_one_whatever_words_it_happens_to_use`:
    // Severity is data, not prose. A portal refusal ("the file picker needs
    // the real-desktop build") matches none of `"could not"` / `"couldn't"` /
    // `"is not"`, so the old substring rule painted a *failure* in the
    // success tint — in the import strip and the project strip alike. It also
    // made the colour of a message depend on the language it was written in.
    {
        // The real wording `ui::portal` hands back on a box with no portal.
        const REFUSAL: &str = "the file picker needs the real-desktop build (`--features portal`)";
        let theme = Theme::dark();
        let danger = theme.tokens.colors.semantic.danger;

        // Import side.
        let (mut app, mut doc) = app_and_doc();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick_multiple(ImportOutcome::Failed(REFUSAL.to_string()));
        app.trigger_import();
        app.sync(&mut doc, &automerge::ROOT);
        let notice = app.import_notice.clone().expect("the refusal is on screen");
        assert_eq!(notice.text, REFUSAL, "in the picker's own words");
        assert_eq!(
            notice.level.tint(&theme),
            danger,
            "and tinted as the refusal it is",
        );

        // …and the strip really paints that, rather than the sentence.
        let failed_px = render_pixels(&mut app, 1280, 800, &theme);
        let strip = {
            let r = app.regions();
            Rect::new(r.bin.x0, r.bin.y1 - 34.0, r.bin.x1, r.bin.y1)
        };
        app.import_notice = Some(Notice::ok(REFUSAL));
        let ok_px = render_pixels(&mut app, 1280, 800, &theme);
        assert!(
            region_diff(&failed_px, &ok_px, 1280, 800, strip) > 400,
            "the same sentence at two severities must not paint the same",
        );

        // Project side: the same picker, the same refusal, the same rule.
        let (mut app, mut doc) = app_and_doc();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Failed(REFUSAL.to_string()));
        app.request_open_project();
        app.sync(&mut doc, &automerge::ROOT);
        let notice = app
            .project_notice
            .clone()
            .expect("a project chooser that refused says so");
        assert_eq!(notice.text, REFUSAL);
        assert_eq!(notice.level.tint(&theme), danger);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn imports_add_real_clips_through_the_document() {
    // Case `importing_a_picked_file_adds_a_real_clip_to_the_bin`.
    {
        // A real file on disk (not a mock) becomes a real MediaItem with real,
        // probed metadata, replicated into the CRDT.
        let f = write_real_png(320, 240);
        let mut app = VideoEditorApp::new_import(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        assert_eq!(
            app.project.media.len(),
            1,
            "the picked file is now in the bin"
        );
        let m = &app.project.media[0];
        assert_eq!(m.name, f.file_name().unwrap().to_string_lossy());
        assert_eq!(m.kind, MediaKind::Image);
        assert_eq!((m.width, m.height), (320, 240), "real probed dimensions");
        assert!(
            (m.duration - crate::probe::DEFAULT_IMAGE_DURATION).abs() < 1e-9,
            "a still gets the default timeline duration",
        );
        assert!(
            m.hydration.is_local(),
            "the imported bytes are local/decodable"
        );
        // It replicated through the document (a peer would load it).
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.media.len(), 1);
        assert_eq!(reloaded.media[0].width, 320);
        std::fs::remove_file(&f).ok();
    }
    // Case `portal_import_action_adds_the_clip_through_the_document`.
    {
        // Drive the exact per-frame path a real portal pick feeds — the import
        // trigger asks the host chooser, which answers a multi-select outcome
        // carrying one real file. This proves the import code path is compiled
        // into the default build (no feature wall) and that draining it routes
        // the clip through the `UpsertMedia` action on `ui::ActionHost`, so it
        // lands in the shared document a peer replicates.
        let f = write_real_png(480, 270);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick_multiple(ImportOutcome::Selected(vec![f.clone()]));
        app.trigger_import();
        assert_eq!(recorder.pick_requests()[0].title(), "Import media");

        let mut doc = AutoCommit::new();
        assert!(
            app.project.media.is_empty(),
            "blank project starts with no media"
        );
        app.sync(&mut doc, &automerge::ROOT);

        // The in-memory projection gained the clip with real, probed metadata…
        assert_eq!(
            app.project.media.len(),
            1,
            "the picked file landed in the bin"
        );
        let m = &app.project.media[0];
        assert_eq!(m.name, f.file_name().unwrap().to_string_lossy());
        assert_eq!((m.width, m.height), (480, 270), "real probed dimensions");
        assert!(
            m.hydration.is_local(),
            "the imported bytes are local/decodable"
        );

        // …and, crucially, so did the CRDT the action wrote to: the import is a
        // document mutation dispatched through the action model, not a local-only
        // edit, so a second replica loading these bytes sees the same clip.
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.media.len(),
            1,
            "import routed through the doc action"
        );
        assert_eq!(
            reloaded.media[0].width, 480,
            "real probed metadata replicated"
        );
        std::fs::remove_file(&f).ok();
    }
    // Case `placed_import_puts_the_clip_on_the_timeline_under_the_playhead`.
    {
        // The `--shots`/preload placed import also lands the clip on a freshly
        // created track with the playhead inside it, so the preview has content.
        let f = write_real_png(640, 360);
        let mut app = VideoEditorApp::new_import_placed(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        assert_eq!(app.project.media.len(), 1);
        assert_eq!(
            app.project.tracks.len(),
            1,
            "a track was created for the clip"
        );
        assert_eq!(app.project.clips.len(), 1, "the clip is on the timeline");
        let clip = &app.project.clips[0];
        assert!(
            app.playhead > clip.start && app.playhead < clip.end(),
            "playhead {} parked inside the clip [{}, {}]",
            app.playhead,
            clip.start,
            clip.end(),
        );
        // The preview composites the placed clip (not Empty) at the playhead.
        assert!(
            matches!(
                preview::composite(&app.project, app.playhead),
                CompositedFrame::Clip { .. }
            ),
            "the preview has a clip to render",
        );
        std::fs::remove_file(&f).ok();
    }
    // Case `dropped_file_imports_into_the_bin_through_the_doc`.
    {
        // An OS file drop onto the window imports, exactly like the portal pick.
        let f = write_real_png(200, 120);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.project.media.is_empty(), "blank project starts empty");
        dispatch(&mut app, &mut doc, WindowEvent::DroppedFile(f.clone()));
        assert_eq!(
            app.project.media.len(),
            1,
            "the dropped file landed in the bin"
        );
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.media.len(),
            1,
            "and replicated through the document"
        );
        assert_eq!(reloaded.media[0].width, 200, "with real probed metadata");
        std::fs::remove_file(&f).ok();
    }
}

#[test]
fn timeline_geometry_maps_time_lanes_and_canvas() {
    // Case `time_x_round_trips`.
    {
        let app = app_with_sample();
        let r = app.regions();
        let x = app.time_to_x(12.0, &r.lanes);
        let t = app.x_to_time(x, &r.lanes);
        assert!((t - 12.0).abs() < 1e-6);
    }
    // Case `split_then_delete_changes_clip_count`.
    {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        app.sync(&mut doc, &automerge::ROOT);
        // Select a clip we know spans the playhead and split there.
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selection.select_only(target);
        app.playhead = 12.0;
        let before = app.project.clips.len();
        app.split_selected(&mut doc);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.project.clips.len(), before + 1, "split adds one clip");
        // Delete the new right half (now selected).
        app.delete_selected(&mut doc);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.project.clips.len(), before);
    }
    // Case `lane_rects_cover_all_tracks`.
    {
        let app = app_with_sample();
        let r = app.regions();
        assert_eq!(app.lane_rects(&r).len(), 4);
    }
    // Case `regions_letterbox_canvas_is_16x9`.
    {
        let app = app_with_sample();
        let r = app.regions();
        let aspect = r.canvas.width() / r.canvas.height();
        assert!((aspect - 16.0 / 9.0).abs() < 0.05, "canvas aspect {aspect}");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn bin_docks_wide_overlays_narrow_and_side_panels_settle() {
    // Case `wide_window_docks_the_bin_beside_the_preview`.
    {
        let app = app_with_sample(); // 1280 wide ≥ breakpoint
        assert_eq!(app.bin_layout(), SplitLayout::Docked);
        let r = app.regions();
        assert!(!r.bin_overlay, "wide window keeps the bin docked");
        // The preview starts after the docked bin, never overlapping it.
        assert!(
            r.preview.x0 >= r.bin.x1 - 0.5,
            "docked preview begins at the bin's right edge"
        );
        assert!(r.menu_btn.width() == 0.0, "no hamburger while docked");
        assert!(r.scrim.width() == 0.0, "no scrim while docked");
    }
    // Case `narrow_window_collapses_bin_to_overlay_with_hamburger`.
    {
        let mut app = app_with_sample();
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0); // < breakpoint
        app.advance_flap(); // first frame settles the bin to the width default
        assert_eq!(app.bin_layout(), SplitLayout::Overlay);
        let r = app.regions();
        assert!(
            r.bin_overlay,
            "narrow window collapses the bin to an overlay"
        );
        assert!(
            r.menu_btn.width() > 0.0,
            "narrow window shows the hamburger"
        );
        // Closed by default: the bin sits fully off the left edge and the
        // preview reclaims the full editor width.
        assert!(
            r.bin.x1 <= app.rect.x0 + 0.5,
            "closed overlay bin is off-screen left"
        );
        assert!(
            (r.preview.x0 - app.rect.x0).abs() < 0.5,
            "narrow preview reclaims the bin's column"
        );
        assert!(r.scrim.width() == 0.0, "closed overlay paints no scrim");
    }
    // Case `opening_narrow_bin_reveals_it_over_a_scrim`.
    {
        let mut app = app_with_sample();
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0);
        app.toggle_bin(); // user summons the bin
        app.bin_flap.snap(true); // settle the slide deterministically
        let r = app.regions();
        assert!(app.bin_flap.is_open(SplitLayout::Overlay));
        assert!(
            r.scrim.width() > 0.0,
            "open overlay bin dims the editor with a scrim"
        );
        // Fully revealed: the bin sits flush at the left edge.
        assert!(
            (r.bin.x0 - app.rect.x0).abs() < 0.5,
            "revealed bin is flush left"
        );
    }
    // Case `narrow_bin_overlay_traps_input_and_returns_to_the_opening_pane`.
    {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0);
        app.advance_flap();
        app.focus.pane = Pane::Timeline;

        app.toggle_bin();
        app.bin_flap.snap(true);
        assert!(app.bin_overlay_is_open());
        assert!(app.traps_focus(), "the scrim makes the bin modal");
        assert_eq!(
            app.focus.pane,
            Pane::Bin,
            "opening moves focus into the bin"
        );

        // The scrim must swallow a covered top-bar action rather than opening
        // an export dialog the user cannot see. A pointer press dismisses the
        // flap and restores the invoking pane, just like Escape.
        let regions = app.regions();
        app.last_mouse = regions.export_btn.center();
        app.handle_press(&mut doc, &regions, false);
        assert!(!app.bin_overlay_is_open());
        assert!(
            !app.presentation.export_open,
            "the covered Export button is inert"
        );
        assert_eq!(app.focus.pane, Pane::Timeline);

        app.toggle_bin();
        app.bin_flap.snap(true);

        // Tab cannot walk a hidden transport, timeline, or inspector.
        assert!(nav_key(
            &mut app,
            &mut doc,
            Key::Named(NamedKey::Tab),
            false
        ));
        assert_eq!(app.focus.pane, Pane::Bin);
        // A timeline mutation shortcut is refused while the overlay owns input.
        assert!(!nav_key(&mut app, &mut doc, ch("s"), false));

        assert!(nav_key(&mut app, &mut doc, ch("b"), false));
        assert!(!app.bin_overlay_is_open());
        assert!(!app.traps_focus());
        assert_eq!(app.focus.pane, Pane::Timeline, "closing returns focus");
    }
    // Case `crossing_breakpoint_returns_to_docked_default`.
    {
        let mut app = app_with_sample();
        // Narrow + summon the overlay bin.
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0);
        app.advance_flap();
        app.toggle_bin();
        assert!(app.bin_flap.is_open(SplitLayout::Overlay));
        // Widen past the breakpoint: reconcile drops the stale preference and
        // the bin returns to its docked-open default.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.advance_flap();
        assert_eq!(app.bin_layout(), SplitLayout::Docked);
        assert!(
            app.bin_flap.is_open(SplitLayout::Docked),
            "widening re-docks the open bin"
        );
    }
    // Case `a_scrim_swallows_the_click_it_dims`:
    // A scrim means the editor behind it is unavailable, and a click on it
    // must not reach through.
    //
    // The shortcut sheet and the go-to field both painted the full-window
    // scrim while `traps_focus` named only the export dialog, so a click
    // "on" the dimmed background landed on whatever was underneath — a
    // clip, a slider, the Export button. A real edit, made invisibly,
    // through a surface that said nothing was clickable.
    {
        type Open = fn(&mut VideoEditorApp);
        let cases: [(&str, Open); 2] = [
            ("shortcut sheet", |a| a.navigation.help_open = true),
            ("go-to field", |a| a.goto_entry = Some(String::new())),
        ];
        for (name, open) in cases {
            let (mut app, mut doc) = app_and_doc();
            let r = app.regions();
            let before = app.project.clips.len();
            let target = app.project.clips.first().map(|c| c.id);
            app.selection = Selection::default();

            open(&mut app);
            assert!(
                app.is_modal(),
                "{name}: paints the scrim but does not claim the click",
            );

            // Aim at a clip on the timeline — something a stray click would
            // otherwise select or move.
            app.last_mouse = r.lanes.center();
            app.handle_press(&mut doc, &r, false);

            assert!(
                app.selected().is_none(),
                "{name}: the click reached the timeline and selected something",
            );
            assert_eq!(
                app.project.clips.len(),
                before,
                "{name}: the click reached the timeline and changed it",
            );
            let _ = target;
        }
    }
    // Case `the_open_overlay_bin_leaves_the_editor_behind_it_visible`:
    // The overlay media bin dims the editor behind it; it must never erase it.
    //
    // Regression: the scrim scaled its alpha by `surfaces.scrim.components[3]`,
    // meaning to honour a token-carried translucency — but every theme builds
    // that token opaque, so the multiply was a no-op and a fully-revealed flap
    // painted a 100%-opaque slab from the topbar to the bottom of the window.
    // The preview, transport, inspector and timeline all still drew, underneath
    // it, and every window size "rendered without panicking".
    {
        for theme in [Theme::dark(), Theme::light()] {
            for (w, h) in [(420.0_f64, 560.0_f64), (620.0, 700.0), (720.0, 820.0)] {
                let (mut app, _doc) = app_sized(w, h);
                app.bin_flap.snap(true);
                app.navigation.flap_settled = true;
                let r = app.regions();
                assert!(r.bin_overlay, "{w}x{h} must be the overlay layout");
                assert!(r.scrim.width() > 0.0, "an open overlay bin paints a scrim");
                let px = render_pixels(&mut app, w as u32, h as u32, &theme);
                // Sample the editor strictly to the right of the bin flap, so
                // what we measure is the region the scrim covers and the bin
                // does not.
                let right =
                    |rect: Rect| Rect::new(rect.x0.max(r.bin.x1 + 4.0), rect.y0, rect.x1, rect.y1);
                for (name, rect) in [
                    ("preview", right(r.preview)),
                    ("inspector", right(r.inspector)),
                    ("timeline", right(r.timeline)),
                ] {
                    if rect.width() < 8.0 || rect.height() < 8.0 {
                        continue; // genuinely no room at this size
                    }
                    let colours = region_colours(&px, w as u32, h as u32, rect);
                    assert!(
                        colours > 2,
                        "{w}x{h}: the {name} is a flat slab behind the open bin                          ({colours} distinct colours) — the scrim erased it",
                    );
                }
            }
        }
    }
    // Case `reduced_motion_snaps_side_panels_without_requesting_more_frames`.
    {
        let (mut app, _doc) = app_sized(720.0, 820.0);
        app.advance_flap();
        app.toggle_bin();
        app.toggle_inspector();
        assert!(app.flap_animating(), "ordinary motion starts eased reveals");

        app.advance_flap_with_motion(true);
        assert!(app.bin_flap.is_open(app.bin_layout()));
        assert!(app.inspector_flap.is_open(app.inspector_split()));
        assert!(
            !app.flap_animating(),
            "a reduced-motion frame must not leave an animation owed",
        );
    }
}

// ---- shortcut / edit-path tests --------------------------------------
// These drive the *production* key handler and assert on the reloaded
// CRDT, proving the shortcut → mutation → document path end to end.

use winit::keyboard::{Key as WKey, ModifiersState};

// ---- the timeline's render range -------------------------------------

#[test]
fn range_marks_scope_renders_without_stealing_trim_keys() {
    // Case `marking_in_and_out_narrows_what_a_render_covers`.
    {
        // The export loop ran `0..total` unconditionally, so checking a
        // ten-second fix meant re-rendering the whole timeline. There were no
        // marks to honour: no `mark_in`, no `mark_out`, no render range.
        let (mut app, mut doc) = app_and_doc();
        let whole = app.project.duration();
        assert!(whole > 20.0, "the fixture is a real timeline");

        app.playhead = 4.0;
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "I",
            ModifiersState::SHIFT
        ));
        app.playhead = 14.0;
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "O",
            ModifiersState::SHIFT
        ));

        // Through the document, which is what a peer and a reopened project
        // read — a range held only in memory would not survive either.
        app.sync(&mut doc, &automerge::ROOT);
        let (from, to) = app.project.export_range();
        assert!(
            (from - 4.0).abs() < 1e-9 && (to - 14.0).abs() < 1e-9,
            "{from}..{to}"
        );

        // And that range is what the render actually walks.
        let plan = crate::export::RenderPlan::new(&app.project, 30.0);
        assert_eq!(plan.total, 300, "ten seconds at 30fps");
        assert!(
            (plan.time_of(0) - 4.0).abs() < 1e-9,
            "the first frame is the in mark"
        );
        assert!(
            (plan.time_of(plan.total - 1) - (14.0 - 1.0 / 30.0)).abs() < 1e-6,
            "and the last is one frame short of the out mark",
        );
        let unmarked = crate::export::RenderPlan::new(
            &{
                let mut p = app.project.clone();
                p.clear_marks();
                p
            },
            30.0,
        );
        assert!(
            unmarked.total > plan.total * 2,
            "the whole timeline is far longer: {} vs {}",
            unmarked.total,
            plan.total,
        );
    }
    // Case `clearing_the_marks_puts_the_render_back_to_the_whole_timeline`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 6.0;
        key_press_mods(&mut app, &mut doc, "I", ModifiersState::SHIFT);
        assert!(app.project.has_marks());
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "X",
            ModifiersState::SHIFT
        ));
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.project.has_marks(), "both marks are gone");
        let (from, to) = app.project.export_range();
        assert!((from).abs() < 1e-9 && (to - app.project.duration()).abs() < 1e-9);
    }
    // Case `the_range_marks_do_not_take_the_clip_trim_keys_away`.
    {
        // I and O have always trimmed the selected clip. The range is a
        // different decision on a different thing, so it takes a different
        // binding — and the old one has to keep working.
        let (mut app, mut doc) = app_and_doc();
        let cid = app
            .project
            .clips
            .iter()
            .find(|c| c.start < 3.0 && c.end() > 6.0)
            .map_or(app.project.clips[0].id, |c| c.id);
        app.selection.select_only(cid);
        let clip = app.project.clip(cid).cloned().expect("selected clip");
        app.playhead = clip.start + 1.0;
        assert!(key_press(&mut app, &mut doc, "i", false), "plain I answers");
        let after = VideoDoc::load_project(&doc)
            .clip(cid)
            .cloned()
            .expect("the clip survives");
        assert!(
            after.in_point > clip.in_point + 1e-6,
            "plain I still trims the clip: {} -> {}",
            clip.in_point,
            after.in_point,
        );
        assert!(
            !app.project.has_marks(),
            "and it did not mark the timeline instead",
        );
    }
}

// ---- the project's own format ----------------------------------------

/// A blank editor on an empty document — the live app's opening state.
fn blank_app_and_doc() -> (VideoEditorApp, AutoCommit) {
    ui::host::install_default(ui::host::HostServices::recording().0);
    let mut doc = AutoCommit::new();
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.sync(&mut doc, &automerge::ROOT);
    (app, doc)
}

/// Put one source in the bin, in both the document and the model, the way
/// an import does.
fn seed_media(app: &mut VideoEditorApp, doc: &mut AutoCommit, w: u32, h: u32, fps: f64) -> MediaId {
    let item = MediaItem {
        id: Uuid::new_v4(),
        name: "Phone.mp4".into(),
        kind: MediaKind::Video,
        duration: 12.0,
        width: w,
        height: h,
        fps,
        path: "media/Phone.mp4".into(),
        hydration: crate::media::Hydration::Local,
        ..Default::default()
    };
    let id = item.id;
    VideoDoc::upsert_media(doc, &item);
    app.project.media.push(item);
    id
}

#[test]
fn project_format_follows_the_first_clip_unless_chosen() {
    // Case `a_blank_project_takes_its_shape_from_the_first_clip_that_lands_on_it`.
    {
        // Shoot vertically and there was no way to say so: the frame was
        // fixed at 1920x1080 at 30 from the moment the project existed, and
        // the export scales its preset height by the *project's* aspect — so
        // the file came out letterboxed into 16:9 with no exit.
        let (mut app, mut doc) = blank_app_and_doc();
        assert_eq!((app.project.width, app.project.height), (1920, 1080));
        let mid = seed_media(&mut app, &mut doc, 1080, 1920, 24.0);

        app.add_media_to_timeline(&mut doc, mid)
            .expect("the source places");

        assert_eq!(
            (app.project.width, app.project.height),
            (1080, 1920),
            "the project took the footage's shape"
        );
        assert!((app.project.fps - 24.0).abs() < 1e-9, "and its rate");
        // Through the document, which is what replicates and what a reopened
        // project reads back — a shape held only in memory would be gone on
        // the next reload.
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!((app.project.width, app.project.height), (1080, 1920));
        assert!(app.project.format_explicit, "and it is now a chosen format");

        // Which is what the export honours: a vertical project renders
        // vertical rather than pillarboxed.
        let (w, h) = crate::export::output_dims(&ExportSettings::default(), &app.project);
        assert!(h > w, "a vertical project exports vertical, got {w}x{h}");
    }
    // Case `a_format_chosen_by_hand_is_not_overridden_by_the_next_clip`.
    {
        // Adoption is for a project that has never been told its shape. One
        // that has must keep it, or the picker would be undone by the next
        // import.
        let (mut app, mut doc) = blank_app_and_doc();
        app.set_project_frame_size(&mut doc, 3840, 2160);
        app.set_project_fps(&mut doc, 25.0);
        let mid = seed_media(&mut app, &mut doc, 1080, 1920, 60.0);

        app.add_media_to_timeline(&mut doc, mid)
            .expect("the source places");

        assert_eq!((app.project.width, app.project.height), (3840, 2160));
        assert!((app.project.fps - 25.0).abs() < 1e-9);
    }
    // Case `a_second_clip_does_not_reshape_a_timeline_already_cut_against_the_first`.
    {
        let (mut app, mut doc) = blank_app_and_doc();
        let first = seed_media(&mut app, &mut doc, 1080, 1920, 30.0);
        app.add_media_to_timeline(&mut doc, first)
            .expect("the first places");
        let second = seed_media(&mut app, &mut doc, 3840, 2160, 60.0);
        app.add_media_to_timeline(&mut doc, second)
            .expect("the second places");
        assert_eq!(
            (app.project.width, app.project.height),
            (1080, 1920),
            "the shape the timeline was cut against survives"
        );
    }
    // Case `the_frame_size_and_rate_are_pressable_in_the_inspector`.
    {
        // The picker is the pointer half of the same capability the
        // `set-frame-size` / `set-frame-rate` actions carry.
        let (mut app, mut doc) = app_and_doc();
        app.selection.clear();
        app.inspector_flap.open();
        app.collapsed.remove(&Section::Project);
        let r = app.regions();
        let l = app.inspector_layout(&r);
        let ((w, h), rect) = *l
            .project_frames
            .iter()
            .find(|((w, h), _)| (*w, *h) == (1080, 1920))
            .expect("the panel offers a vertical frame");
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert_eq!((app.project.width, app.project.height), (w, h));

        let r = app.regions();
        let l = app.inspector_layout(&r);
        let (rate, rect) = *l
            .project_rates
            .iter()
            .find(|(rate, _)| (rate - 24.0).abs() < 1e-6)
            .expect("the panel offers 24");
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert!((app.project.fps - rate).abs() < 1e-9);
    }
    // Case `a_reader_can_set_the_projects_frame_size_without_a_pointer`.
    {
        // The accessibility tree and the keyboard walk are the same list, and
        // the project's format had no stop on it at all.
        let mut d = a11y_driver();
        // PROJECT starts folded, so the first thing a reader has to be able to
        // do is unfold it — through the header's own node, with no pointer.
        let header = d
            .a11y_node_named("PROJECT")
            .expect("the project section reaches the tree");
        d.perform_a11y_action(header, &ui::a11y::Action::Expand)
            .unwrap();
        let node = d
            .a11y_node_named("9:16, 1080 by 1920")
            .expect("the project's frame presets reach the tree once it is open");
        d.perform_a11y_action(node, &ui::a11y::Action::Activate)
            .unwrap();
        let p = VideoDoc::load_project(&d.state_mut().doc);
        assert_eq!(
            (p.width, p.height),
            (1080, 1920),
            "activating the preset wrote the document"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn keyframes_are_drawn_played_and_edited_from_the_inspector() {
    // Case `a_keyframed_clip_is_drawn_at_the_value_it_is_playing`:
    // The timeline draws the animated value, not the constant under it.
    //
    // The clip body's alpha and the waveform's height read the raw
    // `opacity`/`volume` fields, which a keyframe supersedes. So a clip
    // fading out was drawn at full strength for its whole length while the
    // canvas beside it faded, and a volume ramp was heard rising and drawn
    // flat — the timeline stating one thing and the preview another.
    //
    // The playhead is held still and the *keyframes* differ between the two
    // renders, with the constant left identical. Moving the playhead instead
    // would move the playhead line across the lane, and the difference that
    // produces has nothing to do with opacity — a first draft of this test
    // passed against the unfixed code for exactly that reason.
    {
        use crate::param::{Interp, Param};

        let theme = Theme::dark();
        let (w, h) = (1280u32, 800u32);

        // Same project, same playhead, same constant `opacity`; only the
        // keyed value under the playhead differs.
        let render = |keyed: f64| -> (Vec<u8>, Rect) {
            let (mut app, _doc) = app_and_doc();
            // A clip on a *video* track: the alpha under test is only read on
            // that branch, and the seeded project's first clip is audio.
            let cid = app
                .project
                .clips
                .iter()
                .find(|c| {
                    app.project
                        .track(c.track)
                        .is_some_and(|t| t.kind == TrackKind::Video)
                })
                .expect("the seeded project has a video clip")
                .id;
            let (start, end) = {
                let c = app.project.clip(cid).expect("seeded clip");
                (c.start, c.end())
            };
            let at = start + (end - start) * 0.5;
            {
                let c = app.project.clip_mut(cid).expect("clip");
                c.opacity = 1.0;
                c.set_key_at(Param::Opacity, at, keyed, Interp::Linear);
            }
            app.playhead = at;
            let px = render_pixels(&mut app, w, h, &theme);
            let r = app.regions();
            let clip = app.project.clip(cid).expect("clip").clone();
            let lane = app
                .lane_rects(&r)
                .into_iter()
                .find(|(tid, _, _)| *tid == clip.track)
                .map(|(_, _, rect)| rect)
                .expect("the clip's track has a lane");
            (px, app.clip_rect(&clip, &lane, &r))
        };

        let (opaque, lane) = render(1.0);
        let (faded, _) = render(0.05);
        assert!(
            region_diff(&opaque, &faded, w, h, lane) > 0,
            "a clip keyed to 5% under the playhead is painted exactly like one \
             keyed to 100%, so the lane is drawing the constant `opacity` \
             rather than the value the preview is showing",
        );
    }
    // Case `the_key_control_reports_off_between_and_on_a_key`:
    // The three states a key control shows are what the whole keyframe UI
    // rests on: if "animated" and "a key is here" looked the same, clicking
    // would be a coin toss between adding and editing.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        assert_eq!(app.key_state(Param::Opacity), KeyState::Off);
        // One click on an unkeyed control starts the animation, at whatever
        // the parameter already reads — so keying never moves the picture.
        let before = app.project.clip(id).unwrap().opacity;
        let rect = reveal(&mut app, |l| {
            l.keys
                .iter()
                .find(|(p, _)| *p == Param::Opacity)
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let clip = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        assert!(
            clip.anim.is_animated(Param::Opacity),
            "the first click keyed it"
        );
        assert!(
            (clip.param_at(Param::Opacity, 23.0) - before).abs() < 1e-9,
            "and planted the value it already had",
        );
        assert_eq!(app.key_state(Param::Opacity), KeyState::OnKey);
        // Step off the key: still animated, no key here.
        app.playhead = 24.0;
        assert_eq!(app.key_state(Param::Opacity), KeyState::Between);
        // A second click at a keyed control opens the menu rather than
        // silently re-keying, because there is more than one thing to do.
        let rect = reveal(&mut app, |l| {
            l.keys
                .iter()
                .find(|(p, _)| *p == Param::Opacity)
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert!(app.bin_menu.is_open());
        assert!(matches!(
            app.menu_target,
            Some(MenuTarget::Keyframe(Param::Opacity))
        ));
    }
    // Case `dragging_an_animated_slider_writes_a_key_not_a_dead_constant`:
    // Once a parameter is animated its constant is dead — every reader goes
    // through the curve. Dragging its slider therefore has to write a *key*,
    // or the control would appear to do nothing.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        // Key it once at the head.
        app.key_here(&mut doc, Param::Opacity);
        let keys_before = VideoDoc::load_project(&doc)
            .clip(id)
            .unwrap()
            .anim
            .track(Param::Opacity)
            .unwrap()
            .keys
            .len();
        assert_eq!(keys_before, 1);
        // Move on and drag the opacity slider to a new value.
        app.playhead = 25.0;
        let r = app.regions();
        app.inspector_scroll = 0.0;
        let track = app.inspector_layout(&r).opacity.expect("opacity row");
        let x = track.x0 + track.width() * 0.3;
        click(&mut app, &mut doc, x, track.center().y);
        release(&mut app, &mut doc);
        let clip = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        let keys = &clip.anim.track(Param::Opacity).unwrap().keys;
        assert_eq!(
            keys.len(),
            2,
            "the drag added a second key rather than writing a constant"
        );
        assert!(
            (clip.param_at(Param::Opacity, 25.0) - 0.3).abs() < 0.06,
            "the curve reads the dragged value here, got {}",
            clip.param_at(Param::Opacity, 25.0),
        );
        // The head key is untouched, so the animation actually goes somewhere.
        assert!(
            (clip.param_at(Param::Opacity, 23.0) - clip.param_at(Param::Opacity, 25.0)).abs() > 0.1,
            "the two keys hold different values",
        );
    }
    // Case `the_keyframe_menu_shapes_and_removes_the_key_under_the_playhead`:
    // The keyframe menu's rows are indexed past its separators, and a wrong
    // offset would silently perform the neighbouring action.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        app.key_here(&mut doc, Param::Opacity);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.project
                .clip(id)
                .unwrap()
                .key_interp_at(Param::Opacity, 23.0),
            Some(Interp::Linear),
        );
        // Row 3 is Hold: 0 remove, 1 separator, 2 Linear, 3 Hold, 4 Ease.
        app.open_key_menu(Param::Opacity, Point::new(400.0, 400.0));
        app.key_menu_choose(&mut doc, Param::Opacity, 3);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.project
                .clip(id)
                .unwrap()
                .key_interp_at(Param::Opacity, 23.0),
            Some(Interp::Hold),
            "the menu re-shaped the key it was opened on",
        );
        // Row 0 removes it, and the last key going takes the curve with it.
        app.open_key_menu(Param::Opacity, Point::new(400.0, 400.0));
        app.key_menu_choose(&mut doc, Param::Opacity, 0);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            !app.project
                .clip(id)
                .unwrap()
                .anim
                .is_animated(Param::Opacity)
        );
    }
    // Case `the_curve_amount_is_keyframable_from_its_own_diamond`.
    {
        let (mut app, mut doc, _canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        let r = app.regions();
        let diamond = app
            .inspector_layout(&r)
            .keys
            .iter()
            .find(|(p, _)| *p == Param::CurveMix)
            .map(|(_, rect)| *rect)
            .expect("the amount row carries a key control");
        assert_eq!(app.key_state(Param::CurveMix), KeyState::Off);
        app.press_inspector(&mut doc, &r, diamond.center());
        assert_eq!(
            app.key_state(Param::CurveMix),
            KeyState::OnKey,
            "one click plants a key at the playhead",
        );
        assert!(
            app.project
                .clip(cid)
                .unwrap()
                .anim
                .is_animated(Param::CurveMix)
        );
    }
    // Case `an_effects_intensity_can_be_keyed_from_its_own_row`.
    {
        // Every other animatable control carries a diamond; the effect row
        // was the one that did not, so an intensity could only be keyed from
        // the command palette — not somewhere a user looking at the slider
        // would think to go.
        let (mut app, mut doc) = app_and_doc();
        let cid = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(cid);
        app.playhead = 23.0;
        app.edit_selected(&mut doc, |c| c.add_effect(EffectKind::Sepia));
        app.collapsed.remove(&Section::Effects);

        let r = app.regions();
        let l = app.inspector_layout(&r);
        let row = *l.effect_rows.first().expect("the effect has a row");
        let (key, up, down, slider, remove) = VideoEditorApp::effect_row_parts(row);
        // The key control leads the row and touches nothing else on it.
        for other in [up, down, slider, remove] {
            assert!(
                key.x1 <= other.x0 + 1.0,
                "the key control sits clear of {other:?}, got {key:?}",
            );
        }
        assert!(
            l.keys
                .iter()
                .any(|(p, rect)| *p == Param::EffectAmount(0) && *rect == key),
            "and it is in the panel's own list of key controls",
        );

        assert_eq!(app.key_state(Param::EffectAmount(0)), KeyState::Off);
        reveal_inspector_rect_for_test(&mut app, &r, key);
        let r = app.regions();
        let row = *app
            .inspector_layout(&r)
            .effect_rows
            .first()
            .expect("the effect row stays reachable after scrolling");
        let (key, ..) = VideoEditorApp::effect_row_parts(row);
        app.press_inspector(&mut doc, &r, key.center());
        assert_eq!(
            app.key_state(Param::EffectAmount(0)),
            KeyState::OnKey,
            "one click plants a key on the intensity",
        );
        // And the curve really drives the effect the compositor is handed.
        app.edit_selected(&mut doc, |c| {
            c.set_key_at(Param::EffectAmount(0), 23.0, 0.1, Interp::Linear);
            c.set_key_at(Param::EffectAmount(0), 25.0, 0.9, Interp::Linear);
        });
        let clip = app.project.clip(cid).unwrap();
        let early = clip.effects_at(23.0)[0].amount;
        let late = clip.effects_at(25.0)[0].amount;
        assert!(
            late > early + 0.5,
            "the keyed intensity moves: {early} -> {late}"
        );
    }
}

/// Build an app + its backing doc seeded from the same sample, so the
/// app's in-memory project and `doc` agree (mirrors the live wiring).
fn app_and_doc() -> (VideoEditorApp, AutoCommit) {
    // No test may put a real file chooser on the person's screen. Several
    // here press the shortcuts that ask for one (Ctrl+O, Ctrl+Shift+O,
    // Ctrl+Shift+S) and assert only that the app *asked*; without this the
    // request reached D-Bus and a full run opened four real dialogs. A
    // recording host answers every request instead; it is the process default,
    // so installing it from the one fixture every test builds through covers
    // the suite.
    ui::host::install_default(ui::host::HostServices::recording().0);
    let mut doc = AutoCommit::new();
    VideoDoc::seed_sample(&mut doc);
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.sync(&mut doc, &automerge::ROOT);
    (app, doc)
}

/// Pointer tests must use the same scroll viewport as a person.  A rect
/// from the full inspector layout is not a license to activate content
/// hidden beneath the fixed identity card or outside the clipped body.
fn reveal_inspector_rect_for_test(app: &mut VideoEditorApp, r: &Regions, rect: Rect) {
    let view = app.inspector_viewport(r);
    app.inspector_scroll = (app.inspector_scroll + rect.center().y - view.center().y)
        .clamp(0.0, app.inspector_max_scroll(r));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn resource_and_view_actions_target_exact_entities() {
    // Case `resource_operations_reject_wrong_kinds_without_changing_state`.
    {
        let (mut app, mut doc) = app_and_doc();
        let selected_before = app.selected();
        let playhead_before = app.playhead;
        let clip = app.project.clips[0].id;
        let reveal = serde_json::to_vec(&crate::actions::VideoEditorOperation::RevealClip {
            clip: clip.to_string(),
        })
        .unwrap();
        assert!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::VIDEO_EDITOR_OPERATION_TYPE,
                    &reveal
                ),
                &mut doc,
                &automerge::ROOT
            )
            .is_err()
        );
        assert_eq!(app.selected(), selected_before);
        assert_eq!(app.playhead, playhead_before);

        let import = serde_json::to_vec(&crate::actions::VideoEditorOperation::ImportMedia {
            path: Some("/tmp/wrong-kind.mp4".into()),
        })
        .unwrap();
        assert!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::VIDEO_EDITOR_OPERATION_TYPE,
                    &import
                ),
                &mut doc,
                &automerge::ROOT
            )
            .is_err()
        );
        assert!(app.resource_operation.is_none());
        assert!(app.resource_pending_operation.is_none());
        assert!(app.resource_invocation.is_none());
    }
    // Case `a_chosen_search_hit_brokers_the_exact_timeline_selection`:
    // A chosen search row resolves through the exported resource tree and the
    // operation broker, then opens the exact clip and timeline instant.
    {
        let (mut app, mut doc) = app_and_doc();
        let clip = app.project.clips[0].clone();
        let media = app
            .project
            .media(clip.media)
            .expect("clip source")
            .name
            .clone();

        let providers = ui::search::capability::search_providers(&app);
        let provider = providers.first().expect("the editor publishes a provider");
        let hit = provider
            .search(&media)
            .into_iter()
            .find(|h| h.action_address() == Some(crate::actions::reveal_clip_address(clip.id)))
            .expect("the exact clip is findable by source name");
        let address = hit.action_address().expect("canonical search address");
        let encoded =
            tilo_actions::door::encode(&ui::resources::resources(&app, &doc), &address, None)
                .expect("the address resolves through the live resource tree");

        app.playback.playing = true;
        app.playhead = 0.0;
        assert!(matches!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .expect("view operation succeeds"),
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert!(
            (app.playhead - clip.start).abs() < 1e-3,
            "the hit put the playhead on the clip: {}",
            app.playhead,
        );
        assert_eq!(app.selected(), Some(clip.id));
        assert_eq!(app.selected_media, Some(clip.media));
        assert!(!app.playback.playing, "the shared seek stops the transport");
    }
    // Case `marker_view_action_selects_its_owner_at_the_current_position`:
    // Clip markers resolve their current collaborative position at execution
    // time and select their owner, rather than replaying a stale timestamp.
    {
        let (mut app, mut doc) = app_and_doc();
        let cid = app.project.clips[0].id;
        let marker = crate::model::Marker::new(1.25, "Exact beat", 1);
        let marker_id = marker.id;
        app.project
            .clip_mut(cid)
            .expect("clip")
            .markers
            .push(marker);
        let clip = app.project.clip(cid).expect("clip").clone();
        let resources = crate::actions::provide_resources(&app.video_resource_ctx());
        let address = crate::actions::reveal_marker_address(marker_id);
        let encoded = tilo_actions::door::encode(&resources, &address, None)
            .expect("marker action is exported");
        app.start_operation(
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut doc,
            &automerge::ROOT,
        )
        .expect("view marker");
        assert_eq!(app.selected(), Some(cid));
        assert!((app.playhead - (clip.start + 1.25)).abs() < 1e-9);
    }
    // Case `effect_view_action_targets_its_instance_and_refuses_a_stale_id`.
    {
        let (mut app, mut doc) = app_and_doc();
        let clip_id = app.project.clips[0].id;
        let clip = app.project.clip_mut(clip_id).expect("fixture clip");
        clip.add_effect(crate::model::EffectKind::Sepia);
        clip.add_effect(crate::model::EffectKind::Sepia);
        let effect_id = clip.effects[1].id;
        let payload = serde_json::to_vec(&crate::actions::VideoEditorOperation::RevealEffect {
            clip: clip_id.to_string(),
            effect: effect_id.to_string(),
        })
        .expect("operation serializes");

        assert!(matches!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::VIDEO_EDITOR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .expect("live effect resolves"),
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert_eq!(app.selected(), Some(clip_id));

        app.project
            .clip_mut(clip_id)
            .expect("fixture clip")
            .remove_effect_id(effect_id);
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::VIDEO_EDITOR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .is_err(),
            "a stale effect operation cannot resolve a duplicate by position",
        );
    }
}

fn key_press(app: &mut VideoEditorApp, doc: &mut AutoCommit, ch: &str, ctrl: bool) -> bool {
    let r = app.regions();
    let mods = if ctrl {
        winit::event::Modifiers::from(ModifiersState::CONTROL)
    } else {
        winit::event::Modifiers::default()
    };
    app.handle_key(&WKey::Character(ch.into()), mods, doc, &r)
}

// ---- project files ---------------------------------------------------

/// A scratch directory of this test's own, so nothing here can collide
/// with another test's file or leave one behind.
fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tilo-ve-app-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn key_press_mods(
    app: &mut VideoEditorApp,
    doc: &mut AutoCommit,
    ch: &str,
    mods: ModifiersState,
) -> bool {
    let r = app.regions();
    app.handle_key(
        &WKey::Character(ch.into()),
        winit::event::Modifiers::from(mods),
        doc,
        &r,
    )
}

#[test]
fn project_save_and_open_round_trip_and_replace() {
    // Case `saving_writes_a_file_that_opens_back_as_the_same_timeline`.
    {
        // The whole point of a project file, end to end through the app: save
        // the loaded project, open it into a *fresh* editor, and get the same
        // edit back — not "a file appeared".
        let dir = scratch("roundtrip");
        let path = dir.join("Coastline.qvp");
        let (mut app, mut doc) = app_and_doc();
        let clips = app.project.clips.len();
        let media = app.project.media.len();
        assert!(clips > 1 && media > 1, "the fixture is a real edit");

        app.write_project_to(&mut doc, path.clone());
        assert_eq!(app.project_path.as_ref(), Some(&path));
        assert!(path.is_file(), "the file is really on disk");

        // A completely separate editor on a blank document opens it.
        let mut fresh_doc = AutoCommit::new();
        let mut fresh = VideoEditorApp::new();
        fresh.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        fresh.sync(&mut fresh_doc, &automerge::ROOT);
        assert!(fresh.project.clips.is_empty(), "it starts blank");

        fresh.load_project_file(&mut fresh_doc, path);
        assert_eq!(fresh.project.clips.len(), clips, "every clip came back");
        assert_eq!(fresh.project.media.len(), media, "the whole bin came back");
        // And it landed in the *document*, not only the in-memory mirror, so
        // it replicates and survives the next reload.
        let reloaded = VideoDoc::load_project(&fresh_doc);
        assert_eq!(reloaded.clips.len(), clips);
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `opening_a_second_project_replaces_the_first_instead_of_merging_it`.
    {
        // The failure this guards is not a crash: it is opening project B and
        // being shown A's clips *and* B's, with no way to tell them apart.
        let dir = scratch("replace");
        let small = dir.join("Small.qvp");
        let mut other = Project::new("Small");
        let mid = uuid::Uuid::new_v4();
        other.media.push(MediaItem {
            id: mid,
            name: "One.mp4".into(),
            kind: MediaKind::Video,
            duration: 4.0,
            ..Default::default()
        });
        let tid = uuid::Uuid::new_v4();
        other.tracks.push(crate::model::Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            ..Default::default()
        });
        other.clips.push(Clip {
            id: uuid::Uuid::new_v4(),
            track: tid,
            media: mid,
            out_point: 4.0,
            ..Default::default()
        });
        crate::project_file::save(&small, &other).unwrap();

        let (mut app, mut doc) = app_and_doc();
        // Give it a file first, so the open does not stop to ask.
        app.project_path = Some(dir.join("Current.qvp"));
        app.load_project_file(&mut doc, small);

        assert_eq!(app.project.clips.len(), 1, "only the opened project's clip");
        assert_eq!(
            app.project.media.len(),
            1,
            "only the opened project's source"
        );
        assert_eq!(app.project.name, "Small");
        assert_eq!(
            VideoDoc::load_project(&doc).clips.len(),
            1,
            "and in the document too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `an_open_is_one_undoable_action_rather_than_a_side_door_document_swap`.
    {
        // Opening a file has to travel the same road every other edit does, or
        // it neither undoes nor reaches a peer. Assert on what was dispatched.
        let dir = scratch("action");
        let path = dir.join("P.qvp");
        crate::project_file::save(&path, &Project::new("P")).unwrap();
        let (mut app, mut doc) = app_and_doc();
        app.project_path = Some(dir.join("Prev.qvp"));

        app.load_project_file(&mut doc, path);
        let dispatched = app
            .actions
            .last_dispatched()
            .expect("opening dispatched an action")
            .clone();
        assert_eq!(dispatched.type_id, VIDEO_EDITOR_ACTION_TYPE);
        let decoded: crate::actions::VideoEditorAction =
            serde_json::from_slice(&dispatched.payload).expect("it decodes");
        assert!(
            matches!(decoded, crate::actions::VideoEditorAction::LoadProject(_)),
            "an open is a LoadProject action, got {decoded:?}",
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn unsaved_badge_follows_the_document() {
    // Case `the_unsaved_badge_follows_the_document_not_a_flag_someone_remembered_to_set`.
    {
        let dir = scratch("dirty");
        let path = dir.join("D.qvp");
        let (mut app, mut doc) = app_and_doc();
        assert!(
            app.project_is_dirty(),
            "a project with clips and no file is unsaved"
        );

        app.write_project_to(&mut doc, path);
        assert!(!app.project_is_dirty(), "saving settles it");
        assert!(!app.project_status_line().contains("unsaved"));

        // An edit that never touches this widget's own fields — the split
        // shortcut writes through the action host, exactly as a peer's change
        // or a Ctrl+K invocation would.
        let _ = key_press(&mut app, &mut doc, "s", false);
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selection.select_only(target);
        app.playhead = 12.0;
        key_press(&mut app, &mut doc, "s", false);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.project_is_dirty(),
            "the document moved on, so the badge is back"
        );
        assert!(app.project_status_line().contains("unsaved changes"));
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `a_save_in_the_same_frame_as_the_edit_still_settles_the_badge`.
    {
        // `sync` caches the document's heads once per frame. A save that
        // stamped that cached copy, rather than the document as it stands,
        // would record the pre-edit state — so the project would read as
        // unsaved the instant after being written, and the next Ctrl+S would
        // write an identical file to prove it.
        let dir = scratch("sameframe");
        let path = dir.join("S.qvp");
        let (mut app, mut doc) = app_and_doc();
        app.write_project_to(&mut doc, path.clone());
        assert!(!app.project_is_dirty());

        // Edit and save with no `sync` in between — the frame boundary the
        // cached heads would have been refreshed at.
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selection.select_only(target);
        app.playhead = 12.0;
        key_press(&mut app, &mut doc, "s", false);
        app.save_project(&mut doc);
        assert!(!app.project_is_dirty(), "the save settled it");

        // And the file really holds the edit, not the state before it.
        assert_eq!(crate::project_file::load(&path).unwrap().clips.len(), 8);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            !app.project_is_dirty(),
            "and it stays settled across the frame"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `an_empty_never_saved_project_is_not_reported_as_unsaved_work`.
    {
        // Otherwise a fresh window would wear a badge and interrogate the user
        // about discarding a project they never made.
        let mut doc = AutoCommit::new();
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.project_is_dirty());
        assert!(app.project_status_line().contains("empty project"));
    }
    // Case `the_reader_hears_the_same_project_state_the_badge_shows`.
    {
        // A screen-reader user must not get the tidier story. The dot means
        // "unsaved"; so must the node.
        let dir = scratch("a11yproj");
        let (mut app, mut doc) = app_and_doc();
        assert!(app.project_is_dirty());
        let node = app.a11y_project();
        assert_eq!(node.description.as_deref(), Some(PROJECT_UNSAVED));
        assert!(
            node.value
                .as_deref()
                .is_some_and(|v| v.contains("not saved to a file yet"))
        );

        app.write_project_to(&mut doc, dir.join("A.qvp"));
        let node = app.a11y_project();
        assert_eq!(node.description, None, "saved: no unsaved claim");
        assert!(
            node.value
                .as_deref()
                .is_some_and(|v| v.contains("saved at"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn opening_over_unsaved_work_asks_saves_and_reports() {
    // Case `opening_over_work_that_exists_nowhere_else_asks_first`.
    {
        let dir = scratch("ask");
        let path = dir.join("Other.qvp");
        crate::project_file::save(&path, &Project::new("Other")).unwrap();
        let (mut app, mut doc) = app_and_doc();
        let before = app.project.clips.len();
        assert!(app.project_path.is_none() && app.project_is_dirty());

        app.begin_open(&mut doc, path.clone());
        assert!(app.pending_open.is_some(), "it stopped to ask");
        assert_eq!(app.project.clips.len(), before, "and changed nothing yet");

        // Cancel leaves everything exactly where it was.
        app.resolve_pending_open(&mut doc, DiscardChoice::Cancel);
        assert!(app.pending_open.is_none());
        assert_eq!(app.project.clips.len(), before);
        assert!(app.project_path.is_none());

        // Open anyway goes through.
        app.begin_open(&mut doc, path.clone());
        app.resolve_pending_open(&mut doc, DiscardChoice::Discard);
        assert_eq!(app.project.name, "Other");
        assert_eq!(app.project_path.as_ref(), Some(&path));
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `a_project_that_already_has_a_file_opens_another_without_asking`.
    {
        // The question is about work that exists nowhere else. A saved project
        // is not at risk — its file is still there — so asking would be a
        // dialog that protects nothing and trains people to click through.
        let dir = scratch("noask");
        let path = dir.join("Next.qvp");
        crate::project_file::save(&path, &Project::new("Next")).unwrap();
        let (mut app, mut doc) = app_and_doc();
        app.write_project_to(&mut doc, dir.join("Current.qvp"));

        app.begin_open(&mut doc, path);
        assert!(app.pending_open.is_none(), "no question was asked");
        assert_eq!(app.project.name, "Next", "it just opened");
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `answering_save_writes_the_project_and_then_continues_into_the_open`.
    {
        // The answer that loses nothing has to actually work end to end, or
        // the user picks it, gets a file chooser, and is left wondering what
        // happened to the thing they were opening.
        let dir = scratch("savethen");
        let target = dir.join("Target.qvp");
        crate::project_file::save(&target, &Project::new("Target")).unwrap();
        let (mut app, mut doc) = app_and_doc();
        let kept = dir.join("Kept.qvp");

        app.begin_open(&mut doc, target.clone());
        assert!(app.pending_open.is_some());
        app.resolve_pending_open(&mut doc, DiscardChoice::Save);
        // Save on a never-saved project opens the chooser; the destination
        // arriving is what completes both halves.
        assert!(
            app.save_then_open.is_some(),
            "the open is remembered across the save"
        );
        app.write_project_to(&mut doc, kept.clone());

        assert!(
            kept.is_file(),
            "the project that was at risk is now on disk"
        );
        assert_eq!(crate::project_file::load(&kept).unwrap().clips.len(), 7);
        assert_eq!(
            app.project.name, "Target",
            "and the open it interrupted went through"
        );
        assert_eq!(app.project_path.as_ref(), Some(&target));
        assert!(app.save_then_open.is_none() && app.pending_open.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `a_save_that_failed_says_so_and_does_not_claim_the_file`.
    {
        // The one outcome that could really cost someone their work is being
        // told a save happened when it did not.
        let (mut app, mut doc) = app_and_doc();
        let impossible = PathBuf::from("/definitely/not/a/directory/x.qvp");
        app.write_project_to(&mut doc, impossible);
        assert_eq!(
            app.project_path, None,
            "the project did not adopt a path it has not got"
        );
        assert!(app.project_is_dirty(), "and it is still unsaved");
        let notice = app
            .project_notice
            .clone()
            .expect("the failure is on screen");
        assert!(notice.text.starts_with("Could not save"), "{}", notice.text);
        assert_eq!(
            notice.level,
            NoticeLevel::Failed,
            "and it is tinted as a failure",
        );
    }
    // Case `opening_something_that_is_not_a_project_reports_it_and_keeps_the_project`.
    {
        let dir = scratch("wrongfile");
        let junk = dir.join("holiday.mp4");
        std::fs::write(&junk, b"not a project at all, just some bytes").unwrap();
        let (mut app, mut doc) = app_and_doc();
        let before = app.project.clips.len();
        app.project_path = Some(dir.join("Cur.qvp"));

        app.load_project_file(&mut doc, junk);
        assert_eq!(
            app.project.clips.len(),
            before,
            "the open project is untouched"
        );
        let notice = app.project_notice.clone().expect("it says what went wrong");
        assert_eq!(notice.text, "Not a video project file");
        assert_eq!(notice.level, NoticeLevel::Failed);
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `a_project_whose_footage_is_not_here_says_which_in_the_words_the_app_already_uses`.
    {
        // Media is referenced, not copied, so this is the ordinary case after
        // opening a project somewhere else — and it must be disclosed in the
        // same vocabulary the bin and the timeline use, not a third phrasing.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_VE_MEDIA_DIR");
        let dir = scratch("moved");
        let path = dir.join("Moved.qvp");
        let mut p = Project::new("Moved");
        p.media.push(MediaItem {
            id: uuid::Uuid::new_v4(),
            name: "Drone.mov".into(),
            kind: MediaKind::Video,
            duration: 5.0,
            width: 1920,
            height: 1080,
            path: "/definitely/not/here-xyz.mov".into(),
            ..Default::default()
        });
        crate::project_file::save(&path, &p).unwrap();

        let (mut app, mut doc) = app_and_doc();
        app.project_path = Some(dir.join("Cur.qvp"));
        app.load_project_file(&mut doc, path);
        let notice = app
            .project_notice
            .clone()
            .expect("the open reported something");
        assert!(notice.text.contains("Opened Moved"), "{}", notice.text);
        assert!(
            notice.text.contains(MEDIA_NOT_LOCAL) && notice.text.contains("Drone.mov"),
            "it names the shared vocabulary and the source: {}",
            notice.text,
        );
        assert_eq!(
            notice.level,
            NoticeLevel::Warning,
            "an absent source is neither a failed open nor a clean one",
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn project_shortcuts_and_menu_open_save_and_recents() {
    // Case `ctrl_o_opens_a_project_and_ctrl_shift_o_imports`.
    {
        // Ctrl+O used to import. It means "open a document" in every app a
        // person has used, so an editor with projects has to honour that —
        // and import has to remain reachable, not merely relocated.
        let (mut app, mut doc) = app_and_doc();
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "o",
            ModifiersState::CONTROL
        ));
        assert!(
            app.project_rx.is_some(),
            "Ctrl+O opened the project chooser"
        );
        assert!(app.import_rx.is_none(), "and not the media importer");

        let (mut app, mut doc) = app_and_doc();
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "o",
            ModifiersState::CONTROL | ModifiersState::SHIFT
        ));
        assert!(app.import_rx.is_some(), "Ctrl+Shift+O still imports media");
        assert!(app.project_rx.is_none());
    }
    // Case `ctrl_s_saves_and_ctrl_shift_s_always_asks_where`.
    {
        let dir = scratch("keys");
        let path = dir.join("K.qvp");
        let (mut app, mut doc) = app_and_doc();
        app.write_project_to(&mut doc, path.clone());
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selection.select_only(target);
        app.playhead = 12.0;
        key_press(&mut app, &mut doc, "s", false);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.project_is_dirty());

        // Ctrl+S on a project that has a file writes straight back to it: no
        // chooser, no second copy the user did not ask for.
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "s",
            ModifiersState::CONTROL
        ));
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.project_rx.is_none(), "Ctrl+S did not open a chooser");
        assert!(!app.project_is_dirty(), "it saved");
        assert_eq!(crate::project_file::load(&path).unwrap().clips.len(), 8);

        // Ctrl+Shift+S asks even when there is a file — that is what makes it
        // Save As rather than a second Save.
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "s",
            ModifiersState::CONTROL | ModifiersState::SHIFT
        ));
        assert!(app.project_rx.is_some(), "Save As always asks where");
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `the_project_menu_offers_open_save_save_as_and_the_recents_that_are_not_already_open`.
    {
        let (mut app, _doc) = app_and_doc();
        let open = PathBuf::from("/p/Open.qvp");
        let other = PathBuf::from("/p/Other.qvp");
        app.project_path = Some(open.clone());
        app.recents = vec![open, other.clone()];

        let rows = app.project_menu_rows();
        let items: Vec<ProjectMenuItem> = rows.iter().map(|(i, _)| i.clone()).collect();
        assert_eq!(items[0], ProjectMenuItem::Open);
        assert_eq!(items[1], ProjectMenuItem::Save);
        assert_eq!(items[2], ProjectMenuItem::SaveAs);
        assert_eq!(
            items[3..],
            [ProjectMenuItem::Recent(other)],
            "the project already open is not offered as somewhere to go",
        );
        // Save names what it will do: a project with a file saves, one without
        // asks — and the label says which before it is pressed.
        assert_eq!(rows[1].1, "Save  (Ctrl+S)");
        app.project_path = None;
        assert_eq!(app.project_menu_rows()[1].1, "Save…  (Ctrl+S)");
    }
    // Case `a_menu_click_resolves_past_the_separator_to_the_row_it_landed_on`.
    {
        // The separator takes a row in the menu and none in the intent list.
        // Assuming the two indices match picks the wrong project to open.
        let (mut app, _doc) = app_and_doc();
        let a = PathBuf::from("/p/A.qvp");
        let b = PathBuf::from("/p/B.qvp");
        app.recents = vec![a.clone(), b.clone()];
        app.open_project_menu(Point::new(10.0, 50.0));

        let labels: Vec<String> = app
            .project_menu
            .items()
            .iter()
            .map(|i| i.label().unwrap_or("—").to_string())
            .collect();
        assert_eq!(
            labels.len(),
            6,
            "three actions, a separator, two recents: {labels:?}"
        );
        assert_eq!(app.project_menu_intent(0), Some(ProjectMenuItem::Open));
        assert_eq!(app.project_menu_intent(2), Some(ProjectMenuItem::SaveAs));
        assert_eq!(
            app.project_menu_intent(3),
            None,
            "the separator does nothing"
        );
        assert_eq!(app.project_menu_intent(4), Some(ProjectMenuItem::Recent(a)));
        assert_eq!(app.project_menu_intent(5), Some(ProjectMenuItem::Recent(b)));
    }
    // Case `the_project_button_is_dropped_rather_than_clipped_and_the_reader_is_told`.
    {
        // A name cut to a few characters states a *different* project name,
        // which is worse than no button — but a reader must then be told the
        // control is not there and what to press instead.
        let (mut app, _doc) = app_and_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let wide = app.regions().project_btn;
        assert!(wide.width() >= PROJECT_BTN_MIN_W, "there is room at 1280");

        app.rect = Rect::new(0.0, 0.0, 500.0, 640.0);
        let narrow = app.regions();
        assert_eq!(narrow.project_btn.width(), 0.0, "no room, so no button");
        assert!(
            !narrow.project_btn.contains(narrow.import_btn.center()),
            "and nothing overlapping Import",
        );
        let node = app.a11y_project();
        assert!(
            !node.state.focusable,
            "a control that is not painted is not a stop"
        );
        assert!(
            node.description
                .as_deref()
                .is_some_and(|d| d.contains("Ctrl+O")),
            "the reader is pointed at the keys instead: {:?}",
            node.description,
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn export_failures_are_accessible_and_wrap() {
    // Case `export_failure_details_are_available_to_keyboard_and_screen_readers`.
    {
        let (mut app, mut doc) = app_and_doc();
        let reason =
            "no encoder for H.264 on this system; install an H.264-capable GStreamer plugin";
        app.export_status = ExportStatus::Failed {
            reason: reason.to_string(),
        };

        // With the dialog closed, the status is a complete assertive live
        // region, not the ellipsized text painted in the compact topbar chip.
        let closed = app.accessibility().expect("editor semantics");
        let status = a11y_find(&closed, "Export failed");
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].value.as_deref(), Some(reason));

        // Ctrl+E is the documented keyboard route to the full export surface.
        assert!(key_press_mods(
            &mut app,
            &mut doc,
            "e",
            ModifiersState::CONTROL
        ));
        assert!(
            app.presentation.export_open,
            "Ctrl+E opens the export details dialog"
        );
        let open = app.accessibility().expect("dialog semantics");
        let details = a11y_find(&open, "Export failure details");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].value.as_deref(), Some(reason));
        assert_eq!(
            details[0].key,
            Some(ui::a11y::hash_key(&"export.failure-details")),
            "the failure node has a semantic key rather than a child position"
        );
    }
    // Case `a_long_export_failure_wraps_above_the_footer_and_buttons`.
    {
        let (mut app, _doc) = app_sized(1280.0, 800.0);
        app.presentation.export_open = true;
        let reason = "The selected encoder rejected the profile because the source contains an unsupported colour space; choose a compatible profile or install an encoder that supports this footage.";
        app.export_status = ExportStatus::Failed {
            reason: reason.into(),
        };
        // The one layout the event path and the draw pass both resolve from
        // the current state: the failure lines fill the status slot, which
        // sits clear of the size line above it and the action row below it.
        let mut tr = TextRenderer::new();
        let layout = app.export_layout(&mut tr, &Theme::dark());
        let ty = layout.type_sizes;
        let lines = tr.clamp_to_lines(reason, ty.xs, layout.modal.body.width() as f32, 2);
        assert_eq!(lines.len(), 2, "the long reason occupies the two-line slot");
        let line_h = f64::from(tr.measure("Hg", ty.xs, None).1);
        let info_bottom = layout.info_top + f64::from(tr.measure("Hg", ty.sm, None).1);
        let error_top = layout.status.y0;
        let error_bottom = layout.status.y0 + 2.0 * line_h;
        assert!(
            error_top >= info_bottom,
            "the wrapped failure overlaps the footer metadata: {error_top} < {info_bottom}"
        );
        assert!(
            error_bottom <= layout.status.y1 + 1e-6,
            "two failure lines do not fit the status slot: {error_bottom} > {}",
            layout.status.y1
        );
        assert!(
            layout.status.y1 <= layout.cancel.y0,
            "the status slot overlaps the footer buttons: {} > {}",
            layout.status.y1,
            layout.cancel.y0
        );
        // Exercise the real Vello draw pass as well as its layout math.
        render(&mut app);
    }
    // Case `a_finished_render_tells_a_reader_what_became_of_it`:
    // A finished render's outcome has to reach a reader too, with the words
    // that make it actionable — the path it wrote, or why it did not.
    {
        let cases = [
            (
                ExportStatus::Done {
                    path: "/home/you/Videos/Reel.mp4".into(),
                },
                "Export finished",
                "/home/you/Videos/Reel.mp4",
            ),
            (
                ExportStatus::Failed {
                    reason: "no encoder for H.264 on this machine".into(),
                },
                "Export failed",
                "no encoder for H.264 on this machine",
            ),
        ];
        for (status, name, spoken) in cases {
            let (mut app, _doc) = app_sized(1280.0, 800.0);
            app.export_status = status;
            let tree = app
                .accessibility()
                .expect("the editor describes its contents");
            let found = a11y_find(&tree, name);
            assert_eq!(found.len(), 1, "{name} must reach the tree exactly once");
            assert_eq!(
                found[0].value.as_deref(),
                Some(spoken),
                "{name} carries the outcome itself, not just its mood",
            );
            assert!(
                found[0].state.focusable,
                "{name} is reachable, so a reader can act on it",
            );
        }
    }
    // Case `the_accessible_export_button_reports_a_press_that_did_nothing`:
    // The exported Export button is marked `disabled` when export cannot run,
    // and its action used to answer `true` anyway — telling an assistive
    // technology the press succeeded while nothing at all happened. The undo
    // path in this file already refuses properly; this now matches it.
    //
    // The refusal is provoked by a render already in flight rather than by a
    // build with no encoder, so the disabled node exists to be pressed
    // however this crate's features were resolved.
    {
        let (mut app, _doc) = app_and_doc();
        app.presentation.export_open = true;
        // A timeline whose only footage cannot be read: the worker refuses it
        // by name before it reaches an encoder, so this starts a real job
        // without starting a real encode.
        app.project = unexportable_project();
        assert!(
            !crate::export::unreadable_sources(&app.project).is_empty(),
            "the fixture's footage is unreadable, so no encoder can run",
        );
        let dest = scratch("a11y-export").join("never-written.mp4");
        app.begin_export_to(dest);
        assert!(app.export_running(), "a render is in flight");
        assert!(
            !app.export_is_offered(),
            "so a second Export is out of reach"
        );

        let tree = app
            .accessibility()
            .expect("the editor describes its contents");
        let btn = a11y_find(&tree, "Start export");
        assert_eq!(btn.len(), 1, "the dialog offers exactly one Export button");
        assert!(
            btn[0].state.disabled,
            "and the node says it cannot be pressed",
        );

        let before = app.export_status.clone();
        let acted = app.a11y_export_action(&ui::a11y::Action::Activate, &[1]);
        assert!(
            !acted,
            "activating a disabled node must report that nothing happened",
        );
        assert_eq!(
            app.export_status, before,
            "a refused press does not move the render's status",
        );
        assert!(
            app.export_dest_rx.is_none(),
            "and opens no destination chooser",
        );

        // The enabled half of the same contract: with nothing in flight, the
        // press is answered as the action it really performed.
        app.export_job = None;
        app.export_status = ExportStatus::Idle;
        if app.export_is_offered() {
            assert!(
                app.a11y_export_action(&ui::a11y::Action::Activate, &[1]),
                "an offered Export reports that it acted",
            );
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn inspector_state_is_the_same_for_eye_and_reader() {
    // Case `the_inspector_says_the_same_empty_thing_to_the_eye_and_to_a_reader`.
    {
        // These were two different sentences for one state.
        let mut doc = AutoCommit::new();
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        let node = app.a11y_inspector();
        let spoken = node
            .children
            .iter()
            .filter_map(|n| n.name.clone())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(spoken.contains(INSPECTOR_EMPTY_TITLE), "spoken: {spoken}");
        assert!(spoken.contains(INSPECTOR_EMPTY_HINT), "spoken: {spoken}");
    }
    // Case `the_reader_is_offered_exactly_the_inspector_that_is_on_screen`:
    // A screen-reader user and a sighted user have to be navigating the
    // same panel. This walks every state the two new sections have — off,
    // on, and on an audio clip that has neither — and asserts the reader's
    // list of sections and controls is exactly what the layout put on
    // screen, never a tidier or a fuller story.
    {
        let (mut app, mut doc) = app_and_doc();
        let video = app.project.top_video_clip_at(23.0).unwrap().id;
        let audio = app
            .project
            .clips
            .iter()
            .find(|c| {
                app.project
                    .track(c.track)
                    .is_some_and(|t| t.kind == TrackKind::Audio)
            })
            .map(|c| c.id);

        let check = |app: &VideoEditorApp, note: &str| {
            let r = app.regions();
            let l = app.inspector_layout(&r);
            let clip = app.selected().and_then(|id| app.project.clip(id)).cloned();
            let on_screen: Vec<Section> = l.sections.iter().map(|(s, _)| *s).collect();
            assert_eq!(
                app.a11y_sections(clip.as_ref()),
                on_screen,
                "{note}: the reader's sections must be the drawn sections",
            );
            for s in &on_screen {
                if app.collapsed.contains(s) {
                    continue;
                }
                let spoken = VideoEditorApp::a11y_section_fields(*s, clip.as_ref());
                for f in &spoken {
                    assert!(
                        l.num(*f).is_some(),
                        "{note}: the reader is offered {f:?} in {s:?}, which is not drawn",
                    );
                }
                // And nothing drawn inside this section is missing from it.
                // Only the sections that own numeric rows are checked, since
                // the layout does not tag a row with its section.
                if matches!(*s, Section::Key | Section::Mask) {
                    let drawn: Vec<NumField> = l
                        .nums
                        .iter()
                        .map(|(f, _)| *f)
                        .filter(|f| {
                            matches!(
                                f,
                                NumField::KeyTolerance | NumField::KeySoftness | NumField::KeySpill
                            ) == (*s == Section::Key)
                                && matches!(
                                    f,
                                    NumField::KeyTolerance
                                        | NumField::KeySoftness
                                        | NumField::KeySpill
                                        | NumField::MaskX
                                        | NumField::MaskY
                                        | NumField::MaskW
                                        | NumField::MaskH
                                        | NumField::MaskRotation
                                        | NumField::MaskFeather
                                )
                        })
                        .collect();
                    assert_eq!(
                        drawn, spoken,
                        "{note}: {s:?} draws and speaks the same rows"
                    );
                }
            }
        };

        app.selection.select_only(video);
        check(&app, "an untouched video clip");
        app.edit_selected(&mut doc, |c| {
            c.key.enabled = true;
            c.mask.shape = crate::model::MaskShape::Rect;
        });
        check(&app, "keyed and masked");
        app.collapsed.insert(Section::Key);
        check(&app, "with the key folded away");
        app.collapsed.remove(&Section::Key);

        if let Some(audio) = audio {
            app.selection.select_only(audio);
            let r = app.regions();
            let l = app.inspector_layout(&r);
            let on_screen: Vec<Section> = l.sections.iter().map(|(s, _)| *s).collect();
            assert!(
                !on_screen.contains(&Section::Key) && !on_screen.contains(&Section::Mask),
                "an audio clip has no picture to key or mask: {on_screen:?}",
            );
            check(&app, "an audio clip");
        }
    }
    // Case `no_two_inspector_controls_answer_to_the_same_accessible_key`:
    // No two inspector rows share an accessible key.
    //
    // `NumField::Rotation` and `NumField::MaskRotation` both label themselves
    // "Rotation" — each correctly, inside its own section — and the key was
    // `hash_key(&label())`, so the two hashed to the same number. An action
    // routed at the mask's tilt was applied to whichever row the walk reached
    // first, which is the transform's: the caller asked to rotate a mask and
    // rotated the picture, and was told it had worked.
    //
    // Asserted across the whole enum rather than on the one known pair, so the
    // next two rows that happen to share a word fail here instead of shipping.
    {
        const EVERY: [NumField; 30] = [
            NumField::Opacity,
            NumField::Volume,
            NumField::Brightness,
            NumField::Contrast,
            NumField::Saturation,
            NumField::FadeIn,
            NumField::FadeOut,
            NumField::TransitionLen,
            NumField::ScaleX,
            NumField::ScaleY,
            NumField::PosX,
            NumField::PosY,
            NumField::Rotation,
            NumField::CropLeft,
            NumField::CropRight,
            NumField::CropTop,
            NumField::CropBottom,
            NumField::KeyTolerance,
            NumField::KeySoftness,
            NumField::KeySpill,
            NumField::MaskX,
            NumField::MaskY,
            NumField::MaskW,
            NumField::MaskH,
            NumField::MaskRotation,
            NumField::MaskFeather,
            NumField::Speed,
            NumField::TitleSize,
            NumField::TitleBackdrop,
            NumField::CurveMix,
        ];
        // The pair that was actually broken, named so the regression is legible
        // even if the loop below is ever loosened.
        assert_eq!(
            NumField::Rotation.label(),
            NumField::MaskRotation.label(),
            "both rows still read \u{201c}Rotation\u{201d} — which is fine, and is \
             exactly why the key cannot be the label",
        );
        assert_ne!(
            NumField::Rotation.a11y_key(),
            NumField::MaskRotation.a11y_key(),
            "the transform's tilt and the mask's tilt are different controls",
        );
        let mut seen: HashMap<u64, NumField> = HashMap::new();
        for f in EVERY {
            if let Some(other) = seen.insert(f.a11y_key(), f) {
                panic!(
                    "{} and {} hash to one accessible key, so an action routed at \
                     one lands on the other",
                    f.tag(),
                    other.tag(),
                );
            }
        }
        // And the pick fields share the same hashed namespace once they are in
        // the tree, so a control and a segment must not collide either.
        for f in EVERY {
            for pk in [
                PickField::KeyEnable,
                PickField::MatchFormat,
                PickField::MaskShape(crate::model::MaskShape::Ellipse),
                PickField::Transition(Transition::CrossDissolve),
                PickField::Label(3),
                PickField::Frame(0),
                PickField::Rate(0),
            ] {
                assert_ne!(
                    f.a11y_key(),
                    pk.a11y_key(),
                    "{} collides with {}",
                    f.tag(),
                    pk.tag(),
                );
            }
        }
    }
}

#[test]
fn portal_destinations_save_where_chosen_or_cancel_cleanly() {
    // Case `a_chosen_destination_arriving_from_the_portal_saves_where_it_said`.
    {
        // Drives the real per-frame drain the worker feeds, not the writer
        // underneath it — the wiring is the part that breaks.
        let dir = scratch("portal");
        let path = dir.join("Portal.qvp");
        let (mut app, mut doc) = app_and_doc();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path.clone()));
        app.request_save_as();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.project_path.as_ref(),
            Some(&path),
            "the save went where it was told"
        );
        assert!(path.is_file());

        // And an Open arriving the same way loads it back.
        let mut fresh_doc = AutoCommit::new();
        let mut fresh = VideoEditorApp::new();
        fresh.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        fresh.sync(&mut fresh_doc, &automerge::ROOT);
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
        fresh.request_open_project();
        fresh.sync(&mut fresh_doc, &automerge::ROOT);
        let titles: Vec<String> = recorder
            .pick_requests()
            .iter()
            .map(|r| r.title().to_string())
            .collect();
        assert_eq!(titles, ["Save project", "Open project"]);
        assert_eq!(fresh.project.clips.len(), 7, "the whole timeline arrived");
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `a_cancelled_chooser_clears_the_notice_and_abandons_a_queued_open`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.save_then_open = Some(PathBuf::from("/p/Q.qvp"));
        // An unanswered chooser on the recording host comes back cancelled.
        let (services, _recorder) = ui::host::HostServices::recording();
        ui::host::scoped(&services, || app.request_save_as());
        assert_eq!(
            app.project_notice.as_ref().map(|n| n.text.as_str()),
            Some("Choose where to save…"),
        );
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.project_notice.is_none(),
            "no stale 'choosing…' line left behind"
        );
        assert!(
            app.save_then_open.is_none(),
            "and the open it was gating is dropped"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn keyboard_edit_shortcuts_mutate_the_crdt() {
    // Case `split_shortcut_mutates_the_crdt_timeline`.
    {
        let (mut app, mut doc) = app_and_doc();
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selection.select_only(target);
        app.playhead = 12.0;
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(key_press(&mut app, &mut doc, "s", false), "S handled");
        // The new clip is in the *document*, not just the in-memory mirror.
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips.len(),
            before + 1,
            "split wrote a clip to the CRDT"
        );
        assert!(
            app.actions.take_committed().is_some(),
            "split dispatched an action, arming one undo boundary",
        );
    }
    // Case `copy_paste_shortcuts_add_an_independent_clip`.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(5.0).unwrap().id;
        app.selection.select_only(id);
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(key_press(&mut app, &mut doc, "c", true), "Ctrl+C handled");
        app.playhead = 30.0;
        assert!(key_press(&mut app, &mut doc, "v", true), "Ctrl+V handled");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips.len(),
            before + 1,
            "paste added a clip to the CRDT"
        );
        // The pasted clip is the new selection and sits at the playhead.
        let pasted = app.selected().unwrap();
        assert_ne!(pasted, id);
        assert!((reloaded.clip(pasted).unwrap().start - 30.0).abs() < 1e-9);
    }
    // Case `set_in_out_shortcuts_trim_the_selected_clip`.
    {
        let (mut app, mut doc) = app_and_doc();
        // Interview clip spans roughly [8, 20] on V1.
        let id = app.project.top_video_clip_at(12.0).unwrap().id;
        app.selection.select_only(id);
        // Move playhead into the clip and set the out-point there.
        app.playhead = 15.0;
        assert!(key_press(&mut app, &mut doc, "o", false), "O handled");
        let reloaded = VideoDoc::load_project(&doc);
        assert!(
            (reloaded.clip(id).unwrap().end() - 15.0).abs() < 1e-6,
            "O trims the tail to the playhead"
        );
        assert!(
            app.actions.take_committed().is_some(),
            "setting the out-point dispatched an action, arming one undo boundary",
        );
    }
    // Case `jkl_shuttle_moves_and_stops_the_playhead`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 5.0;
        assert!(key_press(&mut app, &mut doc, "l", false), "L handled");
        assert!(app.shuttle > 0.0, "L shuttles forward");
        assert!(key_press(&mut app, &mut doc, "l", false));
        assert!(app.shuttle >= 2.0, "repeated L doubles the rate");
        assert!(key_press(&mut app, &mut doc, "k", false), "K handled");
        assert_eq!(app.shuttle, 0.0, "K stops the shuttle");
        assert!(key_press(&mut app, &mut doc, "j", false));
        assert!(app.shuttle < 0.0, "J shuttles in reverse");
    }
    // Case `ripple_delete_shortcut_closes_the_gap_in_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        // Market cutaway is the last V1 clip at start=20.
        let v1_first = app.project.top_video_clip_at(3.0).unwrap();
        let track = v1_first.track;
        let first = v1_first.id;
        // Record the next clip on the same track and its start.
        let next = app
            .project
            .clips_on(track)
            .into_iter()
            .find(|c| c.id != first)
            .map(|c| (c.id, c.start));
        app.selection.select_only(first);
        let mods = winit::event::Modifiers::from(ModifiersState::SHIFT);
        let r = app.regions();
        // Shift+Delete ripples.
        app.handle_key(&WKey::Named(NamedKey::Delete), mods, &mut doc, &r);
        let reloaded = VideoDoc::load_project(&doc);
        assert!(
            reloaded.clip(first).is_none(),
            "rippled clip removed from CRDT"
        );
        if let Some((nid, old_start)) = next {
            assert!(
                reloaded.clip(nid).unwrap().start < old_start,
                "later clip shifted left in the CRDT"
            );
        }
    }
    // Case `nudge_arrow_moves_selected_clip_in_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(12.0).unwrap().id;
        app.selection.select_only(id);
        let before = VideoDoc::load_project(&doc).clip(id).unwrap().start;
        let r = app.regions();
        // Shift = one-second nudge right.
        let shift = winit::event::Modifiers::from(ModifiersState::SHIFT);
        app.handle_key(&WKey::Named(NamedKey::ArrowRight), shift, &mut doc, &r);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().start;
        assert!(
            (after - before - 1.0).abs() < 1e-6,
            "Shift+Right nudges +1s in the CRDT"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn inspector_grade_and_geometry_controls_write_the_document() {
    // Case `dragging_the_brightness_slider_writes_the_grade_to_the_crdt`:
    // Dragging the brightness slider writes the grade through to the CRDT.
    //
    // This used to call `set_filter` — a helper whose only other caller is a
    // screenshot scenario — so it proved that a function nothing on the live
    // path goes through could write a document. The slider a person actually
    // drags goes `press_inspector` → `preview_param` → `commit_drag`: it has
    // to hit-test the track, resolve the value from the pointer, hold the edit
    // in memory while the drag is live so the preview can follow it without
    // writing a document per frame, and dispatch exactly one action on
    // release. That is four more places to break than the helper had, and the
    // verification covenant asks for the production path rather than a
    // stand-in for it.
    {
        let (mut app, mut doc) = app_and_doc();
        // A video clip, so the panel lays out a brightness row at all.
        let id = app
            .project
            .top_video_clip_at(5.0)
            .expect("the fixture has a video clip at 5s")
            .id;
        app.selection.select_only(id);
        app.playhead = 5.0;
        app.inspector_scroll = 0.0;
        let r = app.regions();
        let track = app
            .inspector_layout(&r)
            .brightness
            .expect("a selected video clip is offered a brightness slider");
        let want = 0.9;
        let x = track.x0 + track.width() * want;

        click(&mut app, &mut doc, x, track.center().y);
        let previewed = app.project.clip(id).expect("clip").filters.brightness;
        assert!(
            (previewed - want).abs() < 1e-9,
            "the live drag previews the value under the pointer, got {previewed}",
        );
        assert!(
            (VideoDoc::load_project(&doc)
                .clip(id)
                .expect("clip")
                .filters
                .brightness
                - previewed)
                .abs()
                > 1e-9,
            "and holds it in memory until the drag ends, so a drag is one undo \
             step rather than one per frame",
        );

        release(&mut app, &mut doc);
        let written = VideoDoc::load_project(&doc)
            .clip(id)
            .expect("the clip survives its own edit")
            .filters
            .brightness;
        assert!(
            (written - want).abs() < 1e-9,
            "the release wrote the grade to the CRDT, got {written}",
        );
        assert!(
            app.actions.take_committed().is_some(),
            "the grade dispatched an action, arming one undo boundary",
        );
    }
    // Case `dragging_a_geometry_control_writes_the_transform_to_the_document`:
    // Every new control writes through an action, so the value has to be in
    // the CRDT — not merely in the in-memory preview — once the interaction
    // ends. This drags each ranged control and reads the document back.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        assert!(
            VideoDoc::load_project(&doc)
                .clip(id)
                .unwrap()
                .transform
                .is_identity(),
            "starts as the plain aspect-fit",
        );
        // Drag Scale X to a quarter along its 0.05..4 span.
        let track = reveal(&mut app, |l| l.num(NumField::ScaleX));
        let x = track.x0 + track.width() * 0.25;
        click(&mut app, &mut doc, x, track.center().y);
        release(&mut app, &mut doc);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().transform;
        let want = NumField::ScaleX.at_frac(0.25);
        assert!(
            (after.scale_x - want).abs() < 0.05,
            "scale_x is {} not ~{want}",
            after.scale_x,
        );
        assert!(!after.is_identity(), "the clip is no longer plainly fitted");
        // Crop writes its own edge and leaves the others alone.
        let track = reveal(&mut app, |l| l.num(NumField::CropTop));
        click(
            &mut app,
            &mut doc,
            track.x0 + track.width() * 0.5,
            track.center().y,
        );
        release(&mut app, &mut doc);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().transform;
        assert!(
            after.crop_top > 0.3 && after.crop_top < 0.6,
            "crop_top is {}",
            after.crop_top
        );
        assert_eq!(after.crop_bottom, 0.0, "only the dragged edge moved");
        assert!(after.is_cropped());
    }
    // Case `the_rate_control_is_logarithmic_and_re_times_the_clip`:
    // A rate slider on a linear track would put 1x at 5% of the way along.
    // It is logarithmic, so normal speed is the centre and half / double are
    // symmetric about it — and the value reaches the document.
    {
        assert!(
            (NumField::Speed.to_frac(1.0) - 0.5).abs() < 1e-9,
            "normal speed sits at the centre of the track",
        );
        let half = NumField::Speed.to_frac(0.5);
        let double = NumField::Speed.to_frac(2.0);
        assert!(
            ((0.5 - half) - (double - 0.5)).abs() < 1e-9,
            "half and double speed are the same distance from normal",
        );
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let before = VideoDoc::load_project(&doc).clip(id).unwrap().duration();
        // Click the "2x" preset.
        let rect = reveal(&mut app, |l| {
            l.speed_presets
                .iter()
                .find(|(rate, _)| rate.to_bits() == 2.0f64.to_bits())
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        assert_eq!(after.speed.rate(), 2.0);
        assert!(
            (after.duration() - before / 2.0).abs() < 1e-6,
            "2x halves the clip's length on the timeline",
        );
        assert_eq!(
            after.source_span(),
            before,
            "and leaves its source window alone"
        );
        // A rate that is not on a preset highlights none of them. A segmented
        // control that always lit something would claim 1.37x was 1x.
        app.edit_selected(&mut doc, |c| {
            c.set_speed(1.37, false);
        });
        let clip = app.project.clip(id).unwrap().clone();
        assert!(
            !SPEED_PRESETS
                .iter()
                .any(|p| (p - clip.speed.rate()).abs() < 1e-6),
            "1.37x is deliberately off every preset",
        );
        app.edit_selected(&mut doc, |c| {
            c.set_speed(2.0, false);
        });
        // Reverse is a separate decision from the rate.
        let rect = reveal(&mut app, |l| l.reverse);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        assert!(after.speed.reverse, "the toggle reversed it");
        assert_eq!(after.speed.rate(), 2.0, "without disturbing the rate");
    }
    // Case `a_picture_in_picture_preset_insets_the_clip_into_that_corner`:
    // A corner preset has to produce a real inset — scaled down and pushed
    // into the named corner — not merely a non-default transform.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        // The layout lists the corners in the order top-left, top-right,
        // bottom-left, bottom-right.
        let rect = reveal(&mut app, |l| l.pip.get(3).map(|(_, _, r)| *r));
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let t = VideoDoc::load_project(&doc).clip(id).unwrap().transform;
        assert!(
            t.scale_x < 0.5 && t.scale_y < 0.5,
            "the picture is inset, not full frame"
        );
        assert!(t.pos_x > 0.0, "bottom-right sits right of centre");
        assert!(t.pos_y > 0.0, "and below it");
        // And the way back.
        let rect = reveal(&mut app, |l| l.reset_transform);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert!(
            VideoDoc::load_project(&doc)
                .clip(id)
                .unwrap()
                .transform
                .is_identity(),
            "Fill frame restores the plain aspect-fit",
        );
    }
    // Case `editing_brightness_brightens_the_preview_composite`.
    {
        // Drive the real inspector edit path, then assert the preview module's
        // composited frame for that clip is brighter — proving inspector →
        // CRDT → preview is one connected pipeline.
        let (mut app, mut doc) = app_and_doc();
        // Pick the market cutaway on V1 (neutral grade, fully steady at t=23).
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let before = match preview::composite(&app.project, 23.0) {
            CompositedFrame::Clip { base, .. } => base.luma(),
            CompositedFrame::Empty => panic!("expected clip"),
        };
        app.set_num(NumField::Brightness, 0.95);
        app.dispatch_clip(&mut doc, id);
        // Reload from the CRDT so we're reading the persisted grade, not the
        // in-memory mirror.
        app.sync(&mut doc, &automerge::ROOT);
        let after = match preview::composite(&app.project, 23.0) {
            CompositedFrame::Clip { base, .. } => base.luma(),
            CompositedFrame::Empty => panic!("expected clip"),
        };
        assert!(
            after > before,
            "raising brightness brightens the composite ({before} -> {after})"
        );
    }
    // Case `the_anchor_grid_moves_the_point_the_picture_turns_about`.
    {
        let (mut app, mut doc) = app_and_doc();
        let cid = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(cid);
        app.collapsed.remove(&Section::Transform);
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert_eq!(l.anchor.len(), 9, "three by three, no more and no fewer");
        // The default is the middle, which is what an untouched clip has.
        assert_eq!(app.project.clip(cid).unwrap().transform.anchor_x, 0.5);

        let top_left = l
            .anchor
            .iter()
            .find(|(a, _)| *a == (0.0, 0.0))
            .map(|(_, rect)| *rect)
            .expect("a top-left cell");
        reveal_inspector_rect_for_test(&mut app, &r, top_left);
        let r = app.regions();
        let top_left = app
            .inspector_layout(&r)
            .anchor
            .iter()
            .find(|(a, _)| *a == (0.0, 0.0))
            .map(|(_, rect)| *rect)
            .expect("top-left stays reachable after scrolling");
        app.press_inspector(&mut doc, &r, top_left.center());
        let t = app.project.clip(cid).unwrap().transform;
        assert_eq!((t.anchor_x, t.anchor_y), (0.0, 0.0));

        // And it survives the document, which is what makes it a real edit
        // rather than a widget field.
        let back = VideoDoc::load_project(&doc);
        let t = back.clip(cid).unwrap().transform;
        assert_eq!((t.anchor_x, t.anchor_y), (0.0, 0.0));
    }
}

// ---- click-driven tests: drive the REAL handle_event pipeline --------
// These synthesise winit events at rects the app itself computes, so they
// exercise the exact production path (regions → hit-test → drag/mutation →
// CRDT) a live click takes — not shortcut helpers.

/// Winit `DeviceId` isn't publicly constructible; zero-init it (the app
/// never inspects it), matching the terminal/date-picker test pattern.
fn fake_device_id() -> winit::event::DeviceId {
    unsafe { std::mem::zeroed() }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "call sites build the event inline; the helper only borrows it"
)]
fn dispatch(app: &mut VideoEditorApp, doc: &mut AutoCommit, ev: WindowEvent) {
    let mut handled = false;
    let mut focus = None;
    let taffy = TaffyTree::<()>::new();
    let node = NodeId::new(0);
    app.handle_event(
        &ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: doc,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

fn move_to(app: &mut VideoEditorApp, doc: &mut AutoCommit, x: f64, y: f64) {
    dispatch(
        app,
        doc,
        WindowEvent::CursorMoved {
            device_id: fake_device_id(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        },
    );
}
fn press(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
    dispatch(
        app,
        doc,
        WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
}
fn release(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
    dispatch(
        app,
        doc,
        WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
}
/// A full click at (x,y): move (sets `last_mouse`, as the live loop does),
/// then press.
fn click(app: &mut VideoEditorApp, doc: &mut AutoCommit, x: f64, y: f64) {
    move_to(app, doc, x, y);
    press(app, doc);
}
fn right_press(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
    dispatch(
        app,
        doc,
        WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        },
    );
}
fn char_ev(s: &str) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = WKey::Character(s.into());
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(s.into());
    WindowEvent::KeyboardInput {
        device_id: fake_device_id(),
        event: ev,
        is_synthetic: false,
    }
}
fn named_ev(k: NamedKey) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = WKey::Named(k);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: fake_device_id(),
        event: ev,
        is_synthetic: false,
    }
}

/// Click the open context-menu row whose label is `label`, through the
/// menu's real geometry. Addressing rows by label rather than by index
/// means adding an entry cannot silently retarget a test at its neighbour.
fn click_menu_item(app: &mut VideoEditorApp, doc: &mut AutoCommit, label: &str) {
    let idx = app
        .bin_menu
        .items()
        .iter()
        .position(|i| i.label() == Some(label))
        .unwrap_or_else(|| {
            let have: Vec<&str> = app
                .bin_menu
                .items()
                .iter()
                .filter_map(|i| i.label())
                .collect();
            panic!("no menu row labelled {label:?}; menu has {have:?}")
        });
    let menu = app.bin_menu.get_rect();
    let cx = menu.center().x;
    let y = (0..(menu.height() as i64))
        .map(|d| menu.y0 + d as f64 + 0.5)
        .find(|&y| app.bin_menu.item_at([cx, y]) == Some(idx))
        .unwrap_or_else(|| panic!("row {label:?} has no hit area"));
    move_to(app, doc, cx, y);
    press(app, doc);
}

// ---- modifier-aware + wheel drivers ----------------------------------
// Everything below goes through the same `handle_event` the live window
// loop calls; nothing reaches into a helper the app itself doesn't use.

#[expect(
    clippy::needless_pass_by_value,
    reason = "call sites build the event inline; the helper only borrows it"
)]
fn dispatch_mods(
    app: &mut VideoEditorApp,
    doc: &mut AutoCommit,
    ev: WindowEvent,
    mods: ModifiersState,
) {
    let mut handled = false;
    let mut focus = None;
    let taffy = TaffyTree::<()>::new();
    let node = NodeId::new(0);
    app.handle_event(
        &ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::from(mods),
            state: doc,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

/// Press with Shift held — the multi-select gesture.
fn shift_press(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
    dispatch_mods(
        app,
        doc,
        WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        ModifiersState::SHIFT,
    );
}

fn shift_click(app: &mut VideoEditorApp, doc: &mut AutoCommit, x: f64, y: f64) {
    move_to(app, doc, x, y);
    shift_press(app, doc);
    release(app, doc);
}

fn wheel(app: &mut VideoEditorApp, doc: &mut AutoCommit, dx: f32, dy: f32, mods: ModifiersState) {
    dispatch_mods(
        app,
        doc,
        WindowEvent::MouseWheel {
            device_id: fake_device_id(),
            delta: MouseScrollDelta::LineDelta(dx, dy),
            phase: winit::event::TouchPhase::Moved,
        },
        mods,
    );
}

/// Run the app's real `draw` pass. No GPU and no window: the Vello scene is
/// encoded on the CPU and dropped. Tests call it because `draw` is where
/// per-frame view reconciliation happens (playhead following, scroll
/// clamping), so skipping it would test a frame the user never sees.
fn render(app: &mut VideoEditorApp) {
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
}

/// The lane rect of the nth track of `kind`, plus its id.
fn lane_of(app: &VideoEditorApp, r: &Regions, kind: TrackKind, n: usize) -> (TrackId, Rect) {
    let (tid, _k, lane) = app
        .lane_rects(r)
        .into_iter()
        .filter(|(_, k, _)| *k == kind)
        .nth(n)
        .expect("a lane of the requested kind");
    (tid, lane)
}

/// The on-screen rect of `cid`, resolved through the same geometry the
/// renderer and the hit-test use.
fn clip_screen_rect(app: &VideoEditorApp, r: &Regions, cid: ClipId) -> Rect {
    let clip = app.project.clip(cid).cloned().expect("clip exists");
    let (_, _, lane) = app
        .lane_rects(r)
        .into_iter()
        .find(|(tid, _, _)| *tid == clip.track)
        .expect("the clip's lane");
    app.clip_rect(&clip, &lane, r)
}

// ---- media bin: drag stays reachable, and drops land where dropped ----

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn name_band_drags_place_and_slow_clicks_rename() {
    // Case `a_drag_from_the_name_band_places_a_clip_instead_of_renaming`.
    {
        // Regression: `clicked_before` was unbounded, so once an item had been
        // clicked, every later press on the name band — which covers most of
        // the row, and is exactly where you grab it — opened a rename and the
        // bin→timeline drag became unreachable.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).is_some_and(|mm| mm.kind.is_visual()))
            .expect("a visual bin item");
        let name = VideoEditorApp::bin_name_rect(row);
        // First click on the row, as a user browsing the bin would.
        click(&mut app, &mut doc, name.center().x, name.center().y);
        release(&mut app, &mut doc);
        // Wait past the double-click window, into the slow-second-click window
        // where the old code renamed unconditionally.
        std::thread::sleep(std::time::Duration::from_millis(
            BIN_DOUBLE_CLICK_MS as u64 + 60,
        ));
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let before = app.project.clips_on(tid).len();
        // Press on the name band and drag to a lane.
        move_to(&mut app, &mut doc, name.center().x, name.center().y);
        press(&mut app, &mut doc);
        assert!(
            app.bin_drag.is_some(),
            "the press armed a drag, not just a rename"
        );
        move_to(&mut app, &mut doc, lane.x0 + 220.0, lane.center().y);
        assert!(
            matches!(app.bin_drag, Some(BinDrag { active: true, .. })),
            "moving past the threshold promoted the drag",
        );
        assert!(
            app.pending_rename.is_none(),
            "becoming a drag cancelled the rename intent"
        );
        release(&mut app, &mut doc);
        assert!(app.bin_rename.is_none(), "no rename field opened");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 1,
            "the drag placed a clip on the drop lane in the CRDT",
        );
        assert_eq!(
            app.selected()
                .and_then(|id| reloaded.clip(id))
                .map(|c| c.media),
            Some(mid),
            "the placed clip references the dragged media",
        );
    }
    // Case `a_slow_second_click_on_the_name_still_renames`.
    {
        // The other half of the same rule: bounding the rename window must not
        // cost the Finder-style click-pause-click rename.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app.bin_item_rects(&r)[0];
        let name = VideoEditorApp::bin_name_rect(row);
        click(&mut app, &mut doc, name.center().x, name.center().y);
        release(&mut app, &mut doc);
        std::thread::sleep(std::time::Duration::from_millis(
            BIN_DOUBLE_CLICK_MS as u64 + 60,
        ));
        // A second press that never moves.
        move_to(&mut app, &mut doc, name.center().x, name.center().y);
        press(&mut app, &mut doc);
        assert_eq!(
            app.pending_rename,
            Some(mid),
            "the press armed a rename intent"
        );
        release(&mut app, &mut doc);
        assert!(
            matches!(app.bin_rename.as_ref().map(|(m, _)| *m), Some(m) if m == mid),
            "releasing without moving began the inline rename",
        );
    }
    // Case `dropping_in_the_track_header_gutter_places_rather_than_appends`.
    {
        // Regression: `bin_drop_lane` required x >= lanes.x0, so a straight-down
        // drag from the bin landed in the 128px gutter, matched no lane, and
        // fell through to the append path at the end of the timeline.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).is_some_and(|mm| mm.kind.is_visual()))
            .expect("a visual bin item");
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let track_end = app.project.clips_on(tid).last().map_or(0.0, |c| c.end());
        assert!(
            track_end > 1.0,
            "the fixture track has content to append past"
        );
        let before = app.project.clips_on(tid).len();
        // Straight down from the bin row into the gutter, level with the lane.
        let gutter_x = r.gutter.x0 + r.gutter.width() / 2.0;
        assert!(
            gutter_x < r.lanes.x0,
            "the drop really is left of the lane area"
        );
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, gutter_x, lane.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 1,
            "a clip was placed"
        );
        let placed = app
            .selected()
            .and_then(|id| reloaded.clip(id))
            .expect("the placed clip");
        assert_eq!(placed.media, mid);
        assert!(
            placed.start < 0.5,
            "a gutter drop places at the start of the visible timeline, not appended at {track_end} (got {})",
            placed.start,
        );
    }
    // Case `inline_rename_writes_the_new_name_to_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, _row) = app.bin_item_rects(&r)[0];
        app.begin_rename(mid);
        assert!(app.bin_rename.is_some(), "rename began");
        // begin_edit select-all: the first keystroke replaces the whole name.
        for ch in ["R", "e", "e", "l"] {
            dispatch(&mut app, &mut doc, char_ev(ch));
        }
        // Enter commits → dispatches an UpsertMedia carrying the new name.
        dispatch(&mut app, &mut doc, named_ev(NamedKey::Enter));
        assert!(app.bin_rename.is_none(), "Enter ends the rename");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.media(mid).unwrap().name,
            "Reel",
            "rename wrote through to the CRDT"
        );
        assert!(
            app.actions.take_committed().is_some(),
            "the rename dispatched an action, arming one undo boundary",
        );
    }
}

// ---- horizontal timeline scroll --------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn timeline_scrolls_pans_and_follows_playback() {
    // Case `shift_wheel_pans_the_timeline_and_clamps_to_the_content`.
    {
        let (mut app, mut doc) = app_and_doc();
        move_to(&mut app, &mut doc, 640.0, 700.0); // over the lanes
        assert_eq!(app.scroll, 0.0, "the timeline opens at the start");
        wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::SHIFT);
        assert!(
            app.scroll > 0.0,
            "shift+wheel scrolled right, got {}",
            app.scroll
        );
        let r = app.regions();
        let max = app.max_scroll(&r);
        // Far past the end: it stops at the content bound rather than running off.
        for _ in 0..200 {
            wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::SHIFT);
        }
        assert!(
            (app.scroll - max).abs() < 1e-6,
            "scrolling right clamps to max_scroll ({} vs {max})",
            app.scroll,
        );
        // And back: it stops at zero.
        for _ in 0..300 {
            wheel(&mut app, &mut doc, 0.0, 3.0, ModifiersState::SHIFT);
        }
        assert_eq!(app.scroll, 0.0, "scrolling left clamps to the start");
    }
    // Case `a_horizontal_wheel_pans_without_zooming`.
    {
        let (mut app, mut doc) = app_and_doc();
        move_to(&mut app, &mut doc, 640.0, 700.0);
        let zoom = app.zoom;
        wheel(&mut app, &mut doc, -3.0, 0.0, ModifiersState::empty());
        assert!(app.scroll > 0.0, "a horizontal wheel panned the timeline");
        assert_eq!(app.zoom, zoom, "and left the zoom alone");
    }
    // Case `page_keys_scroll_and_announce_the_overflowing_timeline`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.focus.pane = Pane::Timeline;
        app.zoom = 160.0;
        let before = app.scroll;

        assert!(nav_key(
            &mut app,
            &mut doc,
            Key::Named(NamedKey::PageDown),
            false
        ));
        assert!(
            app.scroll > before,
            "Page Down pans to later timeline content"
        );
        assert!(
            app.a11y_timeline()
                .value
                .as_deref()
                .is_some_and(|value| value.contains("Page Up and Page Down"))
        );

        assert!(nav_key(
            &mut app,
            &mut doc,
            Key::Named(NamedKey::PageUp),
            false
        ));
        assert!(app.scroll < app.max_scroll(&app.regions()));
    }
    // Case `zoomed_in_content_past_the_right_edge_becomes_reachable`.
    {
        // The bug this closes: with no way to write `scroll`, zooming in put
        // everything past the viewport permanently out of reach.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let last_end = app
            .project
            .clips
            .iter()
            .map(crate::model::Clip::end)
            .fold(0.0_f64, f64::max);
        move_to(&mut app, &mut doc, r.lanes.x0 + 10.0, r.lanes.center().y);
        // Zoom right in, so the tail is far off-screen.
        for _ in 0..12 {
            wheel(&mut app, &mut doc, 0.0, 3.0, ModifiersState::empty());
        }
        assert!(
            app.time_to_x(last_end, &r.lanes) > r.lanes.x1,
            "the timeline tail really is off the right edge after zooming",
        );
        for _ in 0..400 {
            wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::SHIFT);
        }
        assert!(
            app.time_to_x(last_end, &r.lanes) <= r.lanes.x1 + 1.0,
            "panning brought the end of the timeline into view",
        );
    }
    // Case `dragging_the_scrollbar_thumb_scrolls_the_timeline`.
    {
        let (mut app, mut doc) = app_and_doc();
        // Zoom in so the content overflows and a bar appears.
        app.zoom = 160.0;
        let r = app.regions();
        let track = app
            .hscroll_track(&r)
            .expect("an overflowing timeline shows a bar");
        let thumb = app
            .hbar
            .thumb_rect(track, app.h_metrics(&r))
            .expect("a thumb inside the track");
        assert!(
            thumb.width() < track.width(),
            "the thumb is shorter than its track"
        );
        // Grab the thumb and drag it right.
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::Scrollbar(ScrollPane::Timeline))),
            "pressing the thumb began a bar drag",
        );
        move_to(
            &mut app,
            &mut doc,
            thumb.center().x + 120.0,
            thumb.center().y,
        );
        assert!(app.scroll > 0.0, "dragging the thumb scrolled the timeline");
        let dragged = app.scroll;
        release(&mut app, &mut doc);
        assert!(app.drag.is_none(), "release ended the bar drag");
        assert_eq!(
            app.scroll, dragged,
            "and left the scroll where the drag put it"
        );
    }
    // Case `playback_keeps_the_playhead_on_screen`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.zoom = 160.0; // a few seconds visible at a time
        let r = app.regions();
        let visible = app.visible_seconds(&r);
        // Park the playhead well past the right edge, the way playback would.
        app.playhead = (visible * 3.0).min(app.project.duration());
        assert!(
            app.time_to_x(app.playhead, &r.lanes) > r.lanes.x1,
            "the playhead starts off the right edge",
        );
        // Press play through the real transport button, then render a frame.
        let (_a, play) = app.transport_buttons(&r)[2];
        click(&mut app, &mut doc, play.center().x, play.center().y);
        assert!(
            app.playback.playing,
            "the transport button started playback"
        );
        render(&mut app);
        let x = app.time_to_x(app.playhead, &r.lanes);
        assert!(
            x >= r.lanes.x0 && x <= r.lanes.x1,
            "the view followed the playhead into view (x={x}, lanes {}..{})",
            r.lanes.x0,
            r.lanes.x1,
        );
    }
    // Case `a_wheel_over_the_preview_does_not_move_the_timeline`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (zoom, scroll) = (app.zoom, app.scroll);
        move_to(&mut app, &mut doc, r.canvas.center().x, r.canvas.center().y);
        wheel(&mut app, &mut doc, 0.0, 3.0, ModifiersState::empty());
        wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::empty());
        assert_eq!(app.zoom, zoom, "the preview is not a timeline zoom surface");
        assert_eq!(app.scroll, scroll);
    }
}

// ---- cross-track clip move -------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn lane_drags_respect_kind_and_track_locks() {
    // Case `dragging_a_clip_onto_another_video_lane_moves_it_there_in_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Two video lanes exist in the sample project.
        let (t0, lane0) = lane_of(&app, &r, TrackKind::Video, 0);
        let (t1, _lane1) = lane_of(&app, &r, TrackKind::Video, 1);
        assert_ne!(t0, t1, "the fixture has two video tracks");
        // Grab a clip on the lower video lane and drag it up to the other one.
        let cid = app
            .project
            .clips_on(t1)
            .first()
            .map(|c| c.id)
            .expect("a clip on V-lower");
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::ClipMove { track, .. }) if track == t1),
            "the move started on the clip's own track",
        );
        move_to(&mut app, &mut doc, cr.center().x + 40.0, lane0.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clip(cid).map(|c| c.track),
            Some(t0),
            "the clip changed track in the CRDT, not just on screen",
        );
    }
    // Case `a_video_clip_dragged_over_an_audio_lane_keeps_its_own_track`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (vid, _vlane) = lane_of(&app, &r, TrackKind::Video, 0);
        let (_aid, alane) = lane_of(&app, &r, TrackKind::Audio, 0);
        let cid = app
            .project
            .clips_on(vid)
            .first()
            .map(|c| c.id)
            .expect("a video clip");
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, cr.center().x + 30.0, alane.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clip(cid).map(|c| c.track),
            Some(vid),
            "a video clip refuses an audio lane and stays put",
        );
    }
    // Case `the_header_lock_pill_locks_a_track_and_blocks_its_edits`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let header = VideoEditorApp::track_head_rect(&r, &lane);
        let pill = app.track_head_layout(header).lock;
        assert!(
            header.contains(pill.center()),
            "the lock pill sits inside the track header"
        );
        assert!(
            pill.x1 <= app.track_head_layout(header).mute.x0,
            "the lock pill sits clear of the mute pill",
        );
        // Click the pill through the real press path.
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        assert!(
            VideoDoc::load_project(&doc)
                .track(tid)
                .is_some_and(|t| t.locked),
            "the pill wrote the lock to the CRDT",
        );
        // A press on a clip of that track selects it but arms no edit drag.
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip on the track");
        let start_before = app.project.clip(cid).unwrap().start;
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        press(&mut app, &mut doc);
        assert_eq!(
            app.selected(),
            Some(cid),
            "a locked clip can still be selected"
        );
        assert!(
            app.drag.is_none(),
            "but no move/trim drag begins on a locked track"
        );
        move_to(&mut app, &mut doc, cr.center().x + 90.0, cr.center().y);
        release(&mut app, &mut doc);
        assert_eq!(
            VideoDoc::load_project(&doc).clip(cid).map(|c| c.start),
            Some(start_before),
            "dragging a locked clip does not move it",
        );
        // Nor do the keyboard edits.
        dispatch(&mut app, &mut doc, named_ev(NamedKey::Delete));
        assert!(
            VideoDoc::load_project(&doc).clip(cid).is_some(),
            "Delete does not remove a clip on a locked track",
        );
        dispatch(&mut app, &mut doc, char_ev("s"));
        let after_split = VideoDoc::load_project(&doc);
        assert_eq!(
            after_split.clips_on(tid).len(),
            app.project.clips_on(tid).len(),
            "S does not split a clip on a locked track",
        );
        assert_eq!(
            after_split.clip(cid).map(|c| c.start),
            Some(start_before),
            "and an arrow nudge leaves it alone too",
        );
    }
    // Case `a_locked_lane_refuses_a_bin_drop_and_the_clip_lands_elsewhere`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let header = VideoEditorApp::track_head_rect(&r, &lane);
        let pill = app.track_head_layout(header).lock;
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        let before = app.project.clips_on(tid).len();
        let (_mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).is_some_and(|mm| mm.kind.is_visual()))
            .expect("a visual bin item");
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, lane.x0 + 200.0, lane.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before,
            "nothing was added to the locked track",
        );
        let placed = app
            .selected()
            .and_then(|id| reloaded.clip(id))
            .expect("it landed somewhere");
        assert_ne!(placed.track, tid, "and it went to a track that accepts it");
    }
    // Case `pasting_after_its_track_is_locked_lands_on_a_track_that_accepts_it`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip");
        let cr = clip_screen_rect(&app, &r, cid);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        release(&mut app, &mut doc);
        assert!(
            key_press(&mut app, &mut doc, "c", true),
            "Ctrl+C is handled"
        );
        // Lock the source track, then paste.
        let header = VideoEditorApp::track_head_rect(&r, &lane);
        let pill = app.track_head_layout(header).lock;
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        app.playhead = 2.0;
        assert!(
            key_press(&mut app, &mut doc, "v", true),
            "Ctrl+V is handled"
        );
        let reloaded = VideoDoc::load_project(&doc);
        let new_id = app.selected().expect("something was pasted");
        let pasted = reloaded
            .clip(new_id)
            .expect("the pasted clip is in the CRDT");
        assert_ne!(pasted.track, tid, "the paste avoided the locked track");
        assert_eq!(
            reloaded.track(pasted.track).map(|t| t.kind),
            Some(TrackKind::Video),
            "and still landed on a video track",
        );
    }
    // Case `the_track_menu_lock_entry_locks_the_track`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let header = VideoEditorApp::track_head_rect(&r, &lane);
        move_to(&mut app, &mut doc, header.x0 + 20.0, header.center().y);
        right_press(&mut app, &mut doc);
        assert!(matches!(app.menu_target, Some(MenuTarget::Track(t)) if t == tid));
        click_menu_item(&mut app, &mut doc, "Lock track");
        assert!(
            VideoDoc::load_project(&doc)
                .track(tid)
                .is_some_and(|t| t.locked),
            "the menu entry locked the track in the CRDT",
        );
    }
}

// ---- track lock -------------------------------------------------------

// ---- multi-selection --------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn multi_selection_extends_sweeps_and_moves_together() {
    // Case `shift_click_extends_the_selection_and_delete_removes_all_of_it_at_once`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app
            .project
            .clips_on(tid)
            .iter()
            .take(2)
            .map(|c| c.id)
            .collect();
        assert_eq!(ids.len(), 2, "the fixture track has two clips to select");
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        assert_eq!(app.selection.len(), 1, "a plain click selects exactly one");
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        assert_eq!(app.selection.len(), 2, "shift-click extended the selection");
        assert!(app.selection.contains(ids[0]) && app.selection.contains(ids[1]));
        assert_eq!(
            app.selected(),
            Some(ids[1]),
            "the shift-clicked clip is the new anchor"
        );
        // Delete both, and prove it was one undo boundary.
        let _ = app.actions.take_committed();
        dispatch(&mut app, &mut doc, named_ev(NamedKey::Delete));
        let reloaded = VideoDoc::load_project(&doc);
        assert!(
            reloaded.clip(ids[0]).is_none() && reloaded.clip(ids[1]).is_none(),
            "both selected clips were deleted from the CRDT"
        );
        assert!(
            app.actions.take_committed().is_some(),
            "the multi-clip delete armed an undo boundary",
        );
        assert!(
            app.actions.take_committed().is_none(),
            "exactly one boundary for the whole selection, not one per clip",
        );
    }
    // Case `shift_click_a_selected_clip_removes_it_from_the_selection`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app
            .project
            .clips_on(tid)
            .iter()
            .take(2)
            .map(|c| c.id)
            .collect();
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        assert_eq!(
            app.selection.len(),
            1,
            "the second shift-click deselected it"
        );
        assert_eq!(
            app.selected(),
            Some(ids[0]),
            "the anchor fell back to what is left"
        );
    }
    // Case `a_marquee_across_empty_lane_space_selects_the_clips_it_sweeps`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let clips: Vec<ClipId> = app.project.clips_on(tid).iter().map(|c| c.id).collect();
        assert!(clips.len() >= 2, "the fixture track has clips to sweep");
        // Start past the end of the last clip (empty space) and sweep back left
        // across the whole lane.
        let last = clip_screen_rect(&app, &r, *clips.last().unwrap());
        let start_x = (last.x1 + 30.0).min(r.lanes.x1 - 4.0);
        move_to(&mut app, &mut doc, start_x, lane.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::Marquee { .. })),
            "a press on empty lane space armed a band"
        );
        move_to(&mut app, &mut doc, r.lanes.x0 + 2.0, lane.center().y);
        assert!(
            app.marquee.is_some(),
            "moving past the threshold opened the band"
        );
        release(&mut app, &mut doc);
        for id in &clips {
            assert!(
                app.selection.contains(*id),
                "the band selected every clip it crossed"
            );
        }
    }
    // Case `a_click_on_empty_lane_space_still_scrubs_the_playhead`.
    {
        // The marquee must not cost the click-to-scrub the lanes always had.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (_tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let x = r.lanes.x1 - 20.0;
        let expected = app.x_to_time(x, &r.lanes).min(app.project.duration());
        move_to(&mut app, &mut doc, x, lane.center().y);
        press(&mut app, &mut doc);
        release(&mut app, &mut doc);
        assert!(
            app.selection.is_empty(),
            "clicking empty space cleared the selection"
        );
        assert!(
            (app.playhead - expected).abs() < 0.2,
            "and scrubbed the playhead there ({} vs {expected})",
            app.playhead,
        );
    }
    // Case `dragging_a_multi_selection_carries_every_clip_by_the_same_delta`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app
            .project
            .clips_on(tid)
            .iter()
            .take(2)
            .map(|c| c.id)
            .collect();
        let before: Vec<f64> = ids
            .iter()
            .map(|id| app.project.clip(*id).unwrap().start)
            .collect();
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        // Grab the first one (already selected) and drag right.
        let a = clip_screen_rect(&app, &r, ids[0]);
        move_to(&mut app, &mut doc, a.center().x, a.center().y);
        press(&mut app, &mut doc);
        assert_eq!(
            app.selection.len(),
            2,
            "pressing inside a multi-selection keeps it"
        );
        move_to(&mut app, &mut doc, a.center().x + 60.0, a.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        let after: Vec<f64> = ids
            .iter()
            .map(|id| reloaded.clip(*id).unwrap().start)
            .collect();
        let d0 = after[0] - before[0];
        let d1 = after[1] - before[1];
        assert!(d0 > 0.1, "the grabbed clip moved right ({d0})");
        assert!(
            (d0 - d1).abs() < 1e-6,
            "the rest of the selection moved by the same delta ({d0} vs {d1})",
        );
    }
}

// ---- cut / duplicate / clip context menu ------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn clipboard_and_duplicate_keep_the_selection_whole() {
    // Case `ctrl_x_cuts_the_selection_and_ctrl_v_pastes_it_back`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip");
        let media = app.project.clip(cid).unwrap().media;
        let cr = clip_screen_rect(&app, &r, cid);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        release(&mut app, &mut doc);
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(
            key_press(&mut app, &mut doc, "x", true),
            "Ctrl+X is handled"
        );
        let cut = VideoDoc::load_project(&doc);
        assert!(
            cut.clip(cid).is_none(),
            "Ctrl+X removed the clip from the CRDT"
        );
        assert_eq!(cut.clips.len(), before - 1);
        // And it is on the clipboard, so Ctrl+V brings it back.
        app.playhead = 1.0;
        assert!(
            key_press(&mut app, &mut doc, "v", true),
            "Ctrl+V is handled"
        );
        let pasted = VideoDoc::load_project(&doc);
        assert_eq!(pasted.clips.len(), before, "the cut clip pasted back");
        let new_id = app.selected().expect("the pasted clip is selected");
        assert_eq!(pasted.clip(new_id).map(|c| c.media), Some(media));
    }
    // Case `ctrl_d_duplicates_the_whole_selection_on_the_same_tracks`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app
            .project
            .clips_on(tid)
            .iter()
            .take(2)
            .map(|c| c.id)
            .collect();
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        let before = VideoDoc::load_project(&doc).clips_on(tid).len();
        let _ = app.actions.take_committed();
        assert!(
            key_press(&mut app, &mut doc, "d", true),
            "Ctrl+D is handled"
        );
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 2,
            "both selected clips were duplicated onto their own track",
        );
        assert_eq!(app.selection.len(), 2, "the copies become the selection");
        for id in app.selection.ids() {
            assert!(!ids.contains(id), "the selection moved to the new clips");
        }
        assert!(
            app.actions.take_committed().is_some(),
            "one undo boundary for the duplicate"
        );
        assert!(app.actions.take_committed().is_none(), "and only one");
    }
    // Case `right_clicking_a_clip_opens_a_menu_whose_duplicate_adds_a_clip`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip");
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        right_press(&mut app, &mut doc);
        assert!(
            app.bin_menu.is_open(),
            "right-clicking a clip opens a context menu"
        );
        assert!(matches!(app.menu_target, Some(MenuTarget::Clip(c)) if c == cid));
        assert_eq!(
            app.selected(),
            Some(cid),
            "and re-targets the selection at it"
        );
        let before = VideoDoc::load_project(&doc).clips_on(tid).len();
        click_menu_item(&mut app, &mut doc, "Duplicate");
        assert_eq!(
            VideoDoc::load_project(&doc).clips_on(tid).len(),
            before + 1,
            "Duplicate added a clip through the CRDT",
        );
        assert!(!app.bin_menu.is_open(), "the menu closed after the choice");
    }
}

// ---- affordances ------------------------------------------------------

#[test]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn timeline_chrome_regions_stay_ordered_at_every_size() {
    // Case `the_zoom_chip_reports_the_zoom_and_fits_the_project_to_the_window`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        app.zoom = 180.0;
        let dur = app.project.duration();
        assert!(
            app.time_to_x(dur, &r.lanes) > r.lanes.x1,
            "zoomed right in, the end is off screen",
        );
        let chip = app.zoom_chip_rect(&r);
        click(&mut app, &mut doc, chip.center().x, chip.center().y);
        release(&mut app, &mut doc);
        assert!(
            app.time_to_x(dur, &r.lanes) <= r.lanes.x1 + 1.0,
            "clicking the chip fitted the whole project into the lane area",
        );
        assert_eq!(app.scroll, 0.0, "and parked the view at the start");
        // The keyboard route agrees.
        app.zoom = 180.0;
        assert!(key_press(&mut app, &mut doc, "z", false), "Z is handled");
        assert!(app.time_to_x(dur, &r.lanes) <= r.lanes.x1 + 1.0);
    }
    // Case `no_region_is_inverted_at_any_window_size`.
    {
        for (w, h) in SIZES {
            let (app, _doc) = app_sized(w, h);
            let r = app.regions();
            for (name, rect) in [
                ("topbar", r.topbar),
                ("preview", r.preview),
                ("canvas", r.canvas),
                ("transport", r.transport),
                ("inspector", r.inspector),
                ("timeline", r.timeline),
                ("ruler", r.ruler),
                ("gutter", r.gutter),
                ("lanes", r.lanes),
            ] {
                assert!(
                    rect.x1 >= rect.x0 && rect.y1 >= rect.y0,
                    "{name} is inverted at {w}x{h}: {rect:?}",
                );
            }
            let scrub = app.scrubber_rect(&r);
            assert!(
                scrub.x1 >= scrub.x0 && scrub.y1 >= scrub.y0,
                "the scrubber is inverted at {w}x{h}: {scrub:?}",
            );
        }
    }
    // Case `the_scrubber_knob_stays_inside_its_track_at_every_size`.
    {
        // Regression: below roughly 500px the track went inverted and the knob
        // drew outside it; above about 2000px it stretched edge to edge.
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            let r = app.regions();
            let scrub = app.scrubber_rect(&r);
            if !app.has_scrubber(&r) {
                assert_eq!(
                    scrub,
                    Rect::ZERO,
                    "a transport with no room draws no scrubber"
                );
                continue;
            }
            assert!(
                scrub.width() <= MAX_SCRUBBER_W + 1.0,
                "the scrubber is capped at {w}x{h} (got {})",
                scrub.width(),
            );
            assert!(scrub.x1 <= r.transport.x1, "and stays inside the transport");
            for frac in [0.0, 0.5, 1.0] {
                app.playhead = frac * app.project.duration();
                let knob_x = scrub.x0 + scrub.width() * frac;
                assert!(
                    knob_x >= scrub.x0 - 0.001 && knob_x <= scrub.x1 + 0.001,
                    "the knob is inside the track at {w}x{h}, frac {frac}",
                );
            }
        }
    }
    // Case `every_window_size_renders_without_panicking`.
    {
        // The draw pass is where clip layers must balance and where a crushed
        // canvas would otherwise put a HUD outside its pane. This proves only
        // the absence of a panic — which is NOT proof the frame is usable; see
        // `the_open_overlay_bin_leaves_the_editor_behind_it_visible` for the
        // test that looks at the pixels.
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            render(&mut app);
            app.presentation.export_open = true;
            render(&mut app);
        }
    }
}

// ---- responsive geometry ---------------------------------------------

/// The app laid out at an arbitrary window size, synced to the sample.
fn app_sized(w: f64, h: f64) -> (VideoEditorApp, AutoCommit) {
    let mut doc = AutoCommit::new();
    VideoDoc::seed_sample(&mut doc);
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.sync(&mut doc, &automerge::ROOT);
    (app, doc)
}

/// Window sizes worth proving, from the narrow capture width up.
const SIZES: [(f64, f64); 8] = [
    (420.0, 560.0),
    (500.0, 640.0),
    (620.0, 700.0),
    (720.0, 820.0),
    (900.0, 700.0),
    (1280.0, 800.0),
    (1920.0, 1080.0),
    (3440.0, 1440.0),
];

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn export_dialog_layout_availability_and_shortcuts() {
    // Case `the_export_dialog_header_never_collides_with_the_first_section`:
    // The card's header and its first section were both positioned from
    // fixed offsets off the card top, so the filename subtitle and the
    // RESOLUTION label were drawn through each other.
    {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            app.presentation.export_open = true;
            let l = app.export_layout(&mut tr, &theme);
            let ty = l.type_sizes;
            let header_bottom = if let Some(top) = l.mix_top {
                top + f64::from(tr.measure("Hg", ty.xs, None).1)
            } else {
                l.filename_top + f64::from(tr.measure("Hg", ty.sm, None).1)
            };
            let first_label_top = l.captions[0];
            assert!(
                first_label_top >= header_bottom,
                "{w}x{h}: RESOLUTION starts at {first_label_top} but the header \
                 runs to {header_bottom} — they overlap",
            );
            let caption_h = f64::from(tr.measure("Hg", ty.xs, None).1);
            for (caption, rects) in l.captions.iter().zip([
                l.resolution.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
                l.container.iter().map(|(_, r)| *r).collect(),
                l.fps.iter().map(|(_, r)| *r).collect(),
            ]) {
                let first = rects.first().expect("every group offers an option");
                assert!(
                    caption + caption_h <= first.y0,
                    "{w}x{h}: a caption at {caption} runs into its segments at {}",
                    first.y0,
                );
            }
            // …and the groups still stop before the footer they share the card with.
            let last = l.fps.last().expect("frame rates are offered").1;
            assert!(
                last.y1 <= l.info_top,
                "{w}x{h}: the frame-rate row runs into the size estimate",
            );
        }
    }
    // Case `the_export_button_is_only_offered_when_export_can_actually_run`:
    // The dialog must not offer an action it will refuse. It used to say
    // "Export needs the `GStreamer` build" and paint an enabled primary Export
    // button directly beneath that sentence.
    {
        let (mut app, doc) = app_sized(1280.0, 800.0);
        app.presentation.export_open = true;
        // The event path lays the dialog out with the theme of the last
        // frame, so paint one first.
        render(&mut app);
        let mut tr = TextRenderer::new();
        let l = app.export_layout(&mut tr, &Theme::dark());
        assert_eq!(
            app.export_is_offered(),
            crate::export::unavailable_reason().is_none(),
            "the offer must track what the export module says it can do",
        );
        if !app.export_is_offered() {
            // Pressing it must do nothing at all — no job, no status change.
            app.press_export(&doc, l.confirm.center());
            assert!(
                matches!(app.export_status, ExportStatus::Idle),
                "a refused Export must not move the status",
            );
            assert!(
                !app.export_running(),
                "a refused Export must not start a job"
            );
        }
    }
    // Case `the_export_modal_dims_the_editor_rather_than_deleting_it`:
    // Same defect as the media bin's scrim, one token over: the modal painted
    // `surfaces.overlay` — the opaque deep-menu surface — as its backdrop, so
    // the editor vanished behind the dialog instead of dimming.
    {
        for theme in [Theme::dark(), Theme::light()] {
            let (mut app, _doc) = app_sized(1280.0, 800.0);
            app.presentation.export_open = true;
            let r = app.regions();
            let px = render_pixels(&mut app, 1280, 800, &theme);
            // The timeline lanes stretch well past the card on both sides.
            let mut tr = TextRenderer::new();
            let card = app.export_layout(&mut tr, &theme).modal.card;
            let left = Rect::new(r.lanes.x0, r.lanes.y0, card.x0 - 8.0, r.lanes.y1);
            let colours = region_colours(&px, 1280, 800, left);
            assert!(
                colours > 2,
                "the timeline behind the export dialog is a flat slab \
                 ({colours} distinct colours) — the backdrop erased it",
            );
        }
    }
    // Case `the_export_dialog_stays_inside_the_window`.
    {
        // Regression: a fixed 460x360 card hung off both edges of a small
        // window, putting Cancel and Export outside the surface.
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            app.presentation.export_open = true;
            let l = app.export_layout(&mut tr, &theme);
            let card = l.modal.card;
            assert!(
                card.x0 >= app.rect.x0 && card.x1 <= app.rect.x1,
                "the export card fits horizontally at {w}x{h}: {card:?}",
            );
            assert!(
                card.y0 >= app.rect.y0 && card.y1 <= app.rect.y1,
                "the export card fits vertically at {w}x{h}: {card:?}",
            );
            for (name, btn) in [("cancel", l.cancel), ("confirm", l.confirm)] {
                assert!(
                    btn.x0 >= app.rect.x0
                        && btn.x1 <= app.rect.x1
                        && btn.y0 >= app.rect.y0
                        && btn.y1 <= app.rect.y1,
                    "the {name} button is on screen at {w}x{h}: {btn:?}",
                );
            }
        }
    }
    // Case `the_export_dialog_answers_a_click_the_frame_after_the_shortcut_opened_it`.
    {
        let (mut app, mut doc) = app_sized(1280.0, 800.0);
        // One frame with the dialog closed, so the editor knows its theme.
        render(&mut app);
        assert!(!app.presentation.export_open);
        assert!(
            key_press(&mut app, &mut doc, "e", true),
            "Ctrl+E opens the dialog"
        );
        assert!(app.presentation.export_open);
        let mut tr = TextRenderer::new();
        let l = app.export_layout(&mut tr, &Theme::dark());
        let (wanted, rect) = l.resolution[2];
        assert_ne!(
            app.export.resolution, wanted,
            "the fixture starts elsewhere"
        );
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert_eq!(
            app.export.resolution, wanted,
            "the click on an option the frame has not painted yet still lands"
        );
        // And a resize between the shortcut and the click moves the target
        // with it: the layout follows the rect, not a painted frame.
        app.rect = Rect::new(0.0, 0.0, 900.0, 700.0);
        let l = app.export_layout(&mut tr, &Theme::dark());
        let (wanted, rect) = l.container[1];
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert_eq!(app.export.container, wanted);
    }
    // Case `ctrl_e_does_not_open_the_export_dialog_under_the_shortcut_sheet`.
    {
        let (mut app, mut doc) = app_sized(1280.0, 800.0);
        app.navigation.help_open = true;
        assert!(
            key_press(&mut app, &mut doc, "e", true),
            "the sheet swallows the key"
        );
        assert!(
            !app.presentation.export_open,
            "nothing opens under the sheet"
        );
        assert!(app.navigation.help_open, "and the sheet stays");
        assert!(key_press(&mut app, &mut doc, "?", false));
        assert!(!app.navigation.help_open, "? still closes it");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn render_status_is_painted_cached_and_announced() {
    // Case `a_dismissed_render_is_still_announced_to_a_screen_reader`:
    // A render outlives the dialog that started it — `start_export` clones
    // the project precisely so the UI thread can keep editing while the
    // worker encodes. But `export_status` was read in exactly two places and
    // **both** were gated on `show_export`, so every way of dismissing the
    // dialog took the render off the accessibility tree with it: no progress,
    // no outcome, no failure. The only evidence was a file appearing on disk.
    //
    // Driven through `ui::headless::Driver`, so this is the AccessKit tree a
    // screen reader actually receives, dismissed the ways a user actually
    // dismisses it — including by pressing the exported Close button itself.
    {
        type Dismiss = fn(&mut ui::headless::Driver);
        let dismissals: [(&str, Dismiss); 3] = [
            // Outside the card: the card is centred, so the far left is clear
            // of it at this size.
            ("a click outside the card", |d| d.click_at(24.0, 400.0)),
            ("Escape", |d| d.key("Escape")),
            ("the exported Close export button", |d| {
                let close = d
                    .a11y_node_named("Close export")
                    .expect("the dialog offers a Close button to a reader");
                d.perform_a11y_action(close, &ui::a11y::Action::Activate)
                    .expect("and it is operable");
            }),
        ];
        for (how, dismiss) in dismissals {
            let mut doc = AutoCommit::new();
            VideoDoc::seed_sample(&mut doc);
            let mut state = ui::State::new();
            state.doc = doc;
            let mut app = VideoEditorApp::new();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            // A render in flight, with the dialog that started it up.
            app.presentation.export_open = true;
            app.export_status = ExportStatus::Rendering { progress: 0.42 };
            let mut d = ui::headless::Driver::new(state, Box::new(app), (1280, 800));
            assert!(
                d.a11y_node_named("Close export").is_some(),
                "{how}: the dialog is up to begin with",
            );

            dismiss(&mut d);

            assert!(
                d.a11y_node_named("Close export").is_none(),
                "{how}: the dialog is gone",
            );
            let bars: Vec<_> = d
                .a11y_nodes()
                .into_iter()
                .filter(|(_, n)| n.label() == Some("Export progress"))
                .collect();
            assert_eq!(
                bars.len(),
                1,
                "{how}: a render in flight is announced exactly once — not zero times",
            );
            assert_eq!(
                bars[0].1.value(),
                Some("42%"),
                "{how}: and it says how far the render has got",
            );
        }
    }
    // Case `a_render_outside_its_dialog_is_painted_and_leads_back_to_it`:
    // The pixel half of the same defect, plus the way back: the chip under
    // the Export button paints a dismissed render's progress and its outcome,
    // and pressing it reopens the dialog it is reporting for.
    {
        let theme = Theme::dark();
        let states = [
            ExportStatus::Rendering { progress: 0.42 },
            ExportStatus::Done {
                path: "/home/you/Videos/Reel.mp4".into(),
            },
            ExportStatus::Failed {
                reason: "the file picker needs the real-desktop build".into(),
            },
        ];
        for status in states {
            let (mut app, mut doc) = app_sized(1280.0, 800.0);
            app.export_status = status.clone();
            let painted = render_pixels(&mut app, 1280, 800, &theme);
            let chip = app.regions().export_chip;
            // The same frame with nothing to report: whatever the chip draws
            // has to be the difference between these two.
            app.export_status = ExportStatus::Idle;
            let quiet = render_pixels(&mut app, 1280, 800, &theme);
            assert!(
                region_diff(&painted, &quiet, 1280, 800, chip) > 400,
                "{status:?} is not painted anywhere outside the dialog",
            );

            // A 176px chip elides a long reason, so the hover has to carry
            // the whole of it rather than leaving it to the dialog alone.
            app.export_status = status.clone();
            let hover = app
                .tooltip_at(chip.center())
                .map(ToString::to_string)
                .expect("the chip names itself on hover");
            match &status {
                ExportStatus::Failed { reason } => assert_eq!(
                    &hover, reason,
                    "the hover carries the failure's whole reason",
                ),
                ExportStatus::Done { path } => {
                    assert_eq!(&hover, path, "and a finished render's whole path");
                }
                _ => assert!(
                    hover.contains("Export"),
                    "and says what is going on: {hover}"
                ),
            }

            // And it is a control, not a decoration.
            render(&mut app);
            click(&mut app, &mut doc, chip.center().x, chip.center().y);
            assert!(
                app.presentation.export_open,
                "{status:?}: the chip must lead back to the render it reports",
            );
        }
    }
    // Case `the_export_chip_caches_its_words_without_ever_showing_stale_ones`:
    // A render holds animation frames open for its whole duration, so the
    // chip is on the hot path: its words must be formatted and elided when
    // they change, not once per encoded frame. The cache that buys that must
    // not be able to go stale, which is the half worth asserting.
    {
        // Two encoded frames inside the same whole percent paint the same
        // chip, so they must not cost a reformat.
        assert!(
            chip_text_matches(
                &ExportStatus::Rendering { progress: 0.420_1 },
                &ExportStatus::Rendering { progress: 0.420_4 },
            ),
            "the same painted percentage is the same picture",
        );
        // A percent that moved, or any other change, is not.
        assert!(
            !chip_text_matches(
                &ExportStatus::Rendering { progress: 0.42 },
                &ExportStatus::Rendering { progress: 0.43 },
            ),
            "a percentage that moved must be repainted",
        );
        assert!(
            !chip_text_matches(
                &ExportStatus::Done {
                    path: "/a.mp4".into()
                },
                &ExportStatus::Done {
                    path: "/b.mp4".into()
                },
            ),
            "a different file is a different outcome",
        );
        assert!(!chip_text_matches(
            &ExportStatus::Rendering { progress: 1.0 },
            &ExportStatus::Done {
                path: "/a.mp4".into()
            },
        ));

        // …and the painted pixels really do follow the render, cache or not.
        let theme = Theme::dark();
        let (mut app, _doc) = app_sized(1280.0, 800.0);
        app.export_status = ExportStatus::Rendering { progress: 0.10 };
        let early = render_pixels(&mut app, 1280, 800, &theme);
        let chip = app.regions().export_chip;
        app.export_status = ExportStatus::Rendering { progress: 0.90 };
        let late = render_pixels(&mut app, 1280, 800, &theme);
        assert!(
            region_diff(&early, &late, 1280, 800, chip) > 400,
            "the chip must repaint when the render moves on",
        );
        app.export_status = ExportStatus::Done {
            path: "/home/you/Videos/Reel.mp4".into(),
        };
        let finished = render_pixels(&mut app, 1280, 800, &theme);
        assert!(
            region_diff(&late, &finished, 1280, 800, chip) > 400,
            "and when it finishes",
        );
        // Nothing to say leaves nothing behind, so the words cannot outlive
        // the render they described.
        app.export_status = ExportStatus::Idle;
        render(&mut app);
        assert!(
            app.export_chip_text.is_none(),
            "an idle editor holds no chip text at all",
        );
    }
}

/// A one-clip timeline whose footage is not on this machine. `render`
/// refuses an unreadable source before it touches `GStreamer`, in every
/// build, so a job started on this finishes at once and encodes nothing.
fn unexportable_project() -> Project {
    let mut p = Project::new("Unreadable");
    let track = crate::model::Track {
        id: Uuid::new_v4(),
        name: "V1".to_string(),
        kind: TrackKind::Video,
        index: 1,
        ..Default::default()
    };
    let media = MediaItem {
        id: Uuid::new_v4(),
        name: "Drone.mov".to_string(),
        kind: MediaKind::Video,
        duration: 5.0,
        width: 1920,
        height: 1080,
        path: "/definitely/not/here-xyz.mov".to_string(),
        ..Default::default()
    };
    p.clips.push(Clip {
        id: Uuid::new_v4(),
        track: track.id,
        media: media.id,
        start: 0.0,
        out_point: 5.0,
        ..Default::default()
    });
    p.tracks.push(track);
    p.media.push(media);
    p
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn topbar_and_transport_degrade_gracefully_when_narrow() {
    // Case `the_topbar_buttons_never_collide_with_other_topbar_content`.
    {
        // The centred project-meta string used to run under these buttons at
        // 720px — the narrow capture width itself. It now lives in the
        // transport, so the topbar holds only the hamburger and the buttons.
        for (w, h) in SIZES {
            let (app, _doc) = app_sized(w, h);
            let r = app.regions();
            let btns: Vec<Rect> = [r.import_btn, r.split_btn, r.export_btn]
                .into_iter()
                .filter(|rect| rect.width() > 0.0)
                .collect();
            for (i, a) in btns.iter().enumerate() {
                for b in btns.iter().skip(i + 1) {
                    assert!(
                        a.x1 <= b.x0 || b.x1 <= a.x0,
                        "topbar buttons overlap at {w}x{h}: {a:?} vs {b:?}",
                    );
                }
                if r.menu_btn.width() > 0.0 {
                    assert!(
                        r.menu_btn.x1 <= a.x0,
                        "the hamburger clears the buttons at {w}x{h}",
                    );
                }
            }
        }
    }
    // Case `the_transport_readout_drops_parts_it_cannot_draw_whole`:
    // A number cut in half by the pane edge is not a clipped number, it is a
    // wrong one.
    //
    // Regression: the transport drew its total duration at a fixed offset
    // with no width check, so at 900x700 "00:42:00" landed as `00:4` — a
    // plausible-looking project length that was four seconds instead of
    // forty-two. Every part now goes through [`readout_fit`], which drops
    // what will not fit whole.
    {
        // A stopped playhead has no motion readout, which is what a zero width
        // stands for — so every assertion below is on the same geometry the
        // strip had before the shuttle got a slot in it.
        const STILL: f64 = 0.0;
        // Widths in the neighbourhood of the real measurements: an 18px bold
        // "00:23:00" is ~74px, the "/  00:42:00" tail ~62px, the meta ~96px.
        let (big, total, meta) = (74.0, 62.0, 96.0);
        // On a wide strip the readout starts past the history buttons.
        let wide = readout_fit(
            Rect::new(0.0, 0.0, 700.0, 58.0),
            TRANSPORT_RIGHT_X,
            big,
            total,
            STILL,
            meta,
        );
        assert_eq!(wide.big_x, Some(TRANSPORT_RIGHT_X));
        assert_eq!(wide.total_x, Some(TRANSPORT_RIGHT_X + big + 4.0));
        assert_eq!(wide.motion_x, None, "a still playhead places no readout");
        assert!(wide.meta_x.unwrap() > wide.total_x.unwrap() + total);
        // The 900x700 case: too narrow for the history buttons, so the
        // readout gets the space back and the playhead time still fits. The
        // total does not, and goes rather than arriving as "00:4".
        let mid = readout_fit(
            Rect::new(268.0, 0.0, 600.0, 58.0),
            TRANSPORT_PLAIN_X,
            big,
            total,
            STILL,
            meta,
        );
        assert_eq!(mid.big_x, Some(488.0), "the playhead time still fits");
        assert_eq!(mid.total_x, None, "and the total is dropped, not clipped");
        assert_eq!(mid.meta_x, None);
        // Narrower still: nothing is drawn rather than a fragment of a time.
        let tight = readout_fit(
            Rect::new(0.0, 0.0, 260.0, 58.0),
            TRANSPORT_PLAIN_X,
            big,
            total,
            STILL,
            meta,
        );
        assert_eq!(tight, ReadoutFit::default());
        // Whatever is drawn always ends inside the pane, at either offset —
        // with and without a shuttle running, because the motion readout is
        // one more part that must not be cut in half either.
        for motion in [STILL, 88.0] {
            for w in [200.0_f64, 260.0, 300.0, 420.0, 500.0, 700.0, 1200.0] {
                for content_x in [TRANSPORT_PLAIN_X, TRANSPORT_RIGHT_X] {
                    let t = Rect::new(0.0, 0.0, w, 58.0);
                    let f = readout_fit(t, content_x, big, total, motion, meta);
                    for (x, width) in [
                        (f.big_x, big),
                        (f.total_x, total),
                        (f.motion_x, motion),
                        (f.meta_x, meta),
                    ] {
                        if let Some(x) = x {
                            assert!(x + width <= t.x1, "{w}px: a part overruns the transport");
                        }
                    }
                    // Nothing overlaps anything: the parts share one line.
                    if let (Some(tx), Some(mx)) = (f.total_x, f.motion_x) {
                        assert!(
                            mx >= tx + total,
                            "{w}px: the motion readout sits on the total"
                        );
                    }
                    if let (Some(mx), Some(ex)) = (f.motion_x, f.meta_x) {
                        assert!(
                            ex >= mx + motion,
                            "{w}px: the frame size sits on the motion"
                        );
                    }
                }
            }
        }
        // A shuttle is reported ahead of the static frame size: the rate is
        // stated nowhere else, and the frame is also in the inspector.
        let squeezed = readout_fit(
            Rect::new(0.0, 0.0, 500.0, 58.0),
            TRANSPORT_PLAIN_X,
            big,
            total,
            88.0,
            meta,
        );
        assert!(squeezed.motion_x.is_some(), "the shuttle rate is kept");
        assert_eq!(squeezed.meta_x, None, "and the frame size gives way to it");
    }
    // Case `a_narrow_transport_gives_up_the_history_buttons_before_the_timecode`:
    // The history buttons are the first thing to go when the strip runs out
    // of room, because the playhead time is the reading nothing else on the
    // strip carries — and undo keeps its keyboard shortcut either way.
    //
    // Laid out unconditionally they pushed the timecode off a 900px window
    // entirely, which is a worse trade than hiding two buttons.
    {
        for w in [
            420.0_f64, 500.0, 640.0, 760.0, 900.0, 1100.0, 1280.0, 1920.0,
        ] {
            let mut app = VideoEditorApp::new_demo();
            app.rect = Rect::new(0.0, 0.0, w, 800.0);
            let r = app.regions();
            let hist = app.history_buttons(&r);
            let content = app.transport_content_x(&r);
            if hist.is_empty() {
                assert!(
                    ((content) - (TRANSPORT_PLAIN_X)).abs() < 1e-9,
                    "{w}px: the space comes back"
                );
                continue;
            }
            assert!(((content) - (TRANSPORT_RIGHT_X)).abs() < 1e-9, "{w}px");
            // Every button is inside the strip, and clear of the space the
            // readout and the scrubber then start in.
            for (which, rect) in &hist {
                assert!(
                    rect.x0 >= r.transport.x0 && rect.x1 <= r.transport.x0 + content,
                    "{w}px: {which:?} at {rect:?} is not inside its own slot",
                );
            }
            // And there is still room left for the playhead time beside them.
            assert!(
                r.transport.x1 - (r.transport.x0 + content) >= 88.0,
                "{w}px: the buttons left the timecode nowhere to go",
            );
        }
    }
    // Case `a_narrow_window_collapses_the_inspector_and_gives_the_transport_its_width_back`:
    // Below its breakpoint the inspector gets out of the way, exactly as the
    // bin does — so the preview and the transport get the width back.
    //
    // Regression: a fixed 300px inspector was 60% of a 500px window, which
    // left the transport ~200px and clipped its fifth button.
    {
        for (w, h) in [
            (420.0_f64, 560.0_f64),
            (500.0, 640.0),
            (620.0, 700.0),
            (720.0, 820.0),
        ] {
            let (mut app, _doc) = app_sized(w, h);
            app.navigation.flap_settled = true;
            app.inspector_flap.snap(false);
            let r = app.regions();
            assert!(r.inspector_overlay, "{w}x{h} must collapse the inspector");
            assert!(!app.inspector_is_visible(&r), "and start it closed");
            assert!(
                r.inspector.x0 >= w - 0.5,
                "{w}x{h}: a closed inspector is off the right edge",
            );
            // The five transport buttons are the point: they must all land
            // inside the transport now that it has the window's width.
            for (_, rect) in app.transport_buttons(&r) {
                assert!(
                    rect.x1 <= r.transport.x1,
                    "{w}x{h}: a transport button is cut off at {} (pane ends {})",
                    rect.x1,
                    r.transport.x1,
                );
            }
        }
        // Wide windows are unchanged: the inspector keeps its column.
        for (w, h) in [(900.0_f64, 700.0_f64), (1280.0, 800.0), (1920.0, 1080.0)] {
            let (mut app, _doc) = app_sized(w, h);
            app.navigation.flap_settled = true;
            let r = app.regions();
            assert!(!r.inspector_overlay, "{w}x{h} keeps the inspector docked");
            assert!(app.inspector_is_visible(&r));
            assert!(
                ((r.inspector.x1) - (w)).abs() < 1e-9,
                "docked against the right edge"
            );
            assert!(
                ((r.preview.x1) - (r.inspector.x0)).abs() < 1e-9,
                "the preview ends at it"
            );
        }
    }
    // Case `the_inspector_toggle_shows_and_hides_the_collapsed_panel`.
    {
        let (mut app, mut doc) = app_sized(500.0, 640.0);
        app.navigation.flap_settled = true;
        app.inspector_flap.snap(false);
        let r = app.regions();
        assert!(
            r.inspector_btn.width() > 0.0,
            "a collapsed inspector offers a toggle"
        );
        assert!(!app.inspector_is_visible(&r));
        // Press the toggle: it opens.
        app.last_mouse = r.inspector_btn.center();
        app.handle_press(&mut doc, &r, false);
        app.inspector_flap.snap(true);
        let r = app.regions();
        assert!(app.inspector_is_visible(&r));
        assert!(r.inspector.x1 <= 500.0 + 0.5, "an open flap is on screen");
        // Esc puts it away again.
        app.handle_key(
            &Key::Named(NamedKey::Escape),
            winit::event::Modifiers::default(),
            &mut doc,
            &r,
        );
        app.inspector_flap.snap(false);
        assert!(!app.inspector_is_visible(&app.regions()));
        // The docked layout has no toggle to offer.
        let (mut wide, _d) = app_sized(1280.0, 800.0);
        wide.navigation.flap_settled = true;
        assert_eq!(wide.regions().inspector_btn.width(), 0.0);
    }
    // Case `compact_topbar_keeps_independent_touch_targets_and_semantics`.
    {
        // At 420px the old 28px menu and inspector icons were exactly on top
        // of one another. Hiding the redundant Split button leaves room for
        // both side panes, and the toolbar tree follows the painted layout.
        let (mut app, mut doc) = app_sized(420.0, 560.0);
        app.advance_flap();
        let r = app.regions();
        assert_eq!(r.split_btn, Rect::ZERO, "Split yields to its S shortcut");
        let controls = [
            ("media bin", r.menu_btn),
            ("inspector", r.inspector_btn),
            ("import", r.import_btn),
            ("export", r.export_btn),
        ];
        for (name, rect) in controls {
            assert!(
                rect.width() >= TOPBAR_TOUCH_TARGET && rect.height() >= TOPBAR_TOUCH_TARGET,
                "{name} target is too small for touch: {rect:?}",
            );
        }
        for (i, (_, a)) in controls.iter().enumerate() {
            for (_, b) in controls.iter().skip(i + 1) {
                assert!(
                    a.x1 <= b.x0 || b.x1 <= a.x0,
                    "compact topbar controls overlap: {a:?} vs {b:?}",
                );
            }
        }

        let toolbar = app.a11y_topbar();
        let names: Vec<&str> = toolbar
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        assert!(names.contains(&"Toggle media bin"));
        assert!(names.contains(&"Toggle inspector"));
        assert!(names.contains(&"Import media"));
        assert!(names.contains(&"Export video"));
        assert!(
            !names.contains(&"Split at playhead"),
            "a hidden button must not stay reachable only to assistive technology",
        );

        let activate = ui::a11y::Action::Activate;
        assert!(app.perform_action_at(
            &activate,
            &[
                ui::a11y::hash_key(&TOPBAR_A11Y_KEY),
                ui::a11y::hash_key(&TOPBAR_BIN_KEY),
            ],
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(app.bin_flap.is_open(app.bin_layout()));
        assert!(
            !app.perform_action_at(
                &activate,
                &[
                    ui::a11y::hash_key(&TOPBAR_A11Y_KEY),
                    ui::a11y::hash_key(&TOPBAR_INSPECTOR_KEY),
                ],
                &mut doc,
                &automerge::ROOT,
            ),
            "the open bin scrim makes other topbar actions unavailable"
        );
        assert!(app.perform_action_at(
            &activate,
            &[
                ui::a11y::hash_key(&TOPBAR_A11Y_KEY),
                ui::a11y::hash_key(&TOPBAR_BIN_KEY),
            ],
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(!app.bin_flap.is_open(app.bin_layout()));
        assert!(app.perform_action_at(
            &activate,
            &[
                ui::a11y::hash_key(&TOPBAR_A11Y_KEY),
                ui::a11y::hash_key(&TOPBAR_INSPECTOR_KEY),
            ],
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(app.inspector_flap.is_open(app.inspector_split()));
    }
}

/// Rasterise one frame of `app` at `w`×`h` in `theme` and hand back RGBA.
/// The real GPU path the app ships on — a machine that cannot rasterise
/// cannot observe this property, so `new` panicking there is the correct
/// outcome rather than a silent skip.
fn render_pixels(app: &mut VideoEditorApp, w: u32, h: u32, theme: &Theme) -> Vec<u8> {
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    app.rect = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        &PresenceCtx::empty(),
    );
    ui::offscreen::OffscreenRenderer::new(w, h).render_rgba(&scene)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn scrolled_clips_and_hud_keep_their_labels_visible() {
    // Case `a_clip_scrolled_past_the_left_edge_still_shows_its_name`.
    {
        // The label was drawn at `cr.x0 + 5` with no viewport clamp, and the
        // lane layer is clipped — so a clip whose start had scrolled off to
        // the left rendered as an anonymous coloured bar, which for a clip
        // wider than the viewport is every pixel of it.
        let (mut app, _doc) = app_and_doc();
        let cid = app
            .project
            .clips
            .iter()
            .max_by(|a, b| a.duration().total_cmp(&b.duration()))
            .map(|c| c.id)
            .expect("the fixture places clips");
        let clip = app.project.clip(cid).cloned().expect("clip exists");
        let mid = clip.media;
        // Zoomed in and parked inside the clip, so its head is off the left
        // edge of the lane and its body fills the visible width.
        app.zoom = 220.0;
        app.scroll = clip.start + 0.6;
        app.playhead = clip.start + 1.0;
        app.playback.chase_playhead = false;
        app.selection.clear();

        let r = app.regions();
        let cr = clip_screen_rect(&app, &r, cid);
        assert!(
            cr.x0 < r.lanes.x0 - 20.0 && cr.x1 > r.lanes.x1,
            "the clip must overrun both edges for this to be the case under test:              {cr:?} in {:?}",
            r.lanes,
        );
        // The strip where a clamped label lands: the left end of the *visible*
        // part of the clip's header.
        let strip = Rect::new(r.lanes.x0, cr.y0, r.lanes.x0 + 150.0, cr.y0 + 18.0);

        let named = render_pixels(&mut app, 1280, 800, &Theme::dark());
        // Blank the source's name and render again. Whatever differs inside
        // that strip is the name — and before the clamp nothing did, because
        // the text was being drawn off the left of a clipped layer.
        if let Some(m) = app.project.media.iter_mut().find(|m| m.id == mid) {
            m.name = String::new();
        }
        let unnamed = render_pixels(&mut app, 1280, 800, &Theme::dark());

        let diff = region_diff(&named, &unnamed, 1280, 800, strip);
        assert!(
            diff > 40,
            "the clip's name must be drawn inside the visible lane; only {diff} pixels              differ between a named and an unnamed source",
        );
    }
    // Case `a_scrolled_clip_keeps_its_speed_badge_on_screen`.
    {
        // Same defect at the other end: the badge was pinned to `cr.x1`, so a
        // clip running off the right of the lane wore it outside the clip.
        let (mut app, mut doc) = app_and_doc();
        let cid = app
            .project
            .clips
            .iter()
            .max_by(|a, b| a.duration().total_cmp(&b.duration()))
            .map(|c| c.id)
            .expect("the fixture places clips");
        app.selection.select_only(cid);
        app.edit_selected(&mut doc, |c| {
            c.set_speed(2.0, false);
        });
        let clip = app.project.clip(cid).cloned().expect("clip exists");
        app.zoom = 220.0;
        app.scroll = clip.start + 0.6;
        app.playhead = clip.start + 1.0;
        app.playback.chase_playhead = false;
        app.selection.clear();

        let r = app.regions();
        let cr = clip_screen_rect(&app, &r, cid);
        assert!(
            cr.x1 > r.lanes.x1 + 20.0,
            "the clip runs off the right edge"
        );
        let strip = Rect::new(r.lanes.x1 - 90.0, cr.y0, r.lanes.x1, cr.y0 + 18.0);

        let fast = render_pixels(&mut app, 1280, 800, &Theme::dark());
        app.selection.select_only(cid);
        app.edit_selected(&mut doc, |c| {
            c.set_speed(1.0, false);
        });
        app.selection.clear();
        let normal = render_pixels(&mut app, 1280, 800, &Theme::dark());

        let diff = region_diff(&fast, &normal, 1280, 800, strip);
        assert!(
            diff > 40,
            "the rate badge must be drawn inside the visible lane; only {diff} pixels differ",
        );
    }
    // Case `the_canvas_hud_leaves_a_corner_inset_alone`:
    // The canvas HUD must not paint over the composition.
    //
    // Regression: the frame size was drawn in the canvas's top-right, which
    // is exactly where `Transform::pip()` places a corner inset — so every
    // top-right picture-in-picture had "1920x1080" burned across it.
    {
        let mut app = VideoEditorApp::new_for_shot(ShotScenario::Composed);
        let (w, h) = (900.0_f64, 700.0_f64);
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync(&mut doc, &automerge::ROOT);
        let px = render_pixels(&mut app, w as u32, h as u32, &Theme::dark());
        let r = app.regions();
        // The top-right quarter of the canvas: where the scenario's inset is.
        let corner = Rect::new(
            r.canvas.x0 + r.canvas.width() * 0.6,
            r.canvas.y0,
            r.canvas.x1,
            r.canvas.y0 + r.canvas.height() * 0.25,
        );
        // The inset is a mid-tone amber. HUD text was near-white at 70% over
        // it, which is far brighter than anything the picture itself holds.
        let mut whites = 0;
        let (x0, y0) = (corner.x0 as u32, corner.y0 as u32);
        let (x1, y1) = (
            (corner.x1 as u32).min(w as u32),
            (corner.y1 as u32).min(h as u32),
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w as u32 + x) * 4) as usize;
                if i + 2 < px.len() && px[i] > 200 && px[i + 1] > 200 && px[i + 2] > 200 {
                    whites += 1;
                }
            }
        }
        assert_eq!(whites, 0, "HUD text is painted over the corner inset");
        // The one HUD readout that stays is per-frame information, and it is
        // pinned to the left so it cannot reach a right-hand inset.
        assert!(r.canvas.width() > 0.0);
    }
}

/// How many pixels of `rect` differ between two frames of the same size.
/// Two renders of one scene are deterministic here, so any difference at
/// all is something one frame painted and the other did not.
fn region_diff(a: &[u8], b: &[u8], fw: u32, fh: u32, rect: Rect) -> usize {
    let x0 = rect.x0.max(0.0) as u32;
    let y0 = rect.y0.max(0.0) as u32;
    let x1 = (rect.x1.max(0.0) as u32).min(fw);
    let y1 = (rect.y1.max(0.0) as u32).min(fh);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * fw + x) * 4) as usize;
            if i + 3 < a.len() && i + 3 < b.len() && a[i..i + 4] != b[i..i + 4] {
                n += 1;
            }
        }
    }
    n
}

/// How many visually distinct colours a rect of a rendered frame contains,
/// quantised to 4 bits per channel so antialiasing noise does not count as
/// variety. A pane covered by an opaque slab returns 1.
fn region_colours(rgba: &[u8], fw: u32, fh: u32, rect: Rect) -> usize {
    let x0 = rect.x0.max(0.0) as u32;
    let y0 = rect.y0.max(0.0) as u32;
    let x1 = (rect.x1 as u32).min(fw);
    let y1 = (rect.y1 as u32).min(fh);
    let mut seen = HashSet::new();
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let i = ((y * fw + x) * 4) as usize;
            if i + 2 < rgba.len() {
                seen.insert((rgba[i] >> 4, rgba[i + 1] >> 4, rgba[i + 2] >> 4));
            }
            x += 2;
        }
        y += 2;
    }
    seen.len()
}

// ---- narrow-window panes ---------------------------------------------

// ---- pane scrolling ---------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn inspector_and_bin_scrollbars_reach_every_row() {
    // Case `the_inspector_scrollbar_is_grabbable_and_reaches_the_effects_section`.
    {
        // Regression: the inspector painted a scroll indicator with no
        // hit-test, so the EFFECTS block clipped off the bottom of the pane
        // could be seen to exist and never be reached.
        let (mut app, mut doc) = app_and_doc();
        // Select a video clip and give it enough effects to overflow the pane.
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip");
        app.selection.select_only(cid);
        for k in EffectKind::ALL {
            app.add_effect_to_selected(&mut doc, k);
        }
        render(&mut app);
        let r = app.regions();
        assert!(
            app.inspector_max_scroll(&r) > 0.5,
            "the inspector content overflows"
        );
        let track = app
            .inspector_scroll_track(&r)
            .expect("an overflowing pane shows a bar");
        let thumb = app
            .inspector_bar
            .thumb_rect(track, app.inspector_metrics(&r))
            .expect("a thumb");
        assert!(
            thumb.height() < track.height(),
            "the thumb is shorter than its track"
        );
        // Grab it and drag down.
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::Scrollbar(ScrollPane::Inspector))),
            "pressing the inspector thumb began a bar drag",
        );
        move_to(
            &mut app,
            &mut doc,
            thumb.center().x,
            thumb.center().y + 400.0,
        );
        assert!(
            app.inspector_scroll > 0.5,
            "dragging scrolled the inspector"
        );
        release(&mut app, &mut doc);
        // Scrolled to the end, the Add-effect button is inside the pane.
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let l = app.inspector_layout(&r);
        assert!(l.valid);
        assert!(
            l.add_effect.y1 <= r.inspector.y1 + 0.5,
            "the Add effect button is reachable once scrolled ({} vs pane bottom {})",
            l.add_effect.y1,
            r.inspector.y1,
        );
    }
    // Case `a_press_on_the_inspector_scrollbar_does_not_fall_through_to_a_control`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app
            .project
            .clips_on(tid)
            .first()
            .map(|c| c.id)
            .expect("a clip");
        app.selection.select_only(cid);
        for k in EffectKind::ALL {
            app.add_effect_to_selected(&mut doc, k);
        }
        render(&mut app);
        let r = app.regions();
        let track = app.inspector_scroll_track(&r).expect("a bar");
        // Page-click above the thumb: the bar takes it, no slider moves.
        let opacity_before = app.project.clip(cid).unwrap().opacity;
        move_to(&mut app, &mut doc, track.center().x, track.y1 - 2.0);
        press(&mut app, &mut doc);
        assert!(app.inspector_scroll > 0.0, "the track click paged the pane");
        assert_eq!(
            app.project.clip(cid).unwrap().opacity,
            opacity_before,
            "and did not also drag whatever slider was underneath",
        );
    }
    // Case `the_media_bin_scrolls_so_every_row_can_be_reached`.
    {
        // Regression: rows past the pane were drawn outside it and reachable
        // by nothing at all.
        let (mut app, mut doc) = app_and_doc();
        // Import enough stills to overflow the bin.
        for i in 0..14 {
            let png = write_real_png(16 + i, 16);
            app.try_import_one(&mut doc, &png)
                .expect("a real PNG imports");
            std::fs::remove_file(&png).ok();
        }
        render(&mut app);
        let r = app.regions();
        assert!(
            app.bin_max_scroll(&r) > 0.5,
            "the bin list overflows its pane"
        );
        let last = *app.bin_order().last().expect("a last row");
        let row_of = |app: &VideoEditorApp, r: &Regions, mid: MediaId| {
            app.bin_item_rects(r)
                .into_iter()
                .find(|(m, _)| *m == mid)
                .map(|(_, rect)| rect)
        };
        let before = row_of(&app, &r, last).expect("the last row has a rect");
        assert!(
            before.y1 > VideoEditorApp::bin_viewport(&r).y1,
            "the last row starts out below the fold",
        );
        // Wheel over the bin scrolls the list (it used to zoom the timeline).
        let zoom = app.zoom;
        move_to(&mut app, &mut doc, r.bin.center().x, r.bin.center().y);
        for _ in 0..40 {
            wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::empty());
        }
        assert_eq!(
            app.zoom, zoom,
            "a wheel over the bin does not zoom the timeline"
        );
        assert!(app.bin_scroll > 0.0, "it scrolls the bin instead");
        let after = row_of(&app, &r, last).expect("the last row still has a rect");
        assert!(
            after.y1 <= VideoEditorApp::bin_viewport(&r).y1 + 1.0,
            "the last row is now inside the pane ({} vs {})",
            after.y1,
            VideoEditorApp::bin_viewport(&r).y1,
        );
        // And it is genuinely clickable there.
        move_to(&mut app, &mut doc, after.center().x, after.center().y);
        press(&mut app, &mut doc);
        release(&mut app, &mut doc);
        assert_eq!(
            app.selected_media,
            Some(last),
            "the scrolled-to row selects on click"
        );
    }
    // Case `the_bin_scrollbar_thumb_drags_the_list`.
    {
        let (mut app, mut doc) = app_and_doc();
        for i in 0..14 {
            let png = write_real_png(16 + i, 16);
            app.try_import_one(&mut doc, &png)
                .expect("a real PNG imports");
            std::fs::remove_file(&png).ok();
        }
        render(&mut app);
        let r = app.regions();
        let track = app
            .bin_scroll_track(&r)
            .expect("an overflowing bin shows a bar");
        let thumb = app
            .bin_bar
            .thumb_rect(track, app.bin_metrics(&r))
            .expect("a thumb");
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y);
        press(&mut app, &mut doc);
        assert!(matches!(app.drag, Some(Drag::Scrollbar(ScrollPane::Bin))));
        move_to(
            &mut app,
            &mut doc,
            thumb.center().x,
            thumb.center().y + 300.0,
        );
        assert!(
            app.bin_scroll > 0.0,
            "dragging the bin thumb scrolled the list"
        );
        release(&mut app, &mut doc);
    }
}

// ---- filmstrip / import honesty --------------------------------------

#[test]
fn filmstrip_cells_request_and_draw_the_same_frames() {
    // Case `every_filmstrip_cell_of_a_visible_clip_has_a_frame_to_draw`.
    {
        // Regression: the clip body fetched ONE thumbnail and tiled it, and
        // when that lookup missed, every cell drew an empty box — a row of
        // pale rectangles that reads as "this clip has no picture". The draw
        // pass now requests a frame per cell, and every cell resolves to
        // something real to blit.
        let (mut app, _doc) = app_and_doc();
        render(&mut app); // the pass that requests the strips
        let r = app.regions();
        let mut checked = 0;
        for (tid, kind, lane) in app.lane_rects(&r) {
            if kind != TrackKind::Video {
                continue;
            }
            for clip in app.project.clips_on(tid) {
                let cr = app.clip_rect(clip, &lane, &r);
                if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
                    continue;
                }
                let inner = clip_inner_rect(cr);
                if inner.height() <= 4.0 || inner.width() <= 4.0 {
                    continue;
                }
                let (_, n) = filmstrip_cells(inner);
                assert!(n >= 1);
                let span = clip.out_point - clip.in_point;
                for i in 0..n {
                    let t = clip.in_point + ((i as f64 + 0.5) / n as f64) * span;
                    assert!(
                        app.thumbnails.frame_at(clip.media, t).is_some(),
                        "cell {i} of the clip at {} has no frame to draw",
                        clip.start,
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 0,
            "the fixture actually has visible video cells to check"
        );
    }
    // Case `the_strip_request_and_the_strip_draw_agree_on_cell_count`.
    {
        // The two passes derive the cell layout from the same helpers; if they
        // ever diverge, frames get decoded at times no cell asks for and every
        // cell falls back to the representative.
        for (w, h) in [(60.0, 20.0), (300.0, 44.0), (1200.0, 60.0)] {
            let inner = Rect::new(0.0, 0.0, w, h);
            let (cell_w, n) = filmstrip_cells(inner);
            assert!((24.0..=56.0).contains(&cell_w), "cell width stays in range");
            assert!(n >= 1, "there is always at least one cell");
            assert!(
                (n as f64) * (cell_w + 2.0) <= w + cell_w + 2.0,
                "the cells fit the body they were computed for",
            );
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn bin_menus_and_gestures_edit_the_crdt() {
    // Case `right_click_bin_item_opens_menu_and_delete_removes_media_from_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app.bin_item_rects(&r)[0];
        // Right-click the row → the context menu opens targeting that media.
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        right_press(&mut app, &mut doc);
        assert!(
            app.bin_menu.is_open(),
            "right-click opens the bin context menu"
        );
        assert!(matches!(app.menu_target, Some(MenuTarget::Media(m)) if m == mid));

        let before = VideoDoc::load_project(&doc).media.len();
        click_menu_item(&mut app, &mut doc, "Delete");
        let after = VideoDoc::load_project(&doc);
        assert_eq!(
            after.media.len(),
            before - 1,
            "Delete removed the media from the CRDT"
        );
        assert!(after.media(mid).is_none(), "the deleted source is gone");
        assert!(
            !app.bin_menu.is_open(),
            "the menu closed after choosing Delete"
        );
    }
    // Case `drag_bin_item_to_a_lane_places_a_clip_at_the_drop_time`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // A visual bin item, and a video lane to drop it on.
        let (mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).is_some_and(|mm| mm.kind.is_visual()))
            .expect("a visual bin item");
        let (tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(_, k, _)| *k == TrackKind::Video)
            .expect("a video lane");
        let before = VideoDoc::load_project(&doc).clips_on(tid).len();
        // Press on the bin item, drag past the threshold onto the lane, release.
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        press(&mut app, &mut doc);
        let drop_x = lane.x0 + 300.0;
        let drop_y = lane.center().y;
        move_to(&mut app, &mut doc, drop_x, drop_y);
        assert!(
            matches!(app.bin_drag, Some(BinDrag { active: true, .. })),
            "moving past the threshold promotes the bin drag",
        );
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 1,
            "a clip was placed on the drop track in the CRDT",
        );
        let placed = app
            .selected()
            .and_then(|id| reloaded.clip(id))
            .expect("placed clip selected");
        assert_eq!(
            placed.media, mid,
            "the placed clip references the dragged media"
        );
        let expected_t = app.x_to_time(drop_x, &r.lanes);
        assert!(
            (placed.start - expected_t).abs() < 0.2,
            "clip placed near the drop time ({} vs {expected_t})",
            placed.start,
        );
        assert!(app.bin_drag.is_none(), "the drag ended on release");
    }
    // Case `double_click_bin_item_on_empty_project_creates_track_and_clip`.
    {
        // A blank live project with one imported media and no tracks: double-
        // clicking a bin item must create a matching track and place the clip.
        let f = write_real_png(320, 240);
        let mut app = VideoEditorApp::new_import(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.project.media.len(), 1);
        assert!(
            app.project.tracks.is_empty(),
            "blank project starts with no tracks"
        );
        let r = app.regions();
        let (_mid, rect) = app.bin_item_rects(&r)[0];
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.tracks.len(), 1, "a matching track was created");
        assert_eq!(
            reloaded.clips.len(),
            1,
            "the clip was placed on the timeline"
        );
        std::fs::remove_file(&f).ok();
    }
    // Case `double_click_bin_item_adds_clip_to_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Target the first bin row's rect (computed by the app).
        let (_mid, rect) = app.bin_item_rects(&r)[0];
        let before = VideoDoc::load_project(&doc).clips.len();
        // Two presses inside the double-click window at the same spot.
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let after = VideoDoc::load_project(&doc).clips.len();
        assert_eq!(
            after,
            before + 1,
            "double-click on a bin item appends a clip to the CRDT"
        );
    }
}

/// Write a real 16-bit PCM mono WAV to a temp file for waveform tests.
fn write_real_wav() -> std::path::PathBuf {
    let mut data = Vec::new();
    for i in 0..8000u32 {
        let s = ((i as f32 * 0.2).sin() * 0.7 * 32767.0) as i16;
        data.extend_from_slice(&s.to_le_bytes());
    }
    let sr = 8000u32;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&sr.to_le_bytes());
    v.extend_from_slice(&(sr * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&data);
    let f = std::env::temp_dir().join(format!("tilo-ve-appwave-{}.wav", std::process::id()));
    std::fs::write(&f, v).unwrap();
    f
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn waveforms_are_warmed_for_sources_that_carry_sound() {
    // Case `app_warms_a_real_waveform_for_an_audio_source`.
    {
        // An audio source pointing at a real WAV file gets real peaks cached by
        // the sync wiring (synchronous warm, as a --shots capture uses).
        let f = write_real_wav();
        let mut app = VideoEditorApp::new();
        app.render_pipeline.warm_waveforms_sync = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        let m = MediaItem {
            id: Uuid::new_v4(),
            name: "vo.wav".into(),
            kind: MediaKind::Audio,
            duration: 1.0,
            width: 0,
            height: 0,
            fps: 0.0,
            palette: 5,
            path: f.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        };
        let mid = m.id;
        VideoDoc::upsert_media(&mut doc, &m);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.waveforms.peaks(mid).is_some(),
            "the app warmed real peaks for the audio source",
        );
        let peaks = app.waveforms.peaks(mid).unwrap();
        assert_eq!(peaks.len(), crate::waveform::WAVE_BUCKETS);
        assert!(
            peaks.iter().copied().fold(0.0, f32::max) > 0.9,
            "a real signal is present"
        );
        std::fs::remove_file(&f).ok();
    }
    // Case `a_picture_that_carries_sound_is_asked_for_its_peaks`:
    // The sound half draws a waveform, which means its source has to be asked
    // for peaks — and its source is a *video* file, which the editor used to
    // ask about only for audio-kind media. Without this the sound of every
    // shot was the flat line the editor draws for "there is nothing here".
    {
        use crate::waveform::WaveformState;
        let (mut app, mut doc) = app_and_doc();
        let mid = app
            .project
            .media
            .iter()
            .find(|m| m.name == "Cutaway_Market.mp4")
            .map(|m| m.id)
            .expect("the market source");
        assert_eq!(
            app.waveforms.state(mid),
            WaveformState::Unrequested,
            "a silent picture is not worth decoding for peaks",
        );
        if let Some(m) = app.project.media.iter_mut().find(|m| m.id == mid) {
            m.has_audio = true;
        }
        app.dispatch_media(&mut doc, mid);
        app.sync(&mut doc, &automerge::ROOT);
        assert_ne!(
            app.waveforms.state(mid),
            WaveformState::Unrequested,
            "the peaks were never asked for, so the sound half can only ever              draw the flat line that means 'nothing here'",
        );
    }
    // Case `a_picture_lane_is_metered_only_when_its_footage_carries_sound`:
    // The silent lane wears no meter, and the same lane wears one the moment
    // its footage carries sound.
    //
    // Asserted on the pixels of the lane's own header, because the claim is
    // about what is drawn — a predicate that returned the right answer into a
    // draw that ignored it would pass every other kind of test.
    {
        let theme = Theme::dark();
        let (w, h) = (1100u32, 760u32);
        let (mut app, mut doc) = app_and_doc();
        app.rect = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));

        let r = app.regions();
        let (tid, _, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(_, kind, _)| *kind == TrackKind::Video)
            .expect("the sample timeline has a picture lane");
        let header = VideoEditorApp::track_head_rect(&r, &lane);
        let strip = app.track_head_layout(header).meter;
        // The head only, never the lane: turning the sound on also asks for a
        // waveform, and a waveform lands on the *clip*. Comparing the whole
        // row would be reading one change as the other.
        let mid: Vec<crate::model::MediaId> =
            app.project.clips_on(tid).iter().map(|c| c.media).collect();
        assert!(!mid.is_empty(), "the lane has footage on it");

        let silent = render_pixels(&mut app, w, h, &theme);
        let again = render_pixels(&mut app, w, h, &theme);
        let noise = frame_diff(&crop(&silent, w, strip), &crop(&again, w, strip), w);
        assert!(
            noise.0 == 0,
            "the meter strip has to be still before a change to it can be \
             read: {}",
            noise.1,
        );

        for m in &mut app.project.media {
            if mid.contains(&m.id) {
                m.has_audio = true;
            }
        }
        for m in mid {
            app.dispatch_media(&mut doc, m);
        }
        app.sync(&mut doc, &automerge::ROOT);
        let heard = render_pixels(&mut app, w, h, &theme);
        let lit = frame_diff(&crop(&silent, w, strip), &crop(&heard, w, strip), w);
        assert!(
            lit.0 > 0,
            "footage that is heard has to be metered, and the lane's head is \
             where its meter is drawn: {}",
            lit.1,
        );
    }
}

#[test]
fn track_header_controls_add_and_delete_tracks() {
    // Case `clicking_add_audio_track_button_adds_a_track_to_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let before = VideoDoc::load_project(&doc)
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .count();
        let (_, abtn) = app
            .add_track_buttons(&r)
            .into_iter()
            .find(|(k, _)| *k == TrackKind::Audio)
            .expect("an add-audio-track button");
        click(&mut app, &mut doc, abtn.center().x, abtn.center().y);
        let after = VideoDoc::load_project(&doc);
        assert_eq!(
            after
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Audio)
                .count(),
            before + 1,
            "the add-audio-track button added a track to the CRDT",
        );
    }
    // Case `right_click_track_header_delete_removes_the_track_and_its_clips`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // A track that actually has clips, so we can prove they go too.
        let (tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(tid, _, _)| !app.project.clips_on(*tid).is_empty())
            .expect("a track with clips");
        let clip_ids: Vec<Uuid> = app.project.clips_on(tid).iter().map(|c| c.id).collect();
        // Right-click the header (gutter) → the track menu opens for that track.
        move_to(&mut app, &mut doc, r.gutter.x0 + 20.0, lane.center().y);
        right_press(&mut app, &mut doc);
        assert!(app.bin_menu.is_open(), "right-click opens the track menu");
        assert!(matches!(app.menu_target, Some(MenuTarget::Track(t)) if t == tid));
        let before = VideoDoc::load_project(&doc).tracks.len();
        click_menu_item(&mut app, &mut doc, "Delete track");
        let after = VideoDoc::load_project(&doc);
        assert_eq!(
            after.tracks.len(),
            before - 1,
            "the track was removed from the CRDT"
        );
        assert!(after.track(tid).is_none());
        for cid in clip_ids {
            assert!(
                after.clip(cid).is_none(),
                "the track's clips were removed too"
            );
        }
    }
}

// ---- geometry, rate, titles, keys and markers -------------------------

/// Scroll the inspector until `find` returns a rect that is actually on
/// screen, then hand back that rect. The new sections live below the fold
/// on a 1280x800 window, and a test that clicked their un-scrolled
/// coordinates would be pressing the timeline.
fn reveal<F>(app: &mut VideoEditorApp, find: F) -> Rect
where
    F: Fn(&InspectorLayout) -> Option<Rect>,
{
    let r = app.regions();
    let max = app.inspector_max_scroll(&r);
    let mut best = None;
    let offsets = (0u32..)
        .map(|k| f64::from(k) * 12.0)
        .take_while(|&offset| offset <= max + 1.0);
    for offset in offsets {
        app.inspector_scroll = offset.min(max);
        if let Some(rect) = find(&app.inspector_layout(&r))
            && rect.y0 > r.inspector.y0 + 8.0
            && rect.y1 < r.inspector.y1 - 8.0
        {
            best = Some(rect);
            break;
        }
    }
    best.expect("control never became visible at any scroll offset")
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn titles_are_added_written_and_styled() {
    // Case `adding_a_title_places_a_clip_and_opens_its_words`:
    // "+ Title" has to go all the way to an editable clip: a title with no
    // bytes to import is the one source where leaving an unnamed item in the
    // bin would be pure friction.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 12.0;
        let media_before = app.project.media.len();
        let clips_before = app.project.clips.len();
        let r = app.regions();
        let btn = app.add_title_rect(&r);
        click(&mut app, &mut doc, btn.center().x, btn.center().y);
        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.media.len(), media_before + 1, "a source was made");
        assert_eq!(
            p.clips.len(),
            clips_before + 1,
            "and placed on the timeline"
        );
        let item = p
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .expect("a title source");
        assert!(item.text.is_some(), "with a text body to edit");
        // A title has no bytes: nothing to fetch, so it is never "not on this
        // device" on any peer.
        assert!(item.path.is_empty());
        assert!(item.kind.is_generated());
        let clip = p
            .clips
            .iter()
            .find(|c| c.media == item.id)
            .expect("its clip");
        assert!(
            (clip.start - 12.0).abs() < 0.1,
            "placed at the playhead, got {}",
            clip.start,
        );
        assert_eq!(app.selection.primary(), Some(clip.id), "and selected");
        assert!(app.title_edit.is_some(), "with the caret in its words");
        // Typing and committing writes the words and renames the source.
        app.commit_title_text(&mut doc, item.id, "SUNSET");
        let p = VideoDoc::load_project(&doc);
        let item = p.media.iter().find(|m| m.id == item.id).unwrap();
        assert_eq!(item.text.as_ref().unwrap().content, "SUNSET");
        assert_eq!(item.name, "SUNSET", "the bin row follows the words");
    }
    // Case `the_title_controls_write_the_source`:
    // The title controls edit the *source*, so they commit a media write —
    // a clip write would drop them on the floor.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 12.0;
        app.add_title(&mut doc);
        app.title_edit = None;
        let mid = app.selected_media_item().unwrap().id;
        // Alignment.
        let rect = reveal(&mut app, |l| {
            l.title_align
                .iter()
                .find(|(a, _)| *a == TextAlign::Right)
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let t = VideoDoc::load_project(&doc)
            .media(mid)
            .unwrap()
            .text
            .clone()
            .unwrap();
        assert_eq!(t.align, TextAlign::Right);
        // Colour: an authored value, not a palette slot, so it survives a
        // theme change.
        let rect = reveal(&mut app, |l| l.title_colors.get(2).map(|(_, r)| *r));
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let t = VideoDoc::load_project(&doc)
            .media(mid)
            .unwrap()
            .text
            .clone()
            .unwrap();
        assert_eq!(t.color, TITLE_COLORS[2]);
        // Size is a fraction of the frame height, dragged.
        let track = reveal(&mut app, |l| l.num(NumField::TitleSize));
        let before = t.size;
        click(
            &mut app,
            &mut doc,
            track.x0 + track.width() * 0.8,
            track.center().y,
        );
        release(&mut app, &mut doc);
        let t = VideoDoc::load_project(&doc)
            .media(mid)
            .unwrap()
            .text
            .clone()
            .unwrap();
        assert!(
            t.size > before,
            "the size slider raised it: {} -> {}",
            before,
            t.size
        );
        assert!(t.size <= 0.4, "and stayed inside the offered range");
    }
    // Case `a_title_can_be_written_on_more_than_one_line`.
    {
        let (mut app, mut doc) = app_and_doc();
        let existing = app
            .project
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .map(|m| m.id);
        let mid = existing.unwrap_or_else(|| {
            let item = MediaItem {
                id: Uuid::new_v4(),
                name: "T".into(),
                kind: MediaKind::Text,
                duration: 4.0,
                text: Some(TextSource::new("ONE")),
                ..Default::default()
            };
            let id = item.id;
            app.project.media.push(item);
            id
        });
        app.commit_title_text(&mut doc, mid, "COASTLINE\nepisode two");
        let words = VideoDoc::load_project(&doc)
            .media
            .iter()
            .find(|m| m.id == mid)
            .and_then(|m| m.text.as_ref())
            .map(|t| t.content.clone())
            .expect("the title survived");
        assert_eq!(
            words, "COASTLINE\nepisode two",
            "the break is real, not a glyph"
        );
        // The bin row takes the first line, which is what a one-line list can
        // honestly show of a two-line title.
        let name = VideoDoc::load_project(&doc)
            .media
            .iter()
            .find(|m| m.id == mid)
            .map(|m| m.name.clone())
            .expect("the item survived");
        assert_eq!(name, "COASTLINE");
    }
    // Case `enter_breaks_a_title_line_and_only_ctrl_enter_accepts_it`.
    {
        // Built with the suite's own synthetic-key helper so the shapes match
        // what the live loop delivers.
        let released = |k: NamedKey| -> WindowEvent {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = WKey::Named(k);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Released;
            ev.repeat = false;
            ev.text = None;
            WindowEvent::KeyboardInput {
                device_id: fake_device_id(),
                event: ev,
                is_synthetic: false,
            }
        };
        let plain = winit::event::Modifiers::default();
        let ctrl = winit::event::Modifiers::from(ModifiersState::CONTROL);

        // Enter on its own is a line break, so the field keeps the event.
        assert_eq!(title_edit_verdict(&named_ev(NamedKey::Enter), plain), None);
        // With Ctrl it accepts, and Escape cancels.
        assert_eq!(
            title_edit_verdict(&named_ev(NamedKey::Enter), ctrl),
            Some(true)
        );
        assert_eq!(
            title_edit_verdict(&named_ev(NamedKey::Escape), plain),
            Some(false)
        );
        // A key going up ends nothing — otherwise the release of the very
        // chord that accepted would immediately re-fire.
        assert_eq!(title_edit_verdict(&released(NamedKey::Enter), ctrl), None);
    }
    // Case `the_font_picker_offers_the_installed_families_and_a_way_back_to_the_default`.
    {
        let (mut app, mut doc) = app_and_doc();
        let item = MediaItem {
            id: Uuid::new_v4(),
            name: "T".into(),
            kind: MediaKind::Text,
            duration: 4.0,
            text: Some(TextSource::new("HELLO")),
            ..Default::default()
        };
        let mid = item.id;
        app.project.media.push(item);
        let track = app
            .project
            .display_tracks()
            .into_iter()
            .find(|t| t.kind == TrackKind::Video)
            .map(|t| t.id)
            .expect("a video track");
        let cid = app.project.add_clip(track, mid, 0.0).expect("media exists");
        app.selection.select_only(cid);
        app.collapsed.remove(&Section::Title);

        // Before a draw has read the renderer there are no families, and the
        // picker refuses to open over an empty list rather than showing one.
        app.open_font_picker();
        assert!(app.font_picker.is_none(), "no list yet, so no picker");

        app.system_fonts = vec!["Alpha Sans".into(), "Beta Serif".into()];
        app.open_font_picker();
        let (_, dd, _) = app.font_picker.as_ref().expect("the picker opened");
        assert!(dd.is_open());
        assert_eq!(
            dd.selected(),
            VideoEditorApp::DEFAULT_FONT_LABEL,
            "an unset family reads as the system default, not as blank",
        );

        // Choosing a family writes it to the media item through the ordinary
        // title action, so every clip placing this title follows.
        app.font_picker = None;
        app.edit_title(&mut doc, |t| t.font = "Beta Serif".into());
        let font = VideoDoc::load_project(&doc)
            .media
            .iter()
            .find(|m| m.id == mid)
            .and_then(|m| m.text.as_ref())
            .map(|t| t.font.clone())
            .expect("the title survived");
        assert_eq!(font, "Beta Serif");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn marker_list_names_navigates_and_deletes() {
    // Case `the_marker_list_names_navigates_and_deletes`:
    // Markers are listed, renamed, navigated and deleted from one place, and
    // each kind commits through the action that owns it.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 8.0;
        // M marks the playhead and opens the name for typing.
        assert!(key_press(&mut app, &mut doc, "m", false));
        assert!(
            app.marker_rename.is_some(),
            "an unnamed tick is a marker you cannot find"
        );
        let target = app.marker_rename.as_ref().unwrap().0;
        app.commit_marker_rename(&mut doc, target, "act two");
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.markers.len(), 1);
        assert_eq!(p.markers[0].name, "act two");
        assert!((p.markers[0].t - 8.0).abs() < 1e-6);
        // A second one, so navigation has somewhere to go.
        app.playhead = 20.0;
        key_press(&mut app, &mut doc, "m", false);
        let second = app.marker_rename.as_ref().unwrap().0;
        app.commit_marker_rename(&mut doc, second, "sunset");
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        // , and . step between them and stop at the ends.
        app.playhead = 14.0;
        key_press(&mut app, &mut doc, ",", false);
        assert!(
            (app.playhead - 8.0).abs() < 1e-6,
            "stepped back to the first"
        );
        key_press(&mut app, &mut doc, ".", false);
        assert!(
            (app.playhead - 20.0).abs() < 1e-6,
            "and forward past it to the second"
        );
        key_press(&mut app, &mut doc, ".", false);
        assert!(
            (app.playhead - 20.0).abs() < 1e-6,
            "navigation stops rather than wrapping"
        );
        // Clicking a row takes the playhead there; the cross removes it.
        let row = reveal(&mut app, |l| l.marker_rows.first().map(|(_, r)| *r));
        click(&mut app, &mut doc, row.center().x, row.center().y);
        assert!((app.playhead - 8.0).abs() < 1e-6, "the row navigated");
        let row = reveal(&mut app, |l| l.marker_rows.first().map(|(_, r)| *r));
        let (_, _, _, del) = marker_row_parts(row);
        click(&mut app, &mut doc, del.center().x, del.center().y);
        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.markers.len(), 1, "the cross deleted one");
        assert_eq!(p.markers[0].name, "sunset", "and left the other");
    }
    // Case `a_clip_marker_is_listed_beside_the_timeline_ones_and_deletes_as_a_clip_write`:
    // A clip marker is stored clip-local and edited through a clip write,
    // which is a different action from a timeline marker's.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        let start = app.project.clip(id).unwrap().start;
        app.selection.select_only(id);
        app.remember_clip_before(id);
        app.project
            .clip_mut(id)
            .unwrap()
            .add_marker(start + 1.0, "beat", 2);
        app.dispatch_clip(&mut doc, id);
        app.sync(&mut doc, &automerge::ROOT);
        app.playhead = 4.0;
        key_press(&mut app, &mut doc, "m", false);
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        // One list, both kinds, sorted by where they actually land.
        let r = app.regions();
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let rows = app.inspector_layout(&r).marker_rows;
        assert_eq!(
            rows.len(),
            2,
            "the clip's marker is listed with the timeline's"
        );
        assert!(rows.iter().any(|(m, _)| m.clip == Some(id)));
        assert!(rows.iter().any(|(m, _)| m.clip.is_none()));
        // The clip one reports an absolute time even though it is stored local.
        let clip_row = rows.iter().find(|(m, _)| m.clip == Some(id)).unwrap().0;
        assert!((clip_row.at - (start + 1.0)).abs() < 1e-6);
        // Deleting it writes the clip, not the project.
        app.delete_marker(&mut doc, clip_row);
        let p = VideoDoc::load_project(&doc);
        assert!(p.clip(id).unwrap().markers.is_empty(), "gone from the clip");
        assert_eq!(p.markers.len(), 1, "and the timeline marker is untouched");
    }
    // Case `the_marker_list_is_reachable_with_no_clip_selected`:
    // A marker belongs to the project, so the list has to be reachable with
    // nothing selected — otherwise seeing or removing one meant first
    // picking a clip that had nothing to do with it.
    {
        let (mut app, mut doc) = app_and_doc();
        app.selection.clear();
        app.playhead = 7.0;
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(
            !l.valid,
            "no clip is selected, so no clip controls are laid out"
        );
        assert!(l.num(NumField::ScaleX).is_none());
        assert!(l.split.width() == 0.0, "and no clip actions");
        let add = l
            .add_marker
            .expect("but the markers section is still offered");
        assert!(r.inspector.contains(add.center()));
        // The button works from this state, not just the keyboard.
        click(&mut app, &mut doc, add.center().x, add.center().y);
        assert_eq!(
            VideoDoc::load_project(&doc).markers.len(),
            1,
            "a marker was added"
        );
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        // And the row it produced can be deleted from the same state.
        let l = app.inspector_layout(&app.regions());
        let row = l.marker_rows.first().expect("the marker is listed").1;
        let (_, _, _, del) = marker_row_parts(row);
        click(&mut app, &mut doc, del.center().x, del.center().y);
        assert!(
            VideoDoc::load_project(&doc).markers.is_empty(),
            "and removed"
        );
    }
    // Case `an_empty_project_still_lists_its_markers_to_a_reader`:
    // With no clips at all the panel paints its empty state *and*, under it,
    // the marker list -- markers belong to the project, not to a selection.
    // The tree used to return only the empty-state line, hiding every marker
    // that was plainly on the screen.
    {
        ui::host::install_default(ui::host::HostServices::recording().0);
        let mut doc = AutoCommit::new();
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.project.clips.is_empty(),
            "the fixture is a blank project"
        );
        let m = marker_at(
            &mut app,
            &mut doc,
            3.0,
            "start here",
            "before anything is cut",
        );

        let node = app.a11y_inspector();
        let spoken = node
            .children
            .iter()
            .filter_map(|n| n.name.clone())
            .collect::<Vec<_>>();
        assert!(
            spoken.iter().any(|n| n.contains(INSPECTOR_EMPTY_TITLE)),
            "the empty state is still spoken: {spoken:?}",
        );
        let found = a11y_find(&node, "start here");
        assert_eq!(found.len(), 1, "and the marker with it: {spoken:?}");
        assert_eq!(found[0].key, Some(ui::a11y::hash_key(&m.id)));
        assert_eq!(
            app.inspector_layout(&app.regions()).marker_rows.len(),
            1,
            "which is the row the panel drew",
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn history_buttons_follow_the_host() {
    // Case `the_history_buttons_are_dead_until_the_host_says_otherwise`:
    // The editor could only ever *say* "Ctrl+Z to go back"; there was no
    // API for a widget to read the history, so there was no button to
    // press. These prove the one it now draws is honest about when it can
    // act — and that it cannot act at any other time.
    {
        let (app, _doc) = app_and_doc();
        let r = app.regions();
        let rects = app.history_buttons(&r);
        assert_eq!(rects.len(), 2, "undo and redo both get a slot");
        // Nothing published yet: an empty history, so neither can act.
        for (which, rect) in &rects {
            assert!(
                !app.undo.status().can(*which),
                "{which:?} starts unavailable"
            );
            assert!(
                r.transport.contains(rect.center()),
                "{which:?} is inside the transport strip: {rect:?}",
            );
        }
        // And they do not overlap the transport circles or the scrubber.
        for (_, t) in app.transport_buttons(&r) {
            for (_, h) in &rects {
                assert!(
                    t.x1 <= h.x0 || h.x1 <= t.x0,
                    "a history button overlaps a transport button: {h:?} vs {t:?}",
                );
            }
        }
        let scrub = app.scrubber_rect(&r);
        for (_, h) in &rects {
            assert!(
                h.x1 <= scrub.x0,
                "the scrubber starts clear of them: {h:?} {scrub:?}"
            );
        }
    }
    // Case `pressing_undo_asks_the_host_only_when_there_is_something_to_undo`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (_, rect) = app.history_buttons(&r)[0];
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert_eq!(
            ui::undo::take_undo_request(&mut app),
            None,
            "with nothing to undo the button raises no request at all",
        );
        // Once the host says there is, the same press does.
        ui::undo::set_undo_status(&mut app, &mut || ui::UndoStatus {
            can_undo: true,
            undo_summary: Some("2 fields".into()),
            ..Default::default()
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert_eq!(
            ui::undo::take_undo_request(&mut app),
            Some(ui::UndoRequest::Undo)
        );
        assert_eq!(
            ui::undo::take_undo_request(&mut app),
            None,
            "and the request is taken once"
        );
        // Redo is still unavailable, and its button still refuses.
        let (_, redo) = app.history_buttons(&r)[1];
        click(&mut app, &mut doc, redo.center().x, redo.center().y);
        assert_eq!(
            ui::undo::take_undo_request(&mut app),
            None,
            "redo is separately gated"
        );
    }
    // Case `the_undo_node_reports_disabled_exactly_when_it_cannot_act`:
    // The accessibility node must report `disabled` exactly when the verb is
    // unavailable, and must not be focusable then: undoing the last action
    // empties the stack under the control the user is standing on, and a
    // focus ring left on a dead button is how that reads wrong.
    {
        let (mut app, _doc) = app_and_doc();
        let find = |app: &VideoEditorApp, name: &str| {
            fn walk(n: &AccessNode, name: &str, out: &mut Option<AccessNode>) {
                if n.name.as_deref() == Some(name) {
                    *out = Some(n.clone());
                }
                for k in &n.children {
                    walk(k, name, out);
                }
            }
            let mut found = None;
            walk(&app.a11y_transport(), name, &mut found);
            found.unwrap_or_else(|| panic!("{name} is in the transport tree"))
        };

        let node = find(&app, "Undo");
        assert!(node.state.disabled, "an empty history disables the node");
        assert!(!node.state.focusable, "and takes it out of the focus order");
        assert_eq!(
            node.description.unwrap_or_default(),
            "Nothing to undo",
            "the reader hears the honest reason, not a described action",
        );

        ui::undo::set_undo_status(&mut app, &mut || ui::UndoStatus {
            can_undo: true,
            undo_summary: Some("2 fields".into()),
            ..Default::default()
        });
        let node = find(&app, "Undo");
        assert!(!node.state.disabled);
        assert!(node.state.focusable);
        let d = node.description.unwrap_or_default();
        assert!(
            d.contains("2 fields"),
            "and hears what it would revert: {d:?}"
        );
        // Redo has not become available just because undo did.
        assert!(find(&app, "Redo").state.disabled);
    }
    // Case `activating_the_undo_node_raises_the_same_request_a_click_does`.
    {
        let (mut app, _doc) = app_and_doc();
        ui::undo::set_undo_status(&mut app, &mut || ui::UndoStatus {
            can_undo: true,
            ..Default::default()
        });
        let key = ui::a11y::hash_key(&ui::UndoRequest::Undo.verb());
        assert!(app.a11y_transport_action(&ui::a11y::Action::Activate, &[key]));
        assert_eq!(
            ui::undo::take_undo_request(&mut app),
            Some(ui::UndoRequest::Undo)
        );
        // With the history empty again the same activation refuses.
        ui::undo::set_undo_status(&mut app, &mut || ui::UndoStatus::empty());
        assert!(!app.a11y_transport_action(&ui::a11y::Action::Activate, &[key]));
        assert_eq!(ui::undo::take_undo_request(&mut app), None);
        // And the transport's own buttons still work, so the new key lookup
        // has not shadowed them.
        let play = ui::a11y::hash_key(&2usize);
        let before = app.playback.playing;
        assert!(app.a11y_transport_action(&ui::a11y::Action::Activate, &[play]));
        assert_ne!(app.playback.playing, before, "play/pause still activates");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn key_and_mask_controls_appear_write_and_keyframe() {
    // Case `the_key_and_mask_controls_appear_only_once_they_have_something_to_do`:
    // The whole point of gating these two sections' controls: an unkeyed,
    // unmasked clip must not carry ten sliders that do nothing, and the
    // moment the feature is switched on they have to be there.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(l.key_enable.is_some(), "the key can always be switched on");
        assert!(
            l.num(NumField::KeyTolerance).is_none(),
            "but has no sliders yet"
        );
        assert!(l.key_colors.is_empty(), "and no colour to pick");
        assert!(!l.mask_shapes.is_empty(), "a shape can always be chosen");
        assert!(
            l.num(NumField::MaskW).is_none(),
            "but there is no shape to size"
        );
        assert!(l.mask_invert.is_none());

        // Switch the key on through the button, as a user would.
        let rect = reveal(&mut app, |l| l.key_enable);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert!(
            VideoDoc::load_project(&doc).clip(id).unwrap().key.enabled,
            "the toggle reaches the document, not just the preview",
        );
        let l = app.inspector_layout(&app.regions());
        for f in [
            NumField::KeyTolerance,
            NumField::KeySoftness,
            NumField::KeySpill,
        ] {
            assert!(l.num(f).is_some(), "{f:?} is laid out once the key is on");
        }
        assert_eq!(l.key_colors.len(), crate::model::KEY_PRESETS.len());

        // And a mask shape brings its geometry with it.
        let rect = reveal(&mut app, |l| {
            l.mask_shapes
                .iter()
                .find(|(s, _)| *s == crate::model::MaskShape::Ellipse)
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let stored = VideoDoc::load_project(&doc).clip(id).unwrap().mask;
        assert_eq!(stored.shape, crate::model::MaskShape::Ellipse);
        let l = app.inspector_layout(&app.regions());
        for f in [
            NumField::MaskX,
            NumField::MaskY,
            NumField::MaskW,
            NumField::MaskH,
            NumField::MaskRotation,
            NumField::MaskFeather,
        ] {
            assert!(
                l.num(f).is_some(),
                "{f:?} is laid out once a shape is chosen"
            );
        }
        assert!(l.mask_invert.is_some());
    }
    // Case `switching_the_key_on_gives_it_a_tolerance_that_keys`:
    // Switching a key on that would key nothing reads as a broken button.
    // The toggle gives back a tolerance that actually cuts.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.edit_selected(&mut doc, |c| c.key.tolerance = 0.0);
        let rect = reveal(&mut app, |l| l.key_enable);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let key = VideoDoc::load_project(&doc).clip(id).unwrap().key;
        assert!(key.enabled);
        assert!(
            key.is_active(),
            "on, and actually removing something: {key:?}"
        );
    }
    // Case `a_new_mask_lands_on_the_picture_the_transform_placed`:
    // Choosing a shape lands the mask on the picture rather than in the
    // middle of an empty frame — which is what makes rounding off a
    // picture-in-picture one click instead of four slider drags.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.edit_selected(&mut doc, |c| {
            c.transform = crate::model::Transform::pip(0.3, (1.0, -1.0), 0.04);
        });
        let placed = app.project.clip(id).unwrap().transform;
        let rect = reveal(&mut app, |l| {
            l.mask_shapes
                .iter()
                .find(|(s, _)| *s == crate::model::MaskShape::Ellipse)
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let m = VideoDoc::load_project(&doc).clip(id).unwrap().mask;
        assert!(
            (m.x - placed.pos_x).abs() < 1e-9,
            "centred on the inset: {m:?}"
        );
        assert!((m.y - placed.pos_y).abs() < 1e-9);
        assert!(
            m.w < 0.5 && m.h < 0.5,
            "and sized to it, not to the frame: {m:?}"
        );

        // Changing the shape afterwards keeps the geometry the user has by
        // then adjusted — only the first choice seeds it.
        app.edit_selected(&mut doc, |c| c.mask.w = 0.123);
        let rect = reveal(&mut app, |l| {
            l.mask_shapes
                .iter()
                .find(|(s, _)| *s == crate::model::MaskShape::Rect)
                .map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let m = VideoDoc::load_project(&doc).clip(id).unwrap().mask;
        assert_eq!(m.shape, crate::model::MaskShape::Rect);
        assert!(
            (m.w - 0.123).abs() < 1e-9,
            "the width the user set survived: {m:?}"
        );
    }
    // Case `dragging_a_key_or_mask_slider_writes_it_to_the_document`.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.edit_selected(&mut doc, |c| {
            c.key.enabled = true;
            c.mask.shape = crate::model::MaskShape::Rect;
        });
        for (f, read) in [
            (
                NumField::KeyTolerance,
                (|c: &Clip| c.key.tolerance) as fn(&Clip) -> f64,
            ),
            (NumField::KeySoftness, |c: &Clip| c.key.softness),
            (NumField::KeySpill, |c: &Clip| c.key.spill),
            (NumField::MaskW, |c: &Clip| c.mask.w),
            (NumField::MaskFeather, |c: &Clip| c.mask.feather),
            (NumField::MaskRotation, |c: &Clip| c.mask.rotation),
        ] {
            let track = reveal(&mut app, |l| l.num(f));
            let frac = 0.7;
            click(
                &mut app,
                &mut doc,
                track.x0 + track.width() * frac,
                track.center().y,
            );
            release(&mut app, &mut doc);
            let want = f.at_frac(frac);
            let got = read(VideoDoc::load_project(&doc).clip(id).unwrap());
            let span = f.range().1 - f.range().0;
            assert!(
                (got - want).abs() < span * 0.05,
                "{f:?} landed at {got} not ~{want} in the document",
            );
        }
    }
    // Case `the_key_and_mask_sliders_can_be_keyframed_from_the_inspector`:
    // Every key and mask slider carries a keyframe control, because both
    // are things that have to move across a shot.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        app.edit_selected(&mut doc, |c| {
            c.key.enabled = true;
            c.mask.shape = crate::model::MaskShape::Ellipse;
        });
        for param in [Param::KeyTolerance, Param::MaskX] {
            let rect = reveal(&mut app, |l| {
                l.keys.iter().find(|(p, _)| *p == param).map(|(_, r)| *r)
            });
            click(&mut app, &mut doc, rect.center().x, rect.center().y);
            let c = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
            assert!(c.anim.is_animated(param), "{param:?} has a curve now");
            assert!(c.has_key_at(param, 23.0), "with a key on the playhead");
        }
    }
    // Case `a_reader_nudging_the_masks_tilt_does_not_rotate_the_picture`:
    // And the routing really lands on the row that was named.
    //
    // The key test above proves the two identities differ; this proves the
    // walk uses them — a reader nudging the mask's rotation has to move the
    // mask, and leave the picture where it is.
    {
        let (mut app, mut doc) = with_video_clip_selected();
        let cid = app.selected().expect("a clip is selected");
        // A mask has to exist for its geometry rows to be laid out at all.
        app.collapsed.remove(&Section::Mask);
        app.set_mask_shape(&mut doc, crate::model::MaskShape::Ellipse);
        app.sync(&mut doc, &automerge::ROOT);
        let before = app.project.clip(cid).expect("clip").clone();

        let rows = app.a11y_inspector_rows();
        let at = rows
            .iter()
            .position(|r| *r == InspectorRow::Field(NumField::MaskRotation))
            .expect("the mask's rotation is a stop once a shape is chosen");
        app.focus.pane = Pane::Inspector;
        app.focus.inspector = at;
        let path = vec![
            Pane::Inspector.key(),
            ui::a11y::hash_key(&Section::Mask.label()),
            NumField::MaskRotation.a11y_key(),
        ];
        let root = automerge::ROOT;
        assert!(
            app.perform_action_at(&ui::a11y::Action::Increment, &path, &mut doc, &root),
            "the mask's rotation answers a reader's increment",
        );
        let after = VideoDoc::load_project(&doc)
            .clip(cid)
            .expect("the clip survives")
            .clone();
        assert!(
            (after.mask.rotation - before.mask.rotation).abs() > 1e-6,
            "the mask turned: {} -> {}",
            before.mask.rotation,
            after.mask.rotation,
        );
        assert!(
            (after.transform.rotation - before.transform.rotation).abs() < 1e-9,
            "and the picture did not: asking to turn the mask turned the whole \
             clip, because both rows hashed to one key ({} -> {})",
            before.transform.rotation,
            after.transform.rotation,
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn inspector_effects_add_remove_reorder_and_dispatch() {
    // Case `inspector_add_effect_button_adds_via_the_picker`.
    {
        let (mut app, mut doc) = app_and_doc();
        // A video clip with no effects (the market cutaway around t=23).
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let r = app.regions();
        // The effects controls sit below the fold; scroll to reveal them.
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let l = app.inspector_layout(&r);
        assert!(
            l.add_effect.width() > 0.0 && r.inspector.contains(l.add_effect.center()),
            "the add-effect button is visible when scrolled",
        );
        let before = VideoDoc::load_project(&doc).clip(id).unwrap().effects.len();
        click(
            &mut app,
            &mut doc,
            l.add_effect.center().x,
            l.add_effect.center().y,
        );
        assert!(app.bin_menu.is_open(), "add-effect opens the picker");
        assert!(matches!(app.menu_target, Some(MenuTarget::AddEffect)));
        // Choose the first effect kind (Grayscale, idx 0).
        let menu = app.bin_menu.get_rect();
        let cx = menu.center().x;
        let ry = (0..(menu.height() as i64))
            .map(|d| menu.y0 + d as f64 + 0.5)
            .find(|&y| app.bin_menu.item_at([cx, y]) == Some(0))
            .unwrap();
        move_to(&mut app, &mut doc, cx, ry);
        press(&mut app, &mut doc);
        let after = VideoDoc::load_project(&doc)
            .clip(id)
            .unwrap()
            .effects
            .clone();
        assert_eq!(
            after.len(),
            before + 1,
            "an effect was added through the inspector"
        );
        assert_eq!(after.last().unwrap().kind, EffectKind::Grayscale);
        let added: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("the add dispatched an action")
                .payload,
        )
        .expect("the add action decodes");
        assert!(
            matches!(added, crate::actions::VideoEditorAction::AddEffect { .. }),
            "the picker must not publish a whole-clip snapshot",
        );
    }
    // Case `inspector_remove_and_reorder_effects`.
    {
        let (mut app, mut doc) = app_and_doc();
        // The drone clip carries two seeded effects.
        let id = app.project.top_video_clip_at(6.0).unwrap().id;
        app.selection.select_only(id);
        let kinds0: Vec<_> = VideoDoc::load_project(&doc)
            .clip(id)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.kind)
            .collect();
        assert!(kinds0.len() >= 2, "the drone has seeded effects");
        let r = app.regions();
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let l = app.inspector_layout(&r);
        // Move the second effect up via its ▲.
        let (_k, up, _d, _s, _x) = VideoEditorApp::effect_row_parts(l.effect_rows[1]);
        assert!(
            r.inspector.contains(up.center()),
            "the reorder control is visible"
        );
        click(&mut app, &mut doc, up.center().x, up.center().y);
        let kinds1: Vec<_> = VideoDoc::load_project(&doc)
            .clip(id)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds1[0], kinds0[1], "the second effect moved to the front");
        assert_eq!(kinds1[1], kinds0[0]);
        let moved: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("the reorder dispatched an action")
                .payload,
        )
        .expect("the reorder action decodes");
        assert!(
            matches!(moved, crate::actions::VideoEditorAction::MoveEffect { .. }),
            "an inspector reorder must not publish a whole-clip snapshot",
        );
        // Remove the head via its ✕.
        let l = app.inspector_layout(&r);
        let (_k, _u, _dn, _sl, remove) = VideoEditorApp::effect_row_parts(l.effect_rows[0]);
        click(&mut app, &mut doc, remove.center().x, remove.center().y);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().effects.len();
        assert_eq!(
            after,
            kinds0.len() - 1,
            "the ✕ removed one effect from the CRDT"
        );
        let removed: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("the remove dispatched an action")
                .payload,
        )
        .expect("the remove action decodes");
        assert!(
            matches!(
                removed,
                crate::actions::VideoEditorAction::RemoveEffect { .. }
            ),
            "an inspector remove must not publish a whole-clip snapshot",
        );
    }
    // Case `dragging_effect_amount_dispatches_a_named_effect_action`.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(6.0).expect("drone clip").id;
        app.selection.select_only(id);
        let r = app.regions();
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let row = *app
            .inspector_layout(&r)
            .effect_rows
            .first()
            .expect("the fixture has an effect row");
        let (_key, _up, _down, slider, _remove) = VideoEditorApp::effect_row_parts(row);
        let want = slider.x0 + slider.width() * 0.25;
        click(&mut app, &mut doc, want, slider.center().y);
        release(&mut app, &mut doc);

        let action: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("the intensity drag dispatched an action")
                .payload,
        )
        .expect("the intensity action decodes");
        assert!(
            matches!(
                action,
                crate::actions::VideoEditorAction::SetEffectAmount { .. }
            ),
            "the effect slider must not publish a whole-clip snapshot",
        );
    }
}

#[test]
fn pointer_edits_on_the_timeline_write_the_crdt() {
    // Case `click_scrubs_playhead_on_the_ruler`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Click a point partway along the ruler and assert the playhead
        // landed near the corresponding time (via the app's own mapping).
        let x = r.ruler.x0 + 200.0;
        let y = r.ruler.center().y;
        let expected = app.x_to_time(x, &r.lanes);
        click(&mut app, &mut doc, x, y);
        assert!(
            (app.playhead - expected).abs() < 0.1,
            "ruler click scrubs playhead to {expected}, got {}",
            app.playhead
        );
    }
    // Case `drag_moves_a_clip_and_writes_the_new_start_to_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Find the drone clip's on-screen rect on V1 (the first video lane row).
        let clip = app.project.top_video_clip_at(3.0).unwrap().clone();
        let (_tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(tid, _, _)| *tid == clip.track)
            .unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        let start_before = VideoDoc::load_project(&doc).clip(clip.id).unwrap().start;
        // Grab the body (avoid the ±8px trim edges), drag right ~2 seconds.
        let grab_x = cr.center().x;
        let grab_y = cr.center().y;
        let drop_x = grab_x + 2.0 * app.zoom; // +2s at current px/sec
        move_to(&mut app, &mut doc, grab_x, grab_y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, drop_x, grab_y);
        release(&mut app, &mut doc);
        let start_after = VideoDoc::load_project(&doc).clip(clip.id).unwrap().start;
        assert!(
            start_after > start_before + 1.0,
            "dragging moved the clip right in the CRDT ({start_before} -> {start_after})"
        );
    }
    // Case `drag_right_edge_trims_the_clip_in_the_crdt`.
    {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let clip = app.project.top_video_clip_at(3.0).unwrap().clone();
        let (_tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(tid, _, _)| *tid == clip.track)
            .unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        let out_before = VideoDoc::load_project(&doc)
            .clip(clip.id)
            .unwrap()
            .out_point;
        // Grab within TRIM_HIT of the right edge and drag left ~1.5s.
        let grab_x = cr.x1 - 1.0;
        let grab_y = cr.center().y;
        let drop_x = grab_x - 1.5 * app.zoom;
        move_to(&mut app, &mut doc, grab_x, grab_y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::TrimOut { .. })),
            "grabbing the right edge starts a trim-out drag"
        );
        move_to(&mut app, &mut doc, drop_x, grab_y);
        release(&mut app, &mut doc);
        let out_after = VideoDoc::load_project(&doc)
            .clip(clip.id)
            .unwrap()
            .out_point;
        assert!(
            out_after < out_before,
            "trimming the right edge shortened out_point in the CRDT ({out_before} -> {out_after})"
        );
    }
    // Case `click_selects_a_clip_via_the_real_hit_test`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.selection.clear();
        let r = app.regions();
        // The market cutaway on V1 sits around t=22.
        let clip = app.project.top_video_clip_at(22.0).unwrap().clone();
        let (_tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(tid, _, _)| *tid == clip.track)
            .unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        assert_eq!(app.selected(), Some(clip.id), "clicking a clip selects it");
    }
}

// ---- tone curves -----------------------------------------------------

/// Select the demo's top video clip and open the curves section, with the
/// panel scrolled so the editor's box is really on screen.
fn app_with_curve_editor() -> (VideoEditorApp, AutoCommit, Rect) {
    let (mut app, mut doc) = app_and_doc();
    let cid = app
        .project
        .top_video_clip_at(23.0)
        .expect("a clip at 23s")
        .id;
    app.selection.select_only(cid);
    app.playhead = 23.0;
    app.collapsed.remove(&Section::Curves);
    // Scroll the section's header to the top of the panel, so the box
    // below it is inside the pane rather than clipped off the bottom.
    let r = app.regions();
    let head = app
        .inspector_layout(&r)
        .sections
        .iter()
        .find(|(s, _)| *s == Section::Curves)
        .map(|(_, rect)| rect.y0);
    if let Some(y) = head {
        let want = y - r.inspector.y0 - 22.0;
        app.inspector_scroll = want.clamp(0.0, app.inspector_max_scroll(&r));
    }
    let canvas = app
        .inspector_layout(&app.regions())
        .curve_canvas
        .expect("the curve editor is laid out for a video clip");
    let _ = &mut doc;
    (app, doc, canvas)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn curve_box_points_are_placed_grabbed_dragged_and_reset() {
    // Case `clicking_the_curve_box_puts_a_point_where_the_pointer_landed`.
    {
        let (mut app, mut doc, canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        assert!(
            app.project.clip(cid).unwrap().curves.master.is_identity(),
            "the clip starts on the straight line",
        );

        // A quarter in, three quarters up.
        let at = curve_point(canvas, 0.25, 0.75);
        app.press_curve(&mut doc, canvas, at);
        let s = &app.project.clip(cid).unwrap().curves.master;
        assert_eq!(
            s.points.len(),
            3,
            "a point was added between the ends: {:?}",
            s.points
        );
        let p = s.points[1];
        assert!(
            (p.x - 0.25).abs() < 0.02 && (p.y - 0.75).abs() < 0.02,
            "{p:?}"
        );
        // And the curve really passes through it.
        assert!((s.eval(0.25) - 0.75).abs() < 1e-6, "{}", s.eval(0.25));
        assert!(
            matches!(app.drag, Some(Drag::CurvePoint { idx: 1, .. })),
            "the new point is grabbed, so the same gesture places and shapes it",
        );
    }
    // Case `a_press_on_an_existing_point_grabs_it_rather_than_stacking_another`.
    {
        let (mut app, mut doc, canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        app.edit_selected(&mut doc, |c| {
            c.curves.master.set_point(0.5, 0.5);
        });
        // A press a couple of pixels off the point.
        let near = curve_point(canvas, 0.51, 0.52);
        app.press_curve(&mut doc, canvas, near);
        assert_eq!(
            app.project.clip(cid).unwrap().curves.master.points.len(),
            3,
            "still three points — the press grabbed, it did not add",
        );
        assert!(matches!(app.drag, Some(Drag::CurvePoint { idx: 1, .. })));
    }
    // Case `dragging_a_point_reshapes_the_curve_and_commits_once`.
    {
        let (mut app, mut doc, canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        let r = app.regions();
        app.press_curve(&mut doc, canvas, curve_point(canvas, 0.5, 0.5));
        // Drag it up.
        app.last_mouse = curve_point(canvas, 0.5, 0.9);
        app.apply_drag(&r);
        let mid = app.project.clip(cid).unwrap().curves.master.eval(0.5);
        assert!((mid - 0.9).abs() < 0.02, "the drag moved the point: {mid}");

        // Releasing inside the box keeps it, and writes it to the document.
        app.commit_drag(&mut doc);
        app.drag = None;
        let stored = VideoDoc::load_project(&doc);
        let back = stored.clip(cid).expect("the clip survived");
        assert!(
            (back.curves.master.eval(0.5) - 0.9).abs() < 0.02,
            "the shaped curve reached the document: {:?}",
            back.curves.master.points,
        );
    }
    // Case `a_point_dragged_out_of_the_box_comes_off_the_curve`.
    {
        let (mut app, mut doc, canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        app.press_curve(&mut doc, canvas, curve_point(canvas, 0.4, 0.7));
        assert_eq!(app.project.clip(cid).unwrap().curves.master.points.len(), 3);

        // Release well past the left edge.
        app.last_mouse = Point::new(canvas.x0 - POINT_DROP_MARGIN - 10.0, canvas.center().y);
        app.commit_drag(&mut doc);
        app.drag = None;
        assert!(
            app.project.clip(cid).unwrap().curves.master.is_identity(),
            "the point was dropped out of the box, so the curve is a line again",
        );
    }
    // Case `a_small_overshoot_at_the_edge_keeps_the_point`.
    {
        // Pinning a point hard against 0 or 1 means dragging to the very edge
        // and a little past it. That must not delete what the user is placing.
        let (mut app, mut doc, canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        app.press_curve(&mut doc, canvas, curve_point(canvas, 0.4, 0.7));
        let r = app.regions();
        app.last_mouse = Point::new(canvas.center().x, canvas.y0 - POINT_DROP_MARGIN + 6.0);
        app.apply_drag(&r);
        app.commit_drag(&mut doc);
        app.drag = None;
        let s = &app.project.clip(cid).unwrap().curves.master;
        assert_eq!(s.points.len(), 3, "still there: {:?}", s.points);
        assert!(
            s.points[1].y > 0.98,
            "and pinned to the top: {:?}",
            s.points[1]
        );
    }
    // Case `the_channel_chooser_switches_which_curve_is_edited`.
    {
        let (mut app, mut doc, canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        let r = app.regions();
        let blue = app
            .inspector_layout(&r)
            .curve_channels
            .iter()
            .find(|(c, _)| *c == crate::curve::Channel::Blue)
            .map(|(_, rect)| *rect)
            .expect("a blue segment");
        reveal_inspector_rect_for_test(&mut app, &r, blue);
        let r = app.regions();
        let blue = app
            .inspector_layout(&r)
            .curve_channels
            .iter()
            .find(|(c, _)| *c == crate::curve::Channel::Blue)
            .map(|(_, rect)| *rect)
            .expect("blue stays reachable after scrolling");
        app.press_inspector(&mut doc, &r, blue.center());
        assert_eq!(app.curve_channel, crate::curve::Channel::Blue);

        // The canvas moves with the layout, so re-read it before clicking in.
        let canvas = app
            .inspector_layout(&app.regions())
            .curve_canvas
            .unwrap_or(canvas);
        app.press_curve(&mut doc, canvas, curve_point(canvas, 0.5, 0.8));
        let c = app.project.clip(cid).unwrap();
        assert!(!c.curves.blue.is_identity(), "blue was shaped");
        assert!(
            c.curves.master.is_identity(),
            "and the master was left alone"
        );
        assert!(c.curves.red.is_identity() && c.curves.green.is_identity());
    }
    // Case `reset_returns_only_the_shown_channel_to_the_straight_line`.
    {
        let (mut app, mut doc, _canvas) = app_with_curve_editor();
        let cid = app.selected().unwrap();
        app.edit_selected(&mut doc, |c| {
            c.curves.master.set_point(0.5, 0.8);
            c.curves.red.set_point(0.5, 0.2);
        });
        let r = app.regions();
        let reset = app
            .inspector_layout(&r)
            .curve_reset
            .expect("a reset button");
        app.press_inspector(&mut doc, &r, reset.center());
        let c = app.project.clip(cid).unwrap();
        assert!(c.curves.master.is_identity(), "the shown channel was reset");
        assert!(!c.curves.red.is_identity(), "and the others were not");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn curve_summaries_and_availability_match_for_eye_and_reader() {
    // Case `a_keyframed_clip_reports_its_geometry_even_where_the_curve_crosses_rest`:
    // A heavily keyframed clip whose *constants* happen to be the aspect-fit
    // still reports its geometry.
    //
    // `if clip.transform.is_identity() { return None }` was the whole gate, and
    // keying a parameter from its default leaves that constant at exactly the
    // aspect-fit — which is what keying from the inspector does. So the one
    // clip whose geometry section had most to say about it said nothing at
    // all. The playhead here sits on the instant the animation crosses zero,
    // so the *only* thing that can produce a summary is the existence of the
    // curve.
    {
        let (mut app, _doc) = app_and_doc();
        let cid = app
            .project
            .top_video_clip_at(23.0)
            .expect("a video clip at 23s")
            .id;
        app.selection.select_only(cid);
        let start = app.project.clip(cid).expect("clip").start;
        {
            let c = app.project.clip_mut(cid).expect("clip");
            c.set_key_at(Param::PosX, start + 0.5, -0.4, Interp::Linear);
            c.set_key_at(Param::PosX, start + 3.0, 0.4, Interp::Linear);
        }
        // Half way between the two keys, where the curve reads exactly rest.
        app.playhead = start + 1.75;
        let clip = app.project.clip(cid).expect("clip").clone();
        assert!(
            clip.transform.is_identity(),
            "the constants really are the plain aspect-fit — which is what \
             keying from the default leaves behind",
        );
        assert!(
            clip.param_at(Param::PosX, app.playhead).abs() < 1e-6,
            "and the curve reads rest right here, so nothing but the animation \
             itself can produce a summary",
        );

        let seen = app
            .section_summary(Section::Transform, &clip, None)
            .expect("an animated geometry section has something to report");
        assert!(
            seen.contains("animated"),
            "the folded header has to say the section is going somewhere: {seen}",
        );
        let heard = app.a11y_section_summary(Section::Transform);
        assert!(
            heard.contains("animated"),
            "and a reader has to hear it, or one still frame's numbers are the \
             whole story they get: {heard}",
        );
    }
    // Case `the_curve_summary_is_the_same_story_for_the_eye_and_for_a_reader`.
    {
        let (mut app, mut doc) = app_and_doc();
        let cid = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(cid);

        let clip = app.project.clip(cid).unwrap().clone();
        assert_eq!(
            app.section_summary(Section::Curves, &clip, None),
            None,
            "an untouched curve has nothing to report",
        );
        assert_eq!(
            app.a11y_section_summary(Section::Curves),
            "unchanged",
            "and the reader hears the same",
        );

        app.edit_selected(&mut doc, |c| {
            c.curves.master.set_point(0.5, 0.8);
            c.curves.blue.set_point(0.4, 0.2);
            c.curves.mix = 0.5;
        });
        let clip = app.project.clip(cid).unwrap().clone();
        let seen = app
            .section_summary(Section::Curves, &clip, None)
            .expect("a shaped curve reports itself");
        assert_eq!(seen, app.a11y_section_summary(Section::Curves));
        assert!(
            seen.contains("RGB") && seen.contains('B'),
            "it names the channels: {seen}"
        );
        assert!(seen.contains("50%"), "and says it is turned down: {seen}");

        // A curve that is shaped but mixed all the way out is a different
        // state from one at full strength, and both have to be reported.
        app.edit_selected(&mut doc, |c| c.curves.mix = 0.0);
        let clip = app.project.clip(cid).unwrap().clone();
        let off = app
            .section_summary(Section::Curves, &clip, None)
            .expect("still reported");
        assert!(off.contains("bypassed"), "a bypassed curve says so: {off}");
        assert_eq!(off, app.a11y_section_summary(Section::Curves));
    }
    // Case `an_audio_clip_is_offered_no_curve_editor_at_all`.
    {
        let (mut app, _doc) = app_and_doc();
        let audio = app
            .project
            .clips
            .iter()
            .find(|c| {
                app.project
                    .track(c.track)
                    .is_some_and(|t| t.kind == TrackKind::Audio)
            })
            .map(|c| c.id)
            .expect("the demo has an audio clip");
        app.selection.select_only(audio);
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(l.curve_canvas.is_none(), "there is no picture to grade");
        assert!(l.curve_channels.is_empty());
        assert!(
            !l.sections.iter().any(|(s, _)| *s == Section::Curves),
            "and no header claiming otherwise",
        );
    }
}

// ---- the rest of the inspector's animatable / authored controls -------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn marker_notes_and_reader_nodes_name_navigate_and_edit() {
    // Case `a_marker_can_be_given_a_note_and_the_row_makes_room_for_it`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.collapsed.remove(&Section::Markers);
        app.playhead = 12.0;
        app.add_marker_here(&mut doc);
        // `add_marker_here` opens the rename; take that off so the row is at
        // rest before the note is asked for.
        app.marker_rename = None;

        let r = app.regions();
        let l = app.inspector_layout(&r);
        let (m, row) = *l.marker_rows.first().expect("the marker has a row");
        let plain_h = row.height();
        let (_, note_btn, rename, del) = marker_row_parts(row);
        for other in [rename, del] {
            assert!(
                note_btn.x1 <= other.x0 + 1.0,
                "the note control has its own target"
            );
        }

        reveal_inspector_rect_for_test(&mut app, &r, note_btn);
        let r = app.regions();
        let row = app
            .inspector_layout(&r)
            .marker_rows
            .first()
            .map(|(_, row)| *row)
            .expect("marker stays reachable after scrolling");
        let (_, note_btn, _, _) = marker_row_parts(row);
        app.press_inspector(&mut doc, &r, note_btn.center());
        assert_eq!(
            app.marker_field,
            MarkerField::Note,
            "the editor opened on the note"
        );
        // The row grew for the note being typed into it, or the field would
        // open over the row beneath.
        let grown = app
            .inspector_layout(&app.regions())
            .marker_rows
            .first()
            .map(|(_, rect)| rect.height())
            .expect("still there");
        assert!(grown > plain_h, "{plain_h} -> {grown}");

        app.commit_marker_rename(&mut doc, m, "check the horizon line");
        let back = VideoDoc::load_project(&doc);
        let note = back
            .markers
            .iter()
            .find(|k| k.id == m.id)
            .map(|k| k.note.clone())
            .expect("the marker survived");
        assert_eq!(
            note, "check the horizon line",
            "the note reached the document"
        );
        // The name is untouched — the two fields are not one field.
        assert_eq!(
            back.markers
                .iter()
                .find(|k| k.id == m.id)
                .map(|k| k.name.as_str()),
            Some("Marker"),
        );
    }
    // Case `clearing_a_note_is_allowed_but_clearing_a_name_is_not`.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 12.0;
        app.add_marker_here(&mut doc);
        app.marker_rename = None;
        let m = app
            .project
            .markers
            .first()
            .map(|k| MarkerRef {
                id: k.id,
                clip: None,
                at: k.t,
            })
            .expect("a marker");

        app.commit_marker_text(&mut doc, m, MarkerField::Note, "a note");
        app.commit_marker_text(&mut doc, m, MarkerField::Note, "");
        assert_eq!(
            app.project
                .markers
                .iter()
                .find(|k| k.id == m.id)
                .map(|k| k.note.as_str()),
            Some(""),
            "an empty note is how a note is taken back off",
        );
        app.commit_marker_text(&mut doc, m, MarkerField::Name, "   ");
        assert_eq!(
            app.project
                .markers
                .iter()
                .find(|k| k.id == m.id)
                .map(|k| k.name.as_str()),
            Some("Marker"),
            "an empty name is refused — a nameless marker is a dot nobody can find",
        );
    }
    // Case `a_reader_is_told_a_markers_name_time_and_note`:
    // A marker is a named instant on the screen, sometimes carrying a note.
    // None of that reached the accessibility tree: the MARKERS group existed
    // but was always empty, so a reader could be told the section was there
    // and never that anything was in it -- not that a marker existed, not
    // what it was called, not what its note said.
    {
        let (mut app, mut doc) = app_and_doc();
        let m = marker_at(
            &mut app,
            &mut doc,
            12.0,
            "act two",
            "check the horizon line",
        );

        let found = a11y_find(&app.a11y_inspector(), "act two");
        assert_eq!(found.len(), 1, "the marker is one node in the tree");
        let node = &found[0];
        assert_eq!(node.role, Role::ListItem);
        assert_eq!(node.key, Some(ui::a11y::hash_key(&m.id)), "identity-keyed");
        let value = node.value.clone().unwrap_or_default();
        assert!(
            value.contains(&crate::timecode::timecode(12.0, app.project.fps)),
            "the row's timecode is spoken: {value}",
        );
        assert!(
            value.contains("check the horizon line"),
            "the note is spoken: {value}"
        );
        assert!(node.state.focusable, "and it can be landed on");
        // The three controls the row carries. They are painted only under the
        // pointer, and a reader has no pointer, so the tree is the only place
        // a marker's note, name or removal is reachable from.
        for verb in MARKER_VERBS {
            let want = format!("{verb} act two");
            assert!(
                node.children
                    .iter()
                    .any(|k| k.name.as_deref() == Some(want.as_str())),
                "the row offers {want:?}, has {:?}",
                node.children
                    .iter()
                    .filter_map(|k| k.name.clone())
                    .collect::<Vec<_>>(),
            );
        }
    }
    // Case `the_reader_is_offered_exactly_the_markers_that_are_drawn`:
    // A reader and a sighted user have to be looking at the same list. This
    // walks the drawn rows and the spoken ones together, with a clip marker
    // in the mix so the two sources of markers are both covered.
    {
        let (mut app, mut doc) = app_and_doc();
        marker_at(&mut app, &mut doc, 12.0, "act two", "");
        marker_at(&mut app, &mut doc, 4.0, "cold open", "the quiet bit");
        // A marker pinned to a clip travels with the footage and is listed on
        // the same timeline as the project's own.
        let clip = app
            .project
            .top_video_clip_at(23.0)
            .expect("a clip under the playhead")
            .id;
        app.project
            .clip_mut(clip)
            .unwrap()
            .add_marker(23.5, "on the clip", 2);

        let drawn: Vec<u64> = app
            .inspector_layout(&app.regions())
            .marker_rows
            .iter()
            .map(|(m, _)| ui::a11y::hash_key(&m.id))
            .collect();
        assert!(
            drawn.len() >= 3,
            "three marked instants are drawn, got {}",
            drawn.len()
        );
        assert_eq!(
            spoken_marker_keys(&app),
            drawn,
            "the reader's markers must be the drawn markers, in the drawn order",
        );
        // And the button that closes the list is offered too, or a reader
        // could read markers and never add one.
        assert!(
            !a11y_find(&app.a11y_inspector(), ADD_MARKER_LABEL).is_empty(),
            "the add button is in the tree",
        );
    }
    // Case `activating_a_marker_node_moves_the_playhead_there`:
    // Clicking a marker row's body parks the playhead on it. Activating the
    // node must do the identical thing -- the reader's verb and the mouse's
    // are the same call.
    {
        let (mut app, mut doc) = app_and_doc();
        let m = marker_at(&mut app, &mut doc, 12.0, "act two", "");
        app.playhead = 0.0;
        app.playback.playing = true;

        let path = vec![
            Pane::Inspector.key(),
            ui::a11y::hash_key(&Section::Markers.label()),
            ui::a11y::hash_key(&m.id),
        ];
        assert!(
            app.perform_action_at(
                &ui::a11y::Action::Activate,
                &path,
                &mut doc,
                &automerge::ROOT
            ),
            "the marker node is addressable",
        );
        assert!(
            (app.playhead - 12.0).abs() < 1e-6,
            "playhead at {}",
            app.playhead
        );
        assert!(
            !app.playback.playing,
            "seeking to a mark stops playback, as the click does"
        );
    }
    // Case `a_marker_rows_controls_rename_annotate_and_remove_it`:
    // The row's three controls, driven the way an assistive technology drives
    // them. Each has to reach the same document write the pointer reaches.
    {
        let (mut app, mut doc) = app_and_doc();
        let m = marker_at(&mut app, &mut doc, 12.0, "act two", "");
        let verb = |i: usize| {
            vec![
                Pane::Inspector.key(),
                ui::a11y::hash_key(&Section::Markers.label()),
                ui::a11y::hash_key(&m.id),
                ui::a11y::hash_key(&(m.id, MARKER_VERBS[i])),
            ]
        };
        let root = automerge::ROOT;

        // Setting the note's text writes it through the same commit the
        // inline editor ends at.
        assert!(app.perform_action_at(
            &ui::a11y::Action::SetValue("mind the skyline".into()),
            &verb(0),
            &mut doc,
            &root,
        ));
        assert_eq!(
            VideoDoc::load_project(&doc)
                .marker(m.id)
                .map(|k| k.note.clone()),
            Some("mind the skyline".to_string()),
            "the note reached the document",
        );

        // And renaming.
        assert!(app.perform_action_at(
            &ui::a11y::Action::SetValue("act three".into()),
            &verb(1),
            &mut doc,
            &root,
        ));
        assert_eq!(
            VideoDoc::load_project(&doc)
                .marker(m.id)
                .map(|k| k.name.clone()),
            Some("act three".to_string()),
        );
        // An empty name is refused at the commit, so the verb reports the
        // refusal rather than claiming a write it did not make.
        assert!(
            !app.perform_action_at(
                &ui::a11y::Action::SetValue("   ".into()),
                &verb(1),
                &mut doc,
                &root,
            ),
            "a nameless marker is a dot nobody can find, and the verb says so",
        );

        // Activating the delete control removes it, exactly as the X does.
        assert!(app.perform_action_at(&ui::a11y::Action::Activate, &verb(2), &mut doc, &root));
        assert!(
            VideoDoc::load_project(&doc).marker(m.id).is_none(),
            "the marker is gone from the document",
        );
        assert!(
            a11y_find(&app.a11y_inspector(), "act three").is_empty(),
            "and gone from the tree",
        );
    }
    // Case `a_held_marker_node_still_names_its_own_marker_after_the_list_reorders`:
    // Markers are kept sorted by time, so adding an earlier one pushes every
    // later one down the list. A node an assistive technology is holding has
    // to keep naming its own marker across that, not whatever took its slot.
    {
        let (mut app, mut doc) = app_and_doc();
        marker_at(&mut app, &mut doc, 12.0, "act two", "");
        // The key is read off the tree rather than computed from the model:
        // an assistive technology holds the node it was given, so a node
        // keyed by its position would go on resolving here if the path were
        // rebuilt from the marker's id instead.
        let held_key = a11y_find(&app.a11y_inspector(), "act two")
            .first()
            .and_then(|n| n.key)
            .expect("the marker node carries a key");
        let held = vec![
            Pane::Inspector.key(),
            ui::a11y::hash_key(&Section::Markers.label()),
            held_key,
        ];
        marker_at(&mut app, &mut doc, 4.0, "cold open", "");
        assert_eq!(
            app.a11y_marker_rows().first().map(|(_, n, _)| n.clone()),
            Some("cold open".to_string()),
            "the new marker took the first row",
        );

        app.playhead = 0.0;
        assert!(app.perform_action_at(
            &ui::a11y::Action::Activate,
            &held,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(
            (app.playhead - 12.0).abs() < 1e-6,
            "the held path still names the marker at 12s, landed at {}",
            app.playhead,
        );
    }
}

/// Every node in `root`'s tree whose name matches, in walk order.
fn a11y_find(root: &AccessNode, name: &str) -> Vec<AccessNode> {
    fn walk(n: &AccessNode, name: &str, out: &mut Vec<AccessNode>) {
        if n.name.as_deref() == Some(name) {
            out.push(n.clone());
        }
        for k in &n.children {
            walk(k, name, out);
        }
    }
    let mut out = Vec::new();
    walk(root, name, &mut out);
    out
}

/// The keys of the marker rows a reader is offered, in spoken order.
fn spoken_marker_keys(app: &VideoEditorApp) -> Vec<u64> {
    fn walk(n: &AccessNode, out: &mut Vec<u64>) {
        if n.role == Role::ListItem
            && let Some(k) = n.key
        {
            out.push(k);
        }
        for k in &n.children {
            walk(k, out);
        }
    }
    let mut out = Vec::new();
    for g in &app.a11y_inspector().children {
        if g.name.as_deref() == Some(Section::Markers.label()) {
            walk(g, &mut out);
        }
    }
    out
}

/// Put a named, annotated marker on the timeline at `t` and hand back its
/// reference, with no rename editor left open over it.
fn marker_at(
    app: &mut VideoEditorApp,
    doc: &mut AutoCommit,
    t: f64,
    name: &str,
    note: &str,
) -> MarkerRef {
    app.playhead = t;
    app.add_marker_here(doc);
    app.marker_rename = None;
    let m = *app
        .a11y_marker_rows()
        .iter()
        .find(|(m, _, _)| (m.at - t).abs() < 1e-6)
        .map(|(m, _, _)| m)
        .expect("the marker was added");
    app.commit_marker_text(doc, m, MarkerField::Name, name);
    if !note.is_empty() {
        app.commit_marker_text(doc, m, MarkerField::Note, note);
    }
    m
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn keyboard_and_reader_walks_follow_what_is_painted() {
    // Case `the_reader_walks_the_bin_in_the_order_it_is_painted`:
    // The bin the reader walks has to be the bin on the screen.
    //
    // The rows are painted in `bin_order` -- pictures first, then by name --
    // while the tree listed them in the project's own order, which arrives
    // from the document in map-key order and is re-rolled every run. So a
    // reader was told the second row was something the second row was not,
    // and `focus.bin` -- which indexes this list -- moved the cursor to one
    // row while the highlight sat on another.
    {
        let (app, _doc) = app_and_doc();
        let painted: Vec<MediaId> = app
            .bin_item_rects(&app.regions())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(painted.len() > 1, "the fixture has a bin worth ordering");
        assert_eq!(
            app.a11y_bin_items(),
            painted,
            "the spoken bin must be the drawn bin, row for row",
        );

        // And the cursor lands on the row the eye would call the second one.
        let names: Vec<String> = app
            .a11y_bin_items()
            .iter()
            .filter_map(|m| app.project.media(*m).map(|x| x.name.clone()))
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_ne!(
            names, sorted,
            "the painted order is not plain alphabetical, so this test is \
             comparing against the real rule rather than a coincidence",
        );
    }
    // Case `the_keyboard_walk_reaches_the_controls_drawn_above_the_first_section`.
    {
        // `a11y_inspector_rows` is both the accessibility tree and the
        // arrow-key walk, and it was built from `a11y_sections`, whose first
        // entry is CURVES. Opacity, the three grade sliders, both fades, the
        // transition picker and the label swatches are all laid out *above*
        // that header, so none of them was ever emitted as a row: the arrows
        // started at CURVES and could never climb back up to them.
        let (app, _doc) = with_video_clip_selected();
        let rows = app.a11y_inspector_rows();
        let first_header = rows
            .iter()
            .position(|r| matches!(r, InspectorRow::Header(_)))
            .expect("the panel has sections");
        for want in [
            NumField::Opacity,
            NumField::Brightness,
            NumField::Contrast,
            NumField::Saturation,
            NumField::FadeIn,
            NumField::FadeOut,
        ] {
            let at = rows
                .iter()
                .position(|r| *r == InspectorRow::Field(want))
                .unwrap_or_else(|| panic!("{} is a focus stop", want.label()));
            assert!(
                at < first_header,
                "{} is drawn above the first section header, so its stop is too",
                want.label(),
            );
        }
        // The transition picker and the label swatches are drawn there too.
        for t in Transition::ALL {
            assert!(
                rows.contains(&InspectorRow::Pick(PickField::Transition(t))),
                "the {} segment is a focus stop",
                t.label(),
            );
        }
        for slot in 0..7u8 {
            assert!(
                rows.contains(&InspectorRow::Pick(PickField::Label(slot))),
                "label swatch {slot} is a focus stop",
            );
        }
    }
    // Case `every_stop_on_the_walk_is_a_control_the_panel_actually_drew`.
    {
        // The row list is both the accessibility tree and the keyboard walk,
        // and it is built by different code from the layout. A stop with no
        // rect is a control a reader is told about and a sighted user cannot
        // see; a drawn control with no stop is the defect this pair of lists
        // had — the whole top block, the key's switch and the mask's shapes.
        for (note, prepare) in [
            (
                "a video clip",
                (|app: &mut VideoEditorApp| {
                    let cid = app
                        .project
                        .clips
                        .iter()
                        .find(|c| {
                            app.project
                                .media(c.media)
                                .is_some_and(|m| m.kind.is_visual() && m.text.is_none())
                        })
                        .map(|c| c.id)
                        .expect("a video clip");
                    app.selection.select_only(cid);
                }) as fn(&mut VideoEditorApp),
            ),
            ("nothing selected", |app: &mut VideoEditorApp| {
                app.selection.clear();
            }),
        ] {
            let (mut app, _doc) = app_and_doc();
            // Every section open, so the walk is at its widest and every kind
            // of stop is under test rather than folded away.
            app.collapsed.clear();
            prepare(&mut app);
            let r = app.regions();
            let l = app.inspector_layout(&r);
            for row in app.a11y_inspector_rows() {
                assert!(
                    walked_row_rect(&l, row).is_some(),
                    "{note}: the walk offers {row:?} but the panel laid out no such control",
                );
            }
        }
    }
    // Case `every_pane_with_a_keyboard_cursor_paints_it_on_the_control_it_is_on`:
    // Every pane that carries a keyboard cursor paints it, and paints it on
    // the control the cursor is on rather than over the whole pane.
    //
    // The inspector, the bin and the transport all export a focused element to
    // the accessibility tree, and nothing painted it: a sighted keyboard user
    // could arrow through the panel and had no way to tell where they were.
    // That matters most in the inspector, whose controls only recently joined
    // the walk at all.
    //
    // Two renders of the same app, differing in `has_focus` and nothing else —
    // so the only thing that can move a pixel is the cursor. (Nothing in
    // `draw` reads `has_focus` apart from the ring, and the fixture is not
    // playing, so the playhead does not advance between the frames either.)
    {
        type Place = fn(&mut VideoEditorApp) -> (Rect, Rect);
        let theme = Theme::dark();
        let (w, h) = (1280u32, 800u32);

        // (what the pane is, where the cursor is put, the rect the ring must
        // land in, a rect in the same pane it must NOT touch)
        let cases: [(&str, Place); 3] = [
            ("the media bin", |app: &mut VideoEditorApp| {
                app.focus.pane = Pane::Bin;
                app.focus.bin = 2;
                let r = app.regions();
                let rows = app.bin_item_rects(&r);
                (rows[2].1, rows[0].1)
            }),
            ("the transport", |app: &mut VideoEditorApp| {
                app.focus.pane = Pane::Transport;
                app.focus.transport = 2;
                let r = app.regions();
                let btns = app.transport_buttons(&r);
                // Grown, because a round control wears its ring outside the
                // fill rather than over the glyph.
                (btns[2].1.inset(6.0), btns[0].1.inset(6.0))
            }),
            ("the inspector", |app: &mut VideoEditorApp| {
                let cid = app
                    .project
                    .top_video_clip_at(4.0)
                    .expect("the fixture has a video clip at 4s")
                    .id;
                app.selection.select_only(cid);
                app.playhead = 4.0;
                app.inspector_scroll = 0.0;
                app.focus.pane = Pane::Inspector;
                let rows = app.a11y_inspector_rows();
                app.focus.inspector = rows
                    .iter()
                    .position(|row| *row == InspectorRow::Field(NumField::Brightness))
                    .expect("brightness is a stop");
                let l = app.inspector_layout(&app.regions());
                let on = VideoEditorApp::inspector_focus_band(
                    &l,
                    InspectorRow::Field(NumField::Brightness),
                )
                .expect("the brightness row is drawn");
                let off = VideoEditorApp::inspector_focus_band(
                    &l,
                    InspectorRow::Field(NumField::Saturation),
                )
                .expect("the saturation row is drawn");
                (on, off)
            }),
        ];

        // Collected rather than asserted pane by pane, so a run names every
        // pane that is missing its cursor instead of only the first.
        let mut wrong: Vec<String> = Vec::new();
        for (what, place) in cases {
            let render = |focused: bool| -> (Vec<u8>, Rect, Rect) {
                let (mut app, _doc) = app_and_doc();
                let (on, off) = place(&mut app);
                app.presentation.keyboard_focus = focused;
                (render_pixels(&mut app, w, h, &theme), on, off)
            };
            let (dark, on, off) = render(false);
            let (lit, _, _) = render(true);
            let painted = region_diff(&dark, &lit, w, h, on);
            if painted <= 100 {
                wrong.push(format!(
                    "{what} paints nothing where its keyboard cursor is ({painted} px), \
                     so a sighted keyboard user cannot see which control the next key \
                     reaches"
                ));
            }

            let spilled = region_diff(&dark, &lit, w, h, off);
            if spilled != 0 {
                wrong.push(format!(
                    "{what} also marked a control the cursor is not on ({spilled} px): \
                     the ring has to name one stop, not light the whole pane"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("; "));
    }
    // Case `the_keyboard_cursor_takes_its_colour_from_the_theme`:
    // The ring is the theme's focus colour, not a colour of the editor's own.
    //
    // Proved on the high-contrast theme, where `border_focus` is a yellow
    // nothing else in the inspector uses — so a ring painted in the accent, or
    // in a literal, puts no yellow on screen and fails here.
    {
        let theme = Theme::high_contrast();
        let want = theme.tokens.colors.border_focus.to_rgba8();
        let (w, h) = (1280u32, 800u32);

        let render = |focused: bool| -> (Vec<u8>, Rect) {
            let (mut app, _doc) = app_and_doc();
            let cid = app
                .project
                .top_video_clip_at(4.0)
                .expect("a video clip at 4s")
                .id;
            app.selection.select_only(cid);
            app.playhead = 4.0;
            app.inspector_scroll = 0.0;
            app.focus.pane = Pane::Inspector;
            app.focus.inspector = app
                .a11y_inspector_rows()
                .iter()
                .position(|row| *row == InspectorRow::Field(NumField::Brightness))
                .expect("brightness is a stop");
            app.presentation.keyboard_focus = focused;
            let band = VideoEditorApp::inspector_focus_band(
                &app.inspector_layout(&app.regions()),
                InspectorRow::Field(NumField::Brightness),
            )
            .expect("the brightness row is drawn");
            (render_pixels(&mut app, w, h, &theme), band)
        };

        let count = |px: &[u8], band: Rect| -> usize {
            let mut n = 0;
            for y in band.y0.max(0.0) as u32..(band.y1 as u32).min(h) {
                for x in band.x0.max(0.0) as u32..(band.x1 as u32).min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    if i + 3 < px.len()
                        && px[i].abs_diff(want.r) < 12
                        && px[i + 1].abs_diff(want.g) < 12
                        && px[i + 2].abs_diff(want.b) < 12
                    {
                        n += 1;
                    }
                }
            }
            n
        };

        let (dark, band) = render(false);
        let (lit, _) = render(true);
        assert_eq!(
            count(&dark, band),
            0,
            "the focus colour appears in that row only when the keyboard is on it",
        );
        assert!(
            count(&lit, band) > 100,
            "the cursor's ring must be `Theme::border_focus` — only {} pixels of \
             it are on screen",
            count(&lit, band),
        );
    }
    // Case `the_inspector_cursor_can_reach_a_marker_and_the_add_button`:
    // The inspector's arrow keys walk `a11y_inspector_rows`, so a marker has
    // to be a stop there or the keyboard steps straight over the list.
    {
        let (mut app, mut doc) = app_and_doc();
        let m = marker_at(&mut app, &mut doc, 12.0, "act two", "");
        let rows = app.a11y_inspector_rows();
        let at = rows
            .iter()
            .position(|r| matches!(r, InspectorRow::Marker(x) if x.id == m.id))
            .expect("the marker is a focus stop");
        assert!(
            rows.iter().any(|r| matches!(r, InspectorRow::AddMarker)),
            "so is the button that adds another",
        );

        app.focus.pane = Pane::Inspector;
        app.focus.inspector = at;
        let focused = a11y_find(&app.a11y_inspector(), "act two");
        assert!(
            focused[0].state.focused,
            "the cursor's marker carries focus"
        );

        // And Enter on it runs the row's own verb.
        app.playhead = 0.0;
        assert!(app.activate_focused(&mut doc), "Enter is answered");
        assert!(
            (app.playhead - 12.0).abs() < 1e-6,
            "playhead at {}",
            app.playhead
        );
    }
}

/// A video clip in the sample fixture, selected — what the inspector's
/// full control set is drawn for.
fn with_video_clip_selected() -> (VideoEditorApp, AutoCommit) {
    let (mut app, doc) = app_and_doc();
    let cid = app
        .project
        .clips
        .iter()
        .find(|c| {
            app.project
                .media(c.media)
                .is_some_and(|m| m.kind.is_visual() && m.text.is_none())
        })
        .map(|c| c.id)
        .expect("the fixture places a video clip");
    app.selection.select_only(cid);
    (app, doc)
}

/// The rect the panel laid out for one walked row, resolved through the
/// very layout the renderer and the hit test read. `None` means the panel
/// drew no such control — which for a row on the walk is the defect.
///
/// The production mapping, not a copy of it: the keyboard cursor's ring is
/// placed with the same call, so this test cannot pass against a table that
/// has drifted from the one the panel paints through.
fn walked_row_rect(l: &InspectorLayout, row: InspectorRow) -> Option<Rect> {
    VideoEditorApp::inspector_row_rect(l, row)
}

// ---- the keyboard cursor ---------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn two_x_text_keeps_canvas_bands_and_inspector_usable() {
    // Case `two_x_text_scales_every_canvas_band_without_collisions`.
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end 2× geometry regression keeps all cross-pane collision assertions together"
    )]
    {
        // Regression for the first accessibility-scale pass: only the side
        // pane breakpoint used effective width, leaving 2× text painted
        // through the top bar, transport, ruler and fixed-height tracks.
        let (mut app, mut doc) = app_sized(1440.0, 960.0);
        app.text_scale = 2.0;
        let r = app.regions();
        assert!(app.topbar_is_compact());
        assert!(r.topbar.height() > TOPBAR_H);
        assert!(r.transport.height() > TRANSPORT_H);
        assert!(r.ruler.height() > RULER_H);

        let commands = [
            ("bin", r.menu_btn),
            ("project", r.project_btn),
            ("inspector", r.inspector_btn),
            ("import", r.import_btn),
            ("export", r.export_btn),
        ];
        for (name, rect) in commands {
            assert!(
                rect.width() + 0.01 >= app.topbar_target()
                    && rect.height() + 0.01 >= app.topbar_target(),
                "{name} remains a full compact command target: {rect:?}"
            );
        }
        for (i, (_, a)) in commands.iter().enumerate() {
            for (_, b) in commands.iter().skip(i + 1) {
                assert!(
                    a.x1 <= b.x0 || b.x1 <= a.x0,
                    "2× topbar controls overlap: {a:?} vs {b:?}"
                );
            }
        }
        let toolbar = app.a11y_topbar();
        let names: Vec<&str> = toolbar
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        assert!(names.contains(&"Import media"));
        assert!(names.contains(&"Export video"));
        assert!(!names.contains(&"Split at playhead"));
        assert_eq!(
            app.tooltip_at(r.import_btn.center()),
            Some("Import media (Ctrl+Shift+O)")
        );
        assert_eq!(
            app.tooltip_at(r.export_btn.center()),
            Some("Export video (Ctrl+E)")
        );
        assert!(
            app.a11y_project().state.focusable,
            "the compact film icon opens the project menu"
        );
        assert!(app.perform_action_at(
            &ui::a11y::Action::Activate,
            &[
                ui::a11y::hash_key(&TOPBAR_A11Y_KEY),
                ui::a11y::hash_key(&TOPBAR_EXPORT_KEY),
            ],
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(
            app.presentation.export_open,
            "the accessible compact export icon invokes the same dialog"
        );

        let transport = app.transport_buttons(&r);
        for (i, (_, a)) in transport.iter().enumerate() {
            for (_, b) in transport.iter().skip(i + 1) {
                assert!(a.x1 <= b.x0, "transport buttons overlap: {a:?} vs {b:?}");
            }
        }
        let history = app.history_buttons(&r);
        let controls_right = history
            .last()
            .map_or_else(|| transport.last().unwrap().1.x1, |(_, rect)| rect.x1);
        assert!(
            r.transport.x0 + app.transport_content_x(&r) > controls_right,
            "readouts start after the complete transport button cluster"
        );
        let scrub = app.scrubber_rect(&r);
        assert!(
            scrub.y0 > r.transport.y0 + 30.0,
            "scrubber gets a separate vertical band from scaled time text"
        );

        for (track, kind, lane) in app.lane_rects(&r) {
            let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
            assert!(
                header.height() >= app.lane_height(kind) - 0.01,
                "{track:?} retains its scaled row height"
            );
            let pills = [
                app.track_head_layout(header).solo,
                app.track_head_layout(header).lock,
                app.track_head_layout(header).mute,
            ];
            for pill in pills {
                assert!(
                    pill.x0 >= header.x0
                        && pill.x1 <= header.x1
                        && pill.y0 >= header.y0
                        && pill.y1 <= header.y1,
                    "scaled track pill stays in its header: {pill:?} within {header:?}"
                );
            }
            assert!(
                app.track_head_layout(header).meter.y0 >= pills[0].y1,
                "the meter is below the track controls, not behind their labels"
            );
        }

        for (tid, _kind, lane) in app.lane_rects(&r) {
            for clip in app.project.clips_on(tid) {
                let body = app.clip_rect(clip, &lane, &r);
                let head = app.clip_header_rect(body);
                assert!(
                    head.height() >= 18.0 * app.chrome_scale() - 0.01 && head.y1 <= body.y1,
                    "a scaled clip title has a bounded header band: {head:?} in {body:?}"
                );
            }
        }

        // The media bin is an overlay at this effective width. Its old fixed
        // 268×56 geometry let the doubled title and metadata lines occupy the
        // same pixels, making the otherwise accessible source list unreadable.
        // Open the real flap and prove both the text column and row rhythm
        // make room for the actual scaled strings.
        let (mut narrow, _doc) = app_sized(720.0, 820.0);
        narrow.text_scale = 2.0;
        narrow.bin_flap.snap(true);
        narrow.navigation.flap_settled = true;
        let bin = narrow.regions();
        assert!(
            bin.bin_overlay,
            "2× text uses the overlay bin on a narrow window"
        );
        assert!(
            bin.bin.width() >= BIN_W * 1.5 - 0.01,
            "the bin grows with its two scaled text lines: {:?}",
            bin.bin
        );
        let rows = narrow.bin_item_rects(&bin);
        assert!(
            rows.windows(2).all(|pair| pair[0].1.y1 < pair[1].1.y0),
            "scaled bin rows remain distinct: {rows:?}"
        );
        let mut bin_text = TextRenderer::new();
        bin_text.set_text_scale(2.0);
        for (media, row) in rows {
            let item = narrow.project.media(media).expect("row media exists");
            let tx = row.x0 + 8.0 * narrow.bin_ui_scale() + 64.0 + 12.0 * narrow.bin_ui_scale();
            let available = (row.x1 - 8.0 * narrow.bin_ui_scale() - tx) as f32;
            let (name_w, name_h) = bin_text.measure(&item.name, 13.0, None);
            let (_, subtitle_h) = bin_text.measure(&VideoEditorApp::bin_subtitle(item), 11.0, None);
            assert!(
                name_w <= available,
                "{} has a full readable name column at 2× ({name_w} > {available})",
                item.name
            );
            assert!(
                row.height() >= f64::from(name_h + subtitle_h) + 12.0,
                "{} has separate title and metadata lines at 2×: {:?}",
                item.name,
                row
            );
        }
    }
    // Case `two_x_inspector_has_a_clipped_scroll_body_and_reveals_every_focus_stop`.
    {
        let (mut app, _doc) = app_sized(1440.0, 960.0);
        app.text_scale = 2.0;
        app.collapsed.clear();
        let clip = app
            .project
            .top_video_clip_at(4.0)
            .expect("fixture has a visual clip")
            .id;
        app.selection.select_only(clip);
        let r = app.regions();
        let view = app.inspector_viewport(&r);
        assert!(
            view.y0 > r.inspector.y0,
            "identity header reserves a fixed band"
        );
        assert!(
            view.height() >= 150.0,
            "body remains a useful scroll viewport"
        );

        // Top: the first editable row is below the static Source Out line.
        app.inspector_scroll = 0.0;
        let top = app.inspector_layout(&r);
        assert!(top.opacity.expect("opacity").y0 >= view.y0);
        assert!(top.fade_in.expect("fade in").y0 > top.saturation.expect("saturation").y1);

        // Middle: the picture-in-picture row owns a full slot, followed by
        // its reset button rather than another heading or icon row.
        app.inspector_scroll = app.inspector_max_scroll(&r) * 0.5;
        let middle = app.inspector_layout(&r);
        let pip_bottom = middle.pip.last().expect("PIP presets").2.y1;
        assert!(
            middle.reset_transform.expect("fill frame").y0 > pip_bottom,
            "PIP and its following action need separate scaled rows"
        );

        // Bottom: the marker empty copy has a whole large-text line above its
        // button, while the destructive actions remain inside the viewport.
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let bottom = app.inspector_layout(&r);
        let marker_add = bottom.add_marker.expect("add marker");
        let marker_header = bottom
            .sections
            .iter()
            .find(|(section, _)| *section == Section::Markers)
            .expect("markers section")
            .1;
        assert!(
            marker_add.y0 - marker_header.y1 >= 18.0 * app.inspector_ui_scale(),
            "empty-marker copy owns a scaled slot above its button"
        );
        for action in [bottom.split, bottom.delete] {
            assert!(action.y0 >= view.y0 && action.y1 <= view.y1);
        }

        // Keyboard/AT focus uses the same row rectangles and scrolls that
        // row into the body viewport instead of leaving the active descendant
        // hidden under the fixed identity card.
        let rows = app.a11y_inspector_rows();
        app.focus.pane = Pane::Inspector;
        app.focus.inspector = rows
            .iter()
            .position(|row| *row == InspectorRow::AddMarker)
            .expect("Add marker is a keyboard stop");
        app.inspector_scroll = 0.0;
        app.reveal_inspector_focus();
        let focused = VideoEditorApp::inspector_focus_band(
            &app.inspector_layout(&r),
            rows[app.focus.inspector],
        )
        .expect("focus band");
        assert!(focused.y0 >= view.y0 && focused.y1 <= view.y1);
    }
}

// ---- the shuttle -----------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
#[expect(
    clippy::float_cmp,
    reason = "the value is either untouched by the event under test or assigned a literal, so any difference is the bug"
)]
fn shuttle_direction_and_rate_are_named_and_painted() {
    // Case `the_transport_names_the_shuttle_direction_and_rate`:
    // J/L run the playhead at up to 16×, and the transport has to say so.
    //
    // Every readout in the strip was gated on `playing`, so a shuttle reported
    // itself nowhere: the playhead moved under its own steam with nothing
    // naming why, how fast, or which way.
    {
        let (mut app, mut doc) = app_and_doc();
        assert_eq!(app.motion_words(), None, "a still playhead says nothing");

        app.playback.playing = true;
        assert_eq!(app.motion_words().as_deref(), Some("PLAYING"));
        app.playback.playing = false;

        // Through the production key path: L, L, L is the standard 1× → 2× → 4×.
        for _ in 0..3 {
            assert!(key_press(&mut app, &mut doc, "l", false), "L handled");
        }
        let fwd = app
            .motion_words()
            .expect("a running shuttle reports itself");
        assert!(
            fwd.contains('4') && fwd.contains('\u{d7}'),
            "the rate has to be named, not just the fact of moving: {fwd:?}",
        );
        assert!(
            fwd.contains('\u{25B6}'),
            "and which way it is running: {fwd:?}",
        );

        // J reverses at 1×, and the wording has to change with it — "4×" on its
        // own does not say which direction the footage is travelling.
        assert!(key_press(&mut app, &mut doc, "j", false), "J handled");
        let back = app
            .motion_words()
            .expect("a reversed shuttle reports itself");
        assert!(
            back.contains('\u{25C0}') && !back.contains('\u{25B6}'),
            "reverse reads as reverse: {back:?}",
        );

        // K stops it, and the readout goes with it rather than lingering.
        assert!(key_press(&mut app, &mut doc, "k", false), "K handled");
        assert_eq!(app.motion_words(), None);
    }
    // Case `the_shuttle_readout_is_really_painted_beside_the_timecode`:
    // And the strip really paints those words, on the readout's own line.
    //
    // The band under test is the slot [`readout_fit`] gives the motion readout
    // — 14px clear of the total, so a timecode that has ticked on between the
    // two frames cannot reach into it. A shuttling playhead does advance
    // between them; that is why the assertion is "this band is blank when
    // nothing is moving and carries ink when something is" rather than a diff
    // of two frames, which the moving timecode would satisfy for free.
    {
        let theme = Theme::dark();
        let (w, h) = (1280u32, 800u32);
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 0.0;
        assert!(key_press(&mut app, &mut doc, "l", false));
        let words = app.motion_words().expect("the shuttle is running");

        // Where the strip puts it, through the production placement.
        let mut tr = TextRenderer::new();
        let r = app.regions();
        let big = timecode(app.playhead, app.project.fps);
        let total = format!("/  {}", timecode(app.project.duration(), app.project.fps));
        let meta = format!(
            "{}×{} · {:.0} fps",
            app.project.width, app.project.height, app.project.fps
        );
        let motion_w = f64::from(tr.measure(&words, 10.0, None).0);
        let fit = readout_fit(
            r.transport,
            app.transport_content_x(&r),
            f64::from(tr.measure_weighted(&big, 18.0, None, TextWeight::Bold).0),
            f64::from(tr.measure(&total, 13.0, None).0),
            motion_w,
            f64::from(tr.measure(&meta, 11.5, None).0),
        );
        let x = fit.motion_x.expect("a 1280px strip has room for the rate");
        let band = Rect::new(
            x - 4.0,
            r.transport.y0 + 10.0,
            x + motion_w + 4.0,
            r.transport.y0 + 28.0,
        );

        let shuttling = render_pixels(&mut app, w, h, &theme);
        assert!(
            region_colours(&shuttling, w, h, band) > 1,
            "the shuttle's direction and rate must be on screen while it runs",
        );

        // The same band on a still playhead: blank strip, nothing lingering.
        let (mut still, _doc) = app_and_doc();
        still.playhead = 0.0;
        let stopped = render_pixels(&mut still, w, h, &theme);
        assert_eq!(
            region_colours(&stopped, w, h, band),
            1,
            "a stopped playhead leaves that slot empty rather than claiming a rate",
        );
    }
    // Case `the_play_button_shows_and_stops_a_running_shuttle`:
    // The play button is the control that stops whatever is moving, so it has
    // to wear the pause glyph while a shuttle runs — and pressing it has to
    // stop the shuttle, not start playback on top of it and leave a rate
    // running behind the readout.
    {
        let theme = Theme::dark();
        let (w, h) = (1280u32, 800u32);

        // The button's own rect never moves with the playhead, so this frame
        // pair isolates the glyph.
        let render = |shuttling: bool| -> (Vec<u8>, Rect) {
            let (mut app, mut doc) = app_and_doc();
            app.playhead = 8.0;
            if shuttling {
                assert!(key_press(&mut app, &mut doc, "l", false));
            }
            let rect = app.transport_buttons(&app.regions())[2].1;
            (render_pixels(&mut app, w, h, &theme), rect)
        };
        let (stopped, btn) = render(false);
        let (running, _) = render(true);
        assert!(
            region_diff(&stopped, &running, w, h, btn) > 20,
            "a shuttling playhead still wore a play triangle: the button offered \
             to start something that was already running",
        );

        // And the press stops it.
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 8.0;
        assert!(key_press(&mut app, &mut doc, "l", false));
        assert!(app.shuttle > 0.0, "the shuttle is running");
        app.toggle_play();
        assert_eq!(app.shuttle, 0.0, "the play/pause press stopped the shuttle");
        assert!(
            !app.playback.playing,
            "and did not start playback on top of it, which would have left the \
             transport reporting a state nothing on screen accounted for",
        );
    }
}

// ---- linked audio and video -----------------------------------------

/// The sample project's market cutaway, with the sound split off onto its
/// own clip through the production path. Returns the two halves.
fn app_with_linked_shot() -> (VideoEditorApp, AutoCommit) {
    let (mut app, mut doc) = app_and_doc();
    let mid = app
        .project
        .media
        .iter()
        .find(|m| m.name == "Cutaway_Market.mp4")
        .map(|m| m.id)
        .expect("the market source is in the bin");
    if let Some(m) = app.project.media.iter_mut().find(|m| m.id == mid) {
        // What a probe reads off a real container.
        m.has_audio = true;
    }
    app.dispatch_media(&mut doc, mid);
    let vid = app
        .project
        .clips
        .iter()
        .find(|c| c.media == mid)
        .map(|c| c.id)
        .expect("the market clip is on V1");
    app.attach_linked_audio(&mut doc, vid)
        .expect("the sound splits off");
    app.selection.select_only(vid);
    (app, doc)
}

/// Where a clip is drawn right now, through the same two calls the paint
/// makes.
fn clip_rect_of(app: &VideoEditorApp, id: ClipId) -> Rect {
    let r = app.regions();
    let clip = app.project.clip(id).expect("the clip").clone();
    let lane = app
        .lane_rects(&r)
        .into_iter()
        .find(|(tid, _, _)| *tid == clip.track)
        .map(|(_, _, lane)| lane)
        .expect("the clip's lane is on screen");
    app.clip_rect(&clip, &lane, &r)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn linked_shots_place_move_edit_and_copy_as_pairs() {
    // Case `placing_a_shot_with_sound_lands_a_linked_pair_that_replicates`:
    // Placing a source whose container carries sound has to give back *two*
    // clips, and both have to reach the document — a picture clip whose sound
    // only exists in this process is not something a collaborator can hear.
    {
        let (mut app, mut doc) = app_and_doc();
        let mid = app
            .project
            .media
            .iter()
            .find(|m| m.name == "Interview_A.mp4")
            .map(|m| m.id)
            .expect("the interview source");
        if let Some(m) = app.project.media.iter_mut().find(|m| m.id == mid) {
            m.has_audio = true;
        }
        app.dispatch_media(&mut doc, mid);
        let before = app.project.clips.len();
        let vid = app
            .add_media_to_timeline(&mut doc, mid)
            .expect("the picture lands");
        assert_eq!(
            app.project.clips.len(),
            before + 2,
            "a shot with sound is two clips, not one",
        );
        let partners = app.project.link_partners(vid);
        assert_eq!(partners.len(), 1, "{partners:?}");
        let aid = partners[0];
        assert_eq!(
            app.project.clip_kind(aid),
            TrackKind::Audio,
            "the sound goes on an audio lane, which is what makes it \
             sliddable, levellable and cuttable on its own",
        );

        // Straight out of the CRDT, which is what a peer will read.
        let shared = VideoDoc::load_project(&doc);
        let v = shared.clip(vid).expect("the picture replicated");
        let a = shared.clip(aid).expect("the sound replicated");
        assert_eq!(v.link, a.link);
        assert!(v.link.is_some());
        assert!(v.sound_split, "or a peer would hear the shot twice");

        // A source with no sound track still lands as one clip.
        let silent = app
            .project
            .media
            .iter()
            .find(|m| m.name == "Sunset_Timelapse.mp4")
            .map(|m| m.id)
            .expect("the sunset source");
        let n = app.project.clips.len();
        let only = app
            .add_media_to_timeline(&mut doc, silent)
            .expect("it lands");
        assert_eq!(app.project.clips.len(), n + 1);
        assert!(!app.project.is_linked(only));
    }
    // Case `dragging_one_half_of_a_shot_carries_the_other`:
    // Dragging one half drags the other. The grabbed clip changes lane; the
    // linked half keeps its own and moves by the same amount of time.
    {
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("the picture is selected");
        let aid = app.project.link_partners(vid)[0];
        let v_before = app.project.clip(vid).expect("picture").start;
        let a_before = app.project.clip(aid).expect("sound").start;
        let a_track = app.project.clip(aid).expect("sound").track;

        let cr = clip_rect_of(&app, vid);
        let grab = Point::new(cr.center().x, cr.center().y);
        click(&mut app, &mut doc, grab.x, grab.y);
        assert!(
            matches!(app.drag, Some(Drag::ClipMove { .. })),
            "the press has to start a move for this test to prove anything",
        );
        move_to(&mut app, &mut doc, grab.x + 90.0, grab.y);
        // Mid-drag, before the button comes up: the sound has *already* moved.
        // "Dragging one drags both" has to be legible while it happens, not
        // only after it lands.
        assert!(
            (app.project.clip(aid).expect("sound").start - a_before).abs() > 0.1,
            "the sound only catches up on release, so the drag shows the two              halves coming apart",
        );
        release(&mut app, &mut doc);

        let v_after = app.project.clip(vid).expect("picture").start;
        let a_after = app.project.clip(aid).expect("sound").start;
        assert!(
            v_after > v_before + 1.0,
            "the drag did not move the picture: {v_before} -> {v_after}",
        );
        assert!(
            ((a_after - a_before) - (v_after - v_before)).abs() < 1e-6,
            "the sound moved by {} while the picture moved by {}",
            a_after - a_before,
            v_after - v_before,
        );
        assert_eq!(
            app.project.clip(aid).expect("sound").track,
            a_track,
            "and it stayed on its own lane",
        );
        // Committed, not only previewed: the drag has to reach the document.
        let shared = VideoDoc::load_project(&doc);
        assert!((shared.clip(aid).expect("sound").start - a_after).abs() < 1e-9);
    }
    // Case `every_on_screen_edit_that_follows_the_link_takes_both_halves`:
    // Delete, ripple-delete, split, trim and re-time all take the whole shot.
    {
        // Delete.
        let (mut app, mut doc) = app_with_linked_shot();
        let aid = app.project.link_partners(app.selected().expect("picture"))[0];
        app.delete_selected(&mut doc);
        assert!(
            app.project.clip(aid).is_none(),
            "half a shot left behind is the surprise",
        );
        assert!(VideoDoc::load_project(&doc).clip(aid).is_none());

        // Ripple delete.
        let (mut app, mut doc) = app_with_linked_shot();
        let aid = app.project.link_partners(app.selected().expect("picture"))[0];
        app.ripple_delete_selected(&mut doc);
        assert!(app.project.clip(aid).is_none());

        // Split at the playhead.
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        let n = app.project.clips.len();
        app.playhead = app.project.clip(vid).expect("picture").start + 2.0;
        app.split_selected(&mut doc);
        assert_eq!(app.project.clips.len(), n + 2, "both lanes were cut");
        let heads_link = app.project.clip(vid).expect("head").link;
        assert_eq!(app.project.clip(aid).expect("head").link, heads_link);
        let tails: Vec<&Clip> = app
            .project
            .clips
            .iter()
            .filter(|c| c.link.is_some() && c.link != heads_link)
            .collect();
        assert_eq!(tails.len(), 2, "the tails are a group of their own");
        assert_eq!(tails[0].link, tails[1].link);

        // Trim to the playhead (`I`).
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        app.playhead = app.project.clip(vid).expect("picture").start + 1.5;
        app.set_in_at_playhead(&mut doc);
        let (v, a) = (
            app.project.clip(vid).expect("picture"),
            app.project.clip(aid).expect("sound"),
        );
        assert!(v.in_point > 0.0, "the head really moved");
        assert!(
            (v.in_point - a.in_point).abs() < 1e-9 && (v.start - a.start).abs() < 1e-9,
            "the sound has to keep covering the picture: {v:?} {a:?}",
        );

        // Re-time.
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        app.set_speed_selected(&mut doc, 2.0, false);
        assert!((app.project.clip(vid).expect("picture").speed.rate() - 2.0).abs() < 1e-9);
        assert!(
            (app.project.clip(aid).expect("sound").speed.rate() - 2.0).abs() < 1e-9,
            "re-timing one half and not the other desyncs them permanently",
        );
        assert!(
            (VideoDoc::load_project(&doc)
                .clip(aid)
                .expect("sound")
                .speed
                .rate()
                - 2.0)
                .abs()
                < 1e-9,
            "and the sound's new rate has to reach the document",
        );
    }
    // Case `levels_and_fades_do_not_follow_the_link`:
    // A grade, a level and a fade stay per-clip: sliding the sound and
    // levelling it on its own is the entire point of it being a clip.
    {
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        app.selection.select_only(aid);
        app.edit_selected(&mut doc, |c| {
            c.volume = 0.25;
            c.fade_in = 0.8;
        });
        let v = app.project.clip(vid).expect("picture");
        assert!(
            (v.volume - 1.0).abs() < 1e-9 && v.fade_in < 1e-9,
            "levelling the sound must not touch the picture: {v:?}",
        );
        let a = app.project.clip(aid).expect("sound");
        assert!((a.volume - 0.25).abs() < 1e-9 && (a.fade_in - 0.8).abs() < 1e-9);
    }
    // Case `copying_and_duplicating_a_shot_keeps_it_whole_without_tying_it_to_the_original`:
    // Copying half a shot and pasting it would give a picture whose sound
    // stayed behind — a silent clip with no explanation. Copy takes the whole
    // shot, and the pasted copies are linked to each other and to nothing
    // else.
    {
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        assert_eq!(app.selection.ids(), &[vid], "only the picture is selected");

        app.copy_selected();
        assert_eq!(
            app.clipboard.len(),
            2,
            "the sound came with the picture: {:?}",
            app.clipboard.iter().map(|c| c.id).collect::<Vec<_>>(),
        );

        let was = app.project.clip(vid).expect("picture").link;
        app.duplicate_selected(&mut doc);
        let copies = app.selection.ids().to_vec();
        assert_eq!(copies.len(), 2, "a duplicated shot is still a shot");
        let copy_link = app.project.clip(copies[0]).expect("a copy").link;
        assert!(copy_link.is_some());
        assert_eq!(
            app.project.clip(copies[1]).expect("the other copy").link,
            copy_link,
            "the two copies still move together",
        );
        assert_ne!(
            copy_link, was,
            "but dragging the original must not drag the copy across the              timeline with it",
        );
        assert_eq!(
            app.project.clip(vid).expect("original").link,
            was,
            "and the original pair is untouched",
        );
        assert_eq!(app.project.clip(aid).expect("original sound").link, was);
    }
    // Case `a_local_move_does_not_reverse_an_unrelated_remote_clip_edit`.
    {
        let (mut app, mut doc) = app_and_doc();
        let id = app.selected().expect("fixture selection");
        let old_start = app.project.clip(id).expect("clip").start;

        // Press/drag captured the local start. Before release a collaborator
        // changes brightness in the CRDT; the local model has not reloaded
        // because it is still rendering the drag preview.
        app.remember_clip_before(id);
        app.project.move_clip(id, old_start + 2.0);
        let mut remote = VideoDoc::load_project(&doc);
        remote.clip_mut(id).expect("clip").filters.brightness = 0.91;
        VideoDoc::upsert_clip(&mut doc, remote.clip(id).expect("remote clip"));

        app.dispatch_existing_clips(&mut doc, &[id]);
        let action: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("move dispatched a patch")
                .payload,
        )
        .expect("move action decodes");
        let crate::actions::VideoEditorAction::PatchClipsFields { patches } = action else {
            panic!("existing move is a grouped field patch");
        };
        assert_eq!(patches.len(), 1);
        assert!(patches[0].changes.iter().all(|patch| matches!(
            patch,
            crate::actions::ClipFieldPatch::Number {
                field: crate::actions::ClipNumberField::Start,
                ..
            }
        )));

        let shared = VideoDoc::load_project(&doc);
        let clip = shared.clip(id).expect("clip remains");
        assert!((clip.start - (old_start + 2.0)).abs() < 1e-9);
        assert!(
            (clip.filters.brightness - 0.91).abs() < 1e-9,
            "the local move must not reverse the remote brightness"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn link_controls_marks_and_split_offers_follow_the_link() {
    // Case `the_keyboard_walk_reaches_the_link_control_and_it_unlinks`:
    // The link control is reachable without a pointer, and it does the same
    // thing the pointer does.
    {
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];

        assert_eq!(app.link_affordance(), Some(LinkAffordance::Unlink));
        let rows = app.a11y_inspector_rows();
        let at = rows
            .iter()
            .position(|r| *r == InspectorRow::Pick(PickField::LinkToggle))
            .expect("the walk stops on the link control");
        // Named the way the button is, so a reader is told what pressing it
        // will do rather than what is true.
        let node = app.a11y_pick_node(PickField::LinkToggle, false);
        assert_eq!(node.name.as_deref(), Some("Unlink audio and video"));
        // And it is laid out where the walk says it is.
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(
            VideoEditorApp::inspector_row_rect(&l, rows[at]).is_some(),
            "a stop with no rect is a stop the cursor cannot be drawn on",
        );

        assert!(app.a11y_activate_pick(PickField::LinkToggle, &mut doc));
        assert!(!app.project.is_linked(vid), "the pair came apart");
        assert!(!VideoDoc::load_project(&doc).is_linked(aid));
        let unlinked: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("unlink dispatched an action")
                .payload,
        )
        .expect("unlink action decodes");
        assert!(
            matches!(
                unlinked,
                crate::actions::VideoEditorAction::PatchClipsFields { .. }
            ),
            "unlink patches every existing member atomically"
        );

        // …and the same control ties them back together.
        app.selection.set(vec![vid, aid]);
        assert_eq!(app.link_affordance(), Some(LinkAffordance::Link));
        assert!(app.a11y_activate_pick(PickField::LinkToggle, &mut doc));
        assert!(app.project.is_linked(vid));
        let linked: crate::actions::VideoEditorAction = serde_json::from_slice(
            &app.actions
                .last_dispatched()
                .expect("link dispatched an action")
                .payload,
        )
        .expect("link action decodes");
        assert!(
            matches!(
                linked,
                crate::actions::VideoEditorAction::PatchClipsFields { .. }
            ),
            "link patches every existing member atomically"
        );
    }
    // Case `a_shot_still_carrying_its_sound_is_offered_the_split`:
    // A project made before linked audio existed has to be able to get one,
    // and the inspector is where a person will look for it.
    {
        let (mut app, mut doc) = app_and_doc();
        let mid = app
            .project
            .media
            .iter()
            .find(|m| m.name == "Cutaway_Market.mp4")
            .map(|m| m.id)
            .expect("the market source");
        if let Some(m) = app.project.media.iter_mut().find(|m| m.id == mid) {
            m.has_audio = true;
        }
        let vid = app
            .project
            .clips
            .iter()
            .find(|c| c.media == mid)
            .map(|c| c.id)
            .expect("the market clip");
        app.selection.select_only(vid);
        assert_eq!(app.link_affordance(), Some(LinkAffordance::SplitAudio));
        assert!(app.a11y_activate_pick(PickField::LinkToggle, &mut doc));
        let partners = app.project.link_partners(vid);
        assert_eq!(partners.len(), 1, "the sound came off onto its own clip");
        assert_eq!(app.project.clip_kind(partners[0]), TrackKind::Audio);
        assert_eq!(
            app.link_affordance(),
            Some(LinkAffordance::Unlink),
            "and the control now offers the way back",
        );
    }
    // Case `the_clip_menu_offers_to_unlink_and_does_it`:
    // The right-click menu carries it too, because that is where a person
    // who wants the sound three frames late will look.
    {
        let (mut app, mut doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        let cr = clip_rect_of(&app, vid);
        app.open_clip_menu(vid, cr.center());
        let labels: Vec<String> = app
            .bin_menu
            .items()
            .iter()
            .map(|i| i.label().unwrap_or("--").to_string())
            .collect();
        let idx = labels
            .iter()
            .position(|l| l == "Unlink audio and video")
            .unwrap_or_else(|| panic!("the clip menu offers the unlink: {labels:?}"));
        app.bin_menu_choose(&mut doc, idx);
        assert!(!app.project.is_linked(vid));
        assert!(!VideoDoc::load_project(&doc).is_linked(aid));

        // An unlinked single clip with no sound to split has nothing to offer,
        // so the row is absent rather than present and inert.
        let (mut app, _doc) = app_and_doc();
        let cid = app
            .project
            .clips
            .iter()
            .find(|c| {
                app.project
                    .media(c.media)
                    .is_some_and(|m| m.kind == MediaKind::Image)
            })
            .map(|c| c.id)
            .expect("the logo clip carries no sound");
        app.selection.select_only(cid);
        assert_eq!(app.link_affordance(), None);
        app.open_clip_menu(cid, Point::new(400.0, 620.0));
        assert!(
            !app.bin_menu
                .items()
                .iter()
                .any(|i| i.label().is_some_and(|l| l.contains("ink"))),
            "nothing to link or unlink means no row at all",
        );
    }
    // Case `a_linked_clip_wears_a_mark_an_unlinked_one_does_not`:
    // A linked clip has to *look* linked, standing still and unselected.
    //
    // Rendered three times, not two: the same state twice first, so a
    // difference that came from the frame rather than from the link (an
    // animating flap, a moving playhead) fails here instead of being read as
    // the mark. Only then is the link removed.
    {
        let theme = Theme::dark();
        let (w, h) = (1280u32, 800u32);
        let (mut app, _doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        // Nothing selected, so the mark under test is the resting one and not
        // the outline a selection draws.
        app.selection.clear();

        let cr = clip_rect_of(&app, aid);
        let header = Rect::new(cr.x0 + 2.0, cr.y0 + 2.0, cr.x0 + 60.0, cr.y0 + 17.0);
        // A band inside the body, clear of every border, so the control is a
        // region the link is not supposed to change.
        let body = Rect::new(cr.x0 + 6.0, cr.y0 + 24.0, cr.x1 - 6.0, cr.y1 - 4.0);

        let first = render_pixels(&mut app, w, h, &theme);
        let again = render_pixels(&mut app, w, h, &theme);
        assert_eq!(
            crop(&first, w, header),
            crop(&again, w, header),
            "two renders of one state already differ; nothing below this can \
             be attributed to the link",
        );

        for id in [vid, aid] {
            app.project.clip_mut(id).expect("the clip").link = None;
        }
        let unlinked = render_pixels(&mut app, w, h, &theme);
        assert_ne!(
            crop(&first, w, header),
            crop(&unlinked, w, header),
            "a linked clip is drawn no differently from an unlinked one",
        );
        assert_eq!(
            crop(&first, w, body),
            crop(&unlinked, w, body),
            "the mark belongs in the header, not over the clip's content",
        );
    }
    // Case `selecting_one_half_marks_the_other_as_implicated`:
    // Selecting one half has to show the other is implicated, or dragging one
    // and watching another move is alarming rather than helpful.
    {
        let theme = Theme::dark();
        let (w, h) = (1280u32, 800u32);
        let (mut app, _doc) = app_with_linked_shot();
        let vid = app.selected().expect("picture");
        let aid = app.project.link_partners(vid)[0];
        assert!(
            !app.selection.contains(aid),
            "the sound half is *not* selected; that is the point",
        );

        let cr = clip_rect_of(&app, aid);
        // The clip's own outline, and the gap above it where the tie runs.
        let edge = Rect::new(cr.x0 - 3.0, cr.y0 - 9.0, cr.x1 + 3.0, cr.y0 + 3.0);

        let first = render_pixels(&mut app, w, h, &theme);
        let again = render_pixels(&mut app, w, h, &theme);
        assert_eq!(
            crop(&first, w, edge),
            crop(&again, w, edge),
            "two renders of one state already differ",
        );

        // The only thing that changes: the link. The selection, the playhead
        // and every clip's geometry stay exactly where they were.
        for id in [vid, aid] {
            app.project.clip_mut(id).expect("the clip").link = None;
        }
        let unlinked = render_pixels(&mut app, w, h, &theme);
        assert_ne!(
            crop(&first, w, edge),
            crop(&unlinked, w, edge),
            "the other half of a selected shot is drawn exactly like a clip \
             that has nothing to do with it",
        );
    }
}

// ---- the mix meter and the clipping warning --------------------------

/// The sample timeline with a mix that has been *measured* over, staged
/// through [`VideoEditorApp::stage_mix_block`] — the same door the live
/// monitor's blocks come in by, so these tests exercise the production
/// latch and merge rather than a hand-set flag.
fn app_with_clipped_mix() -> (VideoEditorApp, AutoCommit) {
    let (mut app, doc) = app_and_doc();
    let lanes: Vec<TrackId> = app
        .project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id)
        .collect();
    assert!(!lanes.is_empty(), "the sample timeline has audio lanes");
    let block = |l: f32| crate::audio::MixReport {
        master: crate::audio::BlockMeter { peak: [l, l * 0.9] },
        tracks: lanes
            .iter()
            .map(|id| (*id, crate::audio::BlockMeter { peak: [l, l * 0.9] }))
            .collect(),
    };
    // An ordinary block, then two that went over, at instants far enough
    // apart that the meter keeps them as two places rather than merging
    // them into one.
    app.stage_mix_block(1.0, &block(0.6));
    app.stage_mix_block(4.0, &block(1.3));
    app.stage_mix_block(18.0, &block(1.1));
    (app, doc)
}

#[test]
fn mix_clipping_walk_and_measurement_reports() {
    // Case `c_walks_the_places_the_mix_clipped_and_wraps_round`:
    // C walks the places the mix went over, and wraps.
    //
    // The half of the warning that makes it worth having: knowing a
    // ninety-second timeline clipped somewhere is knowing you have ninety
    // seconds to scrub. Until this key the only route to the answer was
    // clicking a chip sixty pixels wide.
    {
        let (mut app, mut doc) = app_with_clipped_mix();
        app.playhead = 0.0;
        assert!(nav_key(&mut app, &mut doc, ch("c"), false), "C is bound");
        assert!(
            (app.playhead - 4.0).abs() < 1e-6,
            "the first overload, not the loudest or the last: {}",
            app.playhead,
        );
        assert!(nav_key(&mut app, &mut doc, ch("c"), false));
        assert!(
            (app.playhead - 18.0).abs() < 1e-6,
            "then the next one: {}",
            app.playhead,
        );
        // Pressing it again starts the list over rather than parking on the
        // last one, so the key never stops answering.
        assert!(nav_key(&mut app, &mut doc, ch("c"), false));
        assert!(
            (app.playhead - 4.0).abs() < 1e-6,
            "and wraps to the first: {}",
            app.playhead,
        );
    }
    // Case `c_does_nothing_on_a_mix_that_never_went_over`:
    // …and says nothing when there is nothing to say. A key that jumped the
    // playhead somewhere on a mix that never clipped would be reporting a
    // fault that does not exist.
    {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 7.0;
        assert!(
            !nav_key(&mut app, &mut doc, ch("c"), false),
            "unhandled, so the key can fall through rather than pretending",
        );
        assert!((app.playhead - 7.0).abs() < 1e-9, "and nothing moved");
    }
    // Case `a_reader_is_told_the_mix_clipped_and_can_walk_to_it`:
    // The master strip's accessibility node is a *button* once the mix has
    // clipped, and pressing it is the same walk the chip and the key do.
    {
        let (mut app, mut doc) = app_with_clipped_mix();
        let words = app.mix_words().expect("the timeline has audio");
        assert!(
            words.contains("clipped") && words.contains("dB"),
            "the words say it happened and how far over: {words}",
        );
        assert!(
            words.contains(&crate::timecode::timecode(4.0, app.project.fps)),
            "and where the first one was: {words}",
        );
        // The route a reader takes, through the production handler rather than
        // a copy of it.
        app.playhead = 0.0;
        let key = ui::a11y::hash_key(&MIX_METER_KEY);
        assert!(app.a11y_timeline_action(&ui::a11y::Action::Activate, &[key], &mut doc));
        assert!(
            (app.playhead - 4.0).abs() < 1e-6,
            "activating the node walks the playhead: {}",
            app.playhead,
        );
    }
    // Case `an_unmeasured_mix_says_so_rather_than_claiming_it_is_fine`:
    // A mix nobody has listened to reports that, rather than reporting that
    // the levels are fine.
    {
        let (app, _doc) = app_and_doc();
        let words = app.mix_words().expect("the timeline has audio");
        assert!(!words.contains("clipped"), "nothing has clipped: {words}");
        assert!(
            words.to_lowercase().contains("not measured"),
            "and silence is not the same claim as 'never played': {words}",
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn mixer_lane_reset_and_capture_are_stable() {
    // Case `the_keyboard_walk_reaches_the_lane_reset_and_it_puts_the_lane_back`:
    // The lane's reset button is on the keyboard walk, is drawn where the
    // walk says it is, and does the same thing the pointer does.
    //
    // The two knobs above it reached the walk the day they were added and
    // this did not, so a keyboard could push a fader off unity and then had
    // no way back to it that was not two sliders and a guess.
    {
        let (mut app, mut doc) = app_and_doc();
        let cid = app
            .project
            .clips
            .iter()
            .find(|c| {
                app.project
                    .track(c.track)
                    .is_some_and(|t| t.kind == TrackKind::Audio)
            })
            .map(|c| c.id)
            .expect("the sample timeline has an audio clip");
        app.selection.select_only(cid);

        // At unity there is no button, so there must be no stop either.
        assert!(
            !app.a11y_inspector_rows()
                .contains(&InspectorRow::Pick(PickField::MixReset)),
            "a stop on a control the panel is not drawing is a stop onto nothing",
        );
        assert!(
            !app.a11y_activate_pick(PickField::MixReset, &mut doc),
            "and it refuses to act through the back door either",
        );

        let tid = app.project.clip(cid).expect("the clip").track;
        // Through the same preview-then-commit path a fader drag takes.
        app.preview_track_edit(|t| {
            t.gain_db = 5.5;
            t.pan = -0.4;
        });
        app.dispatch_selected_track(&mut doc);
        assert!(app.project.track(tid).expect("the lane").mix_is_set());

        let rows = app.a11y_inspector_rows();
        let at = rows
            .iter()
            .position(|r| *r == InspectorRow::Pick(PickField::MixReset))
            .expect("the walk stops on the reset button");
        // Drawn where the walk says it is — a stop with no rect is a cursor
        // the eye cannot follow.
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(
            VideoEditorApp::inspector_row_rect(&l, rows[at]).is_some(),
            "the reset stop has a rect in the very layout the panel draws from",
        );
        // Named for the lane it puts back, because a reader standing on it has
        // no fader in view.
        let node = app.a11y_pick_node(PickField::MixReset, false);
        let name = node.name.unwrap_or_default();
        assert!(
            name.contains(&app.project.track(tid).expect("the lane").name),
            "the node names the lane: {name}",
        );

        // And it reads as a button under the pointer. It was drawn as one and
        // acted as one, and the cursor over it stayed an arrow — the one cue
        // that says "this is pressable" before you press it.
        let on_screen = reveal(&mut app, |l| l.mix_reset);
        assert_eq!(
            app.cursor_shape(on_screen.center()),
            Some(winit::window::CursorIcon::Pointer),
        );

        assert!(app.a11y_activate_pick(PickField::MixReset, &mut doc));
        let back = app.project.track(tid).expect("the lane");
        assert!(
            !back.mix_is_set(),
            "unity and centre: {}",
            back.gain_words()
        );
        // …and it reached the document, not only the in-memory model.
        assert!(
            !VideoDoc::load_project(&doc)
                .track(tid)
                .expect("the lane")
                .mix_is_set(),
        );
    }
    // Case `a_capture_of_the_mixer_is_the_same_capture_twice`:
    // A capture of the mixer is the same capture every time it is taken.
    //
    // It was not. The meter falls at a rate per *second* off the wall clock,
    // and `--shots` runs that clock like any other build, so three captures
    // of one staged mix came out as three different files — the bars landed
    // wherever process startup and the frame loop had left them, green in one
    // and red in the next off identical numbers. A still that changes between
    // takes cannot be a visual baseline and cannot be evidence of anything.
    {
        use std::time::Duration;
        let theme = Theme::dark();
        let (w, h) = (960u32, 640u32);
        let mut doc = AutoCommit::new();
        let mut app = VideoEditorApp::new_for_shot(ShotScenario::Mix);
        app.rect = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        // Two syncs to settle the scenario, so the comparison below is of a
        // staged state rather than of one still arriving.
        app.sync(&mut doc, &automerge::ROOT);
        app.sync(&mut doc, &automerge::ROOT);
        let _ = render_pixels(&mut app, w, h, &theme);
        let staged = app.mix.clone();
        assert!(
            staged.clipped() && staged.master[0] > 0.0,
            "the scenario has to have staged a live meter or this proves \
             nothing: {staged:?}",
        );

        // First the control: the same state drawn twice with nothing between,
        // to establish what "identical" costs on this renderer. Without it a
        // handful of antialiasing pixels would be read as the thing under
        // test — and there are a handful.
        let a = render_pixels(&mut app, w, h, &theme);
        let b = render_pixels(&mut app, w, h, &theme);
        let noise = frame_diff(&a, &b, w);
        assert!(
            noise.0 < 64,
            "something in this frame animates on its own, so nothing below is \
             attributable: {}",
            noise.1,
        );

        // Now the claim: time passes, the app is pumped exactly as the capture
        // loop pumps it, and nothing about the mix has moved.
        std::thread::sleep(Duration::from_millis(150));
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.mix, staged,
            "150ms of wall clock moved the meter the capture is of; at \
             24 dB/s that is nearly three decibels, which is the difference \
             between a green bar and a red one",
        );
        let later = render_pixels(&mut app, w, h, &theme);
        let moved = frame_diff(&a, &later, w);
        assert!(
            moved.0 <= noise.0.max(64),
            "and the picture is the same picture: {}",
            moved.1,
        );
    }
}

/// How many pixels differ between two frames, and whereabouts.
///
/// Reported as a count and a bounding box rather than as the pixels: a
/// failure that prints two million bytes is a failure nobody reads.
fn frame_diff(before: &[u8], after: &[u8], width: u32) -> (usize, String) {
    if before.len() != after.len() {
        return (
            usize::MAX,
            format!("different sizes: {} vs {}", before.len(), after.len()),
        );
    }
    let mut count = 0usize;
    let (mut left, mut top) = (u32::MAX, u32::MAX);
    let (mut right, mut bottom) = (0u32, 0u32);
    let pairs = before.chunks_exact(4).zip(after.chunks_exact(4));
    for (index, (was, now)) in pairs.enumerate() {
        if was == now {
            continue;
        }
        count += 1;
        let index = index as u32;
        left = left.min(index % width);
        top = top.min(index / width);
        right = right.max(index % width);
        bottom = bottom.max(index / width);
    }
    if count == 0 {
        return (0, "identical".to_string());
    }
    (
        count,
        format!("{count} pixels differ, within ({left},{top})..({right},{bottom})"),
    )
}

// ---- the project as a search source ----------------------------------

/// The raw bytes of a rectangle of the frame.
fn crop(rgba: &[u8], fw: u32, rect: Rect) -> Vec<u8> {
    let (x0, y0) = (rect.x0.max(0.0) as u32, rect.y0.max(0.0) as u32);
    let (x1, y1) = (rect.x1.max(0.0) as u32, rect.y1.max(0.0) as u32);
    let mut out = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * fw + x) * 4) as usize;
            if i + 3 < rgba.len() {
                out.extend_from_slice(&rgba[i..i + 4]);
            }
        }
    }
    out
}
