//! What a frame of a long conversation lays out and draws, counted.

use std::collections::HashSet;
use std::fmt::Write as _;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::app::{ChatApp, Focus};
use crate::timeline::TimelineCounts;
use ui::widget::DrawChild;

/// The size the owner's browser shows the Lounge at.
const WINDOW: (f64, f64) = (1440.0, 900.0);

/// A channel of `count` top-level messages, the demo's lines and pictures
/// over and over, a minute apart.
fn long_channel(count: usize) -> (ChatApp, crate::testing::Workspace) {
    let doc = crate::testing::demo_doc();
    let mut app = ChatApp::new_ephemeral();
    app.attach_workspace(&doc);
    app.reload(&doc);
    let seed: Vec<_> = app
        .messages
        .iter()
        .filter(|m| m.parent.is_none())
        .cloned()
        .collect();
    assert!(!seed.is_empty(), "the demo channel has messages");
    let start = seed.first().map_or(0, |m| m.ts);
    app.messages = (0..count)
        .map(|i| {
            let mut m = seed[i % seed.len()].clone();
            m.id = format!("long-{i}");
            m.ts = start + 60 * i64::try_from(i).unwrap_or_default();
            m
        })
        .collect();
    app.messages_revision += 1;
    (app, doc)
}

struct Frames {
    text: TextRenderer,
    theme: Theme,
    presence: PresenceCtx,
}

impl Frames {
    fn new() -> Self {
        Self {
            text: TextRenderer::new(),
            theme: Theme::dark(),
            presence: PresenceCtx::empty(),
        }
    }

    /// One frame of `app` at [`WINDOW`]: what its timeline counted and the
    /// rows it showed.
    fn draw(&mut self, app: &mut ChatApp) -> (TimelineCounts, Vec<String>) {
        self.scene(app);
        let timeline = app.timeline.borrow();
        (timeline.counts(), timeline.shown().to_vec())
    }

    /// One frame of `app` at [`WINDOW`], as drawn.
    fn scene(&mut self, app: &mut ChatApp) -> Scene {
        app.rect = Rect::new(0.0, 0.0, WINDOW.0, WINDOW.1);
        let mut scene = Scene::new();
        app.draw_child(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
        app.mark_drawn();
        scene
    }
}

fn feed(app: &mut ChatApp, doc: &mut crate::testing::Workspace, event: &winit::event::WindowEvent) {
    let mut handled = false;
    app.dispatch_event(
        event,
        1.0,
        winit::event::Modifiers::default(),
        &mut ui::widget::Edits::lend(doc),
        &mut handled,
    );
}

/// The frames before the workspace's pictures have all landed paint their
/// cards again; a frame after that is the conversation at rest.
fn settle(app: &mut ChatApp, frames: &mut Frames) {
    for _ in 0..3 {
        frames.draw(app);
        app.images
            .block_until_idle(std::time::Duration::from_secs(30));
    }
    frames.draw(app);
}

#[test]
fn a_wheel_step_lays_out_nothing_and_draws_only_the_rows_it_brings_into_view() {
    let (mut app, mut doc) = long_channel(300);
    let mut frames = Frames::new();
    let (first, _) = frames.draw(&mut app);
    assert!(
        first.measured > 0 && first.measured < 300,
        "the first frame measures the rows standing in view, not every row: {first:?}"
    );
    settle(&mut app, &mut frames);
    // The pointer rests over the timeline's gutter, where no row lifts.
    let area = app.messages_rect();
    feed(
        &mut app,
        &mut doc,
        &ui::synthetic_input::cursor_moved(area.x0 + 2.0, area.center().y),
    );
    let (_, mut before) = frames.draw(&mut app);
    let mut came = 0;
    for step in 0..24 {
        let scroll = app.scroll;
        feed(
            &mut app,
            &mut doc,
            &ui::synthetic_input::mouse_wheel(0.0, 1.0),
        );
        let (counts, shown) = frames.draw(&mut app);
        assert!(app.scroll > scroll, "step {step} scrolled back");
        let seen: HashSet<&String> = before.iter().collect();
        let entered = shown.iter().filter(|id| !seen.contains(id)).count();
        came += entered;
        assert!(
            counts.measured < 60,
            "step {step} measured only what stands near the view: {counts:?}"
        );
        assert_eq!(
            counts.drawn, entered,
            "step {step} drew only the {entered} rows that came into view: {counts:?}"
        );
        assert_eq!(counts.reused + counts.drawn, shown.len());
        before = shown;
    }
    assert!(came > 3, "the steps brought rows into view");
}

/// A timeline of thousands of messages measures what stands in view, and the rows a step
/// reaches as the reader scrolls: no frame of it walks every row of the conversation.
#[test]
fn a_long_timeline_measures_what_stands_in_view_and_no_more() {
    let (mut app, mut doc) = long_channel(2_000);
    let mut frames = Frames::new();
    let (first, shown) = frames.draw(&mut app);
    assert!(
        first.measured > 0 && first.measured < 200,
        "the first frame measures the rows in view and a margin, not two thousand: {first:?}"
    );
    assert!(!shown.is_empty(), "the newest rows stand in view");
    let first_total = app.timeline_total_height;
    settle(&mut app, &mut frames);
    let area = app.messages_rect();
    feed(
        &mut app,
        &mut doc,
        &ui::synthetic_input::cursor_moved(area.x0 + 2.0, area.center().y),
    );
    // The reader scrolls back through the conversation: every step measures what its
    // window reaches and no more, so a step costs a screenful however long the chat
    // has run.
    let mut measured = 0;
    let mut moved = 0;
    for step in 0..300 {
        let scroll = app.scroll;
        feed(
            &mut app,
            &mut doc,
            &ui::synthetic_input::mouse_wheel(0.0, 1.0),
        );
        let (counts, _) = frames.draw(&mut app);
        // A step that measured rows stands where its anchor was kept, not where the wheel
        // pushed it: the row the reader was reading stays under the pointer.
        assert!(
            app.scroll >= scroll,
            "step {step} does not move forward through history"
        );
        if app.scroll > scroll {
            moved += 1;
        }
        assert!(
            counts.measured < 120,
            "step {step} measures only what its window reaches: {counts:?}"
        );
        measured += counts.measured;
    }
    assert!(measured > 0, "scrolling back measured the rows it reached");
    assert!(moved > 100, "the steps moved back through {moved} rows");
    // What stands there now is the height of two thousand rows, most of them measured as
    // the reader scrolled past, and not the screenful the first frame measured.
    assert!(
        app.timeline_total_height > first_total * 1.5,
        "the timeline stands at the height of its rows: {} against {}",
        app.timeline_total_height,
        first_total
    );
}

#[test]
fn a_keystroke_in_the_composer_repaints_no_row() {
    let (mut app, mut doc) = long_channel(300);
    let mut frames = Frames::new();
    settle(&mut app, &mut frames);
    app.focus = Focus::Main;
    app.set_composer_focus();
    for ch in ["h", "i", "!"] {
        feed(&mut app, &mut doc, &ui::synthetic_input::char_key(ch));
        let (counts, shown) = frames.draw(&mut app);
        assert!(!shown.is_empty());
        assert_eq!(
            counts,
            TimelineCounts {
                measured: 0,
                drawn: 0,
                reused: shown.len(),
            },
            "typing {ch:?} kept every row"
        );
    }
}

#[test]
fn a_row_the_pointer_comes_over_is_painted_again_and_the_others_are_kept() {
    let (mut app, mut doc) = long_channel(300);
    let mut frames = Frames::new();
    settle(&mut app, &mut frames);
    // The thread link, the reactions and the picture cards lift under the
    // pointer: moving the pointer across the column repaints at most the
    // rows it left and entered.
    let area = app.messages_rect();
    let mut y = area.y0 + 4.0;
    for _ in 0..64 {
        if y >= area.y1 {
            break;
        }
        feed(
            &mut app,
            &mut doc,
            &ui::synthetic_input::cursor_moved(area.x0 + 120.0, y),
        );
        let (counts, shown) = frames.draw(&mut app);
        assert_eq!(counts.measured, 0);
        assert!(
            counts.drawn <= 2,
            "a pointer move repaints at most the rows it left and entered: {counts:?}"
        );
        assert_eq!(counts.reused + counts.drawn, shown.len());
        y += 23.0;
    }
}

#[test]
fn a_kept_row_paints_what_painting_it_afresh_does() {
    // The demo's lines over and over: pictures, reactions, threads.
    let (mut app, mut doc) = long_channel(40);
    // The hovered row's bar at rest: its fade is not what is compared.
    app.message_bar.settle_for_visual_review();
    let mut frames = Frames::new();
    settle(&mut app, &mut frames);
    let area = app.messages_rect();
    let pointer = [
        (area.x0 + 2.0, area.center().y),
        (area.x0 + 120.0, area.y1 - 60.0),
        (area.x0 + 300.0, area.center().y),
    ];
    let mut compared = 0;
    for (step, (x, y)) in pointer.iter().cycle().take(18).enumerate() {
        feed(
            &mut app,
            &mut doc,
            &ui::synthetic_input::cursor_moved(*x, *y),
        );
        feed(
            &mut app,
            &mut doc,
            &ui::synthetic_input::mouse_wheel(0.0, 1.0),
        );
        // Pictures the step brought into view are decoded off the thread;
        // one that lands between the kept frame and the fresh one is a
        // different picture, not a stale row, so they land first. The three
        // frames are painted at one instant, so a caret's blink cannot move
        // between them on a slow run either.
        frames.draw(&mut app);
        app.images
            .block_until_idle(std::time::Duration::from_secs(30));
        let (kept, afresh, again) = ui::time::testing::with_monotonic_time(ui::time::now(), || {
            let kept = frames.scene(&mut app);
            assert!(
                app.timeline.borrow().counts().reused > 0,
                "step {step} kept rows"
            );
            *app.timeline.borrow_mut() = crate::timeline::Timeline::default();
            let afresh = frames.scene(&mut app);
            *app.timeline.borrow_mut() = crate::timeline::Timeline::default();
            let again = frames.scene(&mut app);
            (kept, afresh, again)
        });
        // A frame that does not paint the same twice from scratch says
        // nothing about the kept rows.
        if !afresh.encoding_eq(&again) {
            continue;
        }
        compared += 1;
        assert!(
            kept.encoding_eq(&afresh),
            "step {step}: the frame from kept rows is the frame painted afresh"
        );
    }
    assert!(compared >= 9, "{compared} of 18 steps compared");
}

#[test]
fn a_received_message_lays_out_and_draws_only_its_row() {
    // A channel of 300 messages someone else wrote, as a window over the
    // workspace reads it, scrolled to its end.
    let mut doc = crate::testing::demo_doc();
    let mut app = ChatApp::new_ephemeral();
    app.attach_workspace(&doc);
    app.sync(&doc.chat, &automerge::ROOT);
    let channel = app.active_channel.clone();
    for i in 0..300 {
        crate::testing::say(
            &mut doc,
            "u_aria",
            &channel,
            &format!("Line {i} of a long day"),
        );
    }
    app.sync(&doc.chat, &automerge::ROOT);
    let mut frames = Frames::new();
    settle(&mut app, &mut frames);
    assert!(!app.take_structure_dirty());
    let mut took = Vec::new();
    let mut report = String::new();
    let idle = std::time::Instant::now();
    app.canonical_dirty = true;
    let ((), scopes) = ui::tilo_resource::profiling::observe(|| {
        frames.draw(&mut app);
    });
    let _ = writeln!(report, "idle draw {:?} {scopes:?}", idle.elapsed());
    for i in 0..8 {
        let id = crate::testing::say(&mut doc, "u_milo", &channel, &format!("News {i}"));
        let started = std::time::Instant::now();
        let ((), sync_scopes) =
            ui::tilo_resource::profiling::observe(|| app.sync(&doc.chat, &automerge::ROOT));
        let synced = started.elapsed();
        let ((counts, shown), draw_scopes) =
            ui::tilo_resource::profiling::observe(|| frames.draw(&mut app));
        let total = started.elapsed();
        took.push((synced, total.checked_sub(synced).unwrap_or_default()));
        let _ = writeln!(report, "sync {sync_scopes:?}\ndraw {draw_scopes:?}");
        assert!(shown.contains(&id), "the new message shows: {shown:?}");
        assert!(
            !app.take_structure_dirty(),
            "a received message rebuilds no layout tree"
        );
        assert_eq!(
            counts.measured, 1,
            "only the new row is laid out: {counts:?}"
        );
        assert_eq!(counts.drawn, 1, "only the new row is painted: {counts:?}");
        assert_eq!(counts.reused + counts.drawn, shown.len());
    }
    eprintln!("{report}received message (sync, draw): {took:?}");
}

#[test]
fn a_channel_keeps_its_answers_in_a_thread_and_a_direct_conversation_shows_them_inline() {
    use crate::record::{Chat, Participant};
    use ui::tilo_resource::Id;

    let mut doc = crate::testing::demo_doc();
    let mut app = ChatApp::new_ephemeral();
    app.attach_workspace(&doc);
    app.reload(&doc);
    let mut frames = Frames::new();

    // The rows the timeline stands on, as the view groups them.
    let rows = |app: &ChatApp| -> Vec<String> {
        app.timeline_groups()
            .iter()
            .flat_map(|group| group.message_indices.iter())
            .filter_map(|index| app.messages.get(*index))
            .map(|message| message.id.clone())
            .collect()
    };

    // `#design-crit` holds the three answers to one message as a thread: the
    // timeline stands on the message they answer, and the link into them.
    let chat = crate::projection::read(&doc.chat);
    app.open_conversation(&Id::from_raw("c_crit"), &chat);
    assert!(app.keeps_threads(), "a channel keeps its threads");
    let shown = rows(&app);
    assert!(shown.contains(&"m_concept".to_owned()), "{shown:?}");
    assert!(
        !shown.iter().any(|id| id.starts_with("m_thread_")),
        "the channel's answers stay behind their thread: {shown:?}"
    );

    // The same workspace, spoken into a conversation with an agent: the
    // person's message and the agent's answer to it stand in the timeline,
    // in the order they were written.
    let dm = Chat::direct_id([
        Participant::person(crate::model::ME),
        Participant::agent("notes"),
    ]);
    let asked = crate::testing::say(
        &mut doc,
        crate::model::ME,
        dm.as_str(),
        "Fasse die Besprechungsnotizen zusammen.",
    );
    crate::testing::apply_part_as(
        &mut doc,
        "agent:notes",
        crate::demo::post(
            dm.as_str(),
            "Drei Punkte: Budget steht, Termin am Freitag, Ada schreibt das Protokoll.",
            Some(&asked),
        ),
    )
    .expect("the agent answers");
    app.reload(&doc);
    let chat = crate::projection::read(&doc.chat);
    app.open_agent("notes", &chat);
    assert!(
        !app.keeps_threads(),
        "a direct conversation has no threads to fold its answers into"
    );
    let answered = app
        .messages
        .iter()
        .find(|message| message.author.as_str() == "agent:notes")
        .map(|message| message.id.clone())
        .expect("the conversation holds the agent's answer");
    assert_eq!(
        rows(&app),
        vec![asked.clone(), answered.clone()],
        "the answer stands right under the message it answers"
    );
    // And the frame paints it there, as a row of its own.
    let (_, painted) = frames.draw(&mut app);
    assert!(
        painted.contains(&answered),
        "the agent's answer is painted in the conversation: {painted:?}"
    );
    assert!(
        !app.bar_actions(&asked)
            .contains(&crate::app::MenuAction::ReplyInThread),
        "an answer is not hidden behind a thread the conversation does not have"
    );
}
