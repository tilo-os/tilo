//! `word-processor` — the runnable word-processor app. Opens a document
//! (`.odt` / `.md`, or a blank doc) in a `WordProcessorWidget` and runs it as a
//! `ui` application, the same way the terminal and file-explorer apps do.
//!
//! Usage:
//!   word-processor [FILE]              run interactively (FILE optional)
//!   word-processor --shots OUT [FILE]  render one headless frame to OUT.png
//!   word-processor --shots OUT --history [FILE]
//!                                      ...with the version-history strip open
//!
//! Collaboration: launched from the shell, the compositor's broker is
//! reached through the `TILO_SVC_COLLAB_FD` descriptor the launcher injects —
//! nothing to configure. A raw process started outside a session has no
//! collaboration service; the descriptor is inherited, not a socket path an
//! app can fabricate.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
// Thin build only: pull the shared toolkit dylib
// (`libruntime_dylib.so`) into the link graph. It statically contains
// the `ui` toolkit this app links -- and with it wgpu, vello,
// cosmic-text, automerge, taffy and fluent -- so with `-C
// prefer-dynamic` rustc serves `ui` and libstd from the dylib instead
// of baking ~18 MB of `.text` into this binary. The thin-app wrapper also
// size-optimizes, merges, and folds repeated app-local code so the qpkg meets its
// wire size limit without slowing the shared runtime. Naming the crate here
// is what keeps rustc from eliminating it as unused. Compiled out in
// normal (static) builds.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use tilo_automerge::ApplyIncoming;

use tilo_actions::Action;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::text_render::TextRenderer;
use ui::{PresenceCtx, State, Theme, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use word_processor::{WordProcessorWidget, open_document, word_processor::Caret};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};

/// An Automerge actor id that is actually distinct from every other replica's.
///
/// `format!("{prefix}-{pid}")` was not: every instance here runs in its own PID
/// namespace, so every instance sees itself as pid 2, every session wrote under
/// the *same* actor, and Automerge refuses a frame whose actor already used that
/// sequence number — frames silently dropped in the one direction that matters,
/// a change made on one machine never reaching another.
///
/// `have` is the actor the document already carries (random per `AutoCommit`, or
/// persisted per state directory via `ui::persist`) — distinct by construction,
/// the property the old id only looked like it had. Deliberately reads no
/// environment: the failure was an id that was a function of things two replicas
/// share, so only a function of its own arguments can be tested against it.
fn distinct_actor(prefix: &str, have: &automerge::ActorId) -> String {
    let hex = have.to_hex_string();
    // `to_hex_string` is pure ASCII hex digits, so any byte offset is also a
    // char boundary; `.get` still keeps this panic-free rather than trusting it.
    let short = hex.get(..hex.len().min(12)).unwrap_or(hex.as_str());
    format!("{prefix}-{short}")
}

/// The name this replica is given, if anything has named it. `QUICK_APP_ID` is
/// how a dev run points two instances at one shell and gets a readable history;
/// `TILO_USER_ID` is the session's own identity, but doesn't reach a sandboxed
/// app (the launcher builds its environment from an allowlist, and this isn't on
/// it) — so in the product neither answers, and the actor comes from
/// [`distinct_actor`].
fn named_actor(prefix: &str) -> Option<String> {
    if let Ok(id) = std::env::var("QUICK_APP_ID")
        && !id.is_empty()
    {
        return Some(id);
    }
    match std::env::var("TILO_USER_ID") {
        Ok(user) if !user.is_empty() => Some(format!("{prefix}-{user}")),
        _ => None,
    }
}

fn main() {
    // The live editor runs on the app framework: the document is a hosted
    // record with history, per-author undo and permissions. Opening a file
    // by path and the scenario captures (which stage view state directly on
    // a standalone editor) keep the standalone path.
    let mut args = std::env::args().skip(1);
    let mut standalone = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // The framework's own option and its value.
            "--data-dir" => {
                args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => {
                standalone = true;
            }
            file if !file.starts_with('-') => standalone = true,
            _ => {}
        }
    }
    if !standalone {
        ui::framework::run::<word_processor::framework::WordProcessorApp>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.word-processor", live_root)
            .max_positionals(1)
            .allowed_flags(&[
                "--styles",
                "--comments",
                "--threaded-comments",
                "--template-picker",
                "--toolbar-overflow",
                "--find",
                "--error",
                "--image-object",
                "--blank",
                "--outline",
                "--margins",
                "--recovery",
                "--unsaved",
            ])
            .shot_handler(run_shot)
            .prepare_legacy_with_args(prepare_launch),
    );
}

fn file_arg(args: &LaunchArgs) -> Option<&Path> {
    args.positionals().first().map(Path::new)
}

fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    build(file_arg(args))
        .map(|(_, widget)| widget)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

fn run_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    render_requested_shot(args).map_err(|error| LaunchError::Operation(error.to_string()))
}

fn render_requested_shot(args: &LaunchArgs) -> Result<(), Box<dyn Error>> {
    let out = args.shots.as_deref().ok_or("missing screenshot output")?;
    let file = file_arg(args);
    if args.history {
        // The strip renders the document's own change graph, so a shot of it is
        // only worth anything over a document that has one. Give the document a
        // few authored revisions first, through the same editing path a user
        // takes, then open the strip.
        let (mut state, mut wp) = build_concrete(file)?;
        seed_revisions(&mut state, &mut wp)?;
        render_shot(
            state,
            Box::new(wp),
            args.viewport.unwrap_or((1000, 700)),
            &ui::launch::shot_theme(args),
            out,
        )?;
        return Ok(());
    }

    // Plain and scenario shots share the real widget. Scenario flags only seed
    // state that a user could reach through the normal UI, keeping visual
    // review representative rather than maintaining a second mock renderer.
    // Without a file, shots open a realistic sample report (`--blank` shows
    // the empty document instead).
    let (state, mut wp) = if file.is_none() && !args.has("--blank") {
        build_sample()
    } else {
        build_concrete(file)?
    };
    let mut state = state;
    if args.has("--comments") || args.has("--threaded-comments") {
        seed_review(&mut state, &mut wp)?;
    }
    if args.has("--threaded-comments") {
        seed_comment_thread(&mut state, &mut wp)?;
    }
    if args.has("--find") {
        wp.sync(&mut state.doc, &state.root);
        wp.stage_find_for_shot(&state.doc, "report");
    }
    if args.has("--image-object") {
        seed_selected_image(&mut state, &mut wp)?;
    }
    if args.has("--styles") {
        wp.set_styles_panel_open(true);
    }
    if args.has("--outline") {
        wp.set_outline_open(true);
    }
    if args.has("--margins") {
        wp.set_margins_mode();
    }
    if args.has("--recovery") {
        stage_recovery(&mut wp)?;
    }
    if args.has("--unsaved") {
        wp.stage_unsaved_changes_for_shot();
    }
    if args.has("--error") {
        wp.stage_error_for_shot();
    }
    if args.has("--template-picker") {
        wp.stage_template_picker_for_shot();
    }
    if args.has("--toolbar-overflow") {
        wp.stage_toolbar_overflow_for_shot();
    }
    render_shot(
        state,
        Box::new(wp),
        args.viewport.unwrap_or((1000, 700)),
        &ui::launch::shot_theme(args),
        out,
    )?;
    Ok(())
}

/// Offer the recovery banner the way a crash leaves it: a recovery copy of
/// unsaved changes waiting beside the document, in a scratch directory the
/// shot owns.
fn stage_recovery(wp: &mut WordProcessorWidget) -> Result<(), Box<dyn Error>> {
    let dir = std::env::temp_dir().join(format!(
        "word-processor-shot-recovery-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(word_processor::recovery::slot_name(None)),
        include_str!("../tests/fixtures/shot-document.md"),
    )?;
    wp.set_recovery_dir(dir.clone());
    wp.check_for_recovery();
    // The offer holds the copy in memory; the scratch directory can go.
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// A real, selected document image for visual regression shots of the object
/// inspector. The image follows the production insertion model; the scenario
/// only chooses a deterministic caret/panel state that a user can reach.
fn seed_selected_image(
    state: &mut State,
    wp: &mut WordProcessorWidget,
) -> Result<(), Box<dyn Error>> {
    // A chart of the season's water use, placed after the summary the way a
    // person would insert it.
    const SHOT_PNG: &[u8] = include_bytes!("../tests/fixtures/shot-chart.png");

    wp.sync(&mut state.doc, &state.root);
    let containers = tilo_doc::model::open(&state.doc, &state.root)?;
    let anchor = tilo_doc::model::blocks_in_order(&state.doc, &containers)
        .into_iter()
        .filter_map(|id| {
            let text = tilo_doc::model::block_text(&state.doc, &containers, &id)?;
            Some((id, text))
        })
        .find(|(_, text)| text.starts_with("Water use fell"));
    if let Some((block, text)) = anchor {
        wp.model_mut().set_caret(Caret {
            block,
            offset: text.chars().count(),
        });
        wp.model_mut().enter(&mut state.doc);
    } else {
        wp.model_mut()
            .insert_str(&mut state.doc, "Water use by month");
        wp.model_mut().enter(&mut state.doc);
    }
    wp.model_mut().insert_image_bytes(&mut state.doc, SHOT_PNG);
    wp.sync(&mut state.doc, &state.root);

    let c = tilo_doc::model::open(&state.doc, &state.root)?;
    let image = tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .into_iter()
        .find(|block| block.kind == "image")
        .ok_or("image shot did not insert an image block")?;
    // Set an intentional frame size so the canvas itself visibly exercises the
    // selected object alongside the inspector controls.
    wp.model_mut()
        .set_image_size(&mut state.doc, &image.id, 384.0, 216.0);
    wp.model_mut().set_caret(Caret {
        block: image.id,
        offset: 0,
    });
    wp.set_styles_panel_open(true);
    wp.sync(&mut state.doc, &state.root);
    Ok(())
}

fn seed_review(state: &mut State, wp: &mut WordProcessorWidget) -> Result<(), Box<dyn Error>> {
    // A blank headless document has not been through its first draw yet. Bind it
    // before opening the qdoc containers, just as the interactive widget does
    // at the start of that draw. Otherwise the review scenario attempts to read
    // an uninitialised root record and cannot seed a representative comment.
    wp.sync(&mut state.doc, &state.root);
    let containers = tilo_doc::model::open(&state.doc, &state.root)?;
    let has_reviewable_text = tilo_doc::model::blocks_in_order(&state.doc, &containers)
        .into_iter()
        .any(|id| {
            tilo_doc::model::block_text(&state.doc, &containers, &id)
                .is_some_and(|text| !text.is_empty())
        });
    if !has_reviewable_text {
        // A fresh document has no paragraph until its first edit. Seed the
        // reviewable content through the real editing model, rather than making
        // the screenshot renderer special-case a fake annotation card.
        wp.model_mut()
            .insert_str(&mut state.doc, "Quarterly planning summary");
        wp.sync(&mut state.doc, &state.root);
    }
    let containers = tilo_doc::model::open(&state.doc, &state.root)?;
    let Some(block) = tilo_doc::model::blocks_in_order(&state.doc, &containers)
        .into_iter()
        .find(|id| {
            tilo_doc::model::block_text(&state.doc, &containers, id).is_some_and(|t| !t.is_empty())
        })
    else {
        return Ok(());
    };
    let len = tilo_doc::model::block_text(&state.doc, &containers, &block)
        .unwrap_or_default()
        .chars()
        .count();
    let end = len.min(12);
    word_processor::actions::add_comment_action(
        &block,
        0,
        end,
        "Maya Chen",
        "",
        "Could we name the partner farms in the opening?",
    )
    .apply(&mut state.doc)?;
    // A suggested insertion in the summary, where a reviewer would add it.
    let (target, at) = tilo_doc::model::blocks_in_order(&state.doc, &containers)
        .into_iter()
        .find_map(|id| {
            let text = tilo_doc::model::block_text(&state.doc, &containers, &id)?;
            let at = text.find("18 percent").map(|byte| {
                text.get(..byte + "18 percent".len())
                    .map_or(0, |head| head.chars().count())
            })?;
            Some((id, at))
        })
        .unwrap_or((block, len));
    word_processor::actions::RichTextAction::TrackInsert {
        block: target,
        at,
        text: " (about 110 million gallons)".into(),
        author: "Jonas Weber".into(),
        date: "2026-09-05".into(),
    }
    .apply(&mut state.doc)?;
    wp.sync(&mut state.doc, &state.root);
    Ok(())
}

/// Give the ordinary comments scenario a pair of persisted replies, then open
/// the production thread panel. This exercises the same document model used by
/// collaborators, rather than drawing a fixture-only discussion bubble.
fn seed_comment_thread(
    state: &mut State,
    wp: &mut WordProcessorWidget,
) -> Result<(), Box<dyn Error>> {
    let containers = tilo_doc::model::open(&state.doc, &state.root)?;
    let comment = tilo_doc::model::materialize(&state.doc, &containers)
        .comments
        .into_iter()
        .next()
        .map(|comment| comment.id)
        .ok_or("thread shot did not create a comment")?;
    word_processor::actions::reply_to_comment_action(
        &comment,
        "Jonas Weber",
        "2026-09-06",
        "I can revise the opening.",
    )
    .apply(&mut state.doc)?;
    word_processor::actions::reply_to_comment_action(
        &comment,
        "Maya Chen",
        "2026-09-07",
        "Thanks — please keep the decision clear.",
    )
    .apply(&mut state.doc)?;
    state.doc.commit();
    let thread = tilo_doc::model::materialize(&state.doc, &containers)
        .comments
        .into_iter()
        .find(|item| item.id == comment)
        .ok_or("thread shot could not materialize replies")?;
    wp.sync(&mut state.doc, &state.root);
    wp.stage_comment_thread_for_shot(thread);
    Ok(())
}

fn prepare_launch(args: &LaunchArgs) -> Result<ui::launch::LegacyLaunchInputs, LaunchError> {
    prepare_launch_inputs(args).map_err(|error| LaunchError::Operation(error.to_string()))
}

fn prepare_launch_inputs(
    args: &LaunchArgs,
) -> Result<ui::launch::LegacyLaunchInputs, Box<dyn Error>> {
    let room_id = std::env::var("TILO_WORKSPACE_ID")
        .or_else(|_| std::env::var("QUICK_CLUSTER_ID"))
        .unwrap_or_else(|_| "00000000-0000-0000-0000-000000000000".into());
    // Resolve the actual room and retained file scope before file import or
    // untitled defaults. The index and file keep their original distinct writers.
    let (room, room_autosave) = ui::persist::open_room("so.tilo.word-processor", &room_id);
    let records = ui::RecordSet::for_local_files("so.tilo.word-processor")?.with_home(room_id);
    let (mut state, widget): (State, Box<dyn Widget>) = if let Some(path) = file_arg(args) {
        let (state, widget, _) = word_processor::open_document_with_records(
            path,
            &records,
            tilo_content::ContentStore::for_app(),
        )?;
        (state, Box::new(widget))
    } else {
        let reader = records.file_history_reader()?;
        reader.require_local_creation()?;
        let mut widget = WordProcessorWidget::new();
        widget
            .model_mut()
            .set_content_store(tilo_content::ContentStore::for_app());
        widget.retain_file_history(reader);
        (State::new(), Box::new(widget))
    };
    let app_id = named_actor("wp").unwrap_or_else(|| distinct_actor("wp", state.doc.get_actor()));
    ui::diag!("word-processor: automerge actor {app_id}");
    state
        .doc
        .set_actor(automerge::ActorId::from(app_id.as_bytes()));
    let mut registry = tilo_actions::ActionRegistry::new();
    word_processor::actions::register(&mut registry);
    Ok(
        ui::launch::LegacyLaunchInputs::new(state, widget, room_autosave)
            .with_records(records, room.doc)
            .with_registry(registry),
    )
}

/// Give `widget`'s document a real authored history, so `--shots --history` has
/// something worth drawing: three working sessions (two by the local author
/// either side of a long break, one by a collaborator merged back in between),
/// committed with explicit wall clocks since the gaps are the point. Two runs
/// are named — one by the collaborator, under its own actor — so the strip
/// shows a name somebody else wrote. Nothing here seeds the history *pane*: it
/// seeds the document, and the pane derives its band from the resulting change
/// graph exactly as for a document a user typed.
///
/// # Errors
///
/// Returns `Err` if the freshly-built demo document doesn't have the schema
/// or the paragraph this seeding assumes, or if the collaborator fork fails
/// to merge back — all of which would mean the widget/model plumbing above
/// it is broken, not that the demo data itself is missing.
fn seed_revisions(state: &mut State, wp: &mut WordProcessorWidget) -> Result<(), Box<dyn Error>> {
    state
        .doc
        .set_actor(automerge::ActorId::from("Ana Duarte".as_bytes()));
    // The clock a person reads, which a capture pins; every session below is
    // stamped against it.
    let now = ui::clock::local_now().timestamp();
    let base = now - 5 * 3600;
    wp.sync(&mut state.doc, &state.root);
    ui::tilo_resource::history::commit_stamped_at(&mut state.doc, base - 120);

    wp.model_mut()
        .insert_str(&mut state.doc, "Irrigation trial update");
    wp.model_mut()
        .set_paragraph_style(&mut state.doc, "heading", Some(1), None);
    wp.model_mut().enter(&mut state.doc);
    wp.model_mut()
        .set_paragraph_style(&mut state.doc, "paragraph", None, None);
    wp.model_mut().insert_str(
        &mut state.doc,
        "All twelve farms reported through the season.",
    );
    ui::tilo_resource::history::commit_stamped_at(&mut state.doc, base - 60);

    // Session one: five more sentences, half a minute apart.
    let session_one = [
        " Water use fell by 18 percent.",
        " Yields held close to the average.",
        " The orchards saved the most.",
        " Two probes were replaced in July.",
        " One gateway lost power for a weekend.",
    ];
    for (i, sentence) in (0_i64..).zip(session_one) {
        wp.model_mut().insert_str(&mut state.doc, sentence);
        ui::tilo_resource::history::commit_stamped_at(&mut state.doc, base + i * 30);
    }

    // Session two, an hour later, by a collaborator.
    let mut peer = state.doc.fork();
    peer.set_actor(automerge::ActorId::from("Bob Chen".as_bytes()));
    let mut peer_first = None;
    let session_two = [
        " Vineyards on clay held steady.",
        " The vegetable farms used more water in August.",
        " The controllers extended the morning cycles.",
        " Evening runs fell by almost half on sandy soil.",
    ];
    for (i, sentence) in (0_i64..).zip(session_two) {
        let c = tilo_doc::model::open(&peer, &state.root)?;
        let blocks = tilo_doc::model::materialize(&peer, &c).blocks;
        let last = blocks
            .last()
            .ok_or("seed_revisions: no paragraph to append to")?;
        let at = last.text.chars().count();
        word_processor::actions::RichTextAction::SpliceText {
            region: word_processor::actions::RegionKind::Body,
            block: last.id.clone(),
            at,
            del: 0,
            ins: sentence.into(),
        }
        .apply(&mut peer)?;
        let hash = ui::tilo_resource::history::commit_stamped_at(&mut peer, base + 3600 + i * 30);
        if i == 0 {
            peer_first = hash;
        }
    }
    // The collaborator names its own session, in the document, under its own
    // actor — so everyone else's strip says who wrote the name.
    if let (Some(first), Some(last)) = (peer_first, peer.get_heads().first().copied()) {
        let start = ui::history_source::entry_id_from_hash(&first.to_string());
        let end = ui::history_source::entry_id_from_hash(&last.to_string());
        ui::history_groups::name_range(&mut peer, &start, Some(&end), "Results by site");
        ui::tilo_resource::history::commit_stamped_at(&mut peer, base + 3600 + 150);
    }
    state.doc.merge_incoming(&mut peer)?;

    // Session three, three hours after that, back to the local author.
    wp.sync(&mut state.doc, &state.root);
    let mut session_three_start = None;
    let session_three = [
        " The eastern terraces join in spring.",
        " Farm managers will get weekly summaries.",
        " The heat wave override needs a review.",
    ];
    for (i, sentence) in (0_i64..).zip(session_three) {
        wp.model_mut().insert_str(&mut state.doc, sentence);
        let hash =
            ui::tilo_resource::history::commit_stamped_at(&mut state.doc, base + 4 * 3600 + i * 25);
        if i == 0 {
            session_three_start = hash;
        }
    }
    // And the local author names theirs.
    if let (Some(first), Some(last)) = (session_three_start, state.doc.get_heads().first().copied())
    {
        let start = ui::history_source::entry_id_from_hash(&first.to_string());
        let end = ui::history_source::entry_id_from_hash(&last.to_string());
        ui::history_groups::name_range(&mut state.doc, &start, Some(&end), "Next steps");
        ui::tilo_resource::history::commit_stamped_at(&mut state.doc, now - 120);
    }

    wp.toggle_history(&mut state.doc);
    wp.sync(&mut state.doc, &state.root);
    Ok(())
}

/// Like [`build`] but keeps the concrete widget type, and keeps images in memory: headless
/// frames leave nothing behind.
fn build_concrete(file: Option<&Path>) -> Result<(State, WordProcessorWidget), Box<dyn Error>> {
    match file {
        Some(p) => {
            let (state, widget, _fmt) = open_document(p, tilo_content::ContentStore::memory())?;
            Ok((state, widget))
        }
        None => Ok((State::new(), WordProcessorWidget::new())),
    }
}

/// A fresh document holding the sample report shots show: imported through
/// the same Markdown path a user's file takes.
fn build_sample() -> (State, WordProcessorWidget) {
    const SAMPLE: &str = include_str!("../tests/fixtures/shot-document.md");
    let mut state = State::new();
    let mut importer = word_processor::word_processor::WordProcessor::new();
    importer.bind(&mut state.doc, &state.root);
    if !importer.replace_with_markdown(&mut state.doc, SAMPLE.as_bytes()) {
        eprintln!("word-processor: the sample document failed to import");
    }
    state.doc.commit();
    (state, WordProcessorWidget::new())
}

/// Build the document state + widget: open `file` if given, else a blank doc. The document's
/// images live in the app's content store.
fn build(file: Option<&Path>) -> Result<(State, Box<dyn Widget>), Box<dyn Error>> {
    let content = tilo_content::ContentStore::for_app();
    if let Some(p) = file {
        let (state, widget, _fmt) = open_document(p, content)?;
        return Ok((state, Box::new(widget)));
    }
    let mut widget = WordProcessorWidget::new();
    widget.model_mut().set_content_store(content);
    Ok((State::new(), Box::new(widget)))
}

/// Render a single headless frame to `out` (mirrors the other apps' `--shots`).
fn render_shot(
    mut state: State,
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    theme: &Theme,
    out: &Path,
) -> Result<(), Box<dyn Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;
    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the bound nodes come from this exact exclusively owned build.
    unsafe {
        bindings.rebuild(widget.as_ref(), &taffy, child, &node_map);
        ui::widget::compute_widget_layout(
            &mut taffy,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            &node_map,
            &bindings,
            &mut tr,
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        &presence,
        &permission,
    );
    // A real frame always runs the widget's top-layer pass after its body.
    // Keep shots honest: this is where recovery/error toasts, menus, and other
    // transient surfaces are painted.
    widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    if let Some(parent) = out.parent()
        && let Ok(dir) = tilo_files::Dir::open(tilo_files::Location::Native(parent.to_path_buf()))
    {
        let _ = tilo_files::blocking::create_dir_all(&dir, &tilo_files::VPath::root());
    }
    // Shots land on the window ground, as the framework paints it.
    OffscreenRenderer::new(w, h).render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("word-processor: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod actor_tests {
    use super::distinct_actor;

    /// Two replicas in one process do not share an actor, and the actor still names the application.
    #[test]
    fn each_replica_gets_its_own_actor_that_names_the_application() {
        // Phase: two_replicas_in_one_process_do_not_share_an_actor.
        // Two ids minted inside one process must differ, even though anything
        // derived from the process alone (a pid, which is 2 in every sandbox here)
        // produces the same string twice — which is exactly what made Automerge
        // refuse a second replica's frames as duplicate sequence numbers.
        {
            let a = automerge::AutoCommit::new();
            let b = automerge::AutoCommit::new();
            assert_ne!(
                distinct_actor("wp", a.get_actor()),
                distinct_actor("wp", b.get_actor()),
                "two replicas were given the same actor id, which is the failure \
                 that silently drops one of them's changes",
            );
        }

        // Phase: the_actor_still_names_the_application.
        // And it still says what it is, so a history can be read.
        {
            let doc = automerge::AutoCommit::new();
            assert!(distinct_actor("wp", doc.get_actor()).starts_with("wp-"));
        }
    }
}
